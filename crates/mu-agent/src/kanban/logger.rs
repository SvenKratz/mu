use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use serde_json::{json, Value};

use super::state::KanbanState;
use super::{KanbanCommand, KanbanEvent};
use crate::MuAgentError;

pub struct KanbanLogger {
    file: File,
}

impl KanbanLogger {
    pub fn new(log_path: PathBuf) -> Result<Self, MuAgentError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        Ok(Self { file })
    }

    pub fn log_event(&mut self, event: &KanbanEvent, state: &KanbanState) {
        let mut entry = json!({ "ts": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) });

        match event {
            KanbanEvent::DocumentDiscovered { id, name } => {
                entry["event"] = json!("document_discovered");
                entry["message"] = json!(format!("new task: {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanEvent::StateChanged { id, from, to } => {
                entry["event"] = json!("state_changed");
                entry["from"] = json!(from);
                entry["to"] = json!(to);
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("{name}: {from} → {to}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanEvent::ProcessingStarted { id } => {
                entry["event"] = json!("processing_started");
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("processing: {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanEvent::ProcessingComplete { id } => {
                entry["event"] = json!("processing_complete");
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("completed: {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanEvent::FeedbackRequested { id, question } => {
                entry["event"] = json!("feedback_requested");
                entry["question"] = json!(question);
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("feedback needed: {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanEvent::StatsUpdated(stats) => {
                entry["event"] = json!("stats_updated");
                entry["message"] = json!(format!(
                    "stats: todo={} proc={} fb={} done={} err={}",
                    stats.todo, stats.processing, stats.feedback, stats.complete, stats.errored
                ));
            }
            KanbanEvent::Error { id, message } => {
                entry["event"] = json!("error");
                entry["error"] = json!(message);
                if let Some(id) = id {
                    let name = self.doc_name(id, state);
                    entry["message"] = json!(format!("error [{name}]: {message}"));
                    self.enrich(&mut entry, id, state);
                } else {
                    entry["message"] = json!(format!("error: {message}"));
                }
            }
        }

        let _ = writeln!(self.file, "{}", entry);
    }

    pub fn log_command(&mut self, cmd: &KanbanCommand, state: &KanbanState) {
        let mut entry = json!({ "ts": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) });
        entry["event"] = json!("command_received");

        match cmd {
            KanbanCommand::CancelDocument { id } => {
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("cmd: cancel {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanCommand::SubmitDocument { id } => {
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("cmd: submit {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanCommand::RetryDocument { id } => {
                let name = self.doc_name(id, state);
                entry["message"] = json!(format!("cmd: retry {name}"));
                self.enrich(&mut entry, id, state);
            }
            KanbanCommand::CreateDraft { name, .. } => {
                entry["message"] = json!(format!("cmd: create draft {name}"));
            }
            KanbanCommand::CreateTodo { name, .. } => {
                entry["message"] = json!(format!("cmd: create todo {name}"));
            }
            KanbanCommand::ReloadState => {
                entry["message"] = json!("cmd: reload state");
            }
        }

        let _ = writeln!(self.file, "{}", entry);
    }

    fn doc_name(&self, id: &str, state: &KanbanState) -> String {
        state
            .get_document(id)
            .map(|d| d.original_name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn enrich(&self, entry: &mut Value, id: &str, state: &KanbanState) {
        entry["doc_id"] = json!(id);
        if let Some(doc) = state.get_document(id) {
            entry["name"] = json!(doc.original_name);
            if let Some(ref task_id) = doc.task_id {
                entry["task_id"] = json!(task_id);
            }
            if let Some(ref project_id) = doc.project_id {
                entry["project_id"] = json!(project_id);
            }
        }
    }
}
