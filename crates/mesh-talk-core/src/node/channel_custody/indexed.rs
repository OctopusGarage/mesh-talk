//! Exact custody observations stay on disk; opening a published source reads no
//! retained records. Positive and negative facts share the same selected key.
use super::{CustodyRecord, MAGIC};
use crate::node::transcript_repository::{CHANNEL_CUSTODY, publication::PublishedHistorySource};
use crate::{
    eventlog::{EventId, LogError},
    storage::{
        profile::{context::ProfileStorage, manifest},
        record_log::EncryptedRecordLog,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{collections::HashSet, path::Path, sync::Arc};

const SCHEMA: &str = "CREATE TABLE channel_custody_observations(event BLOB NOT NULL CHECK(length(event)=32),peer TEXT NOT NULL,held INTEGER NOT NULL CHECK(held IN(0,1)),PRIMARY KEY(event,peer)) WITHOUT ROWID;";

pub(in crate::node) struct IndexedChannelCustodyStore {
    profile: Arc<ProfileStorage>,
    source: PublishedHistorySource,
    #[cfg(test)]
    pub(super) replayed_records: usize,
}

type CustodyDecision = ((EventId, String), bool);

impl IndexedChannelCustodyStore {
    pub(in crate::node) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy::<CustodyRecord>(
            profile,
            path,
            password,
            CHANNEL_CUSTODY,
            MAGIC,
        )
    }
    pub(in crate::node) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_custody_observations')", [], |r| r.get(0))?;
            if !exists { c.execute_batch(SCHEMA)?; }
            manifest::validate_ddl(c, SCHEMA)
        })?;
        #[cfg(test)]
        let mut replayed_records = 0;
        let source = PublishedHistorySource::open(
            profile.clone(),
            path,
            password,
            CHANNEL_CUSTODY,
            |c, path, password| {
                let file = EncryptedRecordLog::<CustodyRecord>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |row| {
                        #[cfg(test)]
                        {
                            replayed_records += 1;
                        }
                        let (peer, held) = row
                            .peer
                            .strip_prefix('!')
                            .map_or((row.peer.as_str(), true), |peer| (peer, false));
                        Self::put_on(c, row.event, peer, held)?;
                        Ok(())
                    },
                )?;
                file.sync()
            },
        )?;
        Ok(Self {
            profile,
            source,
            #[cfg(test)]
            replayed_records,
        })
    }
    fn put_on(c: &Connection, event: EventId, peer: &str, held: bool) -> Result<(), LogError> {
        c.execute("INSERT INTO channel_custody_observations(event,peer,held) VALUES(?1,?2,?3) ON CONFLICT(event,peer) DO UPDATE SET held=excluded.held WHERE held<>excluded.held", params![event.as_bytes().as_slice(), peer, held])?;
        Ok(())
    }
    pub(in crate::node) fn peers(&self, event: EventId) -> Result<HashSet<String>, LogError> {
        self.profile.database().read(|c| {
            let mut statement = c.prepare(
                "SELECT peer FROM channel_custody_observations WHERE event=?1 AND held=1",
            )?;
            statement
                .query_map([event.as_bytes().as_slice()], |r| r.get(0))?
                .collect::<Result<_, _>>()
                .map_err(Into::into)
        })
    }
    pub(in crate::node) fn observations_after(
        &self,
        after: Option<&(EventId, String)>,
    ) -> Result<Vec<CustodyDecision>, LogError> {
        self.profile.database().read(|c| {
            let query = if after.is_some() {
                "SELECT event,peer,held FROM channel_custody_observations WHERE (event,peer)>(?1,?2) ORDER BY event,peer LIMIT 32"
            } else {
                "SELECT event,peer,held FROM channel_custody_observations WHERE ?1 IS NULL AND ?2 IS NULL ORDER BY event,peer LIMIT 32"
            };
            let mut statement = c.prepare(query)?;
            let mut rows = statement.query(params![after.map(|(event,_)| event.as_bytes().as_slice()), after.map(|(_,peer)| peer.as_str())])?;
            let mut page = Vec::new();
            while let Some(row) = rows.next()? {
                let bytes: Vec<u8> = row.get(0)?;
                let event = bytes.try_into().map(EventId::new).map_err(|_| LogError::CorruptFile("invalid custody event identity".into()))?;
                page.push(((event, row.get(1)?), row.get(2)?));
            }
            Ok(page)
        })
    }
    pub(in crate::node) fn confirm(&mut self, event: EventId, peer: &str) -> Result<(), LogError> {
        if peer.starts_with('!') {
            return Err(LogError::CorruptFile("invalid custody peer".into()));
        }
        self.source.check_write()?;
        self.profile
            .database()
            .transaction(|c| Self::put_on(c, event, peer, true))
    }
    pub(in crate::node) fn revoke(&mut self, event: EventId, peer: &str) -> Result<(), LogError> {
        self.source.check_write()?;
        self.profile.database().transaction(|c| {
            let held: Option<bool> = c
                .query_row(
                    "SELECT held FROM channel_custody_observations WHERE event=?1 AND peer=?2",
                    params![event.as_bytes().as_slice(), peer],
                    |r| r.get(0),
                )
                .optional()?;
            if held == Some(true) {
                Self::put_on(c, event, peer, false)?;
            }
            Ok(())
        })
    }
}
