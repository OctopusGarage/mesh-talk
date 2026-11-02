//! Have closure resumes both the selected vertex and its parent edge. Cursor and
//! flags commit together, so cancellation/failure cannot publish partial output.
use super::*;
use crate::eventlog::sync::network::{PreparationBudget, PreparationStep};
pub(super) const DDL:&str="
CREATE TABLE IF NOT EXISTS participant_sync_export_work(
 view BLOB PRIMARY KEY,rank INTEGER NOT NULL,parent_after BLOB,sibling_closed INTEGER NOT NULL,own_closed INTEGER NOT NULL,
 FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;";
impl EventRepository {
    pub(crate) fn reset_selected_export_step(
        &self,
        selection: &Selection,
        view: &NetworkView,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        let limit = budget.rows_for(32)?;
        self.database.transaction(|c| {
            self.require_selection_on(c,selection)?;self.validate_network_on(c,view)?;
            let deleted=c.execute("DELETE FROM participant_sync_have WHERE view=?1 AND id IN (SELECT id FROM participant_sync_have WHERE view=?1 LIMIT ?2)",params![selection.id.as_slice(),limit as i64])?;
            if deleted==0 {c.execute("INSERT INTO participant_sync_export_work VALUES(?1,-1,NULL,1,1) ON CONFLICT(view) DO UPDATE SET rank=-1,parent_after=NULL,sibling_closed=1,own_closed=1",[selection.id.as_slice()])?;}
            Ok(PreparationStep{ready:deleted==0,rows:deleted,bytes:deleted*32,edges:0})
        })
    }
    pub(crate) fn stage_selected_have(
        &self,
        selection: &Selection,
        view: &NetworkView,
        ids: &[EventId],
    ) -> Result<(), LogError> {
        self.database.transaction(|c| {
            self.require_selection_on(c, selection)?;
            self.validate_network_on(c, view)?;
            let mut stmt =
                c.prepare("INSERT OR IGNORE INTO participant_sync_have VALUES(?1,?2)")?;
            for id in ids {
                stmt.execute(params![selection.id.as_slice(), id.as_bytes().as_slice()])?;
            }
            Ok(())
        })
    }
    pub(crate) fn selected_export_step(
        &self,
        selection: &Selection,
        view: &NetworkView,
        sibling_closure: bool,
        own_closure: bool,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        budget.rows_for_bytes(64)?;
        self.database.transaction(|c| {
            self.require_selection_on(c,selection)?;self.validate_network_on(c,view)?;
            let (mut after,mut edge_after,mut sibling_closed,mut own_closed):(i64,Option<Vec<u8>>,bool,bool)=c.query_row("SELECT rank,parent_after,sibling_closed,own_closed FROM participant_sync_export_work WHERE view=?1",[selection.id.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let mut step=PreparationStep::default();
            while step.rows<budget.rows&&step.edges<budget.edges&&step.bytes+64<=budget.bytes {
                let row:Option<(Vec<u8>,i64,bool,bool,bool)>=c.query_row("SELECT id,rank,candidate,selected,own FROM participant_sync_rows WHERE view=?1 AND rank>?2 ORDER BY rank LIMIT 1",params![selection.id.as_slice(),after],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
                let Some((id,rank,candidate,selected,own))=row else {step.ready=true;break;};
                if candidate&&(sibling_closure||own_closure) {
                    let limit=(budget.edges-step.edges).min((budget.bytes-step.bytes)/64);
                    if limit==0{break;}
                    let mut stmt=c.prepare("SELECT p.parent,EXISTS(SELECT 1 FROM participant_sync_have h WHERE h.view=?3 AND h.id=p.parent),COALESCE((SELECT sibling FROM participant_sync_rows r WHERE r.view=?3 AND r.id=p.parent AND r.rank<?4),0),COALESCE((SELECT outgoing FROM participant_sync_rows r WHERE r.view=?3 AND r.id=p.parent AND r.rank<?4),0) FROM retained_event_parents p WHERE p.domain=?1 AND p.child=?2 AND p.parent>COALESCE(?5,x'') ORDER BY p.parent LIMIT ?6")?;
                    let parents=stmt.query_map(params![self.domain.0.as_slice(),id,selection.id.as_slice(),rank,edge_after,limit as i64],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,bool>(1)?,r.get::<_,bool>(2)?,r.get::<_,bool>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
                    for (_,have,sibling,outgoing) in &parents {sibling_closed&=*have||*sibling;own_closed&=*have||*outgoing;}
                    step.edges+=parents.len();step.bytes+=parents.len()*64;
                    if parents.len()==limit {
                        edge_after=parents.last().map(|(id,_,_,_)|id.clone());
                        continue;
                    }
                }
                let sibling=candidate&&(!sibling_closure||selected&&sibling_closed);
                let outgoing=sibling&&(!own_closure||own&&own_closed);
                c.execute("UPDATE participant_sync_rows SET sibling=?3,outgoing=?4 WHERE view=?1 AND id=?2",params![selection.id.as_slice(),id,sibling,outgoing])?;
                after=rank;edge_after=None;sibling_closed=true;own_closed=true;step.rows+=1;step.bytes+=64;
            }
            c.execute("UPDATE participant_sync_export_work SET rank=?2,parent_after=?3,sibling_closed=?4,own_closed=?5 WHERE view=?1",params![selection.id.as_slice(),after,edge_after,sibling_closed,own_closed])?;
            Ok(step)
        })
    }
}
