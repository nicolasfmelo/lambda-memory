//! Strict UI read framing. Cursors bind the complete normalized filter set.
use super::*;
use crate::domain_models::memory_tree::{Basis, StoreKind, TreeMode, roots};
use crate::domain_models::ui::*;

fn invalid(error: CallError) -> CallError {
    match error {
        CallError::Params => RuntimeError::InvalidInput("UI read arguments or cursor").into(),
        other => other,
    }
}
pub(super) fn memory_ref(value: &MemoryRef) -> Value {
    json!({"node":value.node.as_str(),"title":value.title})
}
pub(super) fn skill_ref(value: &Option<SkillDraftRef>) -> Value {
    value
        .as_ref()
        .map(|s| json!({"candidate":s.candidate.as_str(),"status":s.status.as_str()}))
        .unwrap_or(Value::Null)
}
pub(super) async fn overview(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    let parsed = (|| {
        keys(args, &["project"], &["utc_offset_minutes"])?;
        let offset = match args.get("utc_offset_minutes") {
            None => 0,
            Some(v) => v.as_i64().ok_or(CallError::Params)?,
        };
        if !(-720..=840).contains(&offset) {
            return Err(CallError::Params);
        }
        Ok((project(args)?, offset as i32))
    })()
    .map_err(invalid)?;
    let v = runtime.overview(&parsed.0, parsed.1).await?;
    let drafts: Map<String, Value> = v
        .skill_drafts
        .iter()
        .map(|(s, n)| (s.as_str().to_owned(), json!(n)))
        .collect();
    Ok(
        json!({"project":v.project.as_str(),"mode":match v.mode { TreeMode::Tree=>"tree", TreeMode::Flat=>"flat" },
        "roots":v.roots.iter().map(|r| json!({"root":r.root.as_str(),"label":r.root.label(),"nodes":r.nodes,"latest":r.latest.as_ref().map(memory_ref)})).collect::<Vec<_>>(),
        "flat_active":v.flat_active,"basis":{"observed":v.observed,"concluded":v.concluded},"lessons":{"nodes":v.lessons,"reinforced":v.reinforced},"skill_drafts":drafts,
        "days":v.days.iter().map(|d| json!({"day":d.day,"captured":d.captured,"published":d.published})).collect::<Vec<_>>()}),
    )
}
fn read(args: &Map<String, Value>) -> Result<(NodesQuery, String), CallError> {
    keys(
        args,
        &["project", "root"],
        &[
            "topic", "basis", "updated", "query", "sort", "cursor", "limit",
        ],
    )?;
    let project = project(args)?;
    let root = roots(StoreKind::of(&project))
        .iter()
        .find(|r| Some(r.as_str()) == args.get("root").and_then(Value::as_str))
        .copied()
        .ok_or(CallError::Params)?;
    let topic = args
        .get("topic")
        .map(|v| MemoryId::new(v.as_str().ok_or(CallError::Params)?).map_err(|_| CallError::Params))
        .transpose()?;
    let basis = match args.get("basis").map(Value::as_str) {
        None => None,
        Some(Some("observed")) => Some(Basis::Observed),
        Some(Some("concluded")) => Some(Basis::Concluded),
        _ => return Err(CallError::Params),
    };
    let updated = match args.get("updated") {
        None => false,
        Some(v) => v.as_bool().ok_or(CallError::Params)?,
    };
    let query = match args.get("query") {
        None => String::new(),
        Some(v) => v.as_str().ok_or(CallError::Params)?.to_owned(),
    };
    if args
        .get("query")
        .and_then(Value::as_str)
        .is_some_and(|q| q.len() > 256)
    {
        return Err(CallError::Params);
    }
    let sort = match args.get("sort").map(Value::as_str) {
        None | Some(Some("recent")) => NodesSort::Recent,
        Some(Some("sources")) => NodesSort::Sources,
        _ => return Err(CallError::Params),
    };
    let limit = number(args, "limit", 24)?;
    if !(1..=32).contains(&limit) {
        return Err(CallError::Params);
    }
    let fingerprint = cursor::fingerprint(&[
        project.as_str(),
        root.as_str(),
        topic.as_ref().map_or("", MemoryId::as_str),
        basis.map_or("", Basis::as_str),
        if updated { "true" } else { "false" },
        &query.to_lowercase(),
        if sort == NodesSort::Recent {
            "recent"
        } else {
            "sources"
        },
    ]);
    let after = args
        .get("cursor")
        .map(|v| {
            let key = cursor::decode(v, "nodes", &fingerprint)?;
            let obj = key.as_object().ok_or(CallError::Params)?;
            if obj.len() != 2 {
                return Err(CallError::Params);
            }
            let at = key
                .get("at")
                .and_then(Value::as_u64)
                .ok_or(CallError::Params)?;
            let id = MemoryId::new(
                key.get("node")
                    .and_then(Value::as_str)
                    .ok_or(CallError::Params)?,
            )
            .map_err(|_| CallError::Params)?;
            Ok((at, id))
        })
        .transpose()?;
    Ok((
        NodesQuery {
            project,
            root,
            topic,
            basis,
            updated,
            query,
            sort,
            after,
            limit,
        },
        fingerprint,
    ))
}
pub(super) async fn nodes(
    runtime: &MemoryRuntime,
    args: &Map<String, Value>,
) -> Result<Value, CallError> {
    let (query, fingerprint) = read(args).map_err(invalid)?;
    let page = runtime.nodes(query.clone()).await?;
    let next = if page.more {
        page.nodes.last().map(|n| cursor::encode("nodes",&fingerprint,json!({"at":match query.sort { NodesSort::Recent=>n.node.last_linked_ms,NodesSort::Sources=>u64::from(n.node.direct_sources) },"node":n.node.memory.as_str()}))).transpose()?
    } else {
        None
    };
    Ok(
        json!({"project":query.project.as_str(),"root":query.root.as_str(),"total":page.total,
        "topics":page.topics.iter().map(|t| json!({"node":t.memory.node.as_str(),"title":t.memory.title,"count":t.count})).collect::<Vec<_>>(),
        "nodes":page.nodes.iter().map(|n| json!({"node":n.node.memory.as_str(),"title":n.node.title,"summary":n.summary,"path":n.path,"topic":n.topic.as_ref().map(memory_ref),"depth":n.node.depth,"children":n.node.children,
            "basis":n.node.basis.as_str(),"confidence":n.node.confidence.map(|c| c.as_str()),"derived_count":n.node.derived_from.len(),"valid_from_ms":n.node.valid_from_ms,
            "replaced":n.replaced.as_ref().map(|r| json!({"title":r.title,"valid_until_ms":r.valid_until_ms})),"source_count":n.node.direct_sources,"link_count":n.link_count,"updated_at_ms":n.node.last_linked_ms,
            "lesson":n.lesson.as_ref().map(|l| json!({"sessions":l.sessions.len(),"strategies":l.strategies,"pitfalls":l.pitfalls})),"skill_draft":skill_ref(&n.skill_draft)})).collect::<Vec<_>>(),"next_cursor":next}),
    )
}
