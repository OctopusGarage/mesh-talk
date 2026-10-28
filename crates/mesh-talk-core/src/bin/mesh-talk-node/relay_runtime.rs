//! Dedicated relay production lifecycle: one owner for storage and all sessions.
use futures_util::{StreamExt, stream::FuturesUnordered};
use mesh_talk_core::{
    identity::device::DeviceIdentity,
    postoffice::{
        IndexedPostOffice,
        federation::{IndexedRelayEndpoint, IndexedRelayError},
    },
    transport::SecureChannel,
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::watch,
    task::{JoinHandle, JoinSet},
};

#[derive(Default)]
pub(super) struct BackgroundTasks {
    pub handles: FuturesUnordered<JoinHandle<()>>,
    pub gateway: Option<(watch::Sender<bool>, JoinHandle<()>)>,
}
impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        for task in self.handles.iter() {
            task.abort();
        }
        if let Some((stop, task)) = &self.gateway {
            stop.send_replace(true);
            task.abort();
        }
    }
}
impl BackgroundTasks {
    fn stop_admission(&self) {
        for task in self.handles.iter() {
            task.abort();
        }
        if let Some((stop, _)) = &self.gateway {
            stop.send_replace(true);
        }
    }
    async fn unexpected_exit(&mut self) -> std::io::Error {
        tokio::select! {
            result=self.handles.next(),if !self.handles.is_empty()=>{
                std::io::Error::other(match result {
                    Some(Err(error))=>format!("discovery owner failed: {error}"),
                    _=>"discovery owner exited unexpectedly".into(),
                })
            }
            result=async {match self.gateway.as_mut(){Some((_,task))=>task.await,None=>std::future::pending().await}}=>{
                self.gateway.take();
                std::io::Error::other(match result {Ok(())=>"gateway owner exited unexpectedly".into(),Err(error)=>format!("gateway owner failed: {error}")})
            }
        }
    }
    async fn shutdown(&mut self) -> std::io::Result<()> {
        self.stop_admission();
        let mut failure = None;
        while let Some(result) = self.handles.next().await {
            match result {
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    failure.get_or_insert_with(|| {
                        std::io::Error::other(format!("discovery shutdown: {error}"))
                    });
                }
                Ok(()) => {
                    failure.get_or_insert_with(|| {
                        std::io::Error::other("discovery owner exited before cancellation")
                    });
                }
            }
        }
        if let Some((_, task)) = self.gateway.take()
            && let Err(error) = task.await
        {
            failure
                .get_or_insert_with(|| std::io::Error::other(format!("gateway shutdown: {error}")));
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

pub(super) struct ShutdownSignals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
}
impl ShutdownSignals {
    pub(super) fn install() -> std::io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            #[cfg(unix)]
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        })
    }
    async fn wait(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            tokio::select! { _=self.terminate.recv()=>{}, _=self.interrupt.recv()=>{} };
            Ok(())
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await
        }
    }
}

// An accept failure can describe one abandoned connection rather than a failed
// listener. Keep this paced retry future alive across unrelated select wakes.
#[derive(Debug, PartialEq, Eq)]
enum AcceptRecovery {
    Yield,
    Backoff,
    Fatal,
}
fn accept_recovery(error: &std::io::Error) -> AcceptRecovery {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => return AcceptRecovery::Yield,
        ErrorKind::Interrupted | ErrorKind::WouldBlock => return AcceptRecovery::Backoff,
        _ => {}
    }
    // Mio already consumed and dropped the accepted fd when Darwin's peer
    // sockaddr conversion yields this bare kind. Yield to other runtime work,
    // then drain the next queued connection without a wall-clock penalty.
    #[cfg(target_os = "macos")]
    if error.kind() == ErrorKind::InvalidInput
        && error.raw_os_error().is_none()
        && error.get_ref().is_none()
    {
        return AcceptRecovery::Yield;
    }
    AcceptRecovery::Fatal
}
#[derive(Debug)]
struct AcceptFailure {
    source: std::io::Error,
    retries: u32,
}
impl std::fmt::Display for AcceptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "relay TCP accept failed after {} retries: {}",
            self.retries, self.source
        )
    }
}
impl std::error::Error for AcceptFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
async fn accept_with_retry<T, F, A>(mut accept: F) -> std::io::Result<T>
where
    F: FnMut() -> A,
    A: Future<Output = std::io::Result<T>>,
{
    let mut retries = 0u32;
    let mut backoff_level = 0u32;
    loop {
        match accept().await {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                match accept_recovery(&error) {
                    AcceptRecovery::Fatal => {
                        return Err(std::io::Error::new(
                            error.kind(),
                            AcceptFailure {
                                source: error,
                                retries,
                            },
                        ));
                    }
                    AcceptRecovery::Yield => tokio::task::yield_now().await,
                    AcceptRecovery::Backoff => {
                        let delay = Duration::from_millis(20u64 << backoff_level)
                            .min(Duration::from_secs(1));
                        backoff_level = (backoff_level + 1).min(6);
                        tokio::time::sleep(delay).await;
                    }
                }
                retries = retries.saturating_add(1);
            }
        }
    }
}

async fn drain_sessions(
    office: &IndexedPostOffice,
    sessions: &mut JoinSet<Result<(), IndexedRelayError>>,
) -> Option<Box<dyn std::error::Error>> {
    sessions.abort_all();
    let mut failure: Option<Box<dyn std::error::Error>> = None;
    while let Some(completed) = sessions.join_next().await {
        match completed {
            Ok(Err(error)) if office.requires_reopen() => {
                failure.get_or_insert(Box::new(error));
            }
            Err(error) if !error.is_cancelled() => {
                failure.get_or_insert(Box::new(error));
            }
            _ => {}
        }
    }
    failure
}

pub(super) async fn run(
    office: IndexedPostOffice,
    endpoint: IndexedRelayEndpoint,
    identity: DeviceIdentity,
    listener: TcpListener,
    outgoing: impl Future<Output = Result<(), IndexedRelayError>>,
    mut background: BackgroundTasks,
    mut signals: ShutdownSignals,
) -> Result<(), Box<dyn std::error::Error>> {
    let (stop, rx) = watch::channel(false);
    let mut sessions = JoinSet::new();
    let identity = Arc::new(identity);
    let mut worker_finished = false;
    let result;
    let worker_status;
    let session_failure;
    {
        let worker = office.run_maintenance(rx);
        tokio::pin!(worker);
        {
            tokio::pin!(outgoing);
            let accept = || listener.accept();
            let accepting = accept_with_retry(accept);
            tokio::pin!(accepting);
            result = loop {
                tokio::select! {
                    biased;
                    error=background.unexpected_exit()=>break Err(Box::new(error) as Box<dyn std::error::Error>),
                    signal=signals.wait()=>break signal.map_err(|e|Box::new(e) as Box<dyn std::error::Error>),
                    status=&mut worker=>{worker_finished=true; break status.map_err(|e|Box::new(e) as Box<dyn std::error::Error>);},
                    status=&mut outgoing=>break status.map_err(|e|Box::new(e) as Box<dyn std::error::Error>),
                    completed=sessions.join_next(), if !sessions.is_empty()=>{
                        match completed {
                            Some(Ok(Err(error))) => {
                                if office.requires_reopen() { break Err(Box::new(error) as Box<dyn std::error::Error>); }
                                log::debug!("relay session: {error}");
                            }
                            Some(Err(error)) => break Err(Box::new(error) as Box<dyn std::error::Error>),
                            _ => {}
                        }
                    }
                    incoming=&mut accepting, if sessions.len()<256=>{
                        match incoming {
                            Ok((stream,_))=>{
                                accepting.set(accept_with_retry(accept));
                                let endpoint=endpoint.clone(); let identity=identity.clone();
                                sessions.spawn(async move {
                                    stream.set_nodelay(true)?;
                                    mesh_talk_core::transport::net::set_tcp_keepalive(&stream);
                                    let channel=tokio::time::timeout(Duration::from_secs(10),SecureChannel::accept(stream,&identity)).await
                                        .map_err(|_|IndexedRelayError::Protocol("relay handshake deadline exceeded".into()))??;
                                    endpoint.serve_connection_checked(channel,Some(&identity)).await
                                });
                            }
                            Err(error)=>break Err(Box::new(error) as Box<dyn std::error::Error>),
                        }
                    }
                }
            };
        } // Dropping outgoing cancels its owned exchanges and revokes sessions.
        drop(listener); // Stop admission before canceling existing sessions.
        background.stop_admission();
        session_failure = drain_sessions(&office, &mut sessions).await;
        stop.send_replace(true);
        worker_status = if !worker_finished {
            worker.await
        } else {
            Ok(())
        };
    }
    let background_status = background.shutdown().await;
    // Last endpoint/profile owners drop only after every admitted slice and
    // connection has finished. No background task owns a database reference.
    result
        .and(session_failure.map_or(Ok(()), Err))
        .and(worker_status.map_err(|e| Box::new(e) as Box<dyn std::error::Error>))
        .and(background_status.map_err(|e| Box::new(e) as Box<dyn std::error::Error>))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::storage::profile::ProfileStorageLease;
    #[tokio::test(start_paused = true)]
    async fn accept_retries_nonprogress_errors_then_returns_connection() {
        for kind in [
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::WouldBlock,
        ] {
            let mut results =
                std::collections::VecDeque::from([Err(std::io::Error::from(kind)), Ok(7)]);
            let started = tokio::time::Instant::now();
            let result = accept_with_retry(|| std::future::ready(results.pop_front().unwrap()))
                .await
                .unwrap();
            assert_eq!(result, 7);
            assert!(results.is_empty());
            assert_eq!(started.elapsed(), Duration::from_millis(20));
        }
    }
    #[tokio::test(start_paused = true)]
    async fn consumed_connections_drain_without_wall_clock_delay() {
        let kinds = [
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::ConnectionReset,
        ];
        #[cfg(target_os = "macos")]
        let kinds = [kinds[0], kinds[1], std::io::ErrorKind::InvalidInput];
        for kind in kinds {
            let started = tokio::time::Instant::now();
            let mut attempts = 0;
            let connection = accept_with_retry(|| {
                attempts += 1;
                std::future::ready(if attempts <= 64 {
                    Err(std::io::Error::from(kind))
                } else {
                    Ok(7)
                })
            })
            .await
            .unwrap();
            assert_eq!(connection, 7);
            assert_eq!(attempts, 65);
            assert_eq!(started.elapsed(), Duration::ZERO);
        }
    }
    #[tokio::test(start_paused = true)]
    async fn shutdown_cancels_consumed_connection_drain_between_attempts() {
        let started = tokio::time::Instant::now();
        let attempts = std::cell::Cell::new(0);
        let (stop, mut shutdown) = watch::channel(false);
        {
            let accept = accept_with_retry(|| {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 64 {
                    stop.send_replace(true);
                }
                std::future::ready(Err::<(), _>(std::io::Error::from(
                    std::io::ErrorKind::ConnectionAborted,
                )))
            });
            tokio::pin!(accept);
            tokio::select! {
                biased;
                result=shutdown.changed()=>{result.unwrap();assert!(*shutdown.borrow());}
                result=&mut accept=>panic!("connection drain finished before shutdown: {result:?}"),
            }
        }
        assert_eq!(attempts.get(), 64);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
    #[tokio::test(start_paused = true)]
    async fn accept_many_transient_errors_recover_with_capped_backoff() {
        let started = tokio::time::Instant::now();
        let mut attempts = Vec::new();
        let connection = accept_with_retry(|| {
            attempts.push(started.elapsed());
            std::future::ready(if attempts.len() <= 12 {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(7)
            })
        })
        .await
        .unwrap();
        assert_eq!(connection, 7);
        assert_eq!(
            attempts,
            [
                0, 20, 60, 140, 300, 620, 1260, 2260, 3260, 4260, 5260, 6260, 7260
            ]
            .map(Duration::from_millis)
        );
        assert_eq!(started.elapsed(), Duration::from_millis(7260));
    }
    #[tokio::test(start_paused = true)]
    async fn accept_permanent_error_returns_immediately() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let error = accept_with_retry(|| {
            attempts += 1;
            std::future::ready(Err::<(), _>(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            )))
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            error
                .to_string()
                .contains("relay TCP accept failed after 0 retries")
        );
    }
    #[tokio::test(start_paused = true)]
    async fn pinned_accept_backoff_survives_unrelated_select_wake() {
        let started = tokio::time::Instant::now();
        let mut attempts = Vec::new();
        {
            let accept = accept_with_retry(|| {
                attempts.push(started.elapsed());
                std::future::ready(if attempts.len() < 3 {
                    Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                } else {
                    Ok(7)
                })
            });
            tokio::pin!(accept);
            tokio::select! {
                result=&mut accept=>panic!("accept finished before unrelated wake: {result:?}"),
                _=tokio::time::sleep(Duration::from_millis(5))=>{}
            }
            assert_eq!((&mut accept).await.unwrap(), 7);
        }
        assert_eq!(started.elapsed(), Duration::from_millis(60));
        assert_eq!(attempts, [0, 20, 60].map(Duration::from_millis));
    }
    #[tokio::test(start_paused = true)]
    async fn shutdown_cancels_pinned_accept_without_waiting_for_backoff() {
        let started = tokio::time::Instant::now();
        let attempts = std::cell::Cell::new(0);
        {
            let accept = accept_with_retry(|| {
                attempts.set(attempts.get() + 1);
                std::future::ready(Err::<(), _>(std::io::Error::from(
                    std::io::ErrorKind::Interrupted,
                )))
            });
            tokio::pin!(accept);
            tokio::select! {
                result=&mut accept=>panic!("accept finished before shutdown: {result:?}"),
                _=tokio::time::sleep(Duration::from_millis(5))=>{}
            }
        }
        assert_eq!(attempts.get(), 1);
        assert_eq!(started.elapsed(), Duration::from_millis(5));
    }
    #[test]
    fn accept_invalid_input_retry_is_only_the_bare_macos_mio_error() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            accept_recovery(&Error::from(ErrorKind::InvalidInput)),
            if cfg!(target_os = "macos") {
                AcceptRecovery::Yield
            } else {
                AcceptRecovery::Fatal
            }
        );
        assert_eq!(
            accept_recovery(&Error::new(
                ErrorKind::InvalidInput,
                "invalid listener configuration"
            )),
            AcceptRecovery::Fatal
        );
        assert_eq!(
            accept_recovery(&Error::from_raw_os_error(22)),
            AcceptRecovery::Fatal
        );
    }
    struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    #[tokio::test]
    async fn discovery_exit_or_panic_fails_runtime_and_joins_remaining_owners_before_lease_release()
    {
        for panic in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let identity = DeviceIdentity::generate();
            let (ed, x) = identity.secret_bytes();
            let office = IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(root.path()).unwrap(),
                &root.path().join("relay.log"),
                "pw",
                identity,
            )
            .unwrap();
            let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (entered, ready) = tokio::sync::oneshot::channel();
            let (gone, joined) = tokio::sync::oneshot::channel();
            let background = BackgroundTasks::default();
            background.handles.push(tokio::spawn(async move {
                let _guard = Dropped(Some(gone));
                entered.send(()).unwrap();
                std::future::pending::<()>().await;
            }));
            ready.await.unwrap();
            background.handles.push(tokio::spawn(async move {
                assert!(!panic, "injected discovery task panic");
            }));
            let result = tokio::time::timeout(
                Duration::from_millis(200),
                run(
                    office,
                    endpoint,
                    DeviceIdentity::from_secret_bytes(ed, x),
                    listener,
                    std::future::pending(),
                    background,
                    ShutdownSignals::install().unwrap(),
                ),
            )
            .await;
            assert!(
                result.is_ok(),
                "unexpected discovery completion must wake the actual runtime owner"
            );
            let error = result.unwrap().unwrap_err();
            assert!(error.to_string().contains("discovery"), "{error}");
            joined.await.unwrap();
            assert!(ProfileStorageLease::acquire(root.path()).is_ok());
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_stop_preserves_admitted_failure_and_intentional_cancellation_is_normal() {
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let mut sessions = JoinSet::new();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<bool>();
        sessions.spawn(async move {
            entered.send(()).unwrap();
            // Model an admitted synchronous slice: abort cannot stop this poll.
            assert!(
                !blocked.recv().unwrap(),
                "admitted synchronous task failure"
            );
            Ok(())
        });
        ready.await.unwrap();
        let (waiting, admitted) = tokio::sync::oneshot::channel();
        sessions.spawn(async move {
            waiting.send(()).unwrap();
            std::future::pending().await
        });
        admitted.await.unwrap();
        // Stop while the first admitted poll is held, then let that exact poll
        // fail. There is no sleep or race-dependent admission assumption.
        sessions.abort_all();
        release.send(true).unwrap();
        let failure = drain_sessions(&office, &mut sessions).await;
        assert!(
            failure.is_some(),
            "normal stop must preserve a non-cancellation task failure"
        );
        assert!(
            failure
                .unwrap()
                .to_string()
                .contains("admitted synchronous task failure")
        );
        assert!(sessions.is_empty());
        assert!(!office.requires_reopen());
        let mut cancelled = JoinSet::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        cancelled.spawn(async move {
            tx.send(()).unwrap();
            std::future::pending().await
        });
        rx.await.unwrap();
        assert!(
            drain_sessions(&office, &mut cancelled).await.is_none(),
            "intentional abort is normal shutdown"
        );
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_ok());
    }
}
