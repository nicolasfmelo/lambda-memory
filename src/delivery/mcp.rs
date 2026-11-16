//! Bounded authenticated MCP stdio. Wire JSON stays in delivery; parent owns idle dreaming.
use crate::domain_models::capture::{CaptureStatus, WorkEventKind, WorkOutcome};
use crate::domain_models::procedural::ActionEpisode;
use crate::domain_models::runtime::*;
use crate::domain_models::{MemoryId, ProjectId, RevisionId, SourceId};
use crate::services::runtime_service::MemoryRuntime;
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;
mod cursor;
mod diagnostics;
mod listing;
mod skills;
mod timeline;
mod tree;
mod ui;
use crate::domain_models::runtime_diagnostics::{DiagnosticsWarning, RecordedOperation};
use diagnostics::{activity_json, activity_request, recorded, status_json, warning_json};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const PROTOCOL_LATEST: &str = "2025-11-25";
const PROTOCOLS: &[&str] = &[PROTOCOL_LATEST, "2025-06-18", "2024-11-05"];
const TOKEN_MIN: usize = 32;
const TOKEN_MAX: usize = 1024;

pub fn validate_operator_token(token: &str) -> Result<(), RuntimeError> {
    if !(TOKEN_MIN..=TOKEN_MAX).contains(&token.len())
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(RuntimeError::InvalidInput("operator token rejected"));
    }
    Ok(())
}
pub fn tokens_match(left: &str, right: &str) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= (left.as_bytes().get(index).copied().unwrap_or(0)
            ^ right.as_bytes().get(index).copied().unwrap_or(0)) as usize;
    }
    std::hint::black_box(difference) == 0
}
#[derive(PartialEq, Eq)]
enum Phase {
    New,
    Initialized,
    Ready,
    Denied,
}
pub struct McpSession {
    runtime: Arc<MemoryRuntime>,
    token: String,
    phase: Phase,
}
impl McpSession {
    pub fn new(runtime: Arc<MemoryRuntime>, token: String) -> Result<Self, RuntimeError> {
        validate_operator_token(&token)?;
        Ok(Self {
            runtime,
            token,
            phase: Phase::New,
        })
    }
    pub fn is_denied(&self) -> bool {
        self.phase == Phase::Denied
    }
    pub async fn handle_frame(&mut self, frame: &str) -> Option<String> {
        if self.is_denied() {
            return None;
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Some(rpc_error(Value::Null, -32700, "frame too large").to_string());
        }
        let request: Value = match serde_json::from_str(frame) {
            Ok(value) => value,
            Err(_) => return Some(rpc_error(Value::Null, -32700, "parse error").to_string()),
        };
        let Some(object) = request.as_object() else {
            return Some(rpc_error(Value::Null, -32600, "invalid request").to_string());
        };
        let id = object.get("id").cloned();
        if id.as_ref().is_some_and(|id| {
            !(id.is_null()
                || id.is_i64()
                || id.is_u64()
                || id.as_str().is_some_and(|text| text.len() <= 512))
        }) || object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        {
            return Some(rpc_error(Value::Null, -32600, "invalid request").to_string());
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(
                rpc_error(id.unwrap_or(Value::Null), -32600, "invalid request").to_string(),
            );
        };
        // Valid notifications never receive replies and cannot invoke mutating tools.
        if id.is_none() {
            if method == "notifications/initialized" && self.phase == Phase::Initialized {
                self.phase = Phase::Ready;
            }
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        let reply_id = id.clone();
        let params = object.get("params").unwrap_or(&Value::Null);
        let reply = match method {
            "initialize" => self.initialize(id, params),
            "notifications/initialized" => {
                rpc_error(id, -32600, "initialized must be a notification")
            }
            // MCP allows ping at any time; it never reveals or changes session state.
            "ping" => success(id, json!({})),
            "tools/list" if self.phase == Phase::Ready => {
                success(id, json!({"tools": tool_descriptors()}))
            }
            "tools/call" if self.phase == Phase::Ready => self.call(id, params).await,
            "tools/list" | "tools/call" => rpc_error(id, -32600, "session not ready"),
            _ => rpc_error(id, -32601, "method not found"),
        };
        let reply = reply.to_string();
        if reply.len() > MAX_RESPONSE_BYTES {
            // Correlate the failure so the caller does not wait out its timeout.
            Some(rpc_error(reply_id, -32603, "response exceeds transport bound").to_string())
        } else {
            Some(reply)
        }
    }
    fn initialize(&mut self, id: Value, params: &Value) -> Value {
        if self.phase != Phase::New {
            return rpc_error(id, -32600, "already initialized");
        }
        let token = params
            .get("_meta")
            .and_then(|meta| meta.get("lambdaMemoryToken"))
            .and_then(Value::as_str);
        if !token.is_some_and(|token| tokens_match(&self.token, token)) {
            self.phase = Phase::Denied;
            return rpc_error(id, -32001, "unauthorized");
        }
        let Some(version) = params.get("protocolVersion").and_then(Value::as_str) else {
            return rpc_error(id, -32602, "protocolVersion required");
        };
        if !params.get("capabilities").is_some_and(Value::is_object)
            || !params.get("clientInfo").is_some_and(Value::is_object)
        {
            return rpc_error(id, -32602, "invalid initialization parameters");
        }
        self.phase = Phase::Initialized;
        let protocol = if PROTOCOLS.contains(&version) {
            version
        } else {
            PROTOCOL_LATEST
        };
        success(
            id,
            json!({"protocolVersion":protocol,"capabilities":{"tools":{}},
            "serverInfo":{"name":"lambda-memory","version":env!("CARGO_PKG_VERSION")}}),
        )
    }
    async fn call(&self, id: Value, params: &Value) -> Value {
        let Some(params) = params.as_object() else {
            return rpc_error(id, -32602, "invalid params");
        };
        if params
            .keys()
            .any(|key| !matches!(key.as_str(), "name" | "arguments" | "_meta"))
        {
            return rpc_error(id, -32602, "invalid params");
        }
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return rpc_error(id, -32602, "tool name required");
        };
        let empty = Map::new();
        let args = match params.get("arguments") {
            Some(Value::Object(args)) => args,
            None => &empty,
            _ => return rpc_error(id, -32602, "arguments must be an object"),
        };
        let result = self.dispatch(name, args).await;
        match result {
            Ok(body) => tool_result(id, body, false),
            Err(CallError::Params) => rpc_error(id, -32602, "invalid tool arguments"),
            Err(CallError::Unknown) => rpc_error(id, -32602, "unknown tool"),
            Err(CallError::Domain {
                error,
                diagnostics_warning,
            }) => {
                let mut body = json!({"error":error.to_string()});
                if let Some(warning) = diagnostics_warning {
                    body["diagnostics_warning"] = warning_json(&warning);
                }
                tool_result(id, body, true)
            }
        }
    }
    async fn dispatch(&self, name: &str, args: &Map<String, Value>) -> Result<Value, CallError> {
        match name {
            "memory_capture" => {
                keys(
                    args,
                    &["project", "source", "kind", "outcome", "origin", "content"],
                    &["recorded_at_ms"],
                )?;
                let project = project(args)?;
                let source = SourceId::new(text(args, "source")?).map_err(|_| CallError::Params)?;
                let kind = match text(args, "kind")? {
                    "attempt" => WorkEventKind::Attempt,
                    "result" => WorkEventKind::Result,
                    "error" => WorkEventKind::Error,
                    "decision" => WorkEventKind::Decision,
                    "action" => WorkEventKind::Action,
                    "exchange" => WorkEventKind::Exchange,
                    "lesson" if text(args, "origin")?.starts_with("promote://") => {
                        WorkEventKind::Lesson
                    }
                    _ => return Err(CallError::Params),
                };
                let outcome = match text(args, "outcome")? {
                    "unknown" => WorkOutcome::Unknown,
                    "succeeded" => WorkOutcome::Succeeded,
                    "failed" => WorkOutcome::Failed,
                    _ => return Err(CallError::Params),
                };
                let recorded_at_ms = match args.get("recorded_at_ms") {
                    None => None,
                    Some(value) => {
                        let origin = text(args, "origin")?;
                        let imported =
                            matches!(kind, WorkEventKind::Action | WorkEventKind::Exchange)
                                && origin.starts_with("import://");
                        let promoted =
                            kind != WorkEventKind::Action && origin.starts_with("promote://");
                        if !imported && !promoted {
                            return Err(CallError::Params);
                        }
                        Some(value.as_u64().ok_or(CallError::Params)?)
                    }
                };
                // Exchanges are scanned on their parsed prompt and reply below; scanning the JSON
                // envelope would match its delimiters (`"Bearer ","reply"`).
                if kind == WorkEventKind::Lesson {
                    self.runtime
                        .validate_promoted_lesson(text(args, "content")?)?;
                } else if kind != WorkEventKind::Exchange
                    && text(args, "origin")?.starts_with("promote://")
                {
                    self.runtime
                        .validate_promoted_content(text(args, "content")?)?;
                }
                if kind == WorkEventKind::Action {
                    let episode = action_episode(text(args, "content")?)
                        .ok_or(RuntimeError::InvalidInput("invalid action episode"))?;
                    self.runtime.validate_action(&episode)?;
                }
                if kind == WorkEventKind::Exchange {
                    let (prompt, reply) = crate::integrations::exchange_content::parse_exchange(
                        text(args, "content")?,
                    )
                    .ok_or(RuntimeError::InvalidInput("invalid exchange"))?;
                    self.runtime.validate_exchange(&prompt, &reply)?;
                }
                let receipt = self
                    .runtime
                    .capture_at(
                        &project,
                        source,
                        kind,
                        outcome,
                        text(args, "origin")?.into(),
                        text(args, "content")?.into(),
                        recorded_at_ms,
                    )
                    .await;
                recorded(receipt, |receipt| {
                    json!({
                        "source":receipt.source.as_str(),"status":match receipt.status {
                            CaptureStatus::SavedPending => "saved_pending",
                            CaptureStatus::AlreadySavedPending => "already_saved_pending"
                        }
                    })
                })
            }
            "memory_propose" => {
                keys(
                    args,
                    &["project", "memory", "revision", "sources", "content"],
                    &["predecessor"],
                )?;
                let proposal = RevisionProposal {
                    project: project(args)?,
                    memory: MemoryId::new(text(args, "memory")?).map_err(|_| CallError::Params)?,
                    revision: RevisionId::new(text(args, "revision")?)
                        .map_err(|_| CallError::Params)?,
                    sources: sources(args)?,
                    content: text(args, "content")?.into(),
                    predecessor: optional_revision(args, "predecessor")?,
                };
                recorded(self.runtime.propose(proposal).await, |item| {
                    revision_json(&item, true)
                })
            }
            "memory_review" => {
                keys(
                    args,
                    &["project", "revision", "action"],
                    &["expected_active", "conflict_with"],
                )?;
                let action = match text(args, "action")? {
                    "promote" => ReviewAction::Promote,
                    "reject" => ReviewAction::Reject,
                    "conflict" => ReviewAction::Conflict,
                    _ => return Err(CallError::Params),
                };
                let request = ReviewRequest {
                    project: project(args)?,
                    revision: RevisionId::new(text(args, "revision")?)
                        .map_err(|_| CallError::Params)?,
                    action,
                    expected_active: optional_revision(args, "expected_active")?,
                    conflict_with: optional_revision(args, "conflict_with")?,
                };
                recorded(self.runtime.review(request).await, |item| {
                    revision_json(&item, true)
                })
            }
            "memory_list" => {
                let read = listing::read(args)?;
                let listed = self.runtime.list(read.query.clone()).await;
                let result = match listed.result {
                    Ok(page) => Ok(listing::page_json(&read, page)?),
                    Err(error) => Err(error),
                };
                recorded(
                    RecordedOperation {
                        result,
                        diagnostics_warning: listed.diagnostics_warning,
                    },
                    |value| value,
                )
            }
            "memory_recall" => {
                keys(args, &["task"], &["projects", "budget_bytes", "exclude"])?;
                let exclude = match args.get("exclude") {
                    None => Vec::new(),
                    Some(Value::Array(values)) if values.len() <= 256 => values
                        .iter()
                        .map(|value| {
                            MemoryId::new(value.as_str().ok_or(CallError::Params)?)
                                .map_err(|_| CallError::Params)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => return Err(CallError::Params),
                };
                let projects = match args.get("projects") {
                    None => vec![self.runtime.scope().projects[0].clone()],
                    Some(Value::Array(values)) => values
                        .iter()
                        .map(|value| {
                            ProjectId::new(value.as_str().ok_or(CallError::Params)?)
                                .map_err(|_| CallError::Params)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => return Err(CallError::Params),
                };
                let budget = number(args, "budget_bytes", self.runtime.limits().context_bytes)?;
                let result = self
                    .runtime
                    .recall_excluding(text(args, "task")?, &projects, budget, &exclude)
                    .await;
                recorded(result, recall_json)
            }
            "memory_purge" => {
                keys(args, &["project", "sources"], &[])?;
                recorded(
                    self.runtime.purge(&project(args)?, &sources(args)?).await,
                    |receipt| {
                        json!({"sources":receipt.sources,"revisions":receipt.revisions,"jobs":receipt.jobs,
                        "timeline_entries":receipt.timeline_entries,"installed_skills_with_purged_evidence":receipt.installed_skills_with_purged_evidence})
                    },
                )
            }
            "memory_timeline_write" => recorded(
                self.runtime.timeline_write(timeline::draft(args)?).await,
                timeline::receipt_json,
            ),
            "memory_timeline" => {
                let read = timeline::read(args)?;
                let page = self.runtime.timeline(read.query.clone()).await?;
                timeline::page_json(&read, page)
            }
            "memory_overview" => ui::overview(&self.runtime, args).await,
            "memory_nodes" => ui::nodes(&self.runtime, args).await,
            "memory_node" => tree::read(&self.runtime, args).await,
            "memory_brief" => tree::brief(&self.runtime, args).await,
            "memory_skill_candidates" => skills::list(&self.runtime, args).await,
            "memory_skill_candidate" => skills::read(&self.runtime, args).await,
            "memory_skill_decide" => skills::decide(&self.runtime, args).await,
            "memory_session_end" => {
                keys(args, &["project", "session"], &[])?;
                let session = text(args, "session")?;
                if session.is_empty()
                    || session.chars().count() > 512
                    || session.chars().any(char::is_control)
                {
                    return Err(CallError::Params);
                }
                let result = self.runtime.session_end(&project(args)?, session).await?;
                Ok(json!({"session_key":result.session_key.as_str(),"ended":result.ended}))
            }
            "memory_forwards" => {
                keys(args, &["project"], &["limit"])?;
                let limit = number(args, "limit", 32)?;
                if !(1..=64).contains(&limit) {
                    return Err(CallError::Params);
                }
                let forwards = self.runtime.forwards(&project(args)?, limit).await?;
                Ok(json!({"forwards":forwards.into_iter().map(|f| json!({
                    "forward_id":f.forward_id,"action":f.action,"source_id":f.source_id.as_str(),
                    "source":f.source.map(|s| json!({"source":s.source.as_str(),"origin":s.origin,"recorded_at_ms":s.recorded_at_ms,
                        "kind":match s.kind { WorkEventKind::Attempt=>"attempt", WorkEventKind::Result=>"result", WorkEventKind::Error=>"error", WorkEventKind::Decision=>"decision", WorkEventKind::Action=>"action", WorkEventKind::Exchange=>"exchange", WorkEventKind::Lesson=>"lesson" },
                        "outcome":match s.outcome { WorkOutcome::Unknown=>"unknown", WorkOutcome::Succeeded=>"succeeded", WorkOutcome::Failed=>"failed" },"content":s.content}))
                })).collect::<Vec<_>>()}))
            }
            "memory_forward_done" => {
                keys(args, &["project", "forwards"], &["failed"])?;
                let ids = |name: &str| -> Result<Vec<String>, CallError> {
                    match args.get(name) {
                        None => Ok(vec![]),
                        Some(Value::Array(values)) if values.len() <= 64 => values
                            .iter()
                            .map(|v| {
                                v.as_str()
                                    .filter(|s| !s.is_empty() && s.len() <= 2048)
                                    .map(str::to_owned)
                                    .ok_or(CallError::Params)
                            })
                            .collect(),
                        _ => Err(CallError::Params),
                    }
                };
                let done = ids("forwards")?;
                let failed = ids("failed")?;
                if !(1..=64).contains(&(done.len() + failed.len())) {
                    return Err(CallError::Params);
                }
                let (done, failed) = self
                    .runtime
                    .forward_done(&project(args)?, &done, &failed)
                    .await?;
                Ok(json!({"done":done,"failed":failed}))
            }
            "memory_status" => {
                keys(args, &["project"], &[])?;
                Ok(status_json(self.runtime.status(&project(args)?).await?))
            }
            "memory_activity" => Ok(activity_json(
                self.runtime.activity(activity_request(args)?).await?,
            )),
            _ => Err(CallError::Unknown),
        }
    }
}
enum CallError {
    Params,
    Unknown,
    Domain {
        error: RuntimeError,
        diagnostics_warning: Option<DiagnosticsWarning>,
    },
}
impl From<RuntimeError> for CallError {
    fn from(error: RuntimeError) -> Self {
        Self::Domain {
            error,
            diagnostics_warning: None,
        }
    }
}
/// Episode content is versioned JSON of at most 1024 bytes with exactly the known fields.
fn action_episode(content: &str) -> Option<ActionEpisode> {
    if content.len() > 1024 {
        return None;
    }
    let Value::Object(map) = serde_json::from_str(content).ok()? else {
        return None;
    };
    const FIELDS: [&str; 7] = [
        "v",
        "tool",
        "intent",
        "summary",
        "error_head",
        "session",
        "seq",
    ];
    if map.keys().any(|key| !FIELDS.contains(&key.as_str())) || map.get("v")?.as_u64()? != 1 {
        return None;
    }
    let string = |name: &str| map.get(name)?.as_str().map(str::to_owned);
    let error_head = match map.get("error_head") {
        None | Some(Value::Null) => None,
        Some(Value::String(head)) => Some(head.clone()),
        _ => return None,
    };
    Some(ActionEpisode {
        tool: string("tool")?,
        intent: string("intent")?,
        summary: string("summary")?,
        error_head,
        session: string("session")?,
        seq: map.get("seq")?.as_u64()?,
    })
}
fn keys(args: &Map<String, Value>, required: &[&str], optional: &[&str]) -> Result<(), CallError> {
    if required.iter().any(|key| !args.contains_key(*key))
        || args
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        Err(CallError::Params)
    } else {
        Ok(())
    }
}
fn text<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str, CallError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or(CallError::Params)
}
fn number(args: &Map<String, Value>, name: &str, default: usize) -> Result<usize, CallError> {
    match args.get(name) {
        None => Ok(default),
        Some(value) => {
            usize::try_from(value.as_u64().ok_or(CallError::Params)?).map_err(|_| CallError::Params)
        }
    }
}
fn project(args: &Map<String, Value>) -> Result<ProjectId, CallError> {
    ProjectId::new(text(args, "project")?).map_err(|_| CallError::Params)
}
fn sources(args: &Map<String, Value>) -> Result<Vec<SourceId>, CallError> {
    let array = args
        .get("sources")
        .and_then(Value::as_array)
        .ok_or(CallError::Params)?;
    if array.is_empty() || array.len() > 32 {
        return Err(CallError::Params);
    }
    array
        .iter()
        .map(|value| {
            SourceId::new(value.as_str().ok_or(CallError::Params)?).map_err(|_| CallError::Params)
        })
        .collect()
}
fn optional_revision(
    args: &Map<String, Value>,
    name: &str,
) -> Result<Option<RevisionId>, CallError> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) => RevisionId::new(id).map(Some).map_err(|_| CallError::Params),
        _ => Err(CallError::Params),
    }
}
fn state_name(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Processing => "processing",
        LifecycleState::Active => "active",
        LifecycleState::Rejected => "rejected",
        LifecycleState::Conflicted => "conflicted",
        LifecycleState::Superseded => "superseded",
    }
}
fn recall_json(result: RuntimeRecall) -> Value {
    let hits: Vec<_> = result
        .hits
        .iter()
        .map(|hit| {
            let mut item = revision_json(&hit.item, false);
            item["lexical_score"] = json!(hit.lexical_score);
            if let Some(tree) = result.tree_hits.iter().find(|tree| {
                tree.hit.item.revision.id == hit.item.revision.id
                    && tree.hit.item.revision.project == hit.item.revision.project
            }) {
                item["node"] = json!(tree.node.as_str());
                item["path"] = json!(tree.path);
                item["basis"] = json!(tree.basis.as_str());
                item["confidence"] = json!(tree.confidence.map(|c| c.as_str()));
                item["derived_from"] = json!(
                    tree.derived_from
                        .iter()
                        .map(|r| r.as_str())
                        .collect::<Vec<_>>()
                );
                item["content"] = json!(tree.content);
                item["valid_from_ms"] = json!(tree.valid_from_ms);
                item["replaced"] = json!(
                    tree.replaced
                        .as_ref()
                        .map(|old| json!({"title":old.title,"valid_until_ms":old.valid_until_ms}))
                );
                item["source_count"] = json!(tree.source_count);
            }
            item
        })
        .collect();
    json!({"context":result.context,"mode":result.mode,"truncated":result.truncated,"hits":hits})
}
fn revision_json(item: &RuntimeRevision, content: bool) -> Value {
    let sources: Vec<_> = item.sources.iter().map(|source| json!({"source":source.source.as_str(),
        "origin":source.origin,"recorded_at_ms":source.recorded_at_ms,"kind":match source.kind {
            WorkEventKind::Attempt => "attempt", WorkEventKind::Result => "result", WorkEventKind::Error => "error", WorkEventKind::Decision => "decision", WorkEventKind::Action => "action", WorkEventKind::Exchange => "exchange", WorkEventKind::Lesson => "lesson" },
        "outcome":match source.outcome { WorkOutcome::Unknown => "unknown", WorkOutcome::Succeeded => "succeeded", WorkOutcome::Failed => "failed" }})).collect();
    let mut value = json!({"revision":item.revision.id.as_str(),"memory":item.revision.memory.as_str(),
        "project":item.revision.project.as_str(),"state":state_name(item.state),"provisional":true,
        "predecessor":item.revision.predecessor.as_ref().map(RevisionId::as_str),"sources":sources,"reviewed_at_ms":item.reviewed_at_ms});
    if content {
        value["content"] = json!(item.revision.content);
    }
    value
}
fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn tool_result(id: Value, body: Value, is_error: bool) -> Value {
    success(
        id,
        json!({"content":[{"type":"text","text":body.to_string()}],"isError":is_error}),
    )
}
fn tool_descriptors() -> Vec<Value> {
    let string = json!({"type":"string"});
    let strings = json!({"type":"array","items":{"type":"string"},"maxItems":32});
    let integer = json!({"type":"integer","minimum":0});
    let mut tools = vec![
        tool(
            "memory_capture",
            &["project", "source", "kind", "outcome", "origin", "content"],
            json!({
            "project":string,"source":string,"kind":{"enum":["attempt","result","error","decision","action","exchange","lesson"]},
            "outcome":{"enum":["unknown","succeeded","failed"]},"origin":string,"content":string,
            "recorded_at_ms":integer}),
        ),
        tool(
            "memory_propose",
            &["project", "memory", "revision", "sources", "content"],
            json!({
            "project":string,"memory":string,"revision":string,"sources":strings,"content":string,"predecessor":string}),
        ),
        tool(
            "memory_review",
            &["project", "revision", "action"],
            json!({"project":string,"revision":string,
            "action":{"enum":["promote","reject","conflict"]},"expected_active":string,"conflict_with":string}),
        ),
        tool(
            "memory_list",
            &["project"],
            json!({"project":string,"states":strings,"limit":integer,"exclude_tree":{"type":"boolean"},"memory":{"type":"string","maxLength":512},
                "cursor":{"type":"string","maxLength":cursor::MAX_CURSOR_BYTES}}),
        ),
        tool(
            "memory_recall",
            &["task"],
            json!({"task":string,"projects":strings,"budget_bytes":integer,
                "exclude":{"type":"array","items":{"type":"string"},"maxItems":256}}),
        ),
        tool(
            "memory_purge",
            &["project", "sources"],
            json!({"project":string,"sources":strings}),
        ),
        tool(
            "memory_forwards",
            &["project"],
            json!({"project":string,"limit":{"type":"integer","minimum":1,"maximum":64}}),
        ),
        tool(
            "memory_forward_done",
            &["project", "forwards"],
            json!({"project":string,"forwards":{"type":"array","items":{"type":"string"},"maxItems":64},"failed":{"type":"array","items":{"type":"string"},"maxItems":64}}),
        ),
        tool(
            "memory_overview",
            &["project"],
            json!({"project":string,"utc_offset_minutes":{"type":"integer","minimum":-720,"maximum":840}}),
        ),
        tool(
            "memory_nodes",
            &["project", "root"],
            json!({"project":string,"root":string,"topic":string,"basis":{"enum":["observed","concluded"]},"updated":{"type":"boolean"},"query":{"type":"string","maxLength":256},"sort":{"enum":["recent","sources"]},"cursor":{"type":"string","maxLength":cursor::MAX_CURSOR_BYTES},"limit":{"type":"integer","minimum":1,"maximum":32}}),
        ),
        tool(
            "memory_node",
            &["project", "node"],
            json!({"project":string,"node":string,"history":{"type":"boolean"},"sources":{"type":"boolean"},"cursor":string}),
        ),
        tool("memory_status", &["project"], json!({"project":string})),
        tool(
            "memory_activity",
            &["project"],
            json!({"project":string,
                "limit":{"type":"integer","minimum":1,"maximum":32},
                "operations":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8},
                "outcomes":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":4},
                "cursor":{"type":"string","maxLength":1024}}),
        ),
        tool("memory_brief", &["project"], json!({"project":string})),
        tool(
            "memory_skill_candidates",
            &["project"],
            json!({"project":string,"status":{"enum":["drafted","approved","rejected","installed","stale"]}}),
        ),
        tool(
            "memory_skill_candidate",
            &["project", "candidate_id"],
            json!({"project":string,"candidate_id":string}),
        ),
        tool(
            "memory_skill_decide",
            &["project", "candidate_id", "revision_id", "decision"],
            json!({"project":string,"candidate_id":string,"revision_id":string,"decision":{"enum":["approved","rejected","installed"]},"install_paths":{"type":"array","items":{"type":"string"},"maxItems":4},"install_hashes":{"type":"array","items":{"type":"string","pattern":"^[a-fA-F0-9]{64}$"},"maxItems":4}}),
        ),
        tool(
            "memory_session_end",
            &["project", "session"],
            json!({"project":string,"session":{"type":"string","minLength":1,"maxLength":512}}),
        ),
    ];
    tools.extend(timeline::descriptors());
    tools
}
fn tool(name: &str, required: &[&str], properties: Value) -> Value {
    let description = match name {
        "memory_overview" => {
            "UI read: read scoped dashboard counts and 30 daily activity buckets without model calls or idle postponement."
        }
        "memory_nodes" => {
            "UI read: page active tree nodes with topic and evidence filters without model calls or idle postponement."
        }
        "memory_skill_candidates" => "Service-only: list skill candidates without draft text.",
        "memory_skill_candidate" => "Service-only: read a skill candidate and draft.",
        "memory_skill_decide" => {
            "Service-only: record approval, rejection or installation of a skill draft."
        }
        "memory_forwards" => {
            "Service-only: list pending cross-store forwards without model calls or idle postponement."
        }
        "memory_forward_done" => {
            "Service-only: acknowledge completed or permanently failed cross-store forwards."
        }
        "memory_node" => {
            "Read one memory tree node with its path, children, optional version history and linked original sources, without model calls or idle postponement."
        }
        "memory_session_end" => {
            "Mark an existing session ended using the raw harness session ID, without model calls or idle postponement."
        }
        "memory_brief" => {
            "Read the project's memory tree index (titles of the top nodes under each root) for a session brief, without model calls or idle postponement."
        }
        "memory_status" => {
            "Read project processing status, backlog and dreaming outcomes without model calls or idle postponement."
        }
        "memory_activity" => {
            "Read seven days of project-scoped operational metadata without source content or processing side effects."
        }
        "memory_timeline_write" => {
            "Record a source-backed decision timeline entry; an edit appends a version guarded by expected_version and identical replays are safe."
        }
        "memory_timeline" => {
            "Read the curated decision timeline, one entry's version history, and captured decisions not yet cited, without idle postponement."
        }
        _ => "Authorized source-preserving memory operation",
    };
    // Hosts derive approval from these hints; only purge removes stored evidence.
    let read_only = matches!(
        name,
        "memory_overview"
            | "memory_nodes"
            | "memory_node"
            | "memory_list"
            | "memory_recall"
            | "memory_status"
            | "memory_activity"
            | "memory_timeline"
            | "memory_brief"
            | "memory_forwards"
            | "memory_skill_candidates"
            | "memory_skill_candidate"
    );
    json!({"name":name,"description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":read_only,"destructiveHint":name == "memory_purge",
            "idempotentHint":read_only || name == "memory_session_end","openWorldHint":false}})
}

/// A dedicated bounded reader thread is process-owned: blocked stdin never stalls idle work.
/// On authentication denial the process exits, even if a peer keeps its input pipe open.
pub async fn serve(runtime: Arc<MemoryRuntime>, token: String) -> Result<(), RuntimeError> {
    serve_io(runtime, token, std::io::stdin(), std::io::stdout()).await
}
pub async fn serve_io<R: Read + Send + 'static, W: Write>(
    runtime: Arc<MemoryRuntime>,
    token: String,
    input: R,
    mut output: W,
) -> Result<(), RuntimeError> {
    let mut session = McpSession::new(runtime, token)?;
    let (sender, mut frames) = tokio::sync::mpsc::channel(4);
    let reader = std::thread::spawn(move || {
        let mut input = BufReader::new(input);
        loop {
            let mut bytes = Vec::new();
            let read = (&mut input)
                .take((MAX_FRAME_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes);
            match read {
                Ok(0) => break,
                Ok(_) if bytes.len() <= MAX_FRAME_BYTES && bytes.last() == Some(&b'\n') => {
                    if sender.blocking_send(Ok(bytes)).is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = sender.blocking_send(Err(()));
                    break;
                }
            }
        }
    });
    while let Some(frame) = frames.recv().await {
        let reply = match frame {
            Ok(bytes) => match std::str::from_utf8(&bytes) {
                Ok(text) => session.handle_frame(text).await,
                Err(_) => Some(rpc_error(Value::Null, -32700, "invalid UTF-8").to_string()),
            },
            Err(()) => {
                Some(rpc_error(Value::Null, -32700, "invalid or oversized frame").to_string())
            }
        };
        if let Some(reply) = reply {
            output
                .write_all(reply.as_bytes())
                .and_then(|_| output.write_all(b"\n"))
                .and_then(|_| output.flush())
                .map_err(|_| RuntimeError::StorageUnavailable)?;
        }
        if session.is_denied() {
            break;
        }
    }
    drop(frames);
    if reader.is_finished() {
        reader
            .join()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    Ok(())
}

#[cfg(test)]
mod recall_wire_tests {
    use super::*;
    use crate::domain_models::capture::CaptureEvent;
    use crate::domain_models::consolidation::{KnowledgeKind, Revision, RevisionStatus};
    use crate::domain_models::memory_tree::TreeHit;
    use crate::domain_models::{ActorId, OwnerId};
    fn fixture_hit() -> RuntimeHit {
        let owner = OwnerId::new("owner").unwrap();
        let project = ProjectId::new("project").unwrap();
        let source = SourceId::new("source").unwrap();
        let event = CaptureEvent {
            source: source.clone(),
            owner: owner.clone(),
            actor: ActorId::new("actor").unwrap(),
            project: project.clone(),
            origin: "fixture://test".into(),
            recorded_at_ms: 1,
            kind: WorkEventKind::Decision,
            outcome: WorkOutcome::Unknown,
            content: "original".into(),
        };
        let node = MemoryId::new("node").unwrap();
        RuntimeHit {
            item: RuntimeRevision {
                revision: Revision {
                    id: RevisionId::new("revision").unwrap(),
                    memory: node,
                    owner,
                    project,
                    kind: KnowledgeKind::Knowledge,
                    status: RevisionStatus::PendingReview,
                    content: "summary".into(),
                    prerequisites: vec![],
                    sources: vec![source],
                    predecessor: None,
                    model: "fixture".into(),
                    prompt_version: "fixture".into(),
                },
                state: LifecycleState::Active,
                sources: vec![event],
                reviewed_at_ms: Some(1),
                watermark_ms: 1,
            },
            lexical_score: 0,
        }
    }
    #[test]
    fn tree_metadata_is_present_and_flat_wire_is_unchanged() {
        let hit = fixture_hit();
        let node = hit.item.revision.memory.clone();
        let recall = |tree_hits| RuntimeRecall {
            context: String::new(),
            hits: vec![hit.clone()],
            tree_hits,
            mode: "openrouter_semantic",
            truncated: false,
        };
        let tree = recall_json(recall(vec![TreeHit {
            basis: Default::default(),
            confidence: None,
            derived_from: vec![],
            content: hit.item.revision.content.clone(),
            valid_from_ms: None,
            replaced: None,
            root: crate::domain_models::memory_tree::TreeRoot::Decisions,
            hit: hit.clone(),
            path: vec!["Decisions".into(), "Vault".into()],
            node,
            source_count: 1,
        }]));
        assert_eq!(tree["hits"][0]["node"], "node");
        assert_eq!(tree["hits"][0]["path"], json!(["Decisions", "Vault"]));
        assert_eq!(tree["hits"][0]["content"], "summary");
        assert_eq!(tree["hits"][0]["source_count"], 1);
        let flat = recall_json(recall(vec![]));
        assert!(flat["hits"][0].get("node").is_none());
        assert!(flat["hits"][0].get("path").is_none());
        assert!(flat["hits"][0].get("content").is_none());
        assert!(flat["hits"][0].get("source_count").is_none());
        assert!(flat["hits"][0].get("valid_from_ms").is_none());
        assert!(flat["hits"][0].get("replaced").is_none());
    }

    #[test]
    fn recall_renders_since_and_replaced() {
        use crate::domain_models::memory_tree::{ReplacedPosition, TreeRoot};
        let start = 1_735_689_600_000;
        for replaced in [
            None,
            Some(ReplacedPosition {
                title: "Earlier position".into(),
                valid_until_ms: start,
            }),
        ] {
            let hit = fixture_hit();
            let note = if replaced.is_some() {
                " (since 2025-01-01; replaced: Earlier position (until 2025-01-01))"
            } else {
                " (since 2025-01-01)"
            };
            let tree = TreeHit {
                basis: Default::default(),
                confidence: None,
                derived_from: vec![],
                content: format!("summary{note}"),
                valid_from_ms: Some(start),
                replaced: replaced.clone(),
                root: TreeRoot::Decisions,
                hit: hit.clone(),
                node: hit.item.revision.memory.clone(),
                path: vec!["Decisions".into()],
                source_count: 1,
            };
            let wire = recall_json(RuntimeRecall {
                context: String::new(),
                hits: vec![hit],
                tree_hits: vec![tree],
                mode: "openrouter_semantic",
                truncated: false,
            });
            assert_eq!(wire["hits"][0]["content"], format!("summary{note}"));
            assert_eq!(wire["hits"][0]["valid_from_ms"], start);
            if replaced.is_some() {
                assert_eq!(
                    wire["hits"][0]["replaced"],
                    json!({"title":"Earlier position","valid_until_ms":start})
                );
            } else {
                assert!(wire["hits"][0]["replaced"].is_null());
            }
        }
    }
}
