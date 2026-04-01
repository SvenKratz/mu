<p align="center">
  <img src="logo.svg" width="120" height="120" alt="mu">
</p>

<h1 align="center">Mu</h1>

<p align="center">
  A Rust-first coding agent that runs in your terminal.
</p>

---

Mu streams LLM responses, executes tools, manages branching conversation sessions, and can orchestrate parallel tasks through its built-in kanban system.

## Workspace

```text
crates/
  mu-ai          # provider abstraction and streaming LLM normalization
  mu-agent       # stateful agent loop, tools, sessions, kanban runner
  mu-tui         # ratatui-based interactive terminal app
  mu-kanban-ui   # web server + REST API for kanban board
  mu             # CLI binary
docs/
  architecture.md
  roadmap.md
```

## Modes

Mu can be run in several modes depending on how you want to interact with it.

### Interactive (default)

```bash
mu
mu "explain this codebase"
```

Opens a full-screen terminal UI with a scrollable message list, text input, model picker, session browser, and branching tree viewer. Supports slash commands (see below).

### Print

```bash
mu --print "summarize src/main.rs"
echo "what does this do?" | mu --print
```

Sends a single prompt and streams the assistant's text response to stdout. No TUI, no session persistence by default.

### JSON

```bash
mu --json "refactor the auth module"
```

Streams every agent event (text deltas, tool calls, tool results, usage) as one JSON object per line. Useful for piping into other tools or CI integrations.

### Kanban (headless)

```bash
mu --kanban ./tasks
mu --headless ./tasks
```

`--kanban` runs the kanban board in the background and streams JSON events.
`--headless` adds a built-in web API server on `127.0.0.1:3141` with colored terminal logging, a stability check for completion, and proper exit codes (0 = all tasks done, 1 = any errors).

## CLI Flags

| Flag | Description |
|---|---|
| `--print` | Non-interactive print mode |
| `--json` | Stream JSON agent events |
| `-c, --continue` | Resume the most recent session |
| `-r, --resume [SELECTOR]` | Resume a specific session by path or timestamp |
| `--session <PATH>` | Use a specific session file |
| `--no-session` | Run without persisting session state |
| `--kanban <DIR>` | Run kanban board (JSON event stream) |
| `--headless <DIR>` | Run kanban with web API server |

## Slash Commands

Available inside the interactive TUI:

| Command | Description |
|---|---|
| `/model [ID]` | Switch model (opens picker if no ID given) |
| `/new` | Start a new session |
| `/resume [SELECTOR]` | Resume a session (opens picker if no selector) |
| `/session` | Display current session path and cwd |
| `/tree [NODE_ID]` | View session branching tree or branch to a specific node |
| `/compact [NOTE]` | Compact message history with optional annotation |
| `/kanban [FOLDER]` | Start kanban runner (`/kanban stop`, `/kanban retry`, `/kanban status`) |
| `/kanban-ui [FOLDER]` or `/kui` | Start kanban with web UI (auto-opens browser) |
| `/quit` or `/exit` | Exit |

## Tools

The agent has four core tools:

- **`read`** -- read UTF-8 text files
- **`write`** -- write UTF-8 text files (creates parent directories automatically)
- **`edit`** -- replace exact text spans in a file
- **`bash`** -- execute shell commands (default timeout 10s, max 120s)

In kanban mode, two additional tools are available:

- **`create_task`** -- create new kanban tasks
- **`request_feedback`** -- request user input before proceeding

## Kanban Board

The kanban system orchestrates multi-task workflows. Drop markdown files into a `TODO/` directory and mu processes them through a pipeline:

```
TODO/ -> PROCESSING/ -> COMPLETE/
                     -> FEEDBACK/ (awaiting user input)
                     -> REFINE/   (in review)
                     -> RESULT/   (work output files)
```

Tasks with the same `project_id` share a `RESULT/` directory. Independent tasks run in parallel. Failed tasks transition to an `Error` state and can be retried with `/kanban retry`.

### Task files

Task files use YAML frontmatter:

```markdown
---
task_id: unique-identifier
project_id: shared-project
depends_on: task-a, task-b
work_dir: /custom/work/directory
persona: agent personality description
---

The task prompt goes here.
```

- `depends_on` blocks execution until all listed task IDs reach `Complete`.
- `work_dir` overrides the working directory for that task.
- `persona` customizes the agent's system prompt for the task.

### Web UI

Start with `/kanban-ui` or `/kui` in interactive mode, or `--headless` from the CLI. Serves on `127.0.0.1:3141` with REST endpoints and an SSE stream for real-time updates.

## Providers

Mu supports two LLM provider backends:

### OpenAI-compatible (default)

```bash
export MU_PROVIDER=openai-compatible
export MU_OPENAI_API_KEY=sk-...    # or OPENAI_API_KEY
export MU_OPENAI_BASE_URL=...      # optional, defaults to https://api.openai.com/v1
```

Built-in models: `gpt-5.4`, `gpt-4o-mini`, `o3-mini`

### Anthropic

```bash
export MU_PROVIDER=anthropic
export MU_ANTHROPIC_API_KEY=sk-... # or ANTHROPIC_API_KEY
export MU_ANTHROPIC_BASE_URL=...   # optional
```

Built-in model: `claude-3-5-sonnet-latest`

### Custom models

Add models in `~/.mu/agent/models.toml`:

```toml
[[models]]
id = "my-local-model"
context_window = 32768
max_output_tokens = 4096
```

Override the active model with `MU_MODEL` or the `/model` slash command.

## Sessions

Sessions are stored as tree-structured JSONL files under `~/.mu/agent/sessions/<cwd>/`. Each message has a UUID and an optional parent ID, enabling non-linear branching.

- **Resume** -- `-c` for most recent, `-r` for a specific session, or `/resume` interactively
- **Branch** -- `/tree NODE_ID` to fork the conversation from any historical message
- **Compact** -- `/compact` summarizes the history to reduce context size. Auto-compaction triggers when the message count exceeds the configured threshold (default 48).

## Configuration

### Instructions

Mu loads instruction files and appends them to the system prompt:

1. Global: `~/.mu/agent/AGENTS.md`
2. Project: walks upward from cwd looking for `AGENTS.md`, falls back to `CLAUDE.md`

### Environment variables

| Variable | Description |
|---|---|
| `MU_PROVIDER` | `openai-compatible` or `anthropic` |
| `MU_MODEL` | Override the default model |
| `MU_HOME` | Override `~/.mu` for sessions, instructions, models |
| `MU_OPENAI_API_KEY` | OpenAI API key |
| `MU_OPENAI_BASE_URL` | OpenAI-compatible base URL |
| `MU_ANTHROPIC_API_KEY` | Anthropic API key |
| `MU_ANTHROPIC_BASE_URL` | Anthropic base URL |
| `MU_API_KEY` | Fallback API key |

## Build

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```
