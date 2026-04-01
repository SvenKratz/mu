use async_trait::async_trait;
use serde_json::{json, Value};

use super::{resolve_path, truncate_output, AgentTool, ToolContext, ToolOutput};
use crate::MuAgentError;

pub(crate) struct LsTool;

#[async_trait]
impl AgentTool for LsTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "ls".to_string(),
            description: "List directory contents. Directories are shown with a trailing / suffix."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (default: working directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of entries to return (default: 500)"
                    }
                }
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(500, |v| v as usize);

        let dir_path = match input.get("path").and_then(Value::as_str) {
            Some(p) => resolve_path(&context.working_directory, p),
            None => context.working_directory.clone(),
        };

        let entries = match std::fs::read_dir(&dir_path) {
            Ok(entries) => entries,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("cannot list {}: {e}", dir_path.display()),
                    is_error: true,
                });
            }
        };

        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let mut name = entry.file_name().to_string_lossy().to_string();
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    name.push('/');
                }
            }
            names.push(name);
        }
        names.sort();

        if names.is_empty() {
            return Ok(ToolOutput {
                content: "(empty directory)".to_string(),
                is_error: false,
            });
        }

        let output = names.join("\n");
        Ok(ToolOutput {
            content: truncate_output(&output, limit),
            is_error: false,
        })
    }
}
