# Mu Agent Notes

This file is a compact handoff for AI agents working in this repository.

## Project Shape

Mu is a Rust-first coding agent workspace inspired by `references/pi-mono`.

Current workspace members:

- `crates/mu-ai`: provider abstraction, model registry, SSE parsing, streaming event normalization
- `crates/mu-agent`: agent loop, tools, session persistence, instruction loading, compaction, kanban runner
- `crates/mu-tui`: `ratatui`/`crossterm` app state, overlays, slash-command parsing, key handling
- `crates/mu`: CLI binary and orchestration for interactive, `--print`, `--json`, and `--kanban` modes

Reference material:

- `references/pi-mono` is source material only
- do not port files mechanically; preserve product intent and keep implementations idiomatic Rust

Deferred for now:

- web UI
- Slack/mom integration
- pods/GPU management
- OAuth login flows
- extension runtime compatibility with TypeScript packages

## Important Files

- `README.md`: top-level project summary
- `docs/architecture.md`: layer boundaries and data flow
- `docs/roadmap.md`: explicitly deferred scope
- `crates/mu-ai/src/models.rs`: built-in model registry (default model: `gpt-5.4`)
- `crates/mu-ai/src/providers/openai.rs`: OpenAI-compatible request/stream normalization
- `crates/mu-ai/src/providers/anthropic.rs`: Anthropic request/stream normalization
- `crates/mu-agent/src/lib.rs`: main agent loop and queue handling
- `crates/mu-agent/src/tools/mod.rs`: built-in `read`, `write`, `edit`, `bash` tools
- `crates/mu-agent/src/session.rs`: JSONL session tree storage
- `crates/mu-agent/src/instructions.rs`: `AGENTS.md` / `CLAUDE.md` discovery
- `crates/mu-agent/src/kanban/mod.rs`: kanban runner with parallel task dispatch
- `crates/mu-agent/src/kanban/document.rs`: document state machine (`Draft → Todo → Processing → Complete/Error/Feedback`)
- `crates/mu-agent/src/kanban/state.rs`: kanban state persistence (`kanban_state.json`)
- `crates/mu-agent/src/kanban/stats.rs`: live stats and `kanban_stats.md` generation
- `crates/mu-agent/src/kanban/watcher.rs`: filesystem watcher for board changes
- `crates/mu-tui/src/lib.rs`: interactive app state, overlays, key handling
- `crates/mu/src/lib.rs`: CLI, mode wiring, model/session selection, interactive loop, headless kanban
- `crates/mu/tests/kanban_dag.rs`: integration tests for kanban DAG execution

## Current Behavior

### Providers and models

- Built-in providers: `openai-compatible`, `anthropic`
- Default model: `gpt-5.4` (1M context window, 100K max output tokens)
- Built-in OpenAI-compatible models: `gpt-5.4`, `gpt-4o-mini`, `o3-mini`
- Built-in Anthropic model: `claude-3-5-sonnet-latest`
- Each `ModelSpec` has a `max_output_tokens` field used as `max_tokens` in API requests
- Extra models can be loaded from `~/.mu/agent/models.toml` or `MU_HOME/agent/models.toml`
  - custom models that omit `max_output_tokens` default to 16,384

### OpenAI-compatible API quirk

- GPT-5-family models currently need `max_completion_tokens`, not `max_tokens`
- this is already handled in `crates/mu-ai/src/providers/openai.rs`
- if a new OpenAI-family model errors on unsupported params, check that request builder first

### Sessions

- sessions are JSONL trees, not linear transcripts
- default location: `~/.mu/agent/sessions/<sanitized-cwd>/`
- `MU_HOME` overrides `~/.mu`
- each line stores `id`, optional `parent_id`, timestamp, and serialized `Message`

### Instructions

- global instructions: `~/.mu/agent/AGENTS.md`
- project instructions: walk upward from cwd and load `AGENTS.md`, falling back to `CLAUDE.md`
- the resolved instruction text is appended to the runtime system prompt in `crates/mu/src/lib.rs`

### Kanban mode

- `--kanban <dir>` runs the kanban board headlessly (prints JSON events to stdout, exits when all tasks complete)
- board directory structure: `TODO/`, `PROCESSING/`, `FEEDBACK/`, `REFINE/`, `RESULT/`, `COMPLETE/`
- task files are `.md` documents placed in `TODO/`; they get renamed with a UUID on discovery
- tasks support a YAML frontmatter preamble with fields: `task_id`, `depends_on`, `project_id`, `persona`
- **parallel dispatch**: independent TODO tasks (no unmet dependencies) are spawned concurrently via `tokio::spawn`; dependent tasks wait for predecessors to complete
- **project directories**: tasks sharing a `project_id` collaborate in a single `RESULT/<project_id>/` directory with per-task session logs under `.sessions/<file_stem>/`
- **dependency DAG**: `depends_on` is a comma-separated list of `task_id` values; a task is only dispatched when all listed dependencies have reached `Complete` state
- **error handling**: failed tasks transition to `DocumentState::Error` rather than staying stuck in `Processing`
- **events**: all state changes are broadcast via `tokio::sync::broadcast` as `KanbanEvent` variants
- the headless runner (`run_kanban_headless` in `crates/mu/src/lib.rs`) exits with success when `todo == 0 && processing == 0 && feedback == 0 && refining == 0`, or with error if any tasks errored

### TUI and interaction

- interactive UI is state-driven, not widget-heavy
- `/model` opens a selectable overlay
- model picker highlights the current model
- `Up` / `Down` and `j` / `k` move the selection
- `Enter` selects the highlighted model
- `Ctrl+Q` quits globally, even if an overlay is open
- avoid optimistic user-message insertion in the interactive loop; the UI should use agent events as the source of truth
  - this already fixed a duplicate user-message bug
- messages are color-coded by role (green=user, blue=assistant, yellow=tool, magenta=system/kanban)
- input area shows `<cwd> >` prompt prefix in bold cyan
- footer has colored status spans (green=idle, yellow=streaming, magenta=kanban)

## Commands

Use these from repo root:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

Run the app:

```bash
cargo run -p mu --
```

Common variants:

```bash
cargo run -p mu -- --print "hello"
cargo run -p mu -- --json "hello"
cargo run -p mu -- -c
cargo run -p mu -- --kanban path/to/board
```

Provider env vars:

- OpenAI-compatible:
  - `MU_PROVIDER=openai-compatible`
  - `MU_OPENAI_API_KEY` or `OPENAI_API_KEY`
  - optional `MU_OPENAI_BASE_URL` or `OPENAI_BASE_URL`
- Anthropic:
  - `MU_PROVIDER=anthropic`
  - `MU_ANTHROPIC_API_KEY` or `ANTHROPIC_API_KEY`
  - optional `MU_ANTHROPIC_BASE_URL`

## Editing Guidance

- keep changes Rust-first; do not mirror TypeScript interfaces unless user-facing behavior depends on it
- preserve the crate boundaries above unless there is a clear architectural reason to move code
- prefer adding tests in the crate that owns the behavior:
  - provider/request logic -> `mu-ai`
  - agent loop/session/tool logic -> `mu-agent`
  - kanban runner/dispatch logic -> `mu-agent` (kanban module tests)
  - interaction/overlay/key logic -> `mu-tui`
  - end-to-end CLI behavior -> `crates/mu/tests/cli.rs`
  - kanban integration tests -> `crates/mu/tests/kanban_dag.rs`
- if you touch OpenAI-compatible request fields, test both normal OpenAI models and GPT-5-family models
- if you touch interactive rendering or submit flow, verify user messages are not duplicated
- if you touch kanban dispatch logic, verify parallel and dependency-ordering tests pass

## Known Constraints

- this repo is focused on a working terminal coding agent, not a full pi-mono port
- some defaults are intentionally simple:
  - compaction is basic
  - auth is environment-variable based
  - overlays are lightweight and custom
- the workspace lints warn on `unused_crate_dependencies`, so some crates explicitly allow that at the crate root
- pre-existing clippy `unwrap()` warnings exist in mu-agent test code (not from recent changes)
