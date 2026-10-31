use super::*;
use rusqlite::StatementStatus;
#[test]
fn actual_role_selectors_bound_engine_steps_with_many_expired_rows() {
    // Isolated role-index fixture: no retained bodies, graph, or alternate charge.
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(crate::postoffice::storage::SCHEMA).unwrap();
    for n in 0..10_000u64 {
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&n.to_be_bytes());
        c.execute("INSERT INTO relay_mailboxes(domain,conversation,first_seen,touched) VALUES(?1,?2,?3,?4)",params![RELAY_ARRIVALS.as_slice(),key.as_slice(),0u64.to_be_bytes().as_slice(),n.to_be_bytes().as_slice()]).unwrap();
    }
    for (label, sql, args, index) in [
        (
            "next-after-cutoff",
            super::super::policy::SELECT_NEXT_AFTER,
            vec![RELAY_ARRIVALS.to_vec(), 0u64.to_be_bytes().to_vec()],
            "relay_mailboxes_expiry",
        ),
        (
            "next-first",
            super::super::policy::SELECT_NEXT_FIRST,
            vec![RELAY_ARRIVALS.to_vec()],
            "relay_mailboxes_expiry",
        ),
        (
            "pending-empty",
            SELECT_PENDING,
            vec![RELAY_ARRIVALS.to_vec()],
            "relay_mailboxes_pending",
        ),
        (
            "expired",
            SELECT_EXPIRED,
            vec![RELAY_ARRIVALS.to_vec(), 0u64.to_be_bytes().to_vec()],
            "relay_mailboxes_expiry",
        ),
        (
            "LRU-after-expiry-proof",
            SELECT_LRU,
            vec![RELAY_ARRIVALS.to_vec()],
            "relay_mailboxes_lru",
        ),
    ] {
        let mut stmt = c.prepare(sql).unwrap();
        let _ = stmt
            .query_row(rusqlite::params_from_iter(&args), |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .optional()
            .unwrap();
        let steps = stmt.get_status(StatementStatus::VmStep);
        println!("role {label}: 10000 expired rows, {steps} VM steps");
        assert!(steps < 100);
        assert_eq!(stmt.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(stmt.get_status(StatementStatus::Sort), 0);
        let detail: String = c
            .query_row(
                &format!("EXPLAIN QUERY PLAN {sql}"),
                rusqlite::params_from_iter(&args),
                |r| r.get(3),
            )
            .unwrap();
        assert!(detail.contains(index), "{label}: {detail}");
    }
}
