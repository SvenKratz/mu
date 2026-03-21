use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;

use crate::AppState;

pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_tx.subscribe();

    let stream = async_stream::stream! {
        // Send initial full state as first event
        if let Ok(kanban) = state.refresh_state().await {
            let docs: Vec<_> = kanban.documents.values().cloned().collect();
            if let Ok(data) = serde_json::to_string(&docs) {
                yield Ok(Event::default().event("init").data(data));
            }
        }

        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok(Event::default().event("kanban").data(data));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Missed events — send a full refresh
                    if let Ok(kanban) = state.refresh_state().await {
                        let docs: Vec<_> = kanban.documents.values().cloned().collect();
                        if let Ok(data) = serde_json::to_string(&docs) {
                            yield Ok(Event::default().event("init").data(data));
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
