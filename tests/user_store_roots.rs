use lambda_memory::domain_models::ProjectId;
use lambda_memory::domain_models::memory_tree::{
    CurationAnswer, CurationOp, CurationView, Slot, StoreKind, TreeRoot, roots,
};
use lambda_memory::domain_models::runtime::RuntimeLimits;
use lambda_memory::engines::tree_policy::validate_curation_rules_for_kind;

#[test]
fn store_kind_from_identity() {
    for (identity, kind) in [
        ("user:nicolas", StoreKind::User),
        ("repo:example.com/a/b", StoreKind::Project),
        ("path:/x", StoreKind::Project),
    ] {
        assert_eq!(StoreKind::of(&ProjectId::new(identity).unwrap()), kind);
    }
    assert_eq!(
        roots(StoreKind::Project),
        &[
            TreeRoot::Decisions,
            TreeRoot::Preferences,
            TreeRoot::Architecture,
            TreeRoot::OpenItems,
            TreeRoot::Lessons,
        ]
    );
    assert_eq!(
        roots(StoreKind::User),
        &[
            TreeRoot::Preferences,
            TreeRoot::WorkingRules,
            TreeRoot::GeneralLessons,
        ]
    );
}

#[test]
fn kind_parent_rules_drop_each_unoffered_root_without_rejecting_the_answer() {
    let all = [
        TreeRoot::Decisions,
        TreeRoot::Preferences,
        TreeRoot::Architecture,
        TreeRoot::OpenItems,
        TreeRoot::WorkingRules,
        TreeRoot::GeneralLessons,
        TreeRoot::Lessons,
    ];
    let view = CurationView {
        map: vec![],
        detailed: vec![],
        expandable: vec![],
        root_children: vec![],
    };
    for kind in [StoreKind::Project, StoreKind::User] {
        for root in all {
            let op = CurationOp::Create {
                links: vec![],
                parent: Slot::Root(root),
                title: "Title".into(),
                content: "Content".into(),
            };
            let answer = CurationAnswer {
                per_source: vec![vec![op.clone()]],
            };
            let result = validate_curation_rules_for_kind(
                kind,
                &view,
                &answer,
                1,
                &RuntimeLimits::default(),
            )
            .unwrap();
            if roots(kind).contains(&root) && root != TreeRoot::Lessons {
                assert_eq!(result.per_source, vec![vec![op]]);
                assert_eq!(result.dropped, 0);
            } else {
                assert_eq!(result.per_source, vec![vec![CurationOp::Discard]]);
                assert_eq!(result.dropped, 1);
                assert!(result.root_split_requests.is_empty());
            }
        }
    }
}
