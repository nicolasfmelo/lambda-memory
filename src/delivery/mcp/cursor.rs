//! Opaque keyset cursors: hex-encoded JSON carrying a kind, a query fingerprint and a key.
//! A malformed cursor is invalid params; a well-formed cursor issued for another query is a
//! domain error, so callers can tell a broken client from a reused cursor.
use super::*;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub(super) const MAX_CURSOR_BYTES: usize = 4096;

/// Length-prefixed digest, so no two part lists collide by concatenation.
pub(super) fn fingerprint(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn encode(kind: &str, fingerprint: &str, key: Value) -> Result<String, CallError> {
    let bytes = serde_json::to_vec(&json!({"kind":kind,"fingerprint":fingerprint,"key":key}))
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    if result.len() > MAX_CURSOR_BYTES {
        return Err(RuntimeError::StorageUnavailable.into());
    }
    Ok(result)
}

/// Returns the key of a cursor issued for this `kind` and `fingerprint`.
pub(super) fn decode(cursor: &Value, kind: &str, fingerprint: &str) -> Result<Value, CallError> {
    let text = cursor.as_str().ok_or(CallError::Params)?;
    if text.is_empty() || text.len() > MAX_CURSOR_BYTES || !text.len().is_multiple_of(2) {
        return Err(CallError::Params);
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let bytes = text
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| digit(pair[0]).zip(digit(pair[1])).map(|(a, b)| a * 16 + b))
        .collect::<Option<Vec<u8>>>()
        .ok_or(CallError::Params)?;
    let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(CallError::Params);
    };
    if object.len() != 3
        || !object.get("kind").is_some_and(Value::is_string)
        || !object.get("fingerprint").is_some_and(Value::is_string)
    {
        return Err(CallError::Params);
    }
    let key = object.remove("key").ok_or(CallError::Params)?;
    if object["kind"] != kind || object["fingerprint"] != fingerprint {
        return Err(RuntimeError::InvalidInput("cursor does not match this query").into());
    }
    Ok(key)
}
