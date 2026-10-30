//! A new canonical generation never accumulates a third history-sized derived
//! copy. Obsolete facts retire in bounded transactions before replay continues.
use super::*;
impl ChannelCheckpointStore {
    pub(super) fn cleanup_generation_step(
        &self,
        channel: ConversationId,
        current: [u8; 16],
    ) -> Result<bool, LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let old:Option<Vec<u8>>=c.query_row("SELECT job FROM (SELECT job,channel FROM channel_projection_work UNION SELECT job,channel FROM channel_legacy_scan) WHERE channel=?1 AND job NOT IN (SELECT job FROM channel_projection_queries) AND job<>?2 AND job<>COALESCE((SELECT source_job FROM channel_current WHERE channel=?1),x'') LIMIT 1",params![channel.as_bytes().as_slice(),current.as_slice()],|r|r.get(0)).optional()?;
            let Some(job)=old else {return Ok(false);};
            // Exactly one table and at most 128 identities are reclaimed per
            // turn. No cascading delete or unbounded SQLite temporary set.
            for (table,key) in [
                ("channel_event_authority","event"),
                ("channel_departure_facts","member,event"),
                ("channel_membership_snapshots","rank"),
                ("channel_causal_states","event"),
                ("channel_activities","event"),
                ("channel_replica_preferences","member"),
                ("channel_history_policy_cache","channel"),
                ("channel_history_controls","event"),
                ("channel_history_pending","event"),
                ("channel_history_scan","channel"),
                ("channel_history_send_scan","channel"),
                ("channel_receive_shipments","recipient_ed,recipient_x,grant_account,grant_incarnation,epoch,author"),
                ("channel_authority_facts","kind,key"),
                ("channel_authority_current","channel"),
                ("channel_legacy_candidates","event"),
                ("channel_legacy_scan","channel"),
            ] {
                let sql=format!("DELETE FROM {table} WHERE job=?1 AND channel=?2 AND ({key}) IN (SELECT {key} FROM {table} WHERE job=?1 AND channel=?2 LIMIT 128)");
                if c.execute(&sql,params![&job,channel.as_bytes().as_slice()])?!=0 {return Ok(true);}
            }
            c.execute("DELETE FROM channel_projection_work WHERE job=?1 AND channel=?2",params![job,channel.as_bytes().as_slice()])?;
            Ok(true)
        })
    }
}
