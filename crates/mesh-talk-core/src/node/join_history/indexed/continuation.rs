//! A join-history retry owns its suspended validation stack. Every database or
//! graph operation passes through this local work gate; no task or lock lives
//! across a scheduling turn. Only protocol-bounded selected controls are kept.
use super::*;
use crate::eventlog::sync::network::{PreparationBudget, PreparationStep};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use tokio::sync::oneshot;

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
    result: Option<oneshot::Sender<T>>,
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
                if let Some(result) = self.result.take() {
                    let _ = result.send(value);
                }
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
}
impl HistoryContext {
    pub(super) async fn request<T: Send + 'static>(
        &self,
        action: impl for<'a> FnMut(&IndexedView<'a>, PreparationBudget) -> Result<Step<T>, LogError>
        + Send
        + 'static,
    ) -> Result<T, LogError> {
        let (send, receive) = oneshot::channel();
        {
            let mut request = self.request.lock().expect("history continuation request");
            if request.is_some() {
                return Err(LogError::StaleCursor);
            }
            *request = Some(Box::new(Request {
                action,
                result: Some(send),
            }));
        }
        receive.await.map_err(|_| LogError::StaleCursor)
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
        let mut command = self
            .context
            .request
            .lock()
            .expect("history continuation request")
            .take()
            .ok_or(LogError::StaleCursor)?;
        let (complete, mut work) = command.step(view, budget)?;
        if work.rows > budget.rows || work.edges > budget.edges || work.bytes > budget.bytes {
            return Err(LogError::CorruptFile(
                "join-history preparation exceeded its work budget".into(),
            ));
        }
        if work.rows == 0 && work.edges == 0 && work.bytes == 0 {
            work.rows = 1;
            work.bytes = 64;
        }
        if !complete {
            *self
                .context
                .request
                .lock()
                .expect("history continuation request") = Some(command);
        } else if let Poll::Ready(value) = self.future.as_mut().poll(&mut cx) {
            view.checkpoints.check_source(&self.source)?;
            work.ready = true;
            return Ok(Step::Ready(value?, work));
        }
        view.checkpoints.check_source(&self.source)?;
        work.ready = false;
        Ok(Step::Pending(work))
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
