use lambda_memory::integrations::provider_config::ProviderConfig;
#[test]
fn model_defaults_and_explicit_environment_are_equivalent() {
    let defaults = ProviderConfig::from_lookup(|_| None).unwrap();
    let explicit = ProviderConfig::from_lookup(|key| match key {
        "LAMBDA_PLANNER_MODEL" => Some("anthropic/claude-haiku-5.5".into()),
        "LAMBDA_EMBEDDING_MODEL" => Some("google/gemini-embedding-2".into()),
        "LAMBDA_SELECTOR_MODEL" => Some("openai/gpt-6-luna-decisions".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(defaults.generator_candidates, explicit.generator_candidates);
    assert_eq!(defaults.embedding_model, explicit.embedding_model);
    assert_eq!(defaults.selector_model, explicit.selector_model);
}
#[test]
fn explicit_invalid_environment_never_silently_falls_back() {
    for name in [
        "LAMBDA_PLANNER_MODEL",
        "LAMBDA_EMBEDDING_MODEL",
        "LAMBDA_SELECTOR_MODEL",
    ] {
        for value in ["", "unreviewed/model", " openai/gpt-6-luna-decisions"] {
            assert!(
                ProviderConfig::from_lookup(|key| if key == name {
                    Some(value.into())
                } else {
                    None
                })
                .is_err()
            );
        }
    }
}
#[test]
fn unrelated_environment_cannot_change_models() {
    let config = ProviderConfig::from_lookup(|key| {
        if key == "OPEN_ROUTER_KEY" {
            panic!("model configuration must never read credential")
        } else {
            None
        }
    })
    .unwrap();
    assert_eq!(config.selector_model, "openai/gpt-6-luna-decisions");
}
