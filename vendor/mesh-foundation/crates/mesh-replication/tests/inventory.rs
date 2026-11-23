use mesh_replication::{Inventory, InventoryError};

fn id(tag: u8) -> [u8; 32] {
    [tag; 32]
}

#[test]
fn inventories_compare_independently_of_page_and_event_order() {
    let mut left = Inventory::new(4, 2);
    let mut right = Inventory::new(4, 2);
    left.add_page(&[id(3), id(1)]).unwrap();
    left.add_page(&[id(2)]).unwrap();
    right.add_page(&[id(2), id(1)]).unwrap();
    right.add_page(&[id(3)]).unwrap();
    let left = left.finish().unwrap();
    let right = right.finish().unwrap();
    assert_eq!(left.fingerprint(), right.fingerprint());
    assert_eq!(left.missing_from(&right).next(), None);
}

#[test]
fn divergent_inventories_select_only_missing_ids() {
    let mut left = Inventory::new(4, 2);
    let mut right = Inventory::new(4, 2);
    left.add_page(&[id(1), id(3)]).unwrap();
    right.add_page(&[id(1), id(2)]).unwrap();
    let left = left.finish().unwrap();
    let right = right.finish().unwrap();
    assert_eq!(left.missing_from(&right).collect::<Vec<_>>(), [id(3)]);
    assert_eq!(right.missing_from(&left).collect::<Vec<_>>(), [id(2)]);
}

#[test]
fn page_and_total_budgets_apply_before_acceptance() {
    let mut inventory = Inventory::new(3, 2);
    assert_eq!(
        inventory.add_page(&[id(1), id(2), id(3)]),
        Err(InventoryError::PageLimit)
    );
    inventory.add_page(&[id(1), id(2)]).unwrap();
    assert_eq!(
        inventory.add_page(&[id(3), id(4)]),
        Err(InventoryError::TotalLimit)
    );
    inventory.add_page(&[id(3)]).unwrap();
    assert_eq!(inventory.finish().unwrap().len(), 3);
}

#[test]
fn duplicate_ids_cannot_qualify_for_digest_equality() {
    let mut inventory = Inventory::new(3, 1);
    inventory.add_page(&[id(1)]).unwrap();
    inventory.add_page(&[id(1)]).unwrap();
    assert_eq!(inventory.finish().err(), Some(InventoryError::Duplicate));
}

#[test]
fn streaming_digest_preserves_the_legacy_wire_summary() {
    let mut digest = mesh_replication::InventoryDigest::new(2);
    digest.update(&id(1));
    digest.update(&id(2));
    assert_eq!(
        hex::encode(digest.finish()),
        "ddccdd95280c87eeeabcb482da5a6ab34d2a55081ba6960ad0fb894cef41c436",
    );
}
