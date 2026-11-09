//! Bridge between gpui views and GG's jj worker.
//!
//! jj-lib is not thread-safe, so GG runs one worker thread per window that owns
//! the repository. Views talk to it with [`SessionEvent`]s over a std channel and
//! wait for the reply on a background executor. Push events from the worker
//! (repo status, progress) come back through [`EventSink`] as [`WorkerEvent`]s.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc as async_mpsc;
use gg_lib::messages::mutations::{MutationOptions, MutationResult};
use gg_lib::messages::queries::{LogPage, RevsResult};
use gg_lib::messages::{RepoConfig, RepoStatus, RevSet};
use gg_lib::worker::{EventSink, Mutation, Session as _, SessionEvent, WorkerSession};
use jj_lib::settings::UserSettings;

/// A session that lived this long counts as healthy; the restart delay resets.
const STABLE_SESSION: Duration = Duration::from_secs(10);

/// 0 ms for the first restart, then 250 ms doubling up to 5 s.
fn restart_delay(failures: u32) -> Duration {
    if failures <= 1 {
        return Duration::ZERO;
    }
    Duration::from_millis(250u64.saturating_mul(1 << (failures - 2).min(5)))
        .min(Duration::from_secs(5))
}

/// A push event emitted by the worker, e.g. `gg://repo/status`.
pub struct WorkerEvent {
    pub name: String,
    pub payload: serde_json::Value,
}

/// Forwards worker events into an async channel the UI can await.
struct ChannelSink(async_mpsc::UnboundedSender<WorkerEvent>);

impl EventSink for ChannelSink {
    fn send(&self, event_name: &str, payload: serde_json::Value) {
        // The receiver only goes away when the window closes; nothing to do then.
        let _ = self.0.unbounded_send(WorkerEvent {
            name: event_name.to_owned(),
            payload,
        });
    }
}

/// Handle to one window's worker thread. Cheap to clone.
#[derive(Clone)]
pub struct Worker {
    tx: mpsc::Sender<SessionEvent>,
    /// The worker pages through the *last* log query, so a query and its next
    /// pages must not interleave with another query's. Held across each sequence.
    log_lock: Arc<Mutex<()>>,
    /// Liveness token. The thread holds only a weak reference, so once every
    /// handle is gone (the tab closed) it stops instead of restarting.
    _alive: Arc<()>,
}

impl Worker {
    /// Starts a worker thread. Returns the handle and the stream of push events.
    pub fn spawn(
        workspace: Option<PathBuf>,
        settings: UserSettings,
    ) -> Result<(Self, async_mpsc::UnboundedReceiver<WorkerEvent>)> {
        let (tx, rx) = mpsc::channel::<SessionEvent>();
        let (event_tx, event_rx) = async_mpsc::unbounded();
        let sink: Arc<dyn EventSink> = Arc::new(ChannelSink(event_tx));

        let alive = Arc::new(());
        let liveness = Arc::downgrade(&alive);
        thread::Builder::new()
            .name("jj-worker".into())
            .spawn(move || {
                let mut failures = 0u32;
                loop {
                    let started = Instant::now();
                    let session = WorkerSession::new(
                        sink.clone(),
                        workspace.clone(),
                        settings.clone(),
                        false,
                        // Ask the user for git credentials through Randori's dialog
                        // (main() serves as the askpass helper).
                        true,
                    );
                    match pollster::block_on(session.handle_events(&rx)) {
                        Ok(()) => break,
                        // Every handle was dropped: the channel closed on purpose.
                        Err(_) if liveness.upgrade().is_none() => break,
                        Err(err) => {
                            // Same policy as GG: restart the worker and tell the UI.
                            log::warn!("worker restarted: {err:#}");
                            let payload = serde_json::to_value(RepoConfig::WorkerError {
                                message: format!("{err:#}"),
                            })
                            .unwrap_or_default();
                            sink.send("gg://repo/config", payload);

                            // A session that fails straight away will fail again; back off
                            // so a broken config cannot spin the thread or flood the UI.
                            failures = if started.elapsed() > STABLE_SESSION {
                                0
                            } else {
                                failures + 1
                            };
                            thread::sleep(restart_delay(failures));
                        }
                    }
                }
            })
            .context("failed to start the jj worker thread")?;

        Ok((
            Self {
                tx,
                log_lock: Arc::default(),
                _alive: alive,
            },
            event_rx,
        ))
    }

    /// Sends one request and blocks until the worker answers.
    /// Call it from a background task, never on the UI thread.
    fn call<T>(&self, request: impl FnOnce(mpsc::Sender<T>) -> SessionEvent) -> Result<T> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(request(reply_tx))
            .map_err(|_| anyhow!("the jj worker has stopped"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow!("the jj worker dropped the request"))
    }

    /// Loads the workspace at `wd` (or the worker's default directory).
    pub fn open_workspace(&self, wd: Option<PathBuf>) -> Result<RepoConfig> {
        self.call(|tx| SessionEvent::OpenWorkspace { tx, wd })?
    }

    /// Starts a log query and returns its first page.
    pub fn query_log(&self, query: String) -> Result<LogPage> {
        self.query_log_pages(query, 0)
    }

    /// Runs a log query and keeps paging until at least `min_rows` rows are
    /// loaded (or the log ends), as one uninterrupted sequence.
    pub fn query_log_pages(&self, query: String, min_rows: usize) -> Result<LogPage> {
        let _guard = self
            .log_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut page: LogPage = self.call(|tx| SessionEvent::QueryLog { tx, query })??;
        while page.has_more && page.rows.len() < min_rows {
            let next: LogPage = self.call(|tx| SessionEvent::QueryLogNextPage { tx })??;
            page.has_more = next.has_more;
            page.rows.extend(next.rows);
        }
        Ok(page)
    }

    /// Returns the next page of the current log query.
    pub fn query_log_next_page(&self) -> Result<LogPage> {
        let _guard = self
            .log_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.call(|tx| SessionEvent::QueryLogNextPage { tx })?
    }

    /// Creates a jj repository at `wd` (colocated with git when it already has one).
    pub fn init_workspace(&self, wd: PathBuf, colocated: bool) -> Result<PathBuf> {
        self.call(|tx| SessionEvent::InitWorkspace { tx, wd, colocated })?
    }

    /// Loads headers, parents and the combined diff for a revision set.
    pub fn query_revisions(&self, set: RevSet) -> Result<RevsResult> {
        self.call(|tx| SessionEvent::QueryRevisions { tx, set })?
    }

    /// Runs one repository mutation (new, describe, abandon, push, undo, …).
    /// Immutable revisions are protected: GG answers with a precondition error.
    pub fn mutate(&self, mutation: Box<dyn Mutation + Send + Sync>) -> Result<MutationResult> {
        self.call(|tx| SessionEvent::ExecuteMutation {
            tx,
            mutation,
            options: MutationOptions::default(),
        })
    }

    /// Records working-copy changes made outside Randori (editors, the jj CLI).
    /// Returns the new repository status when the working copy changed.
    pub fn snapshot(&self) -> Result<Option<RepoStatus>> {
        self.call(|tx| SessionEvent::ExecuteSnapshot { tx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_backs_off_and_caps() {
        assert_eq!(restart_delay(0), Duration::ZERO);
        assert_eq!(restart_delay(1), Duration::ZERO);
        assert_eq!(restart_delay(2), Duration::from_millis(250));
        assert_eq!(restart_delay(3), Duration::from_millis(500));
        assert_eq!(restart_delay(7), Duration::from_secs(5));
        assert_eq!(restart_delay(100), Duration::from_secs(5));
    }
}
