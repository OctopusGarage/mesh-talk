//! Durable bounded download scheduling. New validated offers enqueue in their
//! transaction; policy changes scan only metadata and completion uses exact ACKs.
use super::indexed::{IndexedFileBook, active, blob, invalid};
use crate::eventlog::{ConversationId, LogError};
use rusqlite::{Connection, params};

pub(super) const SCHEMA:&str="
CREATE TABLE file_download_scan(account BLOB PRIMARY KEY CHECK(length(account)=32),policy BLOB CHECK(policy IS NULL OR length(policy)=8),after_file BLOB CHECK(after_file IS NULL OR length(after_file)=32),done INTEGER NOT NULL CHECK(done IN(0,1)),next_ticket INTEGER NOT NULL CHECK(typeof(next_ticket)='integer' AND next_ticket>0)) WITHOUT ROWID;
CREATE TABLE file_download_pending(account BLOB NOT NULL CHECK(length(account)=32),file BLOB NOT NULL CHECK(length(file)=32),ticket INTEGER NOT NULL CHECK(typeof(ticket)='integer' AND ticket>0),revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),PRIMARY KEY(account,file),FOREIGN KEY(account,file) REFERENCES file_book_bindings(account,file)) WITHOUT ROWID;
CREATE INDEX file_download_tickets ON file_download_pending(account,ticket,file);
";
#[derive(Clone, Copy)]
pub(crate) struct FileDownload {
    pub file: ConversationId,
    pub revision: i64,
}
fn next_ticket(c: &Connection, account: &[u8; 32]) -> Result<i64, LogError> {
    let ticket:i64=c.query_row("UPDATE file_download_scan SET next_ticket=next_ticket+1 WHERE account=?1 RETURNING next_ticket-1",[account.as_slice()],|r|r.get(0))?;
    Ok(ticket)
}
pub(super) fn enqueue_on(c: &Connection, file: ConversationId) -> Result<(), LogError> {
    let account = active(c)?;
    let ticket = next_ticket(c, &account)?;
    c.execute("INSERT INTO file_download_pending VALUES(?1,?2,?3,1) ON CONFLICT(account,file) DO UPDATE SET revision=revision+1",params![account.as_slice(),file.as_bytes().as_slice(),ticket])?;
    Ok(())
}
impl IndexedFileBook {
    pub(crate) fn request_download(&self, file: ConversationId) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let live:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM file_book_bindings WHERE account=?1 AND file=?2 AND offers>0)",params![self.account.as_slice(),file.as_bytes().as_slice()],|r|r.get(0))?;
            if live {enqueue_on(c,file)?;}
            Ok(())
        })
    }
    pub(crate) fn request_download_scan(&self, generation: u64) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            c.execute("UPDATE file_download_scan SET policy=?1,after_file=NULL,done=0 WHERE account=?2 AND policy IS NOT ?1",params![generation.to_be_bytes().as_slice(),self.account.as_slice()])?;
            Ok(())
        })
    }
    pub(crate) fn discover_downloads(&self, generation: u64) -> Result<(), LogError> {
        self.request_download_scan(generation)?;
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let mut statement=c.prepare_cached("SELECT b.file FROM file_book_bindings b JOIN file_download_scan s ON s.account=b.account WHERE b.account=?1 AND b.offers>0 AND s.done=0 AND b.file>coalesce(s.after_file,x'') ORDER BY b.file LIMIT 128")?;
            let mut rows=statement.query([self.account.as_slice()])?;
            let mut files=Vec::new();while let Some(row)=rows.next()? {files.push(ConversationId::new(blob(row,0)?));}
            drop(rows);drop(statement);
            for file in &files {enqueue_on(c,*file)?;}
            c.execute("UPDATE file_download_scan SET after_file=coalesce(?1,after_file),done=?2 WHERE account=?3",params![files.last().map(|file|file.as_bytes().as_slice()),i64::from(files.len()<128),self.account.as_slice()])?;
            Ok(())
        })
    }
    pub(crate) fn download_work_pending(&self) -> Result<bool, LogError> {
        self.profile.database().read(|c| {self.check(c)?;Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM file_download_pending WHERE account=?1) OR EXISTS(SELECT 1 FROM file_download_scan WHERE account=?1 AND done=0)",[self.account.as_slice()],|r|r.get(0))?)})
    }
    pub(crate) fn claim_downloads(&self) -> Result<Vec<FileDownload>, LogError> {
        self.profile.database().transaction(|c| {
            self.check(c)?;
            let mut statement=c.prepare_cached("SELECT file,revision FROM file_download_pending WHERE account=?1 ORDER BY ticket,file LIMIT 8")?;
            let mut rows=statement.query([self.account.as_slice()])?;
            let mut selected=Vec::new();while let Some(row)=rows.next()? {selected.push(FileDownload {file:ConversationId::new(blob(row,0)?),revision:row.get(1)?});}
            drop(rows);drop(statement);
            for work in &selected {
                let ticket=next_ticket(c,&self.account)?;
                c.execute("UPDATE file_download_pending SET ticket=?1 WHERE account=?2 AND file=?3",params![ticket,self.account.as_slice(),work.file.as_bytes().as_slice()])?;
            }
            Ok(selected)
        })
    }
    pub(crate) fn finish_download(&self, work: FileDownload) -> Result<(), LogError> {
        if work.revision < 1 {
            return Err(invalid("invalid download acknowledgement"));
        }
        self.profile.database().transaction(|c| {
            self.check(c)?;
            c.execute(
                "DELETE FROM file_download_pending WHERE account=?1 AND file=?2 AND revision=?3",
                params![
                    self.account.as_slice(),
                    work.file.as_bytes().as_slice(),
                    work.revision
                ],
            )?;
            Ok(())
        })
    }
}
