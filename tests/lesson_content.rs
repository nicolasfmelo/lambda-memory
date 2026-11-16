#![cfg(feature = "memory-runtime")]
use lambda_memory::integrations::lesson_content::parse_lesson;
use serde_json::{Value, json};

fn valid() -> Value {
    json!({"v":1,"kind":"strategy","scope":"project","title":"Test storage","applies_when":"Changing storage","guidance":"1. Test it.","task":"Verify","task_outcome":"succeeded","evidence":["source"]})
}

#[test]
fn lesson_content_requires_exact_keys_and_types() {
    let valid = valid();
    assert!(parse_lesson(&valid.to_string()).is_some());
    for key in valid.as_object().unwrap().keys() {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(
            parse_lesson(&missing.to_string()).is_none(),
            "missing {key}"
        );
        for wrong_type in [Value::Null, json!(true), json!({})] {
            let mut invalid = valid.clone();
            invalid[key] = wrong_type;
            assert!(
                parse_lesson(&invalid.to_string()).is_none(),
                "type of {key}"
            );
        }
    }
    let mut extra = valid.clone();
    extra["extra"] = json!("untrusted");
    assert!(parse_lesson(&extra.to_string()).is_none());
    assert!(parse_lesson("[]").is_none());
    assert!(parse_lesson("not JSON").is_none());
    let duplicate = valid
        .to_string()
        .replacen('{', "{\"title\":\"duplicate\",", 1);
    assert!(parse_lesson(&duplicate).is_none());
    let mut quoted = valid.clone();
    quoted["guidance"] = json!("show only \"Bearer \"; evidence: C:\\work");
    assert!(parse_lesson(&quoted.to_string()).is_some());
}

#[test]
fn lesson_content_checks_versions_enums_and_evidence_bounds() {
    for (key, invalid_value) in [
        ("v", json!(2)),
        ("v", json!("1")),
        ("v", json!(1.0)),
        ("kind", json!("procedure")),
        ("scope", json!("global")),
        ("task_outcome", json!("unknown")),
        ("evidence", json!([])),
        ("evidence", json!(vec!["source"; 9])),
        ("evidence", json!("source")),
        ("evidence", json!([1])),
        ("evidence", json!([""])),
    ] {
        let mut value = valid();
        value[key] = invalid_value;
        assert!(parse_lesson(&value.to_string()).is_none(), "{value}");
    }
    for kind in ["strategy", "pitfall"] {
        for scope in ["project", "user"] {
            for outcome in ["succeeded", "failed", "corrected_by_user"] {
                let mut value = valid();
                value["kind"] = json!(kind);
                value["scope"] = json!(scope);
                value["task_outcome"] = json!(outcome);
                value["evidence"] = json!(vec!["source"; 8]);
                assert!(parse_lesson(&value.to_string()).is_some());
            }
        }
    }
    let lesson = parse_lesson(&valid().to_string()).unwrap();
    assert_eq!(
        lesson.strings().collect::<Vec<_>>(),
        vec![
            "Test storage",
            "Changing storage",
            "1. Test it.",
            "Verify",
            "source"
        ]
    );
}
