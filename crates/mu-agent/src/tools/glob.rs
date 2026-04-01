use async_trait::async_trait;
use serde_json::{json, Value};

use super::{required_string, resolve_path, run_external, truncate_output, AgentTool, ToolContext, ToolOutput};
use crate::MuAgentError;

pub(crate) struct GlobTool;

#[async_trait]
impl AgentTool for GlobTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "find".to_string(),
            description: "Find files by glob pattern. Respects .gitignore. \
                Returns matching file paths sorted by modification time (newest first)."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files, e.g. \"**/*.rs\", \"src/**/*.toml\""
                    },
                    "path": {
                        "type": "string",
                        "description": "Root directory to search from (default: working directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of file paths to return (default: 1000)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn run(&self, input: Value, context: ToolContext) -> Result<ToolOutput, MuAgentError> {
        let pattern = required_string(&input, "pattern")?;
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(1000, |v| v as usize);

        let search_path = match input.get("path").and_then(Value::as_str) {
            Some(p) => resolve_path(&context.working_directory, p),
            None => context.working_directory.clone(),
        };

        let path_str = search_path.display().to_string();
        let glob_arg = format!("--glob={pattern}");
        let args = vec![
            "--files",
            "--color=never",
            "--sortr=modified",
            &glob_arg,
            &path_str,
        ];

        let (stdout, stderr, exit_code) =
            run_external("rg", &args, &context.working_directory, 30).await?;

        match exit_code {
            Some(0) => Ok(ToolOutput {
                content: truncate_output(&stdout, limit),
                is_error: false,
            }),
            Some(1) => Ok(ToolOutput {
                content: "No files found.".to_string(),
                is_error: false,
            }),
            Some(_) => Ok(ToolOutput {
                content: if stderr.is_empty() {
                    stdout
                } else {
                    stderr
                },
                is_error: true,
            }),
            None => Ok(ToolOutput {
                content: "find timed out after 30s".to_string(),
                is_error: true,
            }),
        }
    }
}
