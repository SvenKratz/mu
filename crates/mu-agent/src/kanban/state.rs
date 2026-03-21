use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::document::{DocumentState, KanbanDocument};
use crate::MuAgentError;

const KANBAN_FOLDERS: &[&str] = &[
    "DRAFT",
    "TODO",
    "FEEDBACK",
    "PROCESSING",
    "RESULT",
    "REFINE",
    "STATS",
    "logs",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KanbanState {
    pub root: PathBuf,
    pub documents: HashMap<String, KanbanDocument>,
}

impl KanbanState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            documents: HashMap::new(),
        }
    }

    pub fn state_path(root: &Path) -> PathBuf {
        root.join("kanban_state.json")
    }

    pub fn load_or_create(root: PathBuf) -> Result<Self, MuAgentError> {
        let state_path = Self::state_path(&root);
        if state_path.exists() {
            let content = std::fs::read_to_string(&state_path)?;
            let mut state: Self = serde_json::from_str(&content)?;
            state.root = root;
            Ok(state)
        } else {
            Ok(Self::new(root))
        }
    }

    pub fn save(&self) -> Result<(), MuAgentError> {
        let state_path = Self::state_path(&self.root);
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(state_path, content)?;
        Ok(())
    }

    pub fn ensure_folders(&self) -> Result<(), MuAgentError> {
        for folder in KANBAN_FOLDERS {
            std::fs::create_dir_all(self.root.join(folder))?;
        }
        Ok(())
    }

    pub fn folder_path(&self, folder: &str) -> PathBuf {
        self.root.join(folder)
    }

    pub fn draft_path(&self) -> PathBuf {
        self.folder_path("DRAFT")
    }

    pub fn todo_path(&self) -> PathBuf {
        self.folder_path("TODO")
    }

    pub fn processing_path(&self) -> PathBuf {
        self.folder_path("PROCESSING")
    }

    pub fn result_path(&self) -> PathBuf {
        self.folder_path("RESULT")
    }

    pub fn feedback_path(&self) -> PathBuf {
        self.folder_path("FEEDBACK")
    }

    pub fn refine_path(&self) -> PathBuf {
        self.folder_path("REFINE")
    }

    pub fn stats_path(&self) -> PathBuf {
        self.folder_path("STATS")
    }

    pub fn insert_document(&mut self, doc: KanbanDocument) {
        self.documents.insert(doc.id.clone(), doc);
    }

    pub fn get_document(&self, id: &str) -> Option<&KanbanDocument> {
        self.documents.get(id)
    }

    pub fn get_document_mut(&mut self, id: &str) -> Option<&mut KanbanDocument> {
        self.documents.get_mut(id)
    }

    pub fn documents_in_state(&self, state: &DocumentState) -> Vec<&KanbanDocument> {
        self.documents
            .values()
            .filter(|doc| doc.state == *state)
            .collect()
    }

    /// List `.md` files in a given folder.
    pub fn list_md_files(folder: &Path) -> Result<Vec<PathBuf>, MuAgentError> {
        let mut files = Vec::new();
        if !folder.exists() {
            return Ok(files);
        }
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn ensure_folders_creates_all_directories() {
        let tempdir = TempDir::new().expect("tempdir should exist");
        let root = tempdir.path().join("my-board");
        let state = KanbanState::new(root.clone());
        state.ensure_folders().expect("should create folders");

        for folder in KANBAN_FOLDERS {
            assert!(root.join(folder).is_dir(), "{folder} should be a directory");
        }
    }

    #[test]
    fn save_and_load_roundtrips() {
        let tempdir = TempDir::new().expect("tempdir should exist");
        let root = tempdir.path().join("my-board");
        std::fs::create_dir_all(&root).expect("should create root");

        let mut state = KanbanState::new(root.clone());
        let doc = KanbanDocument::new("test-id".to_string(), "my-task".to_string());
        state.insert_document(doc);
        state.save().expect("should save");

        let loaded = KanbanState::load_or_create(root).expect("should load");
        assert_eq!(loaded.documents.len(), 1);
        assert!(loaded.documents.contains_key("test-id"));
    }

    #[test]
    fn documents_in_state_filters_correctly() {
        let mut state = KanbanState::new(PathBuf::from("/tmp/test"));
        state.insert_document(KanbanDocument::new("a".to_string(), "task-a".to_string()));
        let mut doc_b = KanbanDocument::new("b".to_string(), "task-b".to_string());
        doc_b.state = DocumentState::Processing;
        state.insert_document(doc_b);

        assert_eq!(state.documents_in_state(&DocumentState::Todo).len(), 1);
        assert_eq!(
            state.documents_in_state(&DocumentState::Processing).len(),
            1
        );
        assert_eq!(
            state.documents_in_state(&DocumentState::Complete).len(),
            0
        );
    }
}
