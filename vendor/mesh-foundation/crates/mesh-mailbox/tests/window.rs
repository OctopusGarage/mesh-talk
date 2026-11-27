use mesh_mailbox::delta::{DeltaCursor, DeltaPosition, WindowSelection};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Cursor {
    generation: u64,
    position: DeltaPosition,
}
impl DeltaCursor for Cursor {
    fn accepts(&self, current: &Self) -> bool {
        self.generation == current.generation
            && self.position.after <= current.position.after
            && self.position.revision <= current.position.revision
    }
    fn position(&self) -> DeltaPosition {
        self.position
    }
    fn set_position(&mut self, position: DeltaPosition) {
        self.position = position;
    }
}
fn cursor(generation: u64, after: i64, revision: i64) -> Cursor {
    Cursor {
        generation,
        position: DeltaPosition {
            after,
            revision,
            window: None,
        },
    }
}
#[test]
fn host_binding_and_id_types_share_one_finite_window_engine() {
    let current = cursor(1, 20, 4);
    let WindowSelection::Scan(mut scan) =
        WindowSelection::<_, u32>::open(&current, None, 1).unwrap()
    else {
        panic!()
    };
    scan.push(10, 7).unwrap();
    assert!(!scan.accept_row());
    let first = scan.finish();
    assert_eq!(first.cursor.position.window, Some((20, 4)));
    let WindowSelection::Scan(mut next) =
        WindowSelection::open(&cursor(1, 30, 5), Some(&first.cursor), 1).unwrap()
    else {
        panic!()
    };
    next.push(12, 8u32).unwrap();
    let end = next.finish();
    assert_eq!(end.cursor, current);
    assert!(end.complete);
    assert_eq!(end.ids, [8]);
    let WindowSelection::Scan(reset) =
        WindowSelection::<_, u32>::open(&cursor(2, 30, 5), Some(&first.cursor), 1).unwrap()
    else {
        panic!()
    };
    let page = reset.finish();
    assert!(page.reset);
    assert_eq!(page.cursor.generation, 2);
}
#[test]
fn lookahead_and_budget_deferral_never_acknowledge_unconsumed_rows() {
    let current = cursor(1, 20, 4);
    let WindowSelection::Scan(mut scan) =
        WindowSelection::<_, u32>::open(&current, None, 2).unwrap()
    else {
        panic!()
    };
    assert!(scan.push(-1, 1).is_err());
    scan.push(10, 7).unwrap();
    scan.defer_remaining();
    let page = scan.finish();
    assert!(!page.complete);
    assert_eq!(page.cursor.position.after, 11);
    assert_eq!(page.cursor.position.window, Some((20, 4)));
    assert!(WindowSelection::<_, u32>::open(&current, None, usize::MAX).is_err());
}
