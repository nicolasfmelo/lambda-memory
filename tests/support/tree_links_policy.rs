use super::*;
fn link(to: usize, relation: LinkRelation) -> CandidateLink {
    CandidateLink { to, relation }
}
fn linked_merge(node: usize, links: Vec<CandidateLink>) -> CurationOp {
    CurationOp::Merge {
        node,
        title: "T".into(),
        content: "C".into(),
        links,
    }
}
#[test]
fn self_link_and_duplicates_are_dropped() {
    let mut view = view();
    view.detailed.push(view.map[1].clone()); // same identity in another slot
    let ops = vec![vec![
        linked_merge(
            1,
            vec![
                link(1, LinkRelation::Related),
                link(99, LinkRelation::Related),
                link(2, LinkRelation::Related),
                link(2, LinkRelation::Related),
            ],
        ),
        linked_merge(3, vec![link(2, LinkRelation::Related)]),
    ]];
    let result = check(&view, ops, 1).unwrap();
    assert_eq!(
        result.per_source[0],
        vec![
            linked_merge(1, vec![link(2, LinkRelation::Related)]),
            linked_merge(3, vec![])
        ]
    );
    let create = CurationOp::Create {
        parent: Slot::Node(1),
        title: "T".into(),
        content: "C".into(),
        links: vec![
            link(1, LinkRelation::Related),
            link(2, LinkRelation::Related),
        ],
    };
    let result = check(&view, vec![vec![create]], 1).unwrap();
    let CurationOp::Create { links, .. } = &result.per_source[0][0] else {
        panic!()
    };
    assert_eq!(links, &vec![link(2, LinkRelation::Related)]);
}
#[test]
fn replaces_requires_supersede() {
    let links = vec![
        link(2, LinkRelation::Replaces),
        link(2, LinkRelation::Related),
    ];
    let supersede = CurationOp::Supersede {
        node: 1,
        title: "T".into(),
        content: "C".into(),
        valid_from: None,
        links: links.clone(),
    };
    assert_eq!(
        check(&view(), vec![vec![supersede.clone()]], 1)
            .unwrap()
            .per_source[0],
        vec![supersede]
    );
    assert_eq!(
        check(&view(), vec![vec![linked_merge(1, links)]], 1)
            .unwrap()
            .per_source[0],
        vec![linked_merge(1, vec![link(2, LinkRelation::Related)])]
    );
}
#[test]
fn more_than_three_links_dropped() {
    let view = CurationView {
        map: vec![],
        detailed: (0..6).map(node).collect(),
        expandable: vec![],
        root_children: vec![],
    };
    let links: Vec<_> = (1..6).map(|i| link(i, LinkRelation::DependsOn)).collect();
    assert_eq!(
        check(&view, vec![vec![linked_merge(0, links.clone())]], 1)
            .unwrap()
            .per_source[0],
        vec![linked_merge(0, links[..3].to_vec())]
    );
}
