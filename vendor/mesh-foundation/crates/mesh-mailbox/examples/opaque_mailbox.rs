use mesh_mailbox::{
    Record, ScopeId,
    capacity::CapacityLimits,
    service::MailboxStore,
    spool::{Deposit, Spool},
};
use mesh_storage::EncryptionKey;
use std::num::NonZeroU64;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let scope = ScopeId::new([7; 32]);
    let limits = CapacityLimits::new(100, 0, 1024 * 1024, 0).map_err(|_| "invalid limits")?;
    // Test-only fixed key. Hosts supply their protected key-management mechanism.
    let mut spool = Spool::open_with_key_provider(
        &dir.path().join("records"),
        scope,
        limits,
        NonZeroU64::new(60).unwrap(),
        |_| Ok(EncryptionKey::from_derived_key([5; 32])),
    )?;
    // Real hosts encrypt/authenticate this payload and authorize the depositor.
    let record = Record::new(scope, b"host-defined opaque bytes".to_vec());
    assert_eq!(spool.deposit(record.clone(), 1)?, Deposit::Stored);
    let page = spool.view(2).delta(&scope, None, 64)?;
    let records = spool
        .view(2)
        .bodies(&scope, &page.cursor, &page.ids, 4096)?;
    assert_eq!(records, [record]);
    println!(
        "Retrieved {} opaque record; no application event schema",
        records.len()
    );
    Ok(())
}
