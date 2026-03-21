# Mu Architecture

Mu is organized as a layered Cargo workspace:

1. `mu-ai` normalizes provider-specific streaming APIs into a single assistant event stream.
2. `mu-agent` orchestrates tool execution, session persistence, compaction, and instruction loading.
3. `mu-tui` holds terminal UI state, rendering helpers, slash-command parsing, and interaction behavior.
4. `mu` wires the layers into a CLI with interactive, print, and JSON modes.

## Provider Flow

`mu-ai` accepts a `StreamRequest` containing:

- model metadata
- conversation messages
- optional tools
- generation parameters

Provider implementations translate that request into upstream wire formats and emit normalized `AssistantEvent` values:

- `text_delta`
- `tool_call_delta`
- `tool_call`
- `usage`
- `stop`

## Agent Flow

`mu-agent` owns:

- mutable conversation state
- the active model and system prompt
- tool registry
- session cursor and JSONL persistence
- queued steering and follow-up messages

Each prompt executes a loop:

1. append the new user message
2. stream assistant output from `mu-ai`
3. persist the assistant message
4. execute tool calls and persist tool results
5. continue until the assistant stops without new tool calls

## Session Model

Sessions are stored as JSONL files under `~/.mu/agent/sessions/` or under the directory pointed to `MU_HOME`.

Each line stores:

- entry id
- optional parent id
- timestamp
- serialized message payload

This keeps branching explicit and lets Mu reconstruct any path through the session tree.

## TUI Model

The terminal app uses `ratatui` and `crossterm`. The app state is intentionally simple:

- scrollable message list
- input buffer
- footer with cwd/session/model/status
- optional modal overlay

Slash commands are parsed in `mu-tui` and executed by the `mu` crate.

