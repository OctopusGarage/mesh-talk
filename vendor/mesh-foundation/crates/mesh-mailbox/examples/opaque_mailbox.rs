use mesh_mailbox::{
    Record, ScopeId,
    capacity::CapacityLimits,
    ports::MailboxStore,
    spool::{Deposit, Retention, Spool, SpoolOptions},
};
use mesh_storage::EncryptionKey;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let scope = ScopeId::new([7; 32]);
    let limits = CapacityLimits::new(100, 0, 1024 * 1024, 0).map_err(|_| "invalid limits")?;
    // Test-only fixed key. Hosts supply their protected key-management mechanism.
    let mut spool = Spool::open_configured(
        &dir.path().join("records"),
        scope,
        SpoolOptions {
            capacity: limits,
            retention: Retention::Manual,
            max_record_bytes: 512 * 1024,
            max_query_records: 100,
        },
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
