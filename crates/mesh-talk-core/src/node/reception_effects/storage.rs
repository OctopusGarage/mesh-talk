use super::*;
use crate::storage::profile::manifest;
use rand_core::{OsRng, RngCore};
use rusqlite::{OptionalExtension, params};

// Identities remain exact after pending metadata is retired. Otherwise replaying
// a committed journal entry after a host ACK could create a second unread event.
const SCHEMA: &str = "
CREATE TABLE reception_stream(account BLOB PRIMARY KEY CHECK(length(account)=32), epoch BLOB NOT NULL CHECK(length(epoch)=16), high_water INTEGER NOT NULL CHECK(high_water>=0), retired_through INTEGER NOT NULL CHECK(retired_through>=0 AND retired_through<=high_water));
CREATE TABLE reception_identities(sequence INTEGER PRIMARY KEY AUTOINCREMENT, account BLOB NOT NULL CHECK(length(account)=32), kind INTEGER NOT NULL CHECK(kind BETWEEN 1 AND 4), conversation BLOB NOT NULL CHECK(length(conversation)=32), event_id BLOB NOT NULL CHECK(length(event_id)=32), terminal_key TEXT NOT NULL CHECK(length(terminal_key)<=200), UNIQUE(account,kind,conversation,event_id,terminal_key));
CREATE INDEX reception_account_sequence ON reception_identities(account,sequence);
CREATE TABLE reception_pending(sequence INTEGER PRIMARY KEY REFERENCES reception_identities(sequence), metadata BLOB NOT NULL CHECK(length(metadata)<=2048));
CREATE TABLE reception_consumers(account BLOB NOT NULL CHECK(length(account)=32), kind INTEGER NOT NULL CHECK(kind BETWEEN 1 AND 3), acknowledged INTEGER NOT NULL CHECK(acknowledged>=0), PRIMARY KEY(account,kind));
";

pub(super) struct StreamState {
    pub head: ReceptionCursor,
    pub retired: u64,
}

pub(super) fn active_account_on(c: &Connection) -> Result<[u8; 32], LogError> {
    c.query_row(
        "SELECT account FROM participant_profile WHERE singleton=1",
        [],
        |row| row.get::<_, Vec<u8>>(0),
    )?
    .try_into()
    .map_err(|_| invalid("invalid reception account authority"))
}
fn ensure_stream_on(c: &Connection, account: &[u8; 32]) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM reception_stream WHERE account=?1)",
        [account.as_slice()],
        |row| row.get(0),
    )?;
    if !present {
        let mut epoch = [0; 16];
        OsRng.fill_bytes(&mut epoch);
        c.execute(
            "INSERT INTO reception_stream VALUES(?1,?2,0,0)",
            params![account.as_slice(), epoch.as_slice()],
        )?;
    }
    Ok(())
}
pub(super) fn state_on(c: &Connection, account: &[u8; 32]) -> Result<StreamState, LogError> {
    if active_account_on(c)? != *account {
        return Err(LogError::StaleCursor);
    }
    let mut statement = c.prepare(
        "SELECT epoch,high_water,retired_through FROM reception_stream WHERE account=?1",
    )?;
    let mut rows = statement.query([account.as_slice()])?;
    let row = rows
        .next()?
        .ok_or_else(|| invalid("missing reception stream owner"))?;
    Ok(StreamState {
        head: ReceptionCursor {
            epoch: blob(row, 0)?,
            sequence: natural(row.get(1)?)?,
        },
        retired: natural(row.get(2)?)?,
    })
}

pub(super) fn validate_cursor(
    state: &StreamState,
    cursor: ReceptionCursor,
) -> Result<(), LogError> {
    if cursor.epoch != state.head.epoch
        || cursor.sequence > state.head.sequence
        || cursor.sequence < state.retired
    {
        return Err(LogError::StaleCursor);
    }
    Ok(())
}

/// Called inside the existing plaintext/evidence transaction, before its journal
/// intent is retired. The source body must already exist in that transaction.
pub(crate) fn install_on(
    c: &Connection,
    source: ReceptionSource,
    metadata: &ReceptionMetadata,
) -> Result<u64, LogError> {
    metadata.validate(source.kind)?;
    let account = active_account_on(c)?;
    ensure_stream_on(c, &account)?;
    let terminal_key = if metadata.reception_only {
        metadata.batch_id.as_deref().unwrap_or_default()
    } else {
        ""
    };
    let previous: Option<i64> = c.query_row(
        "SELECT sequence FROM reception_identities WHERE kind=?1 AND conversation=?2 AND event_id=?3 AND terminal_key=?4 AND account=?5",
        params![source.kind.tag(),source.conversation.as_bytes().as_slice(),source.event_id.as_bytes().as_slice(),terminal_key,account.as_slice()], |row|row.get(0),
    ).optional()?;
    if let Some(sequence) = previous {
        return natural(sequence);
    }
    if !metadata.reception_only {
        let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM transcript_rows WHERE kind=?1 AND conversation=?2 AND row_key=?3)",
            params![source.kind.transcript_kind(),source.conversation.as_bytes().as_slice(),source.event_id.as_bytes().as_slice()],|row|row.get(0))?;
        if !exists {
            return Err(invalid(
                "reception effect has no installed transcript source",
            ));
        }
    }
    let encoded =
        bincode::serialize(metadata).map_err(|_| invalid("invalid reception encoding"))?;
    if encoded.len() > MAX_METADATA_BYTES {
        return Err(invalid("reception metadata exceeds budget"));
    }
    c.execute("INSERT INTO reception_identities(kind,conversation,event_id,terminal_key,account) VALUES(?1,?2,?3,?4,?5)",params![source.kind.tag(),source.conversation.as_bytes().as_slice(),source.event_id.as_bytes().as_slice(),terminal_key,account.as_slice()])?;
    let sequence = c.last_insert_rowid();
    c.execute(
        "INSERT INTO reception_pending VALUES(?1,?2)",
        params![sequence, encoded],
    )?;
    if c.execute(
        "UPDATE reception_stream SET high_water=?1 WHERE account=?2 AND high_water<?1",
        params![sequence, account.as_slice()],
    )? != 1
    {
        return Err(invalid("reception sequence did not advance"));
    }
    natural(sequence)
}

impl ReceptionEffects {
    pub(crate) fn open(profile: Arc<ProfileStorage>) -> Result<Self, LogError> {
        let account=profile.database().transaction(|c| {
            let present: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='reception_stream')",[],|row|row.get(0))?;
            if !present {
                let partial: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('reception_identities','reception_pending','reception_consumers'))",[],|row|row.get(0))?;
                if partial { return Err(invalid("incomplete reception schema")); }
                c.execute_batch(SCHEMA)?;
            }
            manifest::validate_ddl(c,SCHEMA)?;
            let account=active_account_on(c)?;
            ensure_stream_on(c,&account)?;
            state_on(c,&account)?;
            Ok(account)
        })?;
        Ok(Self {
            profile,
            account,
            #[cfg(test)]
            body_reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    pub(crate) fn head(&self) -> Result<ReceptionCursor, LogError> {
        self.profile
            .database()
            .read(|c| Ok(state_on(c, &self.account)?.head))
    }

    /// A consumer name is fixed by the host role, never allocated per window or
    /// per reconnect. Existing durable progress wins over a caller's baseline.
    pub(crate) fn register(
        &self,
        consumer: ReceptionConsumer,
        baseline: ReceptionCursor,
    ) -> Result<ReceptionCursor, LogError> {
        self.profile.database().transaction(|c| {
            let state = state_on(c, &self.account)?;
            if baseline.epoch != state.head.epoch {
                return Err(LogError::StaleCursor);
            }
            if let Some(sequence) = c
                .query_row(
                    "SELECT acknowledged FROM reception_consumers WHERE kind=?1 AND account=?2",
                    params![consumer.tag(), self.account.as_slice()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            {
                return Ok(ReceptionCursor {
                    sequence: natural(sequence)?,
                    ..state.head
                });
            }
            validate_cursor(&state, baseline)?;
            c.execute(
                "INSERT INTO reception_consumers VALUES(?3,?1,?2)",
                params![
                    consumer.tag(),
                    integer(baseline.sequence)?,
                    self.account.as_slice()
                ],
            )?;
            Ok(baseline)
        })
    }
    /// ACK means the host committed its own processing/snapshot, never that a
    /// network peer received or read the message. Expected progress fences retries.
    pub(crate) fn acknowledge(
        &self,
        consumer: ReceptionConsumer,
        expected: ReceptionCursor,
        next: ReceptionCursor,
    ) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            let state = state_on(c,&self.account)?;
            validate_cursor(&state, expected)?;
            validate_cursor(&state, next)?;
            if next.sequence < expected.sequence {
                return Err(LogError::StaleCursor);
            }
            let changed = c.execute(
                "UPDATE reception_consumers SET acknowledged=?1 WHERE kind=?2 AND acknowledged=?3 AND account=?4",
                params![
                    integer(next.sequence)?,
                    consumer.tag(),
                    integer(expected.sequence)?,self.account.as_slice()
                ],
            )?;
            if changed != 1 {
                return Err(LogError::StaleCursor);
            }
            Ok(())
        })
    }
    /// Retire only pending metadata, in bounded work, after every enrolled fixed
    /// consumer has committed the prefix. No consumers means retain pending work.
    pub(crate) fn retire(&self, rows: usize) -> Result<usize, LogError> {
        if rows == 0 || rows > MAX_RECEPTION_ROWS {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().transaction(|c| {
            state_on(c,&self.account)?;
            let floor: Option<i64> = c.query_row("SELECT MIN(acknowledged) FROM reception_consumers WHERE account=?1",[self.account.as_slice()],|row|row.get(0))?;
            let Some(floor) = floor else { return Ok(0) };
            let sequences = {
                let mut statement=c.prepare("SELECT p.sequence FROM reception_pending p JOIN reception_identities i ON i.sequence=p.sequence WHERE p.sequence<=?1 AND i.account=?3 ORDER BY p.sequence LIMIT ?2")?;
                statement.query_map(params![floor,rows as i64,self.account.as_slice()],|row|row.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?
            };
            for sequence in &sequences { c.execute("DELETE FROM reception_pending WHERE sequence=?1",[sequence])?; }
            let next: Option<i64> = c.query_row("SELECT MIN(p.sequence) FROM reception_pending p JOIN reception_identities i ON i.sequence=p.sequence WHERE p.sequence<=?1 AND i.account=?2",params![floor,self.account.as_slice()],|row|row.get(0))?;
            let retired = next.map_or(floor,|sequence|sequence-1);
            c.execute("UPDATE reception_stream SET retired_through=MAX(retired_through,?1) WHERE account=?2",params![retired,self.account.as_slice()])?;
            Ok(sequences.len())
        })
    }
}
