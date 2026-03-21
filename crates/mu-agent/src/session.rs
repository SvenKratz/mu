#![allow(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mu_ai::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MuAgentError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub message: Message,
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(
        &self,
        parent_id: Option<String>,
        message: &Message,
    ) -> Result<SessionEntry, MuAgentError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let entry = SessionEntry {
            id: Uuid::new_v4().to_string(),
            parent_id,
            timestamp: Utc::now(),
            message: message.clone(),
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        use std::io::Write as _;
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        Ok(entry)
    }

    pub fn load_entries(&self) -> Result<Vec<SessionEntry>, MuAgentError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    pub fn branch_to(&self, node_id: &str) -> Result<Vec<SessionEntry>, MuAgentError> {
        let entries = self.load_entries()?;
        let by_id = entries
            .iter()
            .cloned()
            .map(|entry| (entry.id.clone(), entry))
            .collect::<HashMap<_, _>>();
        let mut path = Vec::new();
        let mut current = Some(node_id.to_string());

        while let Some(id) = current {
            let Some(entry) = by_id.get(&id) else {
                return Err(MuAgentError::InvalidState(format!(
                    "session node {id} not found"
                )));
            };
            path.push(entry.clone());
            current = entry.parent_id.clone();
        }
        path.reverse();
        Ok(path)
    }
}

pub fn list_session_files(root: &Path) -> Result<Vec<PathBuf>, MuAgentError> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    walk_sessions(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_sessions(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), MuAgentError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_sessions(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(())
}
