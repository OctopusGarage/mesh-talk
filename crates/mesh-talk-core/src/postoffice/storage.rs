//! Relay-owned arrival metadata in the shared encrypted profile connection.
//! Legacy duplicate arrival records retain the old reader's last-record-wins
//! semantics; missing arrival histories start their clock at upgrade as before.
use super::{MailboxArrival, RETENTION_MAGIC};
use crate::{
    eventlog::LogError,
    storage::{
        database::EncryptedDatabase,
        profile::{
            context::{RELAY_ARRIVALS, RELAY_EVENTS},
            manifest::{invalid, validate_ddl},
            publication::{cut, observe},
        },
        record_log::EncryptedRecordLog,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
const SCHEMA:&str="
CREATE TABLE relay_arrival_stage(conversation BLOB PRIMARY KEY CHECK(length(conversation)=32), first_seen BLOB NOT NULL CHECK(length(first_seen)=8)) WITHOUT ROWID;
CREATE TABLE relay_mailboxes(domain BLOB NOT NULL CHECK(length(domain)=16), conversation BLOB NOT NULL CHECK(length(conversation)=32), first_seen BLOB NOT NULL CHECK(length(first_seen)=8), touched BLOB NOT NULL CHECK(length(touched)=8), PRIMARY KEY(domain,conversation)) WITHOUT ROWID;
CREATE INDEX relay_mailboxes_expiry ON relay_mailboxes(domain,first_seen,conversation);
CREATE INDEX relay_mailboxes_lru ON relay_mailboxes(domain,touched,conversation);
";
pub(crate) fn initialize(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.transaction(|tx| {
        tx.execute_batch(SCHEMA)?;
        Ok(())
    })
}
pub(crate) fn validate(db: &EncryptedDatabase) -> Result<(), LogError> {
    db.read(|c| validate_ddl(c, SCHEMA))
}
pub(crate) fn authenticate(
    path: &Path,
    password: &str,
    mut visit: impl FnMut() -> Result<(), LogError>,
) -> Result<(), LogError> {
    drop(EncryptedRecordLog::open_with_visitor(
        path,
        password,
        RETENTION_MAGIC,
        |_: MailboxArrival| visit(),
    )?);
    Ok(())
}
pub(crate) fn reclaim_step(db: &EncryptedDatabase) -> Result<bool, LogError> {
    db.transaction(|tx| {
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT conversation FROM relay_arrival_stage ORDER BY conversation LIMIT 128",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for id in &ids {
            tx.execute(
                "DELETE FROM relay_arrival_stage WHERE conversation=?1",
                [id],
            )?;
        }
        Ok(ids.len() < 128)
    })
}
pub(crate) fn clear(db: &EncryptedDatabase) -> Result<(), LogError> {
    while !reclaim_step(db)? {}
    Ok(())
}
pub(crate) fn stage(db: &EncryptedDatabase, path: &Path, password: &str) -> Result<i64, LogError> {
    clear(db)?;
    let mut count = 0i64;
    drop(EncryptedRecordLog::<MailboxArrival>::open_with_visitor(
        path,
        password,
        RETENTION_MAGIC,
        |record| {
            observe("decode");
            db.transaction(|tx|{tx.execute("INSERT INTO relay_arrival_stage VALUES(?1,?2) ON CONFLICT(conversation) DO UPDATE SET first_seen=excluded.first_seen",params![record.conversation.as_bytes().as_slice(),record.first_seen_secs.to_be_bytes().as_slice()])?;Ok(())})?;
            count = count
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            cut("arrival-batch")?;
            Ok(())
        },
    )?);
    db.transaction(|tx| {
        let unique: i64 =
            tx.query_row("SELECT count(*) FROM relay_arrival_stage", [], |r| r.get(0))?;
        tx.execute(
            "UPDATE relay_sources SET unique_records=?1 WHERE source=1",
            [unique],
        )?;
        Ok(())
    })?;
    Ok(count)
}
pub(crate) fn publish(c: &Connection, now: u64) -> Result<(), LogError> {
    let unique: i64 = c.query_row("SELECT count(*) FROM relay_arrival_stage", [], |r| r.get(0))?;
    let (expected, records): (i64, i64) = c.query_row(
        "SELECT unique_records,imported_records FROM relay_sources WHERE source=1 AND phase=3",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if unique != expected || unique > records {
        return Err(invalid("verified arrival source coverage changed"));
    }
    let mailboxes: i64 = c.query_row(
        "SELECT count(*) FROM event_conversations WHERE domain=?1 AND event_count>0",
        [RELAY_EVENTS.0.as_slice()],
        |r| r.get(0),
    )?;
    // Legacy cold opens had no persisted LRU source and seeded arbitrary
    // distinct ticks. Keep expiry separate and seed deterministic ticks through
    // bounded indexed metadata lookups in this same publication transaction.
    let mut after = Vec::<u8>::new();
    let mut tick = 0u64;
    loop {
        let next:Option<(Vec<u8>,Option<Vec<u8>>)>=c.query_row(
            "SELECT e.conversation,a.first_seen FROM event_conversations e INDEXED BY event_conversations_active LEFT JOIN relay_arrival_stage a ON a.conversation=e.conversation WHERE e.domain=?1 AND e.event_count>0 AND e.conversation>?2 ORDER BY e.conversation LIMIT 1",
            params![RELAY_EVENTS.0.as_slice(),after],|r|Ok((r.get(0)?,r.get(1)?))
        ).optional()?;
        let Some((conversation, first_seen)) = next else {
            break;
        };
        tick = tick
            .checked_add(1)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let first_seen = first_seen.unwrap_or_else(|| now.to_be_bytes().to_vec());
        c.execute(
            "INSERT INTO relay_mailboxes VALUES(?1,?2,?3,?4)",
            params![
                RELAY_ARRIVALS.as_slice(),
                conversation,
                first_seen,
                tick.to_be_bytes().as_slice()
            ],
        )?;
        after = conversation;
    }
    if i64::try_from(tick).map_err(|_| LogError::RepositoryCounterExhausted)? != mailboxes {
        return Err(invalid("published mailbox count differs from event domain"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::{ConversationId, Event, EventKind},
        identity::device::DeviceIdentity,
        storage::profile::{ProfileStorageLease, context::ProfileStorage},
    };
    #[test]
    fn publication_preserves_last_arrival_and_seeds_missing_clock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("relay.custom");
        let device = DeviceIdentity::generate();
        let first = ConversationId::new([1; 32]);
        let missing = ConversationId::new([2; 32]);
        let stale = ConversationId::new([3; 32]);
        let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
        for conversation in [first, missing] {
            writer
                .append_durable(&Event::new(
                    &device,
                    conversation,
                    1,
                    vec![],
                    1,
                    1,
                    EventKind::Message,
                    vec![],
                ))
                .unwrap();
        }
        drop(writer);
        let (mut writer, _) = EncryptedRecordLog::<MailboxArrival>::open(
            &path.with_extension("retention"),
            "pw",
            RETENTION_MAGIC,
        )
        .unwrap();
        for (conversation, first_seen_secs) in [(first, 20), (stale, 5), (first, u64::MAX)] {
            writer
                .append_durable(&MailboxArrival {
                    conversation,
                    first_seen_secs,
                })
                .unwrap();
        }
        drop(writer);
        let profile = ProfileStorage::open_relay(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &device.public(),
            &path,
            "pw",
            123,
        )
        .unwrap();
        profile.database().read(|c|{
            let coverage:(i64,i64)=c.query_row("SELECT imported_records,unique_records FROM relay_sources WHERE source=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(coverage,(3,2));
            let count:i64=c.query_row("SELECT count(*) FROM relay_mailboxes",[],|r|r.get(0))?;assert_eq!(count,2);
            for (conversation,expected) in [(first,u64::MAX),(missing,123)] {let value:Vec<u8>=c.query_row("SELECT first_seen FROM relay_mailboxes WHERE domain=?1 AND conversation=?2",params![RELAY_ARRIVALS.as_slice(),conversation.as_bytes().as_slice()],|r|r.get(0))?;assert_eq!(value,expected.to_be_bytes());}Ok(())
        }).unwrap();
    }
    #[test]
    fn wrong_arrival_password_does_not_freeze_valid_event_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("relay.custom");
        let device = DeviceIdentity::generate();
        drop(EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap());
        let arrival = path.with_extension("retention");
        let (mut writer, _) =
            EncryptedRecordLog::<MailboxArrival>::open(&arrival, "other", RETENTION_MAGIC).unwrap();
        writer
            .append_durable(&MailboxArrival {
                conversation: ConversationId::new([1; 32]),
                first_seen_secs: 2,
            })
            .unwrap();
        drop(writer);
        let before = std::fs::read(&path).unwrap();
        let arrivals = std::fs::read(&arrival).unwrap();
        assert!(
            ProfileStorage::open_relay(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &device.public(),
                &path,
                "pw",
                123
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(std::fs::read(arrival).unwrap(), arrivals);
        assert!(!root.path().join("profile.db").exists());
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::{
        eventlog::{ConversationId, Event, EventKind},
        identity::device::DeviceIdentity,
        storage::profile::{ProfileStorageLease, context::ProfileStorage, publication::tests::CUT},
    };
    #[test]
    fn interrupted_arrival_batch_restarts_and_publishes_last_record_atomically() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("relay.log");
        let identity = DeviceIdentity::generate();
        let conversation = ConversationId::new([1; 32]);
        let (mut events, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
        events
            .append_durable(&Event::new(
                &identity,
                conversation,
                1,
                vec![],
                1,
                1,
                EventKind::Message,
                vec![],
            ))
            .unwrap();
        drop(events);
        let (mut arrivals, _) = EncryptedRecordLog::<MailboxArrival>::open(
            &path.with_extension("retention"),
            "pw",
            RETENTION_MAGIC,
        )
        .unwrap();
        for first_seen_secs in [1, 9] {
            arrivals
                .append_durable(&MailboxArrival {
                    conversation,
                    first_seen_secs,
                })
                .unwrap();
        }
        drop(arrivals);
        CUT.with(|v| v.replace(Some("arrival-batch")));
        assert!(
            ProfileStorage::open_relay(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &identity.public(),
                &path,
                "pw",
                123
            )
            .is_err()
        );
        let profile = ProfileStorage::open_relay(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &identity.public(),
            &path,
            "pw",
            123,
        )
        .unwrap();
        profile
            .database()
            .read(|c| {
                let time: Vec<u8> = c.query_row(
                    "SELECT first_seen FROM relay_mailboxes WHERE domain=?1 AND conversation=?2",
                    params![
                        RELAY_ARRIVALS.as_slice(),
                        conversation.as_bytes().as_slice()
                    ],
                    |r| r.get(0),
                )?;
                assert_eq!(time, 9u64.to_be_bytes());
                let published: i64 = c.query_row(
                    "SELECT count(*) FROM relay_sources WHERE phase=4",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(published, 2);
                Ok(())
            })
            .unwrap();
    }
}

#[cfg(test)]
mod touch_tests {
    use super::*;
    use crate::{
        eventlog::{ConversationId, Event, EventKind},
        identity::device::DeviceIdentity,
        storage::profile::{ProfileStorageLease, context::ProfileStorage},
    };
    #[test]
    fn upgrade_seeds_distinct_lru_ticks_independently_of_repeated_expiry_times() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("relay.log");
        let identity = DeviceIdentity::generate();
        let (mut events, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
        let (mut arrivals, _) = EncryptedRecordLog::<MailboxArrival>::open(
            &path.with_extension("retention"),
            "pw",
            RETENTION_MAGIC,
        )
        .unwrap();
        for (key, first_seen_secs) in [(1, 100), (2, 100), (3, 5)] {
            let conversation = ConversationId::new([key; 32]);
            events
                .append_durable(&Event::new(
                    &identity,
                    conversation,
                    1,
                    vec![],
                    1,
                    1,
                    EventKind::Message,
                    vec![],
                ))
                .unwrap();
            arrivals
                .append_durable(&MailboxArrival {
                    conversation,
                    first_seen_secs,
                })
                .unwrap();
        }
        drop(events);
        drop(arrivals);
        let profile = ProfileStorage::open_relay(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &identity.public(),
            &path,
            "pw",
            123,
        )
        .unwrap();
        profile.database().read(|c| {
            for (key,time,tick) in [(1,100u64,1u64),(2,100,2),(3,5,3)]{let (first,touched):(Vec<u8>,Vec<u8>)=c.query_row("SELECT first_seen,touched FROM relay_mailboxes WHERE domain=?1 AND conversation=?2",params![RELAY_ARRIVALS.as_slice(),[key as u8;32].as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(first,time.to_be_bytes());assert_eq!(touched,tick.to_be_bytes());}
            let max:Vec<u8>=c.query_row("SELECT touched FROM relay_mailboxes WHERE domain=?1 ORDER BY touched DESC,conversation DESC LIMIT 1",[RELAY_ARRIVALS.as_slice()],|r|r.get(0))?;assert_eq!(max,3u64.to_be_bytes());Ok(())
        }).unwrap();
    }
}
