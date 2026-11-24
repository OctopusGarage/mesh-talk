//! Run with --no-default-features: no network codec or filesystem is required.
use mesh_mailbox::{
    MailboxCursor, MailboxDeltaPage, Record, RecordId, ScopeId,
    client::{self, MailboxTurnLimits},
    ports::MailboxTransport,
};
// Immutable source for the entire exchange; mutable hosts pin their own snapshot.
struct Source {
    scope: ScopeId,
    records: Vec<Record>,
}
impl MailboxTransport for Source {
    type Error = std::io::Error;
    async fn delta(
        &mut self,
        scope: ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<MailboxDeltaPage, Self::Error> {
        if scope != self.scope
            || prior.is_some_and(|c| {
                c.scope != scope
                    || c.store_uuid != [1; 16]
                    || c.epoch != 0
                    || c.after < 0
                    || c.after as u64 > self.records.len() as u64
            })
        {
            return Err(std::io::Error::other("source changed"));
        }
        let after = prior.map_or(0, |c| c.after as usize);
        let mut bytes = 0;
        let mut ids = vec![];
        for r in self.records.iter().skip(after).take(limit) {
            if r.body.len() > max_bytes - bytes {
                break;
            }
            bytes += r.body.len();
            ids.push(r.id);
        }
        if ids.is_empty() && after < self.records.len() {
            return Err(std::io::Error::other("increase payload allowance"));
        }
        let next = after + ids.len();
        let total = self.records.len() as i64;
        Ok(MailboxDeltaPage {
            cursor: MailboxCursor {
                store_uuid: [1; 16],
                scope,
                epoch: 0,
                revision: total,
                after: next as i64,
                window: (next < self.records.len()).then_some((total, total)),
            },
            ids,
            complete: next == self.records.len(),
            reset: false,
        })
    }
    async fn bodies(
        &mut self,
        scope: ScopeId,
        cursor: &MailboxCursor,
        ids: &[RecordId],
        max_bytes: usize,
    ) -> Result<Vec<Record>, Self::Error> {
        if scope != self.scope
            || cursor.scope != scope
            || cursor.store_uuid != [1; 16]
            || cursor.epoch != 0
            || cursor.after < 0
            || cursor.after as u64 > self.records.len() as u64
            || ids.len() > self.records.len()
        {
            return Err(std::io::Error::other("invalid scope/selection"));
        }
        let mut result = vec![];
        let mut bytes = 0;
        for id in ids {
            let r = self
                .records
                .iter()
                .take(cursor.after as usize)
                .find(|r| r.id == *id)
                .ok_or_else(|| std::io::Error::other("missing record"))?;
            if r.body.len() > max_bytes - bytes {
                return Err(std::io::Error::other("payload allowance"));
            }
            bytes += r.body.len();
            result.push(r.clone());
        }
        Ok(result)
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scope = ScopeId::new([1; 32]);
    let mut source = Source {
        scope,
        records: vec![Record::new(scope, vec![1; 128 * 1024])],
    };
    let mut accepted = vec![];
    // Real hosts authenticate bodies and atomically commit them with this cursor.
    let progress = client::sync_turn(
        &mut source,
        scope,
        None,
        MailboxTurnLimits::default(),
        |records, _| {
            accepted.extend_from_slice(records);
            Ok::<_, std::io::Error>(())
        },
    )
    .await?;
    assert!(progress.work.complete);
    assert_eq!(accepted.len(), 1);
    println!(
        "Accepted {} payload bytes through a typed carrier without RPC/storage profiles",
        progress.work.bytes
    );
    Ok(())
}
