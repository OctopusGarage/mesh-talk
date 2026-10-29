use super::*;
use crate::storage::profile::manifest;
use rand_core::{OsRng, RngCore};
use rusqlite::{OptionalExtension, params};

// Identities remain exact after pending metadata is retired. Otherwise replaying
// a committed journal entry after a host ACK could create a second unread event.
const SCHEMA: &str = "
CREATE TABLE reception_stream(singleton INTEGER PRIMARY KEY CHECK(singleton=1), epoch BLOB NOT NULL CHECK(length(epoch)=16), high_water INTEGER NOT NULL CHECK(high_water>=0), retired_through INTEGER NOT NULL CHECK(retired_through>=0 AND retired_through<=high_water));
CREATE TABLE reception_identities(sequence INTEGER PRIMARY KEY AUTOINCREMENT, kind INTEGER NOT NULL CHECK(kind BETWEEN 1 AND 4), conversation BLOB NOT NULL CHECK(length(conversation)=32), event_id BLOB NOT NULL CHECK(length(event_id)=32), terminal_key TEXT NOT NULL CHECK(length(terminal_key)<=200), UNIQUE(kind,conversation,event_id,terminal_key));
CREATE TABLE reception_pending(sequence INTEGER PRIMARY KEY REFERENCES reception_identities(sequence), metadata BLOB NOT NULL CHECK(length(metadata)<=2048));
CREATE TABLE reception_consumers(kind INTEGER PRIMARY KEY CHECK(kind BETWEEN 1 AND 3), acknowledged INTEGER NOT NULL CHECK(acknowledged>=0));
";

pub(super) struct StreamState {
    pub head: ReceptionCursor,
    pub retired: u64,
}

pub(super) fn state_on(c: &Connection) -> Result<StreamState, LogError> {
    let mut statement = c.prepare(
        "SELECT epoch,high_water,retired_through FROM reception_stream WHERE singleton=1",
    )?;
    let mut rows = statement.query([])?;
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
    let terminal_key = if metadata.reception_only {
        metadata.batch_id.as_deref().unwrap_or_default()
    } else {
        ""
    };
    let previous: Option<i64> = c.query_row(
        "SELECT sequence FROM reception_identities WHERE kind=?1 AND conversation=?2 AND event_id=?3 AND terminal_key=?4",
        params![source.kind.tag(),source.conversation.as_bytes().as_slice(),source.event_id.as_bytes().as_slice(),terminal_key], |row|row.get(0),
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
    c.execute("INSERT INTO reception_identities(kind,conversation,event_id,terminal_key) VALUES(?1,?2,?3,?4)",params![source.kind.tag(),source.conversation.as_bytes().as_slice(),source.event_id.as_bytes().as_slice(),terminal_key])?;
    let sequence = c.last_insert_rowid();
    c.execute(
        "INSERT INTO reception_pending VALUES(?1,?2)",
        params![sequence, encoded],
    )?;
    if c.execute(
        "UPDATE reception_stream SET high_water=?1 WHERE singleton=1 AND high_water<?1",
        [sequence],
    )? != 1
    {
        return Err(invalid("reception sequence did not advance"));
    }
    natural(sequence)
}

impl ReceptionEffects {
    pub(crate) fn open(profile: Arc<ProfileStorage>) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let present: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='reception_stream')",[],|row|row.get(0))?;
            if !present {
                let partial: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('reception_identities','reception_pending','reception_consumers'))",[],|row|row.get(0))?;
                if partial { return Err(invalid("incomplete reception schema")); }
                c.execute_batch(SCHEMA)?;
                let mut epoch = [0;16];
                OsRng.fill_bytes(&mut epoch);
                c.execute("INSERT INTO reception_stream VALUES(1,?1,0,0)",[epoch.as_slice()])?;
            }
            manifest::validate_ddl(c,SCHEMA)?;
            state_on(c)?;
            Ok(())
        })?;
        Ok(Self {
            profile,
            #[cfg(test)]
            body_reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    pub(crate) fn head(&self) -> Result<ReceptionCursor, LogError> {
        self.profile.database().read(|c| Ok(state_on(c)?.head))
    }

    /// A consumer name is fixed by the host role, never allocated per window or
    /// per reconnect. Existing durable progress wins over a caller's baseline.
    pub(crate) fn register(
        &self,
        consumer: ReceptionConsumer,
        baseline: ReceptionCursor,
    ) -> Result<ReceptionCursor, LogError> {
        self.profile.database().transaction(|c| {
            let state = state_on(c)?;
            if baseline.epoch != state.head.epoch {
                return Err(LogError::StaleCursor);
            }
            if let Some(sequence) = c
                .query_row(
                    "SELECT acknowledged FROM reception_consumers WHERE kind=?1",
                    [consumer.tag()],
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
                "INSERT INTO reception_consumers VALUES(?1,?2)",
                params![consumer.tag(), integer(baseline.sequence)?],
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
            let state = state_on(c)?;
            validate_cursor(&state, expected)?;
            validate_cursor(&state, next)?;
            if next.sequence < expected.sequence {
                return Err(LogError::StaleCursor);
            }
            let changed = c.execute(
                "UPDATE reception_consumers SET acknowledged=?1 WHERE kind=?2 AND acknowledged=?3",
                params![
                    integer(next.sequence)?,
                    consumer.tag(),
                    integer(expected.sequence)?
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
            let floor: Option<i64> = c.query_row("SELECT MIN(acknowledged) FROM reception_consumers",[],|row|row.get(0))?;
            let Some(floor) = floor else { return Ok(0) };
            let sequences = {
                let mut statement=c.prepare("SELECT sequence FROM reception_pending WHERE sequence<=?1 ORDER BY sequence LIMIT ?2")?;
                statement.query_map(params![floor,rows as i64],|row|row.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?
            };
            for sequence in &sequences { c.execute("DELETE FROM reception_pending WHERE sequence=?1",[sequence])?; }
            let next: Option<i64> = c.query_row("SELECT MIN(sequence) FROM reception_pending WHERE sequence<=?1",[floor],|row|row.get(0))?;
            let retired = next.map_or(floor,|sequence|sequence-1);
            c.execute("UPDATE reception_stream SET retired_through=MAX(retired_through,?1) WHERE singleton=1",[retired])?;
            Ok(sequences.len())
        })
    }
}
