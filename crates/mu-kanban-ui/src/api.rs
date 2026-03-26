use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use mu_agent::kanban::document::{parse_preamble, DocumentState, KanbanDocument};
use mu_agent::kanban::stats::KanbanStats;
use mu_agent::{KanbanCommand, KanbanState, SessionStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::assets::INDEX_HTML;
use crate::sse::sse_handler;
use crate::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/api/state", get(get_state))
        .route("/api/stats", get(get_stats))
        .route("/api/documents/{id}/content", get(get_document_content))
        .route("/api/documents/{id}/content", put(update_document_content))
        .route("/api/documents/{id}/session", get(get_document_session))
        .route("/api/documents", post(create_document))
        .route("/api/documents/{id}/submit", post(submit_document))
        .route("/api/documents/{id}/cancel", post(cancel_document))
        .route("/api/documents/{id}/retry", post(retry_document))
        .route("/api/open-folder/{id}", post(open_folder))
        .route("/api/events", get(sse_handler))
        .with_state(state)
}

async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// Grouped state response: documents organized by column.
#[derive(Serialize)]
struct BoardState {
    columns: Vec<ColumnState>,
}

#[derive(Serialize)]
struct ColumnState {
    name: String,
    state: String,
    documents: Vec<KanbanDocument>,
}

async fn get_state(State(state): State<Arc<AppState>>) -> Result<Json<BoardState>, AppError> {
    let kanban = state.refresh_state().await?;

    let column_defs = [
        ("Draft", DocumentState::Draft),
        ("Todo", DocumentState::Todo),
        ("Processing", DocumentState::Processing),
        ("Feedback", DocumentState::Feedback),
        ("Complete", DocumentState::Complete),
        ("Error", DocumentState::Error),
    ];

    let columns = column_defs
        .iter()
        .map(|(name, doc_state)| {
            let mut docs: Vec<KanbanDocument> = kanban
                .documents
                .values()
                .filter(|d| d.state == *doc_state)
                .cloned()
                .collect();
            docs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            ColumnState {
                name: name.to_string(),
                state: doc_state.to_string(),
                documents: docs,
            }
        })
        .collect();

    Ok(Json(BoardState { columns }))
}

async fn get_stats(State(state): State<Arc<AppState>>) -> Result<Json<KanbanStats>, AppError> {
    let kanban = state.refresh_state().await?;
    Ok(Json(KanbanStats::from_state(&kanban)))
}

async fn get_document_content(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<String, AppError> {
    let kanban = state.refresh_state().await?;
    let doc = kanban
        .documents
        .get(&id)
        .ok_or(AppError::NotFound("document not found".to_string()))?;

    let folder = doc.state.folder_name();
    let file_path = kanban
        .folder_path(folder)
        .join(format!("{}.md", doc.file_stem()));

    if file_path.exists() {
        Ok(std::fs::read_to_string(file_path)?)
    } else {
        // Try DRAFT folder
        let draft_path = kanban.draft_path().join(format!("{}.md", doc.file_stem()));
        if draft_path.exists() {
            Ok(std::fs::read_to_string(draft_path)?)
        } else {
            Err(AppError::NotFound("document file not found".to_string()))
        }
    }
}

/// Return session log entries for a document.
/// Resolves the session path based on project_id / file_stem.
async fn get_document_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<SessionLogEntry>>, AppError> {
    let kanban = state.refresh_state().await?;
    let doc = kanban
        .documents
        .get(&id)
        .ok_or(AppError::NotFound("document not found".to_string()))?;

    let result_dir = if let Some(ref project_id) = doc.project_id {
        kanban.result_path().join(project_id)
    } else {
        kanban.result_path().join(doc.file_stem())
    };

    let session_dir = if doc.project_id.is_some() {
        result_dir.join(".sessions").join(doc.file_stem())
    } else {
        result_dir
    };

    let session_path = session_dir.join("session.jsonl");
    let store = SessionStore::from_path(session_path);
    let entries = store.load_entries().unwrap_or_default();

    let log: Vec<SessionLogEntry> = entries
        .iter()
        .map(|e| {
            let role = format!("{:?}", e.message.role).to_lowercase();
            let text = e.message.plain_text();
            let tool_calls: Vec<String> = e
                .message
                .content
                .iter()
                .filter_map(|part| match part {
                    mu_ai::ContentPart::ToolCall(call) => {
                        let args = call.arguments.to_string();
                        Some(format!("{}({})", call.name, truncate(&args, 120)))
                    }
                    _ => None,
                })
                .collect();
            SessionLogEntry {
                role,
                text: truncate(&text, 2000),
                tool_calls,
                timestamp: e.timestamp.to_rfc3339(),
            }
        })
        .collect();

    Ok(Json(log))
}

#[derive(Serialize)]
struct SessionLogEntry {
    role: String,
    text: String,
    tool_calls: Vec<String>,
    timestamp: String,
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[derive(Deserialize)]
struct UpdateContent {
    content: String,
}

async fn update_document_content(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateContent>,
) -> Result<StatusCode, AppError> {
    let kanban = state.refresh_state().await?;
    let doc = kanban
        .documents
        .get(&id)
        .ok_or(AppError::NotFound("document not found".to_string()))?;

    if doc.state != DocumentState::Draft {
        return Err(AppError::BadRequest(
            "can only edit documents in draft state".to_string(),
        ));
    }

    let draft_path = kanban.draft_path().join(format!("{}.md", doc.file_stem()));
    std::fs::write(draft_path, &body.content)?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CreateDocumentRequest {
    name: String,
    content: String,
    work_dir: Option<String>,
}

async fn create_document(
    State(app): State<Arc<AppState>>,
    Json(body): Json<CreateDocumentRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let name = body.name.clone();
    let content = body.content;
    let work_dir = body.work_dir;

    // Write directly to disk so the card appears immediately,
    // even if the runner is busy processing other tasks.
    let id = Uuid::now_v7().to_string();
    let mut doc = KanbanDocument::new(id.clone(), name.clone());
    doc.state = DocumentState::Draft;
    doc.work_dir = work_dir.map(PathBuf::from);

    let (preamble, _body) = parse_preamble(&content);
    doc.task_id = preamble.task_id;
    doc.depends_on = preamble.depends_on;
    doc.persona = preamble.persona;
    if doc.task_id.is_none() {
        doc.task_id = Some(name.clone());
    }
    if let Some(ref pid) = preamble.project_id {
        doc.project_id = Some(pid.clone());
    }
    if doc.work_dir.is_none() {
        if let Some(ref wd) = preamble.work_dir {
            doc.work_dir = Some(PathBuf::from(wd));
        }
    }

    // Write the file to DRAFT/
    let draft_dir = app.kanban_root.join("DRAFT");
    std::fs::create_dir_all(&draft_dir)?;
    let draft_file = draft_dir.join(format!("{}.md", doc.file_stem()));
    std::fs::write(&draft_file, &content)?;

    // Update kanban_state.json on disk
    let mut kanban = KanbanState::load_or_create(app.kanban_root.clone())?;
    kanban.insert_document(doc);
    kanban.save()?;

    // Tell runner to reload state from disk when it can
    let _ = app.command_tx.try_send(KanbanCommand::ReloadState);

    // Emit event so SSE clients see the new card immediately
    let _ = app.event_tx.send(mu_agent::KanbanEvent::DocumentDiscovered {
        id,
        name: name.clone(),
    });

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "name": name })),
    ))
}

async fn submit_document(
    State(app): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    // Write directly to disk so it takes effect immediately
    let mut kanban = KanbanState::load_or_create(app.kanban_root.clone())?;
    let doc = kanban
        .get_document(&id)
        .ok_or(AppError::NotFound("document not found".to_string()))?
        .clone();

    if doc.state != DocumentState::Draft {
        return Err(AppError::BadRequest(
            "can only submit documents in draft state".to_string(),
        ));
    }

    // Move file from DRAFT/ to TODO/
    let draft_path = kanban.draft_path().join(format!("{}.md", doc.file_stem()));
    let todo_path = kanban.todo_path().join(format!("{}.md", doc.file_stem()));
    if draft_path.exists() {
        std::fs::rename(&draft_path, &todo_path)?;
    }

    if let Some(d) = kanban.get_document_mut(&id) {
        d.transition_to(DocumentState::Todo);
    }
    kanban.save()?;

    // Tell runner to reload + emit event
    let _ = app.command_tx.try_send(KanbanCommand::ReloadState);
    let _ = app.event_tx.send(mu_agent::KanbanEvent::StateChanged {
        id,
        from: "draft".to_string(),
        to: "todo".to_string(),
    });

    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    state
        .command_tx
        .send(KanbanCommand::CancelDocument { id })
        .await
        .map_err(|e| AppError::Internal(format!("failed to send command: {e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn retry_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    state
        .command_tx
        .send(KanbanCommand::RetryDocument { id })
        .await
        .map_err(|e| AppError::Internal(format!("failed to send command: {e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn open_folder(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let kanban = state.refresh_state().await?;
    let doc = kanban
        .documents
        .get(&id)
        .ok_or(AppError::NotFound("document not found".to_string()))?;

    let folder = doc.state.folder_name();
    let folder_path = kanban.folder_path(folder);

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(&folder_path)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&folder_path)
            .spawn();
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Error type for API responses.
enum AppError {
    NotFound(String),
    BadRequest(String),
    Internal(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e)
    }
}

impl From<mu_agent::MuAgentError> for AppError {
    fn from(e: mu_agent::MuAgentError) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}
