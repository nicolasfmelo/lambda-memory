use lambda_memory::domain_models::memory_tree::*;
use lambda_memory::domain_models::runtime::RuntimeLimits;
use lambda_memory::domain_models::{MemoryId, RevisionId};
use lambda_memory::engines::tree_policy::*;

fn node(i: usize) -> TreeNode {
    TreeNode {
        memory: MemoryId::new(format!("m{i}")).unwrap(),
        revision: RevisionId::new(format!("r{i}")).unwrap(),
        root: TreeRoot::Decisions,
        parent: None,
        depth: 1,
        title: format!("title{i}"),
        content: "content".into(),
        direct_sources: 0,
        children: 0,
        last_linked_ms: 0,
    }
}
fn view() -> CurationView {
    CurationView {
        map: vec![node(0), node(1)],
        detailed: vec![node(2)],
        expandable: vec![0],
        root_children: vec![],
    }
}
fn merge(n: usize) -> CurationOp {
    CurationOp::Merge {
        node: n,
        title: "title".into(),
        content: "content".into(),
    }
}
fn create(parent: Slot) -> CurationOp {
    CurationOp::Create {
        parent,
        title: "title".into(),
        content: "content".into(),
    }
}
fn check(
    v: &CurationView,
    ops: Vec<Vec<CurationOp>>,
    n: usize,
) -> Result<ValidatedCuration, TreeError> {
    validate_curation(
        v,
        &CurationAnswer { per_source: ops },
        n,
        &RuntimeLimits::default(),
    )
}
#[test]
fn merge_into_unknown_slot_is_rejected() {
    assert_eq!(
        check(&view(), vec![vec![merge(99)]], 1),
        Err(TreeError::InvalidOutput)
    );
}
#[test]
fn two_merges_into_same_node_are_allowed() {
    let validated = check(&view(), vec![vec![merge(1)], vec![merge(1)]], 2).unwrap();
    assert!(validated.parked.is_empty());
    assert_eq!(validated.per_source.len(), 2);
}
#[test]
fn every_source_needs_an_answer_and_an_empty_one_is_a_discard() {
    assert!(check(&view(), vec![], 1).is_err());
    assert_eq!(
        check(&view(), vec![vec![]], 1).unwrap().per_source,
        vec![vec![CurationOp::Discard]]
    );
}
#[test]
fn extra_ops_are_dropped_beyond_the_cap() {
    let validated = check(&view(), vec![vec![CurationOp::Discard; 6]], 1).unwrap();
    assert_eq!(validated.per_source[0].len(), MAX_OPERATIONS);
}
#[test]
fn create_under_full_parent_parks_and_requests_split() {
    let mut v = view();
    v.map[0].children = 16;
    let result = check(&v, vec![vec![create(Slot::Node(0))]], 1).unwrap();
    assert_eq!(result.parked, vec![0]);
    assert_eq!(result.split_requests, vec![0]);
}
#[test]
fn create_beyond_depth_is_dropped() {
    let mut v = view();
    v.map[0].depth = 4;
    let validated = check(&v, vec![vec![create(Slot::Node(0))]], 1).unwrap();
    assert_eq!(
        (validated.dropped, validated.per_source[0].clone()),
        (1, vec![CurationOp::Discard])
    );
}
#[test]
fn expand_parks_for_any_known_node_and_rejects_unknown() {
    assert_eq!(
        check(&view(), vec![vec![CurationOp::Expand { node: 2 }]], 1)
            .unwrap()
            .parked,
        vec![0]
    );
    assert_eq!(
        validate_curation_rules(
            &view(),
            &CurationAnswer {
                per_source: vec![vec![CurationOp::Expand { node: 9 }]]
            },
            1,
            &RuntimeLimits::default()
        ),
        Err(TreeRule::UnknownSlot)
    );
    assert_eq!(
        check(&view(), vec![vec![CurationOp::Expand { node: 0 }]], 1)
            .unwrap()
            .parked,
        vec![0]
    );
}
#[test]
fn content_with_secret_rejected() {
    assert!(validate_content(&format!("sk-ant-{}", "a".repeat(24))).is_err());
}
#[test]
fn title_over_80_or_multiline_rejected() {
    assert!(validate_title(&"a".repeat(81)).is_err());
    assert!(validate_title("a\nb").is_err());
    assert!(validate_title(&"é".repeat(80)).is_ok());
}
#[test]
fn content_over_1200_bytes_rejected() {
    assert!(validate_content(&"é".repeat(601)).is_err());
    assert!(validate_content("  ").is_err());
}
#[test]
fn tree_map_respects_byte_cap_and_order() {
    let mut a = node(0);
    a.last_linked_ms = 3;
    let mut b = node(1);
    b.last_linked_ms = 3;
    b.direct_sources = 2;
    let mut c = node(2);
    c.depth = 3;
    assert_eq!(
        tree_map(&[a.clone(), b.clone(), c], b.title.len() + 16),
        vec![b]
    );
}
#[test]
fn source_terms_caps_at_twelve_and_skips_stopwords() {
    let terms = source_terms("Vamos usar SQLite para o vault de segredos, não Postgres");
    for s in ["sqlite", "vault", "segredos", "postgres"] {
        assert!(terms.contains(&s.to_string()));
    }
    for s in ["para", "vamos"] {
        assert!(!terms.contains(&s.to_string()));
    }
    assert_eq!(
        source_terms("aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk llll mmmm").len(),
        12
    );
}
#[test]
fn split_partition_must_be_complete_and_disjoint() {
    let make = |parts: Vec<Vec<usize>>| SplitAnswer {
        node_title: "node".into(),
        node_content: "content".into(),
        children: parts
            .into_iter()
            .map(|p| ("title".into(), "content".into(), p))
            .collect(),
    };
    let n = node(0);
    let l = RuntimeLimits::default();
    assert!(validate_split(&n, 2, &make(vec![vec![0], vec![1]]), &l).is_ok());
    for p in [
        vec![vec![0], vec![0]],
        vec![vec![0], vec![]],
        vec![vec![0, 1]],
        vec![vec![0], vec![1], vec![], vec![], vec![]],
    ] {
        assert!(validate_split(&n, 2, &make(p), &l).is_err());
    }
}
#[test]
fn root_index_is_sorted_titles() {
    let mut a = node(0);
    a.title = "Zebra".into();
    let mut b = node(1);
    b.title = "Alpha".into();
    assert_eq!(root_index(&[a, b]), "- Alpha\n- Zebra");
}
#[test]
fn tree_limits_parse_and_fail_closed() {
    let defaults = RuntimeLimits::default();
    assert_eq!(
        (
            defaults.tree_max_children,
            defaults.tree_max_depth,
            defaults.tree_split_sources,
            defaults.tree_map_bytes,
            defaults.curation_batch
        ),
        (16, 4, 12, 6144, 4)
    );
    let keys = [
        "LAMBDA_MEMORY_TREE_MAX_CHILDREN",
        "LAMBDA_MEMORY_TREE_MAX_DEPTH",
        "LAMBDA_MEMORY_TREE_SPLIT_SOURCES",
        "LAMBDA_MEMORY_TREE_MAP_BYTES",
        "LAMBDA_MEMORY_CURATION_BATCH",
    ];
    // This integration test is the only test in this process accessing these variables.
    unsafe {
        std::env::set_var("LAMBDA_MEMORY_DATA_DIR", "test");
        std::env::set_var("LAMBDA_MEMORY_TOKEN", "a".repeat(32));
        std::env::set_var("LAMBDA_MEMORY_OWNER", "owner");
        std::env::set_var("LAMBDA_MEMORY_ACTOR", "actor");
        std::env::set_var("LAMBDA_MEMORY_PROJECT", "project");
        for key in keys {
            std::env::remove_var(key);
        }
    }
    for key in keys {
        for value in ["", "0", "invalid"] {
            unsafe {
                std::env::set_var(key, value);
            }
            assert!(
                lambda_memory::integrations::runtime_config::RuntimeConfig::from_environment()
                    .is_err(),
                "{key}={value:?}"
            );
        }
        unsafe {
            std::env::set_var(key, "2");
        }
    }
    let limits = lambda_memory::integrations::runtime_config::RuntimeConfig::from_environment()
        .unwrap()
        .limits;
    assert_eq!(
        (
            limits.tree_max_children,
            limits.tree_max_depth,
            limits.tree_split_sources,
            limits.tree_map_bytes,
            limits.curation_batch
        ),
        (2, 2, 2, 2, 2)
    );
    unsafe {
        std::env::set_var("LAMBDA_MEMORY_TREE_MAX_DEPTH", "256");
    }
    assert!(
        lambda_memory::integrations::runtime_config::RuntimeConfig::from_environment().is_err()
    );
}

#[test]
fn create_under_full_root_parks_and_requests_root_split() {
    let mut v = view();
    v.root_children = vec![(TreeRoot::Decisions, 63)];
    let op = create(Slot::Root(TreeRoot::Decisions));
    let result = check(&v, vec![vec![op.clone()], vec![op.clone()], vec![op]], 3).unwrap();
    assert_eq!(result.parked, vec![1, 2]);
    assert_eq!(result.root_split_requests, vec![TreeRoot::Decisions]);
    assert_eq!(result.per_source[0].len(), 1);
    assert!(result.per_source[1].is_empty());
    assert!(result.per_source[2].is_empty());
    assert!(
        check(
            &view(),
            vec![vec![create(Slot::Root(TreeRoot::Preferences))]],
            1
        )
        .unwrap()
        .parked
        .is_empty()
    );
}
fn root_split(parts: Vec<Vec<usize>>) -> SplitAnswer {
    SplitAnswer {
        node_title: "root".into(),
        node_content: "root content".into(),
        children: parts
            .into_iter()
            .map(|p| ("child".into(), "content".into(), p))
            .collect(),
    }
}
#[test]
fn root_split_partition_must_be_complete_and_disjoint() {
    let limits = RuntimeLimits::default();
    assert!(validate_root_split(3, &root_split(vec![vec![0, 1], vec![2]]), &limits).is_ok());
    for parts in [
        vec![vec![0], vec![0, 2]],
        vec![vec![0], vec![1]],
        vec![vec![0], vec![1, 3]],
        vec![vec![0, 1, 2]],
        vec![vec![0], vec![1], vec![2], vec![], vec![]],
    ] {
        assert!(validate_root_split(3, &root_split(parts), &limits).is_err());
    }
    let mut invalid = root_split(vec![vec![0], vec![1]]);
    invalid.node_title.clear();
    assert!(validate_root_split(2, &invalid, &limits).is_err());
    invalid.node_title = "root".into();
    invalid.children[0].1.clear();
    assert!(validate_root_split(2, &invalid, &limits).is_err());
}
#[test]
fn root_split_child_over_limit_rejected() {
    let limits = RuntimeLimits {
        tree_max_children: 2,
        ..Default::default()
    };
    assert_eq!(
        validate_root_split(4, &root_split(vec![vec![0, 1, 2], vec![3]]), &limits),
        Err(TreeError::Bounds)
    );
    assert!(validate_root_split(4, &root_split(vec![vec![0, 1], vec![2, 3]]), &limits).is_ok());
}
#[test]
fn parked_source_ops_are_cleared() {
    let mut v = view();
    v.map[1].children = 16;
    let result = check(
        &v,
        vec![
            vec![merge(1), create(Slot::Node(1))],
            vec![
                create(Slot::Root(TreeRoot::Architecture)),
                CurationOp::Expand { node: 0 },
            ],
            vec![CurationOp::Discard],
        ],
        3,
    )
    .unwrap();
    assert_eq!(result.parked, vec![0, 1]);
    assert!(result.per_source[0].is_empty());
    assert!(result.per_source[1].is_empty());
    assert_eq!(result.per_source[2], vec![CurationOp::Discard]);
}
#[test]
fn merge_into_map_only_node_is_parked_like_expand() {
    let validated = check(&view(), vec![vec![merge(0)]], 1).unwrap();
    assert_eq!(validated.parked, vec![0]);
    assert!(validated.per_source[0].is_empty());
    assert!(check(&view(), vec![vec![merge(1)]], 1).is_ok());
    assert!(check(&view(), vec![vec![merge(2)]], 1).is_ok());
}

#[test]
fn tree_map_caps_node_count() {
    let nodes: Vec<_> = (0..200).map(node).collect();
    assert_eq!(tree_map(&nodes, 1_000_000).len(), TREE_MAP_MAX_NODES);
}

#[test]
fn single_bad_operations_are_dropped_and_structure_still_fails() {
    let limits = RuntimeLimits::default();
    let answer = |ops: Vec<Vec<CurationOp>>| CurationAnswer { per_source: ops };
    assert_eq!(
        validate_curation_rules(&view(), &answer(vec![]), 1, &limits),
        Err(TreeRule::MissingOperation)
    );
    assert_eq!(
        validate_curation_rules(&view(), &answer(vec![vec![merge(9)]]), 1, &limits),
        Err(TreeRule::UnknownSlot)
    );
    let secret = CurationOp::Create {
        parent: Slot::Root(TreeRoot::Decisions),
        title: "T".into(),
        content: format!("key sk-ant-{}", "a".repeat(24)),
    };
    let empty = CurationOp::Create {
        parent: Slot::Root(TreeRoot::Decisions),
        title: " ".into(),
        content: "c".into(),
    };
    let good = CurationOp::Create {
        parent: Slot::Root(TreeRoot::Decisions),
        title: "T".into(),
        content: "C".into(),
    };
    let validated = validate_curation_rules(
        &view(),
        &answer(vec![vec![secret, empty, good.clone()]]),
        1,
        &limits,
    )
    .unwrap();
    assert_eq!(
        (validated.dropped, validated.per_source[0].clone()),
        (2, vec![good])
    );
}
