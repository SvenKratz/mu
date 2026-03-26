use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentState {
    Draft,
    Todo,
    Processing,
    Feedback,
    Complete,
    Refining,
    Error,
}

impl DocumentState {
    pub fn folder_name(&self) -> &'static str {
        match self {
            Self::Draft => "DRAFT",
            Self::Todo => "TODO",
            Self::Processing => "PROCESSING",
            Self::Feedback => "FEEDBACK",
            Self::Complete => "RESULT",
            Self::Refining => "PROCESSING",
            Self::Error => "PROCESSING",
        }
    }

    pub fn can_transition_to(&self, target: &Self) -> bool {
        matches!(
            (self, target),
            (Self::Draft, Self::Todo)
                | (Self::Todo, Self::Processing)
                | (Self::Todo, Self::Draft) // cancel queued task
                | (Self::Processing, Self::Complete)
                | (Self::Processing, Self::Feedback)
                | (Self::Processing, Self::Error)
                | (Self::Processing, Self::Todo) // cancel back to queue
                | (Self::Processing, Self::Draft) // cancel to draft for revision
                | (Self::Feedback, Self::Processing)
                | (Self::Complete, Self::Refining)
                | (Self::Refining, Self::Processing)
                | (Self::Error, Self::Todo) // allow retry
                | (Self::Error, Self::Draft) // revise errored task
        )
    }
}

impl std::fmt::Display for DocumentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Draft => write!(f, "draft"),
            Self::Todo => write!(f, "todo"),
            Self::Processing => write!(f, "processing"),
            Self::Feedback => write!(f, "feedback"),
            Self::Complete => write!(f, "complete"),
            Self::Refining => write!(f, "refining"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KanbanDocument {
    pub id: String,
    pub original_name: String,
    pub state: DocumentState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub error: Option<String>,
    pub refine_count: u32,
    /// When set, this document continues work in an existing completed task's result directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Per-task working directory override. When set, the agent uses this
    /// path as its working directory instead of the RESULT/ folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<PathBuf>,
}

impl KanbanDocument {
    pub fn new(id: String, original_name: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            original_name,
            state: DocumentState::Todo,
            created_at: now,
            updated_at: now,
            error: None,
            refine_count: 0,
            continues_from: None,
            task_id: None,
            project_id: None,
            depends_on: Vec::new(),
            persona: None,
            work_dir: None,
        }
    }

    pub fn file_stem(&self) -> String {
        format!("{}_{}", self.original_name, self.id)
    }

    pub fn transition_to(&mut self, target: DocumentState) -> bool {
        if self.state.can_transition_to(&target) {
            self.state = target;
            self.updated_at = Utc::now();
            true
        } else {
            false
        }
    }
}

/// Parse a kanban filename like `build-parser_01964e2c-4f5a-7abc-8000-abcdef123456.md`
/// into `(original_name, id)`.
pub fn parse_kanban_filename(filename: &str) -> Option<(String, String)> {
    let stem = filename.strip_suffix(".md")?;
    // UUIDv7 is 36 chars: 8-4-4-4-12
    if stem.len() < 37 {
        return None;
    }
    let separator_pos = stem.len() - 37;
    if stem.as_bytes().get(separator_pos) != Some(&b'_') {
        return None;
    }
    let name = &stem[..separator_pos];
    let id = &stem[separator_pos + 1..];
    // Quick UUID format validation
    if uuid::Uuid::parse_str(id).is_ok() {
        Some((name.to_string(), id.to_string()))
    } else {
        None
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Preamble {
    pub task_id: Option<String>,
    pub project_id: Option<String>,
    pub depends_on: Vec<String>,
    pub persona: Option<String>,
    pub work_dir: Option<String>,
}

/// Parse optional `---` delimited frontmatter from document content.
/// Returns (preamble, body) where body is everything after the closing `---`.
pub fn parse_preamble(content: &str) -> (Preamble, &str) {
    let trimmed = content.strip_prefix('\u{feff}').unwrap_or(content);

    // Must start with --- followed by newline
    let after_open = if let Some(rest) = trimmed.strip_prefix("---\n") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("---\r\n") {
        rest
    } else {
        return (Preamble::default(), content);
    };

    // Find closing --- on its own line (either at very start of after_open, or after \n)
    let close_pos = if after_open.starts_with("---\n") {
        Some((0, 4))
    } else if after_open.starts_with("---\r\n") {
        Some((0, 5))
    } else {
        after_open
            .find("\n---\n")
            .map(|p| (p, p + 5))
            .or_else(|| after_open.find("\n---\r\n").map(|p| (p, p + 6)))
            .or_else(|| {
                if after_open.ends_with("\n---") {
                    Some((after_open.len() - 4, after_open.len()))
                } else {
                    None
                }
            })
    };

    let (frontmatter_end, body_start_in_after) = match close_pos {
        Some((fm_end, bs)) => (fm_end, bs),
        None => return (Preamble::default(), content),
    };

    let frontmatter = &after_open[..frontmatter_end];
    let body_offset = trimmed.len() - after_open.len() + body_start_in_after;
    let body = &trimmed[body_offset..];
    // If content had BOM, body slice is from trimmed which skipped BOM — but we want
    // body relative to original content. Since we only stripped BOM for detection,
    // just use trimmed-based body. The caller gets usable body either way.

    let mut preamble = Preamble::default();
    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "task_id" => preamble.task_id = Some(value.to_string()),
                "project_id" => preamble.project_id = Some(value.to_string()),
                "persona" => preamble.persona = Some(value.to_string()),
                "work_dir" => preamble.work_dir = Some(value.to_string()),
                "depends_on" => {
                    preamble.depends_on = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                _ => {} // unknown keys silently ignored
            }
        }
    }

    (preamble, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_state_transitions() {
        assert!(DocumentState::Todo.can_transition_to(&DocumentState::Processing));
        assert!(DocumentState::Processing.can_transition_to(&DocumentState::Complete));
        assert!(DocumentState::Processing.can_transition_to(&DocumentState::Feedback));
        assert!(DocumentState::Feedback.can_transition_to(&DocumentState::Processing));
        assert!(DocumentState::Complete.can_transition_to(&DocumentState::Refining));
        assert!(!DocumentState::Todo.can_transition_to(&DocumentState::Complete));
        assert!(!DocumentState::Complete.can_transition_to(&DocumentState::Todo));
    }

    #[test]
    fn document_transition_updates_timestamp() {
        let mut doc = KanbanDocument::new("test-id".to_string(), "my-task".to_string());
        let before = doc.updated_at;
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(doc.transition_to(DocumentState::Processing));
        assert!(doc.updated_at > before);
        assert_eq!(doc.state, DocumentState::Processing);
    }

    #[test]
    fn file_stem_format() {
        let doc = KanbanDocument::new("abc-123".to_string(), "build-parser".to_string());
        assert_eq!(doc.file_stem(), "build-parser_abc-123");
    }

    #[test]
    fn parse_kanban_filename_valid() {
        let (name, id) =
            parse_kanban_filename("build-parser_01964e2c-4f5a-7abc-8000-abcdef123456.md")
                .expect("should parse");
        assert_eq!(name, "build-parser");
        assert_eq!(id, "01964e2c-4f5a-7abc-8000-abcdef123456");
    }

    #[test]
    fn parse_kanban_filename_invalid() {
        assert!(parse_kanban_filename("plain.md").is_none());
        assert!(parse_kanban_filename("no-uuid_short.md").is_none());
    }

    #[test]
    fn parse_preamble_no_frontmatter() {
        let content = "Just some plain content\nwith multiple lines.";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble, Preamble::default());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_preamble_full() {
        let content = "---\ntask_id: build-login\nproject_id: 01964e2c-4f5a-7abc-8000-abcdef123456\ndepends_on: setup-db, create-schema\npersona: software engineer\n---\nBuild a login page.\n";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble.task_id.as_deref(), Some("build-login"));
        assert_eq!(
            preamble.project_id.as_deref(),
            Some("01964e2c-4f5a-7abc-8000-abcdef123456")
        );
        assert_eq!(preamble.depends_on, vec!["setup-db", "create-schema"]);
        assert_eq!(preamble.persona.as_deref(), Some("software engineer"));
        assert_eq!(body, "Build a login page.\n");
    }

    #[test]
    fn parse_preamble_partial() {
        let content = "---\ntask_id: only-id\n---\nBody here.";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble.task_id.as_deref(), Some("only-id"));
        assert_eq!(preamble.project_id, None);
        assert!(preamble.depends_on.is_empty());
        assert_eq!(preamble.persona, None);
        assert_eq!(body, "Body here.");
    }

    #[test]
    fn parse_preamble_empty_frontmatter() {
        let content = "---\n---\nBody after empty preamble.";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble, Preamble::default());
        assert_eq!(body, "Body after empty preamble.");
    }

    #[test]
    fn parse_preamble_unknown_keys_ignored() {
        let content = "---\ntask_id: my-task\nfoo: bar\nbaz: qux\n---\nContent.";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble.task_id.as_deref(), Some("my-task"));
        assert_eq!(body, "Content.");
    }

    #[test]
    fn parse_preamble_no_closing_delimiter() {
        let content = "---\ntask_id: orphan\nThis line has no closing delimiter.";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble, Preamble::default());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_preamble_single_depends_on() {
        let content = "---\ndepends_on: setup-db\n---\nBody.";
        let (preamble, _body) = parse_preamble(content);
        assert_eq!(preamble.depends_on, vec!["setup-db"]);
    }

    #[test]
    fn parse_preamble_multiple_depends_on() {
        let content = "---\ndepends_on: setup-db, create-schema, seed-data\n---\nBody.";
        let (preamble, _body) = parse_preamble(content);
        assert_eq!(
            preamble.depends_on,
            vec!["setup-db", "create-schema", "seed-data"]
        );
    }

    #[test]
    fn cancel_transitions_todo_to_draft() {
        assert!(DocumentState::Todo.can_transition_to(&DocumentState::Draft));
    }

    #[test]
    fn cancel_transitions_processing_to_draft() {
        assert!(DocumentState::Processing.can_transition_to(&DocumentState::Draft));
    }

    #[test]
    fn revise_transitions_error_to_draft() {
        assert!(DocumentState::Error.can_transition_to(&DocumentState::Draft));
    }

    #[test]
    fn parse_preamble_work_dir() {
        let content = "---\ntask_id: deploy\nwork_dir: /home/user/project\n---\nDeploy instructions.";
        let (preamble, body) = parse_preamble(content);
        assert_eq!(preamble.task_id.as_deref(), Some("deploy"));
        assert_eq!(preamble.work_dir.as_deref(), Some("/home/user/project"));
        assert_eq!(body, "Deploy instructions.");
    }
}
