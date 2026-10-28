//! Read-only tree node framing.
use super::*;
use crate::domain_models::memory_tree::LinkOperation;

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
        "kind":match s.event.kind { WorkEventKind::Attempt=>"attempt", WorkEventKind::Result=>"result", WorkEventKind::Error=>"error", WorkEventKind::Decision=>"decision", WorkEventKind::Action=>"action", WorkEventKind::Exchange=>"exchange" },
        "operation":match s.operation { LinkOperation::Create=>"create", LinkOperation::Merge=>"merge", LinkOperation::Split=>"split", LinkOperation::Rebuild=>"rebuild", LinkOperation::Migrate=>"migrate" }, "revision":s.revision.as_str(), "linked_at_ms":s.linked_at_ms})).collect();
    Ok(
        json!({"node":view.node.memory.as_str(),"revision":view.node.revision.as_str(),"title":view.node.title,"content":view.node.content,"path":view.path,
        "children":view.children.iter().map(|(id,title)| json!({"node":id.as_str(),"title":title})).collect::<Vec<_>>(),
        "history":view.history.iter().map(|r| revision_json(r,true)).collect::<Vec<_>>(),"sources":originals,"next_cursor":next}),
    )
}
