use std::path::Path;
use std::time::Duration;

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::MuAgentError;

pub struct KanbanWatcher {
    _watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
}

impl KanbanWatcher {
    pub fn new(root: &Path) -> Result<Self, MuAgentError> {
        let (tx, rx) = mpsc::channel(128);

        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                // Best effort send; if the channel is full we'll catch up on the next poll
                let _ = tx.blocking_send(result);
            },
            Config::default().with_poll_interval(Duration::from_millis(500)),
        )
        .map_err(|err| MuAgentError::Io(std::io::Error::other(err)))?;

        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|err| MuAgentError::Io(std::io::Error::other(err)))?;

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
        })
    }

    /// Wait for the next filesystem event, or return `None` if the channel is closed.
    pub async fn next_event(&mut self) -> Option<notify::Result<notify::Event>> {
        self.receiver.recv().await
    }

    /// Drain any pending events without blocking.
    pub fn drain(&mut self) {
        while self.receiver.try_recv().is_ok() {}
    }
}
