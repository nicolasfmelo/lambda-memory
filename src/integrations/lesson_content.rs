//! Strict decoding of stored lesson content. Secret scans use parsed strings, not JSON framing.
use crate::domain_models::SourceId;
use serde_json::Value;

pub struct LessonContent {
    pub text: [String; 4],
    pub evidence: Vec<SourceId>,
}
impl LessonContent {
    pub fn strings(&self) -> impl Iterator<Item = &str> {
        self.text
            .iter()
            .map(String::as_str)
            .chain(self.evidence.iter().map(SourceId::as_str))
    }
}

pub fn parse_lesson(content: &str) -> Option<LessonContent> {
    let Value::Object(map) = serde_json::from_str(content).ok()? else {
        return None;
    };
    const FIELDS: [&str; 9] = [
        "v",
        "kind",
        "scope",
        "title",
        "applies_when",
        "guidance",
        "task",
        "task_outcome",
        "evidence",
    ];
    if map.len() != FIELDS.len()
        || map.keys().any(|key| !FIELDS.contains(&key.as_str()))
        || map.get("v")?.as_u64()? != 1
        || !matches!(map.get("kind")?.as_str()?, "strategy" | "pitfall")
        || !matches!(map.get("scope")?.as_str()?, "project" | "user")
        || !matches!(
            map.get("task_outcome")?.as_str()?,
            "succeeded" | "failed" | "corrected_by_user"
        )
    {
        return None;
    }
    // All values are scalar strings/numbers or an array of strings. Counting unquoted
    // separators rejects duplicate fields that the JSON value map would otherwise collapse.
    let (mut quoted, mut escaped, mut fields) = (false, false, 0);
    for byte in content.bytes() {
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
    if fields != FIELDS.len() {
        return None;
    }
    let evidence = map.get("evidence")?.as_array()?;
    if !(1..=8).contains(&evidence.len()) {
        return None;
    }
    Some(LessonContent {
        text: [
            map.get("title")?.as_str()?.to_owned(),
            map.get("applies_when")?.as_str()?.to_owned(),
            map.get("guidance")?.as_str()?.to_owned(),
            map.get("task")?.as_str()?.to_owned(),
        ],
        evidence: evidence
            .iter()
            .map(|id| SourceId::new(id.as_str()?).ok())
            .collect::<Option<_>>()?,
    })
}
