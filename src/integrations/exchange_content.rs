//! Decode versioned conversation content at the integration boundary.
use serde_json::Value;

pub fn parse_exchange(content: &str) -> Option<(String, String)> {
    let Value::Object(map) = serde_json::from_str(content).ok()? else {
        return None;
    };
    if map.len() != 3 || map.get("v")?.as_u64()? != 1 {
        return None;
    }
    Some((
        map.get("prompt")?.as_str()?.to_owned(),
        map.get("reply")?.as_str()?.to_owned(),
    ))
}
