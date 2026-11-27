use mesh_replication::range::{Prefix, RangeIndex, Summary, singleton_summary};

#[test]
fn independent_adapter_uses_the_same_canonical_summary_rules() {
    let mut index = RangeIndex::default();
    let id = [7; 32];
    assert!(!index.contains(&id));
    index.insert(id);
    assert!(index.contains(&id));
    assert_eq!(
        Summary::empty(),
        RangeIndex::default().summary(Prefix::root())
    );
    assert_eq!(
        Summary::leaf(id),
        index.summary(Prefix::new(256, id).unwrap())
    );
    assert_eq!(
        singleton_summary(Prefix::root(), id),
        Some(index.summary(Prefix::root()))
    );
    let parent = Prefix::new(255, id).unwrap();
    let [left, right] = parent.children().unwrap();
    assert_eq!(
        Summary::combine(parent, index.summary(left), index.summary(right)),
        Some(index.summary(parent))
    );
    assert_eq!(
        Summary::combine(left, Summary::empty(), Summary::empty()),
        None
    );
    assert_eq!(
        Summary::combine(
            Prefix::root(),
            Summary {
                count: usize::MAX,
                digest: [0; 32]
            },
            Summary::leaf(id)
        ),
        None
    );
}
