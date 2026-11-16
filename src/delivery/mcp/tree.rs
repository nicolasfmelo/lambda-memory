//! Read-only tree node framing.
use super::*;
use crate::domain_models::memory_tree::{LinkOperation, TreeMode};

pub(super) async fn brief(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    keys(args, &["project"], &[])?;
    let brief = runtime.brief(&project(args)?).await?;
    let roots: Vec<_> = brief.roots.iter().map(|root| json!({
        "root": root.root.label(),
        "nodes": root.nodes.iter().map(|node| json!({
            "node": node.node.as_str(), "title": node.title, "children": node.children, "basis":node.basis.as_str(),
        })).collect::<Vec<_>>()
    })).collect();
    Ok(
        json!({"project": brief.project.as_str(), "mode": match brief.mode { TreeMode::Tree => "tree", TreeMode::Flat => "flat" },
        "roots": roots, "truncated": brief.truncated, "skill_drafts_pending": brief.skill_drafts_pending}),
    )
}

pub(super) async fn read(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    keys(
        args,
        &["project", "node"],
        &["history", "sources", "cursor"],
    )?;
    let project = project(args)?;
    let node = MemoryId::new(text(args, "node")?).map_err(|_| CallError::Params)?;
    let flag = |key| match args.get(key) {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or(CallError::Params),
    };
    let history = flag("history")?;
    let sources = flag("sources")?;
    let fingerprint = cursor::fingerprint(&[project.as_str(), node.as_str()]);
    let after = match args.get("cursor") {
        None => None,
        Some(value) => Some(
            cursor::decode(value, "node", &fingerprint)?
                .get("after")
                .and_then(Value::as_str)
                .ok_or(CallError::Params)?
                .to_owned(),
        ),
    };
    let view = runtime
        .node(&project, &node, history, sources, after)
        .await?;
    let next = view
        .next_cursor
        .map(|after| cursor::encode("node", &fingerprint, json!({"after":after})))
        .transpose()?;
    let originals: Vec<_> = view.sources.iter().map(|s| json!({"source":s.event.source.as_str(), "content":s.event.content, "origin":s.event.origin, "recorded_at_ms":s.event.recorded_at_ms,
        "kind":match s.event.kind { WorkEventKind::Attempt=>"attempt", WorkEventKind::Result=>"result", WorkEventKind::Error=>"error", WorkEventKind::Decision=>"decision", WorkEventKind::Action=>"action", WorkEventKind::Exchange=>"exchange", WorkEventKind::Lesson=>"lesson" },
        "operation":match s.operation { LinkOperation::Create=>"create", LinkOperation::Merge=>"merge", LinkOperation::Supersede=>"supersede", LinkOperation::Split=>"split", LinkOperation::Rebuild=>"rebuild", LinkOperation::Migrate=>"migrate", LinkOperation::Reflect=>"reflect" }, "revision":s.revision.as_str(), "linked_at_ms":s.linked_at_ms})).collect();
    let mut result = json!({"node":view.node.memory.as_str(),"revision":view.node.revision.as_str(),"title":view.node.title,"content":view.node.content,"path":view.path,"valid_from_ms":view.node.valid_from_ms,"basis":view.node.basis.as_str(),"confidence":view.node.confidence.map(|c|c.as_str()),"derived_from":view.node.derived_from.iter().map(|r|r.as_str()).collect::<Vec<_>>(),
        "links":view.links.iter().map(|link| json!({"node":link.node.as_str(),"title":link.title,"relation":link.relation.as_str(),"direction":if link.outgoing { "out" } else { "in" }})).collect::<Vec<_>>(),
        "children":view.children.iter().map(|(id,title)| json!({"node":id.as_str(),"title":title})).collect::<Vec<_>>(),
        "history":view.history.iter().map(|r| {
            let mut entry = revision_json(&r.item,true);
            entry["basis"] = json!(r.basis.as_str());
            entry["confidence"] = json!(r.confidence.map(|c|c.as_str()));
            entry["derived_from"] = json!(r.derived_from.iter().map(|id|id.as_str()).collect::<Vec<_>>());
            entry["valid_from_ms"] = json!(r.valid_from_ms);
            entry["valid_until_ms"] = json!(r.valid_until_ms);
            entry
        }).collect::<Vec<_>>(),"sources":originals,"next_cursor":next});
    result["replaced"] = json!(
        view.replaced
            .as_ref()
            .map(|r| json!({"title":r.title,"valid_until_ms":r.valid_until_ms}))
    );
    result["source_total"] = json!(view.source_total);
    result["skill_draft"] = ui::skill_ref(&view.skill_draft);
    result["derived_memories"] = json!(
        view.derived_memories
            .iter()
            .map(ui::memory_ref)
            .collect::<Vec<_>>()
    );
    if let Some(lesson) = view.lesson {
        result["lesson"] = json!({"reinforced_sessions":lesson.sessions.len(), "strategies":lesson.strategies, "pitfalls":lesson.pitfalls});
    }
    Ok(result)
}
