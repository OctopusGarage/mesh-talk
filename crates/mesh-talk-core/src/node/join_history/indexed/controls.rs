//! Parsed control identities are selectors, never authority. The canonical
//! reducer installs them with its cursor; selected payloads are validated later.
use super::*;
const SCHEMA: &str = "
CREATE TABLE channel_history_controls(job BLOB NOT NULL,channel BLOB NOT NULL,event BLOB NOT NULL,rank INTEGER NOT NULL,role INTEGER NOT NULL CHECK(role IN(1,2,3)),invitation BLOB,recipient TEXT,offer BLOB,PRIMARY KEY(job,channel,event)) WITHOUT ROWID;
CREATE INDEX channel_history_invitation ON channel_history_controls(job,channel,invitation,recipient,rank) WHERE role=1;
CREATE INDEX channel_history_release ON channel_history_controls(job,channel,offer,rank) WHERE role IN(2,3);
CREATE TABLE channel_history_pending(job BLOB NOT NULL,channel BLOB NOT NULL,event BLOB NOT NULL,rank INTEGER NOT NULL,attempt_stamp INTEGER NOT NULL DEFAULT -1,PRIMARY KEY(job,channel,event)) WITHOUT ROWID;
CREATE INDEX channel_history_retry ON channel_history_pending(job,channel,rank,event);
CREATE TABLE channel_history_scan(job BLOB NOT NULL,channel BLOB NOT NULL,stamp INTEGER NOT NULL,after_rank INTEGER NOT NULL,PRIMARY KEY(job,channel)) WITHOUT ROWID;
";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
pub(in crate::node::join_history) fn install_on(
    c: &Connection,
    job: [u8; 16],
    channel: ConversationId,
    event: &Event,
    rank: i64,
) -> Result<(), LogError> {
    let (role, invitation, recipient, offer) = match event.kind {
        EventKind::ChannelJoinHistory => match decode(&event.ciphertext) {
            Some(HistoryControl::Offer(value)) => {
                (1, Some(value.invitation), Some(value.recipient), None)
            }
            Some(HistoryControl::Release(value)) => (2, None, None, Some(value.offer)),
            None => return Ok(()),
        },
        EventKind::ChannelDeviceInheritance => {
            let Some(value) = decode_account_release(&event.ciphertext) else {
                return Ok(());
            };
            (
                3,
                None,
                Some(value.recipient.user_id()),
                Some(value.release.offer),
            )
        }
        _ => return Ok(()),
    };
    c.execute(
        "INSERT INTO channel_history_controls VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            job.as_slice(),
            channel.as_bytes().as_slice(),
            event.id.as_bytes().as_slice(),
            rank,
            role,
            invitation.as_ref().map(|id| id.as_bytes().as_slice()),
            recipient,
            offer.as_ref().map(|id| id.as_bytes().as_slice())
        ],
    )?;
    if role != 1 {
        c.execute("INSERT INTO channel_history_pending(job,channel,event,rank) SELECT ?1,?2,?3,?4 WHERE NOT EXISTS(SELECT 1 FROM channel_processed WHERE channel=?2 AND event=?3)",params![job.as_slice(),channel.as_bytes().as_slice(),event.id.as_bytes().as_slice(),rank])?;
    }
    Ok(())
}
impl IndexedView<'_> {
    pub(super) fn offer_candidates(
        &self,
        invitation: EventId,
        recipient: &str,
        after: Option<i64>,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.checkpoints.profile().database().read(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            let mut statement=c.prepare("SELECT rank,event FROM channel_history_controls WHERE job=?1 AND channel=?2 AND role=1 AND invitation=?3 AND recipient=?4 AND rank>?5 ORDER BY rank LIMIT 64")?;
            let mut rows=statement.query(params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),invitation.as_bytes().as_slice(),recipient,after.unwrap_or(-1)])?;
            let mut out=Vec::new();
            while let Some(row)=rows.next()? {
                let bytes:Vec<u8>=row.get(1)?;
                out.push((row.get(0)?,EventId::new(bytes.try_into().map_err(|_|LogError::CorruptFile("invalid history offer identity".into()))?)));
            }
            Ok(out)
        })
    }
    pub(super) fn first_offer(
        &self,
        selected: EventId,
    ) -> Result<Option<(Event, Offer)>, LogError> {
        let Some(event) = self.repository.get(&selected)? else {
            return Ok(None);
        };
        if event.kind != EventKind::ChannelJoinHistory {
            return Ok(None);
        }
        let Some(HistoryControl::Offer(shape)) = decode(&event.ciphertext) else {
            return Ok(None);
        };
        let mut after = None;
        loop {
            let page = self.offer_candidates(shape.invitation, &shape.recipient, after)?;
            if page.is_empty() {
                return Ok(None);
            }
            for (rank, id) in page {
                after = Some(rank);
                let Some(candidate) = self.repository.get(&id)? else {
                    return Err(LogError::StaleCursor);
                };
                if let Some(offer) = self.offer(&candidate)? {
                    return Ok((id == selected).then_some((candidate, offer)));
                }
                if id == selected {
                    return Ok(None);
                }
            }
        }
    }
}
