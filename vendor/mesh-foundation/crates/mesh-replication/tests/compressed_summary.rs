use mesh_replication::range::{Prefix, RangeIndex, Summary};
#[test]
fn endpoint_prefix_and_unary_lift_match_the_canonical_index() {
    let a = [0; 32];
    let mut b = [0; 32];
    b[31] = 3;
    let from = Prefix::common(a, b);
    assert_eq!(from.bits(), 254);
    let mut index = RangeIndex::default();
    index.insert(a);
    index.insert(b);
    for bits in [0, 1, 128, 253, 254] {
        let to = Prefix::new(bits, a).unwrap();
        assert_eq!(index.summary(from).lift(from, to), Some(index.summary(to)));
    }
    assert_eq!(
        Summary::empty().lift(from, Prefix::root()),
        Some(Summary::empty())
    );
    assert!(
        index
            .summary(from)
            .lift(from, Prefix::new(255, a).unwrap())
            .is_none()
    );
    assert!(
        Summary::leaf(a)
            .lift(
                Prefix::new(256, a).unwrap(),
                Prefix::new(1, [255; 32]).unwrap()
            )
            .is_none()
    );
    assert!(
        Summary {
            count: 2,
            digest: [0; 32]
        }
        .lift(Prefix::new(256, a).unwrap(), Prefix::root())
        .is_none()
    );
}
