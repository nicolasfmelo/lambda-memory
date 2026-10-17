//! Bounded authenticated MCP stdio. Wire JSON stays in delivery; parent owns idle dreaming.
use crate::domain_models::capture::{CaptureStatus, WorkEventKind, WorkOutcome};
use crate::domain_models::runtime::*;
use crate::domain_models::{MemoryId, ProjectId, RevisionId, SourceId};
use crate::services::runtime_service::MemoryRuntime;
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;
mod cursor;
mod diagnostics;
mod listing;
mod timeline;
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
                    &[],
                )?;
                let project = project(args)?;
                let source = SourceId::new(text(args, "source")?).map_err(|_| CallError::Params)?;
                let kind = match text(args, "kind")? {
                    "attempt" => WorkEventKind::Attempt,
                    "result" => WorkEventKind::Result,
                    "error" => WorkEventKind::Error,
                    "decision" => WorkEventKind::Decision,
                    _ => return Err(CallError::Params),
                };
                let outcome = match text(args, "outcome")? {
                    "unknown" => WorkOutcome::Unknown,
                    "succeeded" => WorkOutcome::Succeeded,
                    "failed" => WorkOutcome::Failed,
                    _ => return Err(CallError::Params),
                };
                let receipt = self
                    .runtime
                    .capture(
                        &project,
                        source,
                        kind,
                        outcome,
                        text(args, "origin")?.into(),
                        text(args, "content")?.into(),
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
                keys(args, &["task"], &["projects", "budget_tokens"])?;
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
                let budget = number(args, "budget_tokens", self.runtime.limits().context_tokens)?;
                let result = self
                    .runtime
                    .recall(text(args, "task")?, &projects, budget)
                    .await;
                recorded(result, |result| {
                    let hits: Vec<_> = result
                        .hits
                        .iter()
                        .map(|hit| {
                            let mut item = revision_json(&hit.item, false);
                            item["lexical_score"] = json!(hit.lexical_score);
                            item
                        })
                        .collect();
                    json!({"context":result.context,"mode":result.mode,"truncated":result.truncated,"hits":hits})
                })
            }
            "memory_purge" => {
                keys(args, &["project", "sources"], &[])?;
                recorded(
                    self.runtime.purge(&project(args)?, &sources(args)?).await,
                    |receipt| {
                        json!({"sources":receipt.sources,"revisions":receipt.revisions,"jobs":receipt.jobs,
                        "timeline_entries":receipt.timeline_entries})
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
fn revision_json(item: &RuntimeRevision, content: bool) -> Value {
    let sources: Vec<_> = item.sources.iter().map(|source| json!({"source":source.source.as_str(),
        "origin":source.origin,"recorded_at_ms":source.recorded_at_ms,"kind":match source.kind {
            WorkEventKind::Attempt => "attempt", WorkEventKind::Result => "result", WorkEventKind::Error => "error", WorkEventKind::Decision => "decision" },
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
            "project":string,"source":string,"kind":{"enum":["attempt","result","error","decision"]},
            "outcome":{"enum":["unknown","succeeded","failed"]},"origin":string,"content":string}),
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
            json!({"project":string,"states":strings,"limit":integer,"memory":{"type":"string","maxLength":512},
                "cursor":{"type":"string","maxLength":cursor::MAX_CURSOR_BYTES}}),
        ),
        tool(
            "memory_recall",
            &["task"],
            json!({"task":string,"projects":strings,"budget_tokens":integer}),
        ),
        tool(
            "memory_purge",
            &["project", "sources"],
            json!({"project":string,"sources":strings}),
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
    ];
    tools.extend(timeline::descriptors());
    tools
}
fn tool(name: &str, required: &[&str], properties: Value) -> Value {
    let description = match name {
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
        "memory_list" | "memory_recall" | "memory_status" | "memory_activity" | "memory_timeline"
    );
    json!({"name":name,"description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":read_only,"destructiveHint":name == "memory_purge",
            "idempotentHint":read_only,"openWorldHint":false}})
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
