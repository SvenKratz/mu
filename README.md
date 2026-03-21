# Mu

Mu is a Rust-first coding agent workspace inspired by `references/pi-mono`.

This repository implements the core terminal stack as Rust crates:

- `mu-ai`: provider abstraction and streaming LLM normalization
- `mu-agent`: stateful agent loop, tools, sessions, and instruction loading
- `mu-tui`: terminal UI primitives and the interactive app state
- `mu`: the CLI binary for interactive, print, and JSON modes

The reference TypeScript monorepo remains under `references/pi-mono` as source material only. Mu does not try to be a line-by-line port. It keeps the same broad product shape while using idiomatic Rust interfaces and implementation choices.

## Workspace

```text
crates/
  mu-ai
  mu-agent
  mu-tui
  mu
docs/
  architecture.md
  roadmap.md
references/
  pi-mono/
```

## Current Scope

The initial implementation covers:

- OpenAI-compatible and Anthropic streaming chat providers
- A tool-driven agent loop with `read`, `write`, `edit`, and `bash`
- JSONL session storage with branching and resume support
- A `ratatui`-based interactive terminal app
- `--print` and `--json` command modes

Deferred areas are tracked in [docs/roadmap.md](docs/roadmap.md).

## Build

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

## Mapping From pi-mono

- `packages/ai` -> `crates/mu-ai`
- `packages/agent` -> `crates/mu-agent`
- `packages/tui` -> `crates/mu-tui`
- `packages/coding-agent` -> `crates/mu`

`packages/web-ui`, `packages/mom`, and `packages/pods` are intentionally deferred.

