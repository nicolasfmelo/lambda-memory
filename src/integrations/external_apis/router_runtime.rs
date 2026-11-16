//! OpenRouter DTOs are dressed here; runtime scope and review remain application-owned.
use super::router_http::{
    DEFAULT_BASE, INVALID_PROVIDER_RESPONSE, RUNTIME_SOURCE_SLOTS, RouterEmbeddingClient,
    RouterHttp, RouterLunaSelector, RouterQueryPlanner, validate_base_url,
};
use crate::domain_models::capture::CaptureEvent;
use crate::domain_models::consolidation::JobSnapshot;
use crate::domain_models::embeddings::{Embedding, EmbeddingSpace};
use crate::domain_models::memory_tree::{
    CandidateLink, CurationAnswer, CurationOp, CurationView, LinkRelation, Slot, SplitAnswer,
    StoreKind, TreeNode, TreeRoot, roots,
};
use crate::domain_models::runtime::{RuntimeError, RuntimeHit};
use crate::domain_models::selection_context::{
    CandidateValidity, SelectionContext, SelectionIntent,
};
use crate::domain_models::{
    Memory, MemoryError, RecallRequest, SelectionRequest, SourceId, SupportedSelectionOutcome,
};
use crate::integrations::embedding_ports::EmbeddingClient;
use crate::integrations::memory_ports::MemoryQueryPlanner;
use crate::integrations::openrouter_embeddings::OpenRouterEmbeddingConfig;
use crate::integrations::provider_config::ProviderConfig;
use serde_json::{Value, json};

mod reflection;
pub struct RouterRuntimeProviders {
    http: RouterHttp,
    planner: RouterQueryPlanner,
    embedder: RouterEmbeddingClient,
    selector: RouterLunaSelector,
}
impl RouterRuntimeProviders {
    pub fn new(
        secret: &str,
        models: ProviderConfig,
        dimensions: usize,
    ) -> Result<Self, RuntimeError> {
        Self::with_base(None, secret, models, dimensions)
    }
    /// `base` is the optional `LAMBDA_MEMORY_ROUTER_BASE_URL`; `None` keeps the OpenRouter default.
    pub fn with_base(
        base: Option<&str>,
        secret: &str,
        models: ProviderConfig,
        dimensions: usize,
    ) -> Result<Self, RuntimeError> {
        if !(128..=3072).contains(&dimensions) {
            return Err(RuntimeError::InvalidInput(
                "runtime embedding dimensions must be 128..3072",
            ));
        }
        if models.generator_candidates != ["anthropic/claude-haiku-5.5"]
            || models.embedding_model != "google/gemini-embedding-2"
            || models.selector_model != "openai/gpt-6-luna-decisions"
        {
            return Err(RuntimeError::InvalidInput(
                "unreviewed runtime model profile",
            ));
        }
        // Construction never contacts the provider: a failure here is local configuration.
        let base = base.unwrap_or(DEFAULT_BASE);
        validate_base_url(base)
            .map_err(|_| RuntimeError::InvalidInput("LAMBDA_MEMORY_ROUTER_BASE_URL rejected"))?;
        let http = RouterHttp::runtime_with_base(base, secret).map_err(|_| {
            RuntimeError::InvalidInput("OpenRouter key or client configuration rejected")
        })?;
        let planner =
            RouterQueryPlanner::new(http.clone(), models.generator_candidates[0].clone(), 256)
                .map_err(|_| {
                    RuntimeError::InvalidInput("OpenRouter planner configuration rejected")
                })?;
        Ok(Self {
            http: http.clone(),
            planner,
            embedder: RouterEmbeddingClient {
                http: http.clone(),
                config: OpenRouterEmbeddingConfig {
                    dimensions,
                    index_revision: "runtime-gemini-retrieval-v2".into(),
                },
            },
            selector: RouterLunaSelector { http },
        })
    }
    /// Same wiring as `new`, but over a loopback fake; compiled only for in-crate tests.
    #[cfg(test)]
    pub(crate) fn mock(base: &str, dimensions: usize) -> Self {
        let http = RouterHttp::mock_runtime(base);
        Self {
            http: http.clone(),
            planner: RouterQueryPlanner::new(
                http.clone(),
                "anthropic/claude-haiku-5.5".into(),
                256,
            )
            .unwrap(),
            embedder: RouterEmbeddingClient {
                http: http.clone(),
                config: OpenRouterEmbeddingConfig {
                    dimensions,
                    index_revision: "runtime-gemini-retrieval-v2".into(),
                },
            },
            selector: RouterLunaSelector { http },
        }
    }
    pub fn space(&self) -> Result<EmbeddingSpace, RuntimeError> {
        self.embedder
            .config
            .space()
            .map_err(|_| RuntimeError::ProviderUnavailable)
    }
    pub async fn derive(
        &self,
        snapshot: &JobSnapshot,
    ) -> Result<Vec<(SourceId, String)>, RuntimeError> {
        if snapshot.sources.is_empty() || snapshot.sources.len() > RUNTIME_SOURCE_SLOTS.len() {
            return Err(RuntimeError::InvalidInput("source-bound batch size"));
        }
        let input = json!({"sources": snapshot.sources.iter().enumerate().map(|(index, event)| json!({
            "source": RUNTIME_SOURCE_SLOTS[index], "kind": format!("{:?}", event.kind),
            "outcome": format!("{:?}", event.outcome), "content": event.content
        })).collect::<Vec<_>>()});
        let raw = self
            .http
            .consolidate_runtime(input)
            .await
            .map_err(|error| {
                if error.uncharged() {
                    RuntimeError::ProviderRejected
                } else {
                    RuntimeError::ProviderUnavailable
                }
            })?;
        decode_claims(&raw, snapshot)
    }

    pub async fn curate(
        &self,
        kind: StoreKind,
        sources: &[CaptureEvent],
        view: &CurationView,
    ) -> Result<CurationAnswer, RuntimeError> {
        check_bounds(sources.len(), view.map.len() + view.detailed.len(), true)?;
        let nodes: Vec<&TreeNode> = view.map.iter().chain(view.detailed.iter()).collect();
        let input = json!({"sources":source_values(sources), "map":view.map.iter().enumerate().map(|(i,n)| node_value(n,i,&nodes,false)).collect::<Vec<_>>(), "detailed":view.detailed.iter().enumerate().map(|(i,n)| node_value(n,i+view.map.len(),&nodes,true)).collect::<Vec<_>>(), "roots":(roots(kind).iter().map(|root| json!({"slot":root.as_str(),"children":view.root_children.iter().find(|(r,_)| r==root).map_or(0,|(_,count)| *count)})).collect::<Vec<_>>())});
        let raw = self
            .http
            .curate(kind, input)
            .await
            .map_err(tree_provider_error)?;
        let mut output = object(parse_tree(&raw)?, &["results"])?;
        let Value::Object(mut results) = output.remove("results").unwrap() else {
            return Err(RuntimeError::ProviderInvalidOutput);
        };
        if results.len() != sources.len() {
            return Err(RuntimeError::ProviderInvalidOutput);
        }
        let mut per_source = Vec::new();
        for slot in &RUNTIME_SOURCE_SLOTS[..sources.len()] {
            let Some(Value::Array(ops)) = results.remove(*slot) else {
                return Err(RuntimeError::ProviderInvalidOutput);
            };
            let mut decoded = Vec::new();
            for op in ops {
                let Value::Object(mut fields) = op else {
                    return Err(RuntimeError::ProviderInvalidOutput);
                };
                // Optional on the operation; the request schema always offers it explicitly.
                let valid_from = match fields.remove("valid_from") {
                    Some(Value::String(date)) => Some(date),
                    Some(Value::Null) | None => None,
                    _ => return Err(RuntimeError::ProviderInvalidOutput),
                };
                // Like valid_from, tolerate older fixtures without links. Malformed links
                // are unusable individually, never a reason to lose the source's operation.
                let links = match fields.remove("links") {
                    Some(Value::Array(links)) => links
                        .into_iter()
                        .filter_map(|link| {
                            let relation = match link.get("relation")?.as_str()? {
                                "depends_on" => LinkRelation::DependsOn,
                                "replaces" => LinkRelation::Replaces,
                                "related" => LinkRelation::Related,
                                _ => return None,
                            };
                            let to = link
                                .get("to")
                                .and_then(Value::as_str)
                                .and_then(|slot| node_index(slot, nodes.len()).ok())
                                .unwrap_or(usize::MAX);
                            Some(CandidateLink { to, relation })
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let mut v = object(
                    Value::Object(fields),
                    &["op", "node", "parent", "title", "content"],
                )?;
                let kind = take_string(&mut v, "op")?;
                if kind == "promote" {
                    decoded.push(CurationOp::Promote);
                    continue;
                }
                let node = take_optional_string(&mut v, "node")?;
                let parent = take_optional_string(&mut v, "parent")?;
                let title = take_optional_string(&mut v, "title")?;
                let content = take_optional_string(&mut v, "content")?;
                let bad = || RuntimeError::ProviderInvalidOutput;
                // Only the fields an operation uses are read; models often fill the others or swap
                // node and parent. An operation with no usable target or text is skipped on its own;
                // a source left with nothing counts as discarded downstream.
                let target_node = node
                    .as_deref()
                    .or(parent.as_deref())
                    .and_then(|slot| node_index(slot, nodes.len()).ok());
                let target_parent = parent
                    .as_deref()
                    .or(node.as_deref())
                    .and_then(|slot| parent_slot(slot, nodes.len()).ok());
                let text = title
                    .as_deref()
                    .zip(content.as_deref())
                    .map(|(t, c)| (fit_title(t), fit_content(c)));
                let op =
                    match kind.as_str() {
                        "discard" => Some(CurationOp::Discard),
                        "merge" => target_node.zip(text).map(|(node, (title, content))| {
                            CurationOp::Merge {
                                node,
                                title,
                                content,
                                links,
                            }
                        }),
                        "supersede" => target_node.zip(text).map(|(node, (title, content))| {
                            CurationOp::Supersede {
                                node,
                                title,
                                content,
                                valid_from,
                                links,
                            }
                        }),
                        "create" => target_parent.zip(text).map(|(parent, (title, content))| {
                            CurationOp::Create {
                                parent,
                                title,
                                content,
                                links,
                            }
                        }),
                        "expand" => target_node.map(|node| CurationOp::Expand { node }),
                        _ => return Err(bad()),
                    };
                decoded.extend(op);
            }
            per_source.push(decoded);
        }
        Ok(CurationAnswer { per_source })
    }
    pub async fn reflect(
        &self,
        input: &crate::domain_models::reflection::ReflectionInput,
    ) -> Result<crate::domain_models::reflection::ReflectionAnswer, RuntimeError> {
        reflection::reflect(&self.http, input).await
    }
    pub async fn draft_skill(
        &self,
        input: &crate::domain_models::skills::SkillDraftInput,
    ) -> Result<crate::domain_models::skills::SkillDraftAnswer, RuntimeError> {
        let value = json!({"node":{"title":input.node.title,"content":input.node.content,"path":input.path},"pitfalls":input.pitfalls.iter().map(|node|json!({"title":node.title,"content":node.content})).collect::<Vec<_>>(),"sessions":input.sessions});
        let raw = self
            .http
            .draft_skill(value)
            .await
            .map_err(tree_provider_error)?;
        let mut v = object(
            parse_tree(&raw)?,
            &["name", "description", "steps", "pitfalls"],
        )?;
        // Reject duplicate keys, which a Value map would otherwise collapse.
        let (mut quoted, mut escaped, mut fields) = (false, false, 0);
        for b in raw.bytes() {
            if escaped {
                escaped = false;
                continue;
            }
            match b {
                b'\\' if quoted => escaped = true,
                b'\"' => quoted = !quoted,
                b':' if !quoted => fields += 1,
                _ => {}
            }
        }
        if fields != 4 {
            return Err(RuntimeError::ProviderInvalidOutput);
        }
        let strings = |v: Option<Value>| -> Result<Vec<String>, RuntimeError> {
            let Some(Value::Array(items)) = v else {
                return Err(RuntimeError::ProviderInvalidOutput);
            };
            items
                .into_iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s),
                    _ => Err(RuntimeError::ProviderInvalidOutput),
                })
                .collect()
        };
        Ok(crate::domain_models::skills::SkillDraftAnswer {
            name: take_string(&mut v, "name")?,
            description: take_string(&mut v, "description")?,
            steps: strings(v.remove("steps"))?,
            pitfalls: strings(v.remove("pitfalls"))?,
        })
    }
    pub async fn extract_lessons(
        &self,
        input: &crate::domain_models::lessons::LessonInput,
    ) -> Result<crate::domain_models::lessons::LessonAnswer, RuntimeError> {
        let raw = self
            .http
            .extract_lessons(lesson_input_value(input))
            .await
            .map_err(tree_provider_error)?;
        decode_lessons(&raw, input.sources.len())
    }
    pub async fn classify_promotions(
        &self,
        nodes: &[crate::domain_models::promotion::PromotionCandidate],
    ) -> Result<Vec<(usize, crate::domain_models::promotion::PromotionVerdict)>, RuntimeError> {
        use crate::domain_models::promotion::PromotionVerdict;
        if nodes.is_empty() || nodes.len() > 16 {
            return Err(RuntimeError::InvalidInput("promotion batch"));
        }
        let input = json!({"nodes":nodes.iter().enumerate().map(|(i,c)| json!({"slot":format!("n{i}"),"root":c.node.root.as_str(),"path":c.path,"title":c.node.title,"content":c.node.content})).collect::<Vec<_>>()});
        let raw = self
            .http
            .classify_promotions(input)
            .await
            .map_err(tree_provider_error)?;
        let mut output = object(parse_tree(&raw)?, &["results"])?;
        let Some(Value::Object(results)) = output.remove("results") else {
            return Err(RuntimeError::ProviderInvalidOutput);
        };
        // Value maps collapse duplicate JSON keys. This schema has only one outer field and
        // string-valued slot fields, so counting unquoted separators also rejects duplicates.
        let (mut quoted, mut escaped, mut fields) = (false, false, 0);
        for byte in raw.bytes() {
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' if quoted => escaped = true,
                b'"' => quoted = !quoted,
                b':' if !quoted => fields += 1,
                _ => {}
            }
        }
        if results.len() != nodes.len() || fields != nodes.len() + 1 {
            return Err(RuntimeError::ProviderInvalidOutput);
        }
        results
            .into_iter()
            .map(|(slot, value)| {
                let index = node_index(&slot, nodes.len())?;
                let verdict = match value.as_str() {
                    Some("user") => PromotionVerdict::User,
                    Some("project") => PromotionVerdict::Project,
                    _ => return Err(RuntimeError::ProviderInvalidOutput),
                };
                Ok((index, verdict))
            })
            .collect()
    }
    pub async fn refresh_node(
        &self,
        node: &TreeNode,
        children: &[TreeNode],
    ) -> Result<(String, String), RuntimeError> {
        check_bounds(0, children.len() + 1, false)?;
        let nodes: Vec<&TreeNode> = std::iter::once(node).chain(children.iter()).collect();
        let input = json!({"node":node_value(node,0,&nodes,true),"children":children.iter().enumerate().map(|(i,n)|node_value(n,i+1,&nodes,true)).collect::<Vec<_>>()});
        decode_tree_text(
            &self
                .http
                .refresh_node(input)
                .await
                .map_err(tree_provider_error)?,
        )
    }
    pub async fn split_node(
        &self,
        node: &TreeNode,
        children: &[TreeNode],
        sources: &[CaptureEvent],
    ) -> Result<SplitAnswer, RuntimeError> {
        let root_split = node.depth == 0;
        check_bounds(
            sources.len(),
            children.len() + usize::from(!root_split),
            false,
        )?;
        let nodes: Vec<&TreeNode> = if root_split {
            children.iter().collect()
        } else {
            std::iter::once(node).chain(children.iter()).collect()
        };
        let mut parent = node_value(node, 0, &nodes, true);
        if root_split {
            parent["slot"] = json!(node.root.as_str());
        }
        let mut input = json!({"node":parent,"children":children.iter().enumerate().map(|(i,n)|node_value(n,i+usize::from(!root_split),&nodes,true)).collect::<Vec<_>>()});
        if !root_split {
            input["sources"] = json!(source_values(sources));
        }
        let raw = self
            .http
            .split_node(input)
            .await
            .map_err(tree_provider_error)?;
        let mut v = object(
            parse_tree(&raw)?,
            &["node_title", "node_content", "children"],
        )?;
        let node_title = fit_title(&take_string(&mut v, "node_title")?);
        let node_content = fit_content(&take_string(&mut v, "node_content")?);
        let Some(Value::Array(items)) = v.remove("children") else {
            return Err(RuntimeError::ProviderInvalidOutput);
        };
        let mut result = Vec::new();
        for item in items {
            let mut child = object(item, &["title", "content", "sources"])?;
            let title = fit_title(&take_string(&mut child, "title")?);
            let content = fit_content(&take_string(&mut child, "content")?);
            let Some(Value::Array(slots)) = child.remove("sources") else {
                return Err(RuntimeError::ProviderInvalidOutput);
            };
            let mut indexes = Vec::new();
            for slot in slots {
                let Value::String(slot) = slot else {
                    return Err(RuntimeError::ProviderInvalidOutput);
                };
                let index = if node.depth == 0 {
                    node_index(&slot, nodes.len())?
                } else {
                    RUNTIME_SOURCE_SLOTS[..sources.len()]
                        .iter()
                        .position(|s| *s == slot)
                        .ok_or(RuntimeError::ProviderInvalidOutput)?
                };
                indexes.push(index);
            }
            result.push((title, content, indexes));
        }
        Ok(SplitAnswer {
            node_title,
            node_content,
            children: result,
        })
    }
    pub async fn dependency_node(
        &self,
        node: &TreeNode,
        sources: &[CaptureEvent],
        dependency: &TreeNode,
    ) -> Result<(String, String), RuntimeError> {
        check_bounds(sources.len(), 2, false)?;
        let input = json!({"node":{"title":node.title,"content":node.content},"sources":source_values(sources),"dependency":{"title":dependency.title,"content":dependency.content}});
        decode_tree_text(
            &self
                .http
                .dependency_node(input)
                .await
                .map_err(tree_provider_error)?,
        )
    }
    pub async fn rebuild_node(
        &self,
        node: &TreeNode,
        sources: &[CaptureEvent],
    ) -> Result<(String, String), RuntimeError> {
        check_bounds(sources.len(), 1, false)?;
        let input =
            json!({"node":node_value(node,0,&[node],true),"sources":source_values(sources)});
        decode_tree_text(
            &self
                .http
                .rebuild_node(input)
                .await
                .map_err(tree_provider_error)?,
        )
    }

    /// Search terms for one recall. `ProviderInvalidOutput` means the call completed and was
    /// billed but the model output was unusable (the only error the caller may degrade on);
    /// every other failure maps like the other provider calls.
    pub async fn plan(&self, request: &RecallRequest) -> Result<Vec<String>, RuntimeError> {
        self.planner
            .plan(request)
            .await
            .map(|plan| plan.terms)
            .map_err(|error| match error {
                MemoryError::InvalidInput(reason) if reason == INVALID_PROVIDER_RESPONSE => {
                    RuntimeError::ProviderInvalidOutput
                }
                other => provider_error(other),
            })
    }
    pub async fn embed_document(&self, content: &str) -> Result<Embedding, RuntimeError> {
        self.embedder
            .embed(&document_input(content))
            .await
            .map_err(provider_error)
    }
    pub async fn embed_query(&self, content: &str) -> Result<Embedding, RuntimeError> {
        self.embedder
            .embed(&query_input(content))
            .await
            .map_err(provider_error)
    }
    pub async fn select(
        &self,
        request: &RecallRequest,
        terms: &[String],
        hits: &[RuntimeHit],
        now_ms: u64,
    ) -> Result<(Vec<Memory>, SupportedSelectionOutcome), RuntimeError> {
        let candidates = hits
            .iter()
            .map(|hit| {
                let revision = &hit.item.revision;
                Ok(Memory {
                    id: crate::domain_models::MemoryId::new(revision.id.as_str())
                        .map_err(|_| RuntimeError::StorageUnavailable)?,
                    owner: revision.owner.clone(),
                    actor: request.actor.clone(),
                    project: revision.project.clone(),
                    text: revision.content.clone(),
                    source: revision
                        .sources
                        .first()
                        .ok_or(RuntimeError::StorageUnavailable)?
                        .clone(),
                    active: true,
                    restricted: false,
                })
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let validity = candidates
            .iter()
            .zip(hits)
            .map(|(memory, hit)| CandidateValidity {
                memory: memory.id.clone(),
                source: memory.source.clone(),
                owner: memory.owner.clone(),
                project: memory.project.clone(),
                valid_from_ms: hit
                    .item
                    .sources
                    .iter()
                    .map(|source| source.recorded_at_ms)
                    .max()
                    .unwrap_or(now_ms),
                expires_at_ms: None,
            })
            .collect();
        let context = SelectionContext {
            intent: SelectionIntent::TopicRecall,
            owner: request.owner.clone(),
            project: request.project.clone(),
            as_of_ms: now_ms,
            validity,
        };
        let selection = SelectionRequest {
            task: request.task.clone(),
            retrieval_terms: terms.to_vec(),
            candidates,
        };
        let answer = self
            .selector
            .evaluate_contractual(&selection, &context)
            .await
            .map_err(provider_error)?;
        Ok((selection.candidates, answer))
    }
}
pub(crate) fn lesson_input_value(input: &crate::domain_models::lessons::LessonInput) -> Value {
    json!({"exchanges":input.exchanges.iter().map(|e| json!({"slot":format!("s{}",e.slot),"kind":"exchange","prompt":e.prompt,"reply":e.reply})).collect::<Vec<_>>(),
        "session_digest":input.digest.iter().map(|d| {
            let mut v = json!({"kind":if d.fixed_by.is_some() {"failure_fix"} else {"failure"},"failed":d.failed,"error":d.error,"evidence":d.evidence.iter().map(|i|format!("s{i}")).collect::<Vec<_>>()});
            if let Some(key) = &d.fixed_by { v["fixed_by"] = json!(key); } v
        }).collect::<Vec<_>>()})
}
fn decode_lessons(
    raw: &str,
    count: usize,
) -> Result<crate::domain_models::lessons::LessonAnswer, RuntimeError> {
    use crate::domain_models::lessons::*;
    let bad = || RuntimeError::ProviderInvalidOutput;
    let mut output = object(parse_tree(raw)?, &["tasks", "lessons"])?;
    let Some(Value::Array(tasks)) = output.remove("tasks") else {
        return Err(bad());
    };
    let Some(Value::Array(lessons)) = output.remove("lessons") else {
        return Err(bad());
    };
    if tasks.len() > 12 || lessons.len() > 6 {
        return Err(bad());
    }
    // Value maps collapse duplicate keys. All objects have fixed fields, so an exact count
    // of unquoted field separators additionally rejects duplicate keys at every depth.
    let (mut quoted, mut escaped, mut fields) = (false, false, 0);
    for byte in raw.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b':' if !quoted => fields += 1,
            _ => {}
        }
    }
    if fields != 2 + tasks.len() * 3 + lessons.len() * 7 {
        return Err(bad());
    }
    let slots = |value: Option<Value>| -> Result<Vec<usize>, RuntimeError> {
        let Some(Value::Array(items)) = value else {
            return Err(bad());
        };
        items
            .into_iter()
            .map(|s| {
                let Value::String(s) = s else {
                    return Err(bad());
                };
                s.strip_prefix('s')
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|i| *i < count && s == format!("s{i}"))
                    .ok_or_else(bad)
            })
            .collect()
    };
    Ok(LessonAnswer {
        tasks: tasks
            .into_iter()
            .map(|t| {
                let mut t = object(t, &["title", "outcome", "evidence"])?;
                Ok(LessonTask {
                    title: take_string(&mut t, "title")?,
                    evidence: slots(t.remove("evidence"))?,
                    outcome: match take_string(&mut t, "outcome")?.as_str() {
                        "succeeded" => TaskOutcome::Succeeded,
                        "failed" => TaskOutcome::Failed,
                        "corrected_by_user" => TaskOutcome::CorrectedByUser,
                        "unknown" => TaskOutcome::Unknown,
                        _ => return Err(bad()),
                    },
                })
            })
            .collect::<Result<_, RuntimeError>>()?,
        lessons: lessons
            .into_iter()
            .map(|l| {
                let mut l = object(
                    l,
                    &[
                        "kind",
                        "scope",
                        "title",
                        "applies_when",
                        "guidance",
                        "task",
                        "evidence",
                    ],
                )?;
                Ok(Lesson {
                    kind: match take_string(&mut l, "kind")?.as_str() {
                        "strategy" => LessonKind::Strategy,
                        "pitfall" => LessonKind::Pitfall,
                        _ => return Err(bad()),
                    },
                    scope: match take_string(&mut l, "scope")?.as_str() {
                        "project" => LessonScope::Project,
                        "user" => LessonScope::User,
                        _ => return Err(bad()),
                    },
                    title: take_string(&mut l, "title")?,
                    applies_when: take_string(&mut l, "applies_when")?,
                    guidance: take_string(&mut l, "guidance")?,
                    task: l
                        .remove("task")
                        .and_then(|v| v.as_u64())
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(bad)?,
                    evidence: slots(l.remove("evidence"))?,
                })
            })
            .collect::<Result<_, RuntimeError>>()?,
    })
}
/// Refused before billing → rejected; a completed but unusable answer (cut by the output limit,
/// wrong model, bad shape) → invalid output, which does not stop background work; transport → unavailable.
fn tree_provider_error(error: super::router_http::RouterError) -> RuntimeError {
    if error.uncharged() {
        RuntimeError::ProviderRejected
    } else if error == super::router_http::RouterError::Response {
        RuntimeError::ProviderInvalidOutput
    } else {
        RuntimeError::ProviderUnavailable
    }
}
/// Node slots are generated (`n0`…); 128 covers a full root (64 children) and the capped tree map (96).
const TREE_NODE_SLOTS: usize = 128;
fn check_bounds(sources: usize, nodes: usize, required: bool) -> Result<(), RuntimeError> {
    if sources > 32 || nodes > TREE_NODE_SLOTS || (required && sources == 0) {
        Err(RuntimeError::InvalidInput("tree slot bounds"))
    } else {
        Ok(())
    }
}
fn source_values(sources: &[CaptureEvent]) -> Vec<Value> {
    sources.iter().enumerate().map(|(i,event)| json!({"source":RUNTIME_SOURCE_SLOTS[i],"kind":format!("{:?}",event.kind).to_lowercase(),"outcome":format!("{:?}",event.outcome),"content":event.content,"recorded_at_ms":event.recorded_at_ms})).collect()
}
fn node_value(node: &TreeNode, index: usize, nodes: &[&TreeNode], detailed: bool) -> Value {
    let parent = node.parent.as_ref().and_then(|id| {
        if *id == node.root.memory_id() {
            Some(node.root.as_str().to_owned())
        } else {
            nodes
                .iter()
                .position(|n| n.memory == *id)
                .map(|i| format!("n{i}"))
        }
    });
    let mut value = json!({"slot":format!("n{index}"),"title":node.title,"depth":node.depth,"parent_slot":parent});
    if detailed {
        value["content"] = json!(node.content);
    }
    value
}
fn parse_tree(raw: &str) -> Result<Value, RuntimeError> {
    serde_json::from_str(raw).map_err(|_| RuntimeError::ProviderInvalidOutput)
}
fn object(value: Value, keys: &[&str]) -> Result<serde_json::Map<String, Value>, RuntimeError> {
    let Value::Object(map) = value else {
        return Err(RuntimeError::ProviderInvalidOutput);
    };
    if map.len() != keys.len() || keys.iter().any(|k| !map.contains_key(*k)) {
        return Err(RuntimeError::ProviderInvalidOutput);
    }
    Ok(map)
}
fn take_string(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, RuntimeError> {
    match map.remove(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(RuntimeError::ProviderInvalidOutput),
    }
}
fn take_optional_string(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, RuntimeError> {
    match map.remove(key) {
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Null) => Ok(None),
        _ => Err(RuntimeError::ProviderInvalidOutput),
    }
}
fn node_index(slot: &str, count: usize) -> Result<usize, RuntimeError> {
    (0..count)
        .find(|i| slot == format!("n{i}"))
        .ok_or(RuntimeError::ProviderInvalidOutput)
}
// Preserve recognized roots for pure kind validation, which drops and counts invalid parents.
fn parent_slot(slot: &str, count: usize) -> Result<Slot, RuntimeError> {
    let root = match slot {
        "decisions" => Some(TreeRoot::Decisions),
        "preferences" => Some(TreeRoot::Preferences),
        "architecture" => Some(TreeRoot::Architecture),
        "open_items" => Some(TreeRoot::OpenItems),
        "working_rules" => Some(TreeRoot::WorkingRules),
        "general_lessons" => Some(TreeRoot::GeneralLessons),
        "lessons" => Some(TreeRoot::Lessons),
        _ => None,
    };
    if let Some(root) = root {
        Ok(Slot::Root(root))
    } else {
        Ok(Slot::Node(node_index(slot, count)?))
    }
}
fn decode_tree_text(raw: &str) -> Result<(String, String), RuntimeError> {
    let mut v = object(parse_tree(raw)?, &["title", "content"])?;
    Ok((
        fit_title(&take_string(&mut v, "title")?),
        fit_content(&take_string(&mut v, "content")?),
    ))
}
/// Node titles are one line of at most 80 characters; a longer model title is cut at a word boundary.
fn fit_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if line.chars().count() <= 80 {
        return line;
    }
    let cut: String = line.chars().take(80).collect();
    match cut.rfind(' ') {
        Some(i) if i > 40 => cut[..i].trim_end().to_owned(),
        _ => cut,
    }
}
/// Node summaries are at most 1200 UTF-8 bytes; a longer one is cut at the last sentence end that fits.
fn fit_content(text: &str) -> String {
    let text = text.trim();
    if text.len() <= 1200 {
        return text.to_owned();
    }
    let mut end = 1200;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    match cut.rfind(['.', '!', '?', '\n']) {
        Some(i) if i > 600 => cut[..=i].trim_end().to_owned(),
        _ => cut.trim_end().to_owned(),
    }
}
fn query_input(content: &str) -> String {
    format!("task: search result | query: {content}")
}
fn document_input(content: &str) -> String {
    format!("title: none | text: {content}")
}
fn provider_error(error: MemoryError) -> RuntimeError {
    if error == MemoryError::ProviderRejected {
        RuntimeError::ProviderRejected
    } else {
        RuntimeError::ProviderUnavailable
    }
}
fn decode_claims(
    raw: &str,
    snapshot: &JobSnapshot,
) -> Result<Vec<(SourceId, String)>, RuntimeError> {
    if snapshot.sources.is_empty() || snapshot.sources.len() > RUNTIME_SOURCE_SLOTS.len() {
        return Err(RuntimeError::ProviderUnavailable);
    }
    let Value::Object(mut value) =
        serde_json::from_str(raw).map_err(|_| RuntimeError::ProviderUnavailable)?
    else {
        return Err(RuntimeError::ProviderUnavailable);
    };
    if value.len() != 1 {
        return Err(RuntimeError::ProviderUnavailable);
    }
    let Some(Value::Object(mut claims)) = value.remove("claims") else {
        return Err(RuntimeError::ProviderUnavailable);
    };
    if claims.len() != snapshot.sources.len() {
        return Err(RuntimeError::ProviderUnavailable);
    }
    let mut output = Vec::with_capacity(claims.len());
    for (index, event) in snapshot.sources.iter().enumerate() {
        let Some(Value::String(content)) = claims.remove(RUNTIME_SOURCE_SLOTS[index]) else {
            return Err(RuntimeError::ProviderUnavailable);
        };
        if content.trim().is_empty() || content.len() > 8192 {
            return Err(RuntimeError::ProviderUnavailable);
        }
        output.push((event.source.clone(), content));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::capture::{CaptureEvent, WorkEventKind, WorkOutcome};
    use crate::domain_models::{ActorId, JobId, OwnerId, ProjectId};
    #[test]
    fn retrieval_inputs_and_dimension_profile_are_explicit() {
        assert_eq!(
            query_input("storage?"),
            "task: search result | query: storage?"
        );
        assert_eq!(
            document_input("Use SQLite."),
            "title: none | text: Use SQLite."
        );
        for dimensions in [128, 1536, 3072] {
            let providers = RouterRuntimeProviders::new(
                "synthetic-unused-key",
                ProviderConfig::default(),
                dimensions,
            )
            .unwrap();
            let space = providers.space().unwrap();
            assert_eq!(space.dimensions, dimensions);
            assert_eq!(space.revision, "runtime-gemini-retrieval-v2");
        }
        assert!(
            RouterRuntimeProviders::with_base(
                Some("http://example.com"),
                "synthetic-unused-key",
                ProviderConfig::default(),
                128
            )
            .is_err()
        );
        assert!(
            RouterRuntimeProviders::with_base(
                Some("https://proxy.example.com:8443/"),
                "synthetic-unused-key",
                ProviderConfig::default(),
                128
            )
            .is_ok()
        );
        for dimensions in [0, 127, 3073] {
            assert!(
                RouterRuntimeProviders::new(
                    "synthetic-unused-key",
                    ProviderConfig::default(),
                    dimensions
                )
                .is_err()
            );
        }
    }
    #[test]
    fn model_claims_require_exact_slots_and_map_to_native_source_identity() {
        let snapshot = JobSnapshot {
            id: JobId::new("job").unwrap(),
            owner: OwnerId::new("owner").unwrap(),
            project: ProjectId::new("project").unwrap(),
            watermark_ms: 1,
            sources: ["one", "two"]
                .into_iter()
                .map(|source| CaptureEvent {
                    source: SourceId::new(source).unwrap(),
                    owner: OwnerId::new("owner").unwrap(),
                    actor: ActorId::new("actor").unwrap(),
                    project: ProjectId::new("project").unwrap(),
                    recorded_at_ms: 1,
                    kind: WorkEventKind::Error,
                    outcome: WorkOutcome::Failed,
                    origin: "fixture://failed".into(),
                    content: "Migration failed; rollback succeeded.".into(),
                })
                .collect(),
        };
        let valid = r#"{"claims":{"s1":"Migration failed.","s0":"Rollback succeeded after the failed migration."}}"#;
        let claims = decode_claims(valid, &snapshot).unwrap();
        assert_eq!(
            claims,
            vec![
                (
                    snapshot.sources[0].source.clone(),
                    "Rollback succeeded after the failed migration.".into()
                ),
                (
                    snapshot.sources[1].source.clone(),
                    "Migration failed.".into()
                ),
            ]
        );
        for invalid in [
            r#"{"claims":{"s0":"Claim.","foreign":"Another."}}"#,
            r#"{"claims":{"s0":"Claim.","s2":"Another."}}"#,
            r#"{"claims":{"s0":"Claim."}}"#,
            r#"{"claims":{"s0":"","s1":"Another."}}"#,
            r#"{"claims":{"s0":["Claim.","Another."],"s1":"Another."}}"#,
            r#"{"claims":{"s0":"Claim.","s1":"Another.","s2":"Extra."}}"#,
            r#"{"claims":[{"source":"one","content":"Claim."},{"source":"two","content":"Another."}]}"#,
        ] {
            assert_eq!(
                decode_claims(invalid, &snapshot).unwrap_err(),
                RuntimeError::ProviderUnavailable
            );
        }
    }
}
