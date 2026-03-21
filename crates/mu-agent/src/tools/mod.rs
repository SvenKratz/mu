#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::MuAgentError;

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub working_directory: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn spec(&self) -> mu_ai::ToolSpec;
    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError>;
}

pub fn default_tools(working_directory: &Path) -> Vec<Arc<dyn AgentTool>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool {
            default_working_directory: working_directory.to_path_buf(),
        }),
    ]
}

struct ReadTool;

#[async_trait]
impl AgentTool for ReadTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "read".to_string(),
            description: "Read a UTF-8 text file.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let path = required_string(&input, "path")?;
        let path = resolve_path(&context.working_directory, &path);
        let content = std::fs::read_to_string(path)?;
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

struct WriteTool;

#[async_trait]
impl AgentTool for WriteTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "write".to_string(),
            description: "Write a UTF-8 text file.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let path = required_string(&input, "path")?;
        let content = required_string(&input, "content")?;
        let path = resolve_path(&context.working_directory, &path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        Ok(ToolOutput {
            content: format!("wrote {}", path.display()),
            is_error: false,
        })
    }
}

struct EditTool;

#[async_trait]
impl AgentTool for EditTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "edit".to_string(),
            description: "Replace one exact text span in a file.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let path = required_string(&input, "path")?;
        let old_text = required_string(&input, "old_text")?;
        let new_text = required_string(&input, "new_text")?;
        let path = resolve_path(&context.working_directory, &path);
        let content = std::fs::read_to_string(&path)?;
        let count = content.matches(&old_text).count();
        if count != 1 {
            return Ok(ToolOutput {
                content: format!("expected exactly one match, found {count}"),
                is_error: true,
            });
        }
        let updated = content.replacen(&old_text, &new_text, 1);
        std::fs::write(&path, updated)?;
        Ok(ToolOutput {
            content: format!("edited {}", path.display()),
            is_error: false,
        })
    }
}

struct BashTool {
    default_working_directory: PathBuf,
}

#[async_trait]
impl AgentTool for BashTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "bash".to_string(),
            description: "Run a shell command.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let command = required_string(&input, "command")?;
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(10);
        let working_directory = if context.working_directory.as_os_str().is_empty() {
            self.default_working_directory.clone()
        } else {
            context.working_directory
        };

        let mut shell = Command::new("sh");
        shell
            .kill_on_drop(true)
            .arg("-lc")
            .arg(&command)
            .current_dir(working_directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(Duration::from_secs(timeout_secs), shell.output()).await;
        let output = match output {
            Ok(result) => result?,
            Err(_) => {
                return Ok(ToolOutput {
                    content: format!("command timed out after {timeout_secs}s"),
                    is_error: true,
                });
            }
        };
        let mut rendered = String::new();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.trim().is_empty() {
            rendered.push_str(&stdout);
        }
        if !stderr.trim().is_empty() {
            if !rendered.is_empty() && !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered.push_str(&stderr);
        }
        if rendered.is_empty() {
            rendered = format!("command exited with {}", output.status);
        }
        Ok(ToolOutput {
            content: rendered.trim_end().to_string(),
            is_error: !output.status.success(),
        })
    }
}

pub fn kanban_tools(working_directory: &Path, kanban_root: &Path) -> Vec<Arc<dyn AgentTool>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool {
            default_working_directory: working_directory.to_path_buf(),
        }),
        Arc::new(RequestFeedbackTool),
        Arc::new(CreateTaskTool {
            todo_path: kanban_root.join("TODO"),
        }),
    ]
}

struct CreateTaskTool {
    todo_path: PathBuf,
}

#[async_trait]
impl AgentTool for CreateTaskTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "create_task".to_string(),
            description: "Create a new task on the kanban board. The task file is placed in \
                the TODO queue and will be picked up and processed by another agent. \
                Use this to decompose complex work into smaller, focused subtasks. \
                The content should be a complete markdown task document, optionally \
                with a YAML frontmatter preamble for metadata."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short kebab-case name for the task (becomes the filename, e.g. 'setup-database')"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full markdown content of the task document. Can include a ---delimited frontmatter with fields: task_id, project_id, depends_on (comma-separated task_ids), work_dir, persona."
                    }
                },
                "required": ["name", "content"]
            }),
        }
    }

    async fn run(&self, input: Value, _context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let name = required_string(&input, "name")?;
        let content = required_string(&input, "content")?;

        // Sanitize name to be filesystem-safe
        let safe_name: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();

        // Auto-prepend frontmatter with task_id if none present
        let final_content = if content.trim_start().starts_with("---") {
            content
        } else {
            format!("---\ntask_id: {safe_name}\n---\n{content}")
        };

        let file_path = self.todo_path.join(format!("{safe_name}.md"));

        // Ensure TODO/ exists
        std::fs::create_dir_all(&self.todo_path)?;
        std::fs::write(&file_path, &final_content)?;

        Ok(ToolOutput {
            content: format!("created task: {safe_name} (queued in TODO/)"),
            is_error: false,
        })
    }
}

struct RequestFeedbackTool;

#[async_trait]
impl AgentTool for RequestFeedbackTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "request_feedback".to_string(),
            description: "Request feedback from the user. Use this when you need clarification or input before proceeding. The question will be presented to the user and processing will pause until they respond.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question or clarification request for the user"
                    }
                },
                "required": ["question"]
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let question = required_string(&input, "question")?;
        let feedback_path = context.working_directory.join("feedback_request.md");
        std::fs::write(&feedback_path, &question)?;
        Ok(ToolOutput {
            content: format!("Feedback requested. Processing will pause until the user responds. Question: {question}"),
            is_error: false,
        })
    }
}

fn required_string(input: &Value, field: &str) -> Result<String, MuAgentError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| {
            MuAgentError::InvalidState(format!("tool input missing string field {field}"))
        })
}

fn resolve_path(working_directory: &Path, raw_path: &str) -> PathBuf {
    let path = PathBuf::from(raw_path);
    if path.is_absolute() {
        path
    } else {
        working_directory.join(path)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{default_tools, AgentTool, ToolContext};

    #[tokio::test]
    async fn create_task_tool_writes_to_todo() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let todo_path = tempdir.path().join("TODO");
        let tool = super::CreateTaskTool {
            todo_path: todo_path.clone(),
        };
        let output = match tool
            .run(
                serde_json::json!({
                    "name": "setup-database",
                    "content": "---\ntask_id: setup-db\n---\nCreate the database schema."
                }),
                ToolContext {
                    working_directory: tempdir.path().to_path_buf(),
                },
            )
            .await
        {
            Ok(value) => value,
            Err(error) => panic!("create_task should run: {error}"),
        };
        assert!(!output.is_error);
        assert!(output.content.contains("setup-database"));
        let task_file = todo_path.join("setup-database.md");
        assert!(task_file.exists(), "task file should exist in TODO/");
        let content = std::fs::read_to_string(task_file).unwrap();
        assert!(content.contains("task_id: setup-db"));
        assert!(content.contains("Create the database schema."));
    }

    #[tokio::test]
    async fn create_task_without_frontmatter_gets_default_task_id() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let todo_path = tempdir.path().join("TODO");
        let tool = super::CreateTaskTool {
            todo_path: todo_path.clone(),
        };
        let output = match tool
            .run(
                serde_json::json!({
                    "name": "build-landing-page",
                    "content": "Build a responsive landing page with a hero section."
                }),
                ToolContext {
                    working_directory: tempdir.path().to_path_buf(),
                },
            )
            .await
        {
            Ok(value) => value,
            Err(error) => panic!("create_task should run: {error}"),
        };
        assert!(!output.is_error);
        let task_file = todo_path.join("build-landing-page.md");
        assert!(task_file.exists());
        let content = std::fs::read_to_string(task_file).unwrap();
        // Should have auto-prepended frontmatter with task_id
        assert!(
            content.contains("task_id: build-landing-page"),
            "should auto-generate task_id from name"
        );
        assert!(content.contains("Build a responsive landing page"));
    }

    #[tokio::test]
    async fn bash_tool_runs_commands() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let tools = default_tools(tempdir.path());
        let bash = match tools.into_iter().find(|tool| tool.spec().name == "bash") {
            Some(value) => value,
            None => panic!("bash tool should exist"),
        };
        let output = match bash
            .run(
                serde_json::json!({"command": "printf hello"}),
                ToolContext {
                    working_directory: tempdir.path().to_path_buf(),
                },
            )
            .await
        {
            Ok(value) => value,
            Err(error) => panic!("bash should run: {error}"),
        };
        assert_eq!(output.content, "hello");
        assert!(!output.is_error);
    }
}
