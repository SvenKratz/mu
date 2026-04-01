use async_trait::async_trait;
use serde_json::{json, Value};

use super::{required_string, resolve_path, run_external, truncate_output, AgentTool, ToolContext, ToolOutput};
use crate::MuAgentError;

pub(crate) struct GrepTool;

#[async_trait]
impl AgentTool for GrepTool {
    fn spec(&self) -> mu_ai::ToolSpec {
        mu_ai::ToolSpec {
            name: "grep".to_string(),
            description: "Search file contents using ripgrep. Respects .gitignore. \
                Returns matching lines with file paths and line numbers."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for (or literal string if literal=true)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (default: working directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "File glob filter, e.g. \"*.rs\" or \"*.{ts,tsx}\""
                    },
                    "ignore_case": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)"
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat pattern as a literal string, not regex (default: false)"
                    },
                    "context": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Lines of context to show around each match (default: 0)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of result lines to return (default: 100)"
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
            .map_or(100, |v| v as usize);

        let search_path = match input.get("path").and_then(Value::as_str) {
            Some(p) => resolve_path(&context.working_directory, p),
            None => context.working_directory.clone(),
        };

        let mut args = vec![
            "--no-heading".to_string(),
            "--line-number".to_string(),
            "--color=never".to_string(),
        ];

        if input
            .get("ignore_case")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            args.push("--ignore-case".to_string());
        }

        if input
            .get("literal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            args.push("--fixed-strings".to_string());
        }

        if let Some(ctx) = input.get("context").and_then(Value::as_u64) {
            args.push(format!("--context={ctx}"));
        }

        if let Some(glob) = input.get("glob").and_then(Value::as_str) {
            args.push(format!("--glob={glob}"));
        }

        args.push(pattern);
        args.push(search_path.display().to_string());

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (stdout, stderr, exit_code) =
            run_external("rg", &arg_refs, &context.working_directory, 30).await?;

        match exit_code {
            Some(0) => Ok(ToolOutput {
                content: truncate_output(&stdout, limit),
                is_error: false,
            }),
            Some(1) => Ok(ToolOutput {
                content: "No matches found.".to_string(),
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
                content: "grep timed out after 30s".to_string(),
                is_error: true,
            }),
        }
    }
}
