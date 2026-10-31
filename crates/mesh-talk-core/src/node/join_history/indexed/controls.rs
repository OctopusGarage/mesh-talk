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
