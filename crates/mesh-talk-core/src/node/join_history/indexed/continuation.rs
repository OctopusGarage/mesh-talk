//! A join-history retry owns its suspended validation stack. Every database or
//! graph operation passes through this local work gate; no task or lock lives
//! across a scheduling turn. Only protocol-bounded selected controls are kept.
use super::*;
use crate::eventlog::sync::network::{PreparationBudget, PreparationStep};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

pub(super) enum Step<T> {
    Pending(PreparationStep),
    Ready(T, PreparationStep),
}
trait Command: Send {
    fn step(
        &mut self,
        view: &IndexedView<'_>,
        budget: PreparationBudget,
    ) -> Result<(bool, PreparationStep), LogError>;
}
struct Request<T, F> {
    action: F,
    result: Arc<Mutex<Option<T>>>,
}
impl<T: Send, F> Command for Request<T, F>
where
    F: for<'a> FnMut(&IndexedView<'a>, PreparationBudget) -> Result<Step<T>, LogError> + Send,
{
    fn step(
        &mut self,
        view: &IndexedView<'_>,
        budget: PreparationBudget,
    ) -> Result<(bool, PreparationStep), LogError> {
        match (self.action)(view, budget)? {
            Step::Pending(work) => Ok((false, work)),
            Step::Ready(value, work) => {
                *self.result.lock().expect("history continuation result") = Some(value);
                Ok((true, work))
            }
        }
    }
}
#[derive(Clone)]
pub(super) struct HistoryContext {
    request: Arc<Mutex<Option<Box<dyn Command>>>>,
}
pub(super) struct HistoryQuery<T> {
    pub(super) source: ChannelSource,
    context: HistoryContext,
    future: Pin<Box<dyn Future<Output = Result<T, LogError>> + Send>>,
    // An admitted command that did not fit a prior turn's residual bytes gets
    // a fresh allowance until it completes; selected rows are never skipped.
    exclusive_command: bool,
}
impl HistoryContext {
    pub(super) async fn request<T: Send + 'static>(
        &self,
        action: impl for<'a> FnMut(&IndexedView<'a>, PreparationBudget) -> Result<Step<T>, LogError>
        + Send
        + 'static,
    ) -> Result<T, LogError> {
        // This future is polled by our measured command scheduler, not Tokio.
        // Tokio oneshot adds a task-cooperation Pending even after send; that
        // would look like a missing command and discard a valid continuation.
        let result = Arc::new(Mutex::new(None));
        {
            let mut request = self.request.lock().expect("history continuation request");
            if request.is_some() {
                return Err(LogError::StaleCursor);
            }
            *request = Some(Box::new(Request {
                action,
                result: Arc::clone(&result),
            }));
        }
        Ok(poll_fn(
            move |_| match result.lock().expect("history continuation result").take() {
                Some(value) => Poll::Ready(value),
                None => Poll::Pending,
            },
        )
        .await)
    }
}
impl<T: Send + 'static> HistoryQuery<T> {
    pub(super) fn new<F>(source: ChannelSource, build: impl FnOnce(HistoryContext) -> F) -> Self
    where
        F: Future<Output = Result<T, LogError>> + Send + 'static,
    {
        let context = HistoryContext {
            request: Arc::new(Mutex::new(None)),
        };
        let future = Box::pin(build(context.clone()));
        Self {
            source,
            context,
            future,
            exclusive_command: false,
        }
    }
    pub(super) fn step(
        &mut self,
        view: &IndexedView<'_>,
        budget: PreparationBudget,
    ) -> Result<Step<T>, LogError> {
        budget.validate()?;
        view.checkpoints.check_source(&self.source)?;
        if view.source != self.source {
            return Err(LogError::StaleCursor);
        }
        if budget.rows < 2 || budget.bytes < 512 {
            return Err(LogError::InvalidPageBudget);
        }
        let mut cx = Context::from_waker(Waker::noop());
        if let Poll::Ready(value) = self.future.as_mut().poll(&mut cx) {
            view.checkpoints.check_source(&self.source)?;
            return Ok(Step::Ready(
                value?,
                PreparationStep {
                    ready: true,
                    ..Default::default()
                },
            ));
        }
        let mut total = PreparationStep::default();
        // Every command shares this allowance. The command cap also bounds
        // phase-only work that reports bytes without consuming a row.
        for _ in 0..budget.rows {
            let remaining = PreparationBudget {
                rows: budget.rows - total.rows,
                edges: budget.edges - total.edges,
                bytes: budget.bytes - total.bytes,
            };
            if remaining.rows < 2 || remaining.edges == 0 || remaining.bytes < 512 {
                break;
            }
            let mut command = self
                .context
                .request
                .lock()
                .expect("history continuation request")
                .take()
                .ok_or(LogError::StaleCursor)?;
            let outcome = command.step(view, remaining);
            let (complete, mut work) = match outcome {
                Ok(value) => value,
                Err(LogError::PageBudgetTooSmall { required })
                    if (total.rows != 0 || total.edges != 0 || total.bytes != 0)
                        && required <= budget.bytes =>
                {
                    // A nested owner may have done bounded prefix work before
                    // refusing the selected allocation. Its error has no spent
                    // counters, so conservatively charge this entire allowance.
                    total.rows = budget.rows;
                    total.edges = budget.edges;
                    total.bytes = budget.bytes;
                    self.exclusive_command = true;
                    *self
                        .context
                        .request
                        .lock()
                        .expect("history continuation request") = Some(command);
                    view.checkpoints.check_source(&self.source)?;
                    return Ok(Step::Pending(total));
                }
                Err(error) => return Err(error),
            };
            if work.rows == 0 && work.edges == 0 && work.bytes == 0 {
                work.rows = 1;
                work.bytes = 64;
            }
            if work.rows > remaining.rows
                || work.edges > remaining.edges
                || work.bytes > remaining.bytes
            {
                return Err(LogError::CorruptFile(
                    "join-history preparation exceeded its work budget".into(),
                ));
            }
            total.rows += work.rows;
            total.edges += work.edges;
            total.bytes += work.bytes;
            if !complete {
                *self
                    .context
                    .request
                    .lock()
                    .expect("history continuation request") = Some(command);
            } else {
                self.exclusive_command = false;
                if let Poll::Ready(value) = self.future.as_mut().poll(&mut cx) {
                    view.checkpoints.check_source(&self.source)?;
                    total.ready = true;
                    return Ok(Step::Ready(value?, total));
                }
            }
            view.checkpoints.check_source(&self.source)?;
            if self.exclusive_command {
                break;
            }
        }
        view.checkpoints.check_source(&self.source)?;
        Ok(Step::Pending(total))
    }
}
pub(super) fn units(rows: usize, bytes: usize) -> PreparationStep {
    PreparationStep {
        rows,
        bytes,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn join_history_selected_offer_yields_and_matches_independent_legacy_authority() {
        let directory = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (messages, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Default::default())),
            dm,
            messages,
            files,
            &directory.path().join("events"),
            &directory.path().join("sent"),
            "pw",
        )
        .unwrap();
        let channel = node.create_channel("query", vec![]).await.unwrap();
        node.set_channel_history_policy(channel, HistoryPolicy::LastTen)
            .await
            .unwrap();
        for _ in 0..257 {
            node.append_event(channel, EventKind::Message, vec![])
                .unwrap();
        }
        let recipient = DeviceIdentity::generate().public();
        let (name, epoch, owner) = {
            let book = node.channels.lock().unwrap();
            let old = book.state(&channel).unwrap();
            (old.name().to_owned(), old.epoch(), old.owner().to_owned())
        };
        let mut members = vec![node.identity.public(), recipient.clone()];
        members.sort_by_key(PublicIdentity::user_id);
        let invitation = crate::channel::ChannelMeta {
            name,
            members,
            epoch: epoch + 1,
            owner,
        };
        let invitation = node
            .append_event_with_profile_parent(
                channel,
                EventKind::MembershipChange,
                invitation.encode_invitation(),
                None,
            )
            .unwrap()
            .1;
        let offer = Offer {
            recipient: recipient.user_id(),
            invitation,
            policy: HistoryPolicy::LastTen,
            keys: SealedPayload::seal(
                &node.identity,
                &[node.identity.public()],
                GroupKey::generate().as_bytes(),
            )
            .unwrap(),
            rows: vec![],
        };
        let selected = node
            .append_event_with_profile_parent(
                channel,
                EventKind::ChannelJoinHistory,
                encode(&HistoryControl::Offer(offer)),
                None,
            )
            .unwrap()
            .1;
        let log = node.log.lock().unwrap();
        let repository = log.repository().unwrap();
        while !node
            .channel_checkpoints
            .project_step(repository, &node.identity, channel)
            .unwrap()
        {}
        let events = log.events(&channel).unwrap();
        let refs: Vec<_> = events.iter().collect();
        let event = events
            .iter()
            .find(|event| event.id == selected)
            .unwrap()
            .clone();
        let expected = super::super::super::View::new(&refs)
            .offer(&event)
            .is_some();
        assert!(expected, "fixture must select a valid frozen offer");
        let source = node
            .channel_checkpoints
            .source_binding(channel)
            .unwrap()
            .unwrap();
        let view = IndexedView::new(
            &node.channel_checkpoints,
            repository,
            &node.identity,
            source,
        );
        let mut query = HistoryQuery::new(source, move |context| async move {
            Ok(context.offer(&event).await?.is_some())
        });
        let budget = PreparationBudget {
            rows: 8,
            edges: 32,
            bytes: 64 * 1024,
        };
        let facts = || {
            node.storage
                .database()
                .read(|c| {
                    c.query_row("SELECT count(*) FROM channel_event_authority", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .map_err(Into::into)
                })
                .unwrap()
        };
        let mut ready = None;
        let mut turns = 0;
        for _ in 0..20000 {
            let before = facts();
            let bodies = repository.body_read_counts().1;
            let work = match query.step(&view, budget).unwrap() {
                Step::Pending(work) => work,
                Step::Ready(value, work) => {
                    ready = Some(value);
                    work
                }
            };
            assert!(
                work.rows <= budget.rows
                    && work.edges <= budget.edges
                    && work.bytes <= budget.bytes
            );
            assert!(facts().saturating_sub(before) <= budget.rows as i64);
            assert!(repository.body_read_counts().1 - bodies <= budget.rows);
            turns += 1;
            if ready.is_some() {
                break;
            }
        }
        assert!(
            turns > 32,
            "deep authority must yield before returning a verdict"
        );
        assert_eq!(ready, Some(expected));
        // More than Tokio's cooperative task allowance must still progress
        // without an unrelated executor yield. The narrow supported allowance
        // keeps every handoff measured without losing its suspended result.
        let handoff_budget = PreparationBudget {
            rows: 2,
            edges: 1,
            bytes: 512,
        };
        let mut handoffs = HistoryQuery::new(source, move |context| async move {
            let mut found = 0;
            for _ in 0..300 {
                found += usize::from(context.header(selected).await?.is_some());
            }
            Ok(found)
        });
        let before = repository.body_read_counts();
        let mut count = None;
        let mut handoff_turns = 0;
        for _ in 0..301 {
            handoff_turns += 1;
            let work = match handoffs.step(&view, handoff_budget).unwrap() {
                Step::Pending(work) => work,
                Step::Ready(found, work) => {
                    count = Some(found);
                    work
                }
            };
            assert!(
                work.rows <= handoff_budget.rows
                    && work.edges <= handoff_budget.edges
                    && work.bytes <= handoff_budget.bytes
            );
            if count.is_some() {
                break;
            }
        }
        assert_eq!(count, Some(300));
        assert!(handoff_turns > 128);
        assert_eq!(repository.body_read_counts(), before);
        // Many real selected header reads share one allowance. The prior
        // one-command scheduler required 300 turns despite ample row budget.
        let mut batched = HistoryQuery::new(source, move |context| async move {
            let mut found = 0;
            for _ in 0..300 {
                found += usize::from(context.header(selected).await?.is_some());
            }
            Ok(found)
        });
        let batch_budget = PreparationBudget::default();
        let mut completed = None;
        let mut batch_turns = 0;
        let mut header_reads = 0;
        let before_bodies = repository.body_read_counts();
        let before_facts = facts();
        for _ in 0..10 {
            batch_turns += 1;
            let work = match batched.step(&view, batch_budget).unwrap() {
                Step::Pending(work) => work,
                Step::Ready(value, work) => {
                    completed = Some(value);
                    work
                }
            };
            assert!(
                work.rows <= batch_budget.rows
                    && work.edges == 0
                    && work.bytes <= batch_budget.bytes
            );
            header_reads += work.rows;
            if completed.is_some() {
                break;
            }
        }
        assert_eq!(completed, Some(300));
        assert_eq!(header_reads, 300);
        assert!(batch_turns > 1 && batch_turns <= 10);
        assert_eq!(facts(), before_facts);
        assert_eq!(repository.body_read_counts(), before_bodies);

        // A stateful command may make several measured phase transitions in
        // one turn, but must yield before exposing an unfinished result.
        let mut phases = HistoryQuery::new(source, move |context| async move {
            let mut after = 0;
            context
                .request(move |_, _| {
                    after += 1;
                    if after == 12 {
                        Ok(Step::Ready(after, units(1, 128)))
                    } else {
                        Ok(Step::Pending(units(1, 128)))
                    }
                })
                .await
        });
        let phase_budget = PreparationBudget {
            rows: 8,
            edges: 1,
            bytes: 4096,
        };
        let Step::Pending(work) = phases.step(&view, phase_budget).unwrap() else {
            panic!("unfinished phases must not expose a partial result");
        };
        assert!(
            work.rows > 1 && work.rows <= phase_budget.rows && work.bytes <= phase_budget.bytes
        );
        let Step::Ready(value, work) = phases.step(&view, phase_budget).unwrap() else {
            panic!("remaining phases must use the next bounded allowance");
        };
        assert_eq!(value, 12);
        assert!(work.rows <= phase_budget.rows && work.bytes <= phase_budget.bytes);

        // The second real selected body fits a fresh turn but not the residual
        // allowance. No oversized body may be read during the refusal, and the
        // conservative charge includes all work before that admission failure.
        let selected_header = repository.header(&selected).unwrap().unwrap();
        let body_bytes = selected_header.stored_bytes * 3 + std::mem::size_of::<Event>();
        assert!(body_bytes > 768);
        let allocation_budget = PreparationBudget {
            rows: 8,
            edges: 32,
            bytes: body_bytes + 896,
        };
        let mut allocation = HistoryQuery::new(source, move |context| async move {
            let first = context.body(selected).await?.map(|event| event.id);
            let second = context.body(selected).await?.map(|event| event.id);
            Ok((first, second))
        });
        let before_bodies = repository.body_read_counts().1;
        let before_facts = facts();
        let Step::Pending(work) = allocation.step(&view, allocation_budget).unwrap() else {
            panic!("selected allocation must wait for a fresh turn");
        };
        assert_eq!(work.rows, allocation_budget.rows);
        assert_eq!(work.edges, allocation_budget.edges);
        assert_eq!(work.bytes, allocation_budget.bytes);
        let read = repository.body_read_counts().1 - before_bodies;
        assert_eq!(read, 1);
        assert!(read <= allocation_budget.rows && read * body_bytes <= allocation_budget.bytes);
        assert_eq!(facts(), before_facts);
        let before_bodies = repository.body_read_counts().1;
        let Step::Ready(value, work) = allocation.step(&view, allocation_budget).unwrap() else {
            panic!("selected allocation must progress with its fresh allowance");
        };
        assert_eq!(value, (Some(selected), Some(selected)));
        assert!(
            work.rows <= allocation_budget.rows
                && work.edges <= allocation_budget.edges
                && work.bytes <= allocation_budget.bytes
        );
        let read = repository.body_read_counts().1 - before_bodies;
        assert_eq!(read, 1);
        assert!(read <= allocation_budget.rows && read * body_bytes <= allocation_budget.bytes);
        assert_eq!(facts(), before_facts);

        // A body that cannot fit the caller's full allowance remains an explicit
        // budget error, rather than an indefinitely retained pending command.
        let mut too_small = HistoryQuery::new(source, move |context| async move {
            context.body(selected).await
        });
        let before_bodies = repository.body_read_counts();
        assert!(matches!(
            too_small.step(
                &view,
                PreparationBudget {
                    bytes: body_bytes - 1,
                    ..allocation_budget
                }
            ),
            Err(LogError::PageBudgetTooSmall { required }) if required == body_bytes
        ));
        assert_eq!(repository.body_read_counts(), before_bodies);
        assert_eq!(facts(), before_facts);

        // A yielded decision cannot be used after a signed source append.
        let event = repository.get(&selected).unwrap().unwrap();
        let mut stale = HistoryQuery::new(source, move |context| async move {
            Ok(context.offer(&event).await?.is_some())
        });
        assert!(matches!(
            stale.step(&view, budget).unwrap(),
            Step::Pending(_)
        ));
        drop(log);
        node.append_event(channel, EventKind::Message, vec![])
            .unwrap();
        let log = node.log.lock().unwrap();
        let view = IndexedView::new(
            &node.channel_checkpoints,
            log.repository().unwrap(),
            &node.identity,
            source,
        );
        assert!(matches!(
            stale.step(&view, budget),
            Err(LogError::StaleCursor)
        ));
    }
}
