#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

mod api;
mod assets;
mod sse;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use mu_agent::{KanbanCommand, KanbanEvent, KanbanState};
use tokio::sync::{broadcast, mpsc, Mutex};

/// Shared application state for the web server.
pub struct AppState {
    pub kanban_root: PathBuf,
    pub event_tx: broadcast::Sender<KanbanEvent>,
    pub command_tx: mpsc::Sender<KanbanCommand>,
    /// Cached state — refreshed on each API call by re-reading from disk.
    pub state: Mutex<KanbanState>,
}

impl AppState {
    /// Reload kanban state from disk.
    pub async fn refresh_state(&self) -> Result<KanbanState, mu_agent::MuAgentError> {
        let state = KanbanState::load_or_create(self.kanban_root.clone())?;
        let mut cached = self.state.lock().await;
        *cached = state.clone();
        Ok(state)
    }
}

/// Configuration for starting the kanban web UI server.
pub struct KanbanUiConfig {
    pub addr: SocketAddr,
    pub kanban_root: PathBuf,
    pub event_tx: broadcast::Sender<KanbanEvent>,
    pub command_tx: mpsc::Sender<KanbanCommand>,
}

/// Start the kanban web UI server. Returns the actual bound address.
pub async fn start_server(config: KanbanUiConfig) -> anyhow::Result<SocketAddr> {
    let state = KanbanState::load_or_create(config.kanban_root.clone())?;

    let app_state = Arc::new(AppState {
        kanban_root: config.kanban_root,
        event_tx: config.event_tx,
        command_tx: config.command_tx,
        state: Mutex::new(state),
    });

    let app = api::router(app_state);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    let actual_addr = listener.local_addr()?;

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    Ok(actual_addr)
}
