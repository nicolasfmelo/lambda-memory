//! Strict reflection DTO decoding; no truncation or partial acceptance.
use super::*;
use crate::domain_models::{memory_tree::Confidence, reflection::*};
fn confidence(s: &str) -> Result<Confidence, RuntimeError> {
    match s {
        "low" => Ok(Confidence::Low),
        "medium" => Ok(Confidence::Medium),
        "high" => Ok(Confidence::High),
        _ => Err(RuntimeError::ProviderInvalidOutput),
    }
}
pub(super) async fn reflect(
    http: &RouterHttp,
    input: &ReflectionInput,
) -> Result<ReflectionAnswer, RuntimeError> {
    let value = json!({"nodes":input.nodes.iter().enumerate().map(|(i,n)|json!({"slot":format!("n{i}"),"basis":n.node.basis.as_str(),"confidence":n.node.confidence.map(|c|c.as_str()),"title":n.node.title,"content":n.node.content,"path":n.path,"read_only":!n.inside})).collect::<Vec<_>>()});
    if value.to_string().len() > crate::domain_models::reflection::INPUT_BYTES {
        return Err(RuntimeError::ProviderInvalidOutput);
    }
    let raw = http.reflect(value).await.map_err(tree_provider_error)?;
    decode(&raw, input.nodes.len())
}
fn array(v: Option<Value>) -> Result<Vec<Value>, RuntimeError> {
    match v {
        Some(Value::Array(v)) => Ok(v),
        _ => Err(RuntimeError::ProviderInvalidOutput),
    }
}
fn decode(raw: &str, count: usize) -> Result<ReflectionAnswer, RuntimeError> {
    let mut v = object(
        parse_tree(raw)?,
        &["conclusions", "confidence_updates", "contradictions"],
    )?;
    let mut answer = ReflectionAnswer::default();
    for c in array(v.remove("conclusions"))? {
        let mut c = object(
            c,
            &["title", "content", "confidence", "derived_from", "parent"],
        )?;
        answer.conclusions.push(Conclusion {
            title: take_string(&mut c, "title")?,
            content: take_string(&mut c, "content")?,
            confidence: confidence(&take_string(&mut c, "confidence")?)?,
            parent: node_index(&take_string(&mut c, "parent")?, count)?,
            derived_from: array(c.remove("derived_from"))?
                .into_iter()
                .map(|s| {
                    s.as_str()
                        .ok_or(RuntimeError::ProviderInvalidOutput)
                        .and_then(|s| node_index(s, count))
                })
                .collect::<Result<_, _>>()?,
        });
    }
    for u in array(v.remove("confidence_updates"))? {
        let mut u = object(u, &["node", "confidence"])?;
        answer.confidence_updates.push((
            node_index(&take_string(&mut u, "node")?, count)?,
            confidence(&take_string(&mut u, "confidence")?)?,
        ));
    }
    for c in array(v.remove("contradictions"))? {
        let mut c = object(c, &["concluded", "observed"])?;
        answer.contradictions.push((
            node_index(&take_string(&mut c, "concluded")?, count)?,
            node_index(&take_string(&mut c, "observed")?, count)?,
        ));
    }
    // Value maps collapse duplicate keys; count unquoted separators to reject those too.
    let (mut quoted, mut escaped, mut fields) = (false, false, 0);
    for b in raw.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b':' if !quoted => fields += 1,
            _ => {}
        }
    }
    if fields
        != 3 + 5 * answer.conclusions.len()
            + 2 * answer.confidence_updates.len()
            + 2 * answer.contradictions.len()
    {
        return Err(RuntimeError::ProviderInvalidOutput);
    }
    Ok(answer)
}
