#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn openai_stream_body(text: &str) -> String {
    format!(
        concat!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\n",
            "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}}}\n\n",
            "data: [DONE]\n\n"
        ),
        text = text
    )
}

async fn mock_openai() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(openai_stream_body("Done")),
        )
        .expect(4..)
        .mount(&server)
        .await;
    server
}

fn build_command(tempdir: &TempDir, server: &MockServer, kanban_dir: &str) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("mu"));
    let project_dir = tempdir.path().join("project");
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    command
        .current_dir(&project_dir)
        .env("MU_HOME", tempdir.path().join("home"))
        .env("MU_PROVIDER", "openai-compatible")
        .env("MU_MODEL", "gpt-4o-mini")
        .env("MU_OPENAI_API_KEY", "test-key")
        .env("MU_OPENAI_BASE_URL", server.uri())
        .arg("--kanban")
        .arg(kanban_dir);
    command
}

/// Create the 4-task snake game kanban board with dependency DAG:
///   A (constants) ──┐
///                    ├── C (game) ── D (main)
///   B (snake) ──────┘
fn create_snake_board(board_dir: &std::path::Path) {
    let todo_dir = board_dir.join("TODO");
    std::fs::create_dir_all(&todo_dir).expect("create TODO dir");

    let project_id = "snake-game-project";

    std::fs::write(
        todo_dir.join("task-a.md"),
        format!(
            "---\ntask_id: task-a\nproject_id: {project_id}\n---\n\
             Create a Python file `constants.py` with: SCREEN_WIDTH=40, SCREEN_HEIGHT=20, SNAKE_CHAR='O', FOOD_CHAR='*', EMPTY_CHAR='.'\n"
        ),
    )
    .expect("write task-a");

    std::fs::write(
        todo_dir.join("task-b.md"),
        format!(
            "---\ntask_id: task-b\nproject_id: {project_id}\n---\n\
             Create a Python file `snake.py` with a Snake class: __init__(self, x, y), move(direction), grow(), body property returns list of (x,y) tuples.\n"
        ),
    )
    .expect("write task-b");

    std::fs::write(
        todo_dir.join("task-c.md"),
        format!(
            "---\ntask_id: task-c\nproject_id: {project_id}\ndepends_on: task-a, task-b\n---\n\
             Create `game.py` that imports from constants.py and snake.py. Implement a Game class with: init, update, render_to_string, is_game_over methods.\n"
        ),
    )
    .expect("write task-c");

    std::fs::write(
        todo_dir.join("task-d.md"),
        format!(
            "---\ntask_id: task-d\nproject_id: {project_id}\ndepends_on: task-c\n---\n\
             Create `main.py` that imports Game from game.py and runs a console game loop using curses. Add score display and game-over screen.\n"
        ),
    )
    .expect("write task-d");
}

#[tokio::test]
async fn kanban_headless_respects_dependency_ordering() {
    let tempdir = TempDir::new().expect("tempdir");
    let board_dir = tempdir.path().join("snake-board");
    create_snake_board(&board_dir);

    let server = mock_openai().await;
    let output = build_command(&tempdir, &server, board_dir.to_str().expect("valid utf8 path"))
        .output()
        .expect("command should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "kanban headless should exit successfully.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Parse all JSON events from stdout
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    assert!(
        !events.is_empty(),
        "should have received kanban events on stdout"
    );

    // Verify all 4 documents were discovered
    let discovered: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["type"] == "document_discovered")
        .collect();
    assert_eq!(
        discovered.len(),
        4,
        "should discover exactly 4 documents, got: {discovered:?}"
    );

    // Collect processing order from ProcessingStarted events
    let processing_started: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "processing_started")
        .filter_map(|e| e["id"].as_str().map(String::from))
        .collect();

    // All 4 documents should have been processed
    assert_eq!(
        processing_started.len(),
        4,
        "all 4 documents should have been processed, got: {processing_started:?}"
    );

    // To verify dependency ordering, we need to map UUIDs back to task_ids.
    // Read the kanban_state.json to get the mapping.
    let state_path = board_dir.join("kanban_state.json");
    assert!(state_path.exists(), "kanban_state.json should exist");
    let state_content = std::fs::read_to_string(&state_path).expect("read state");
    let state: serde_json::Value = serde_json::from_str(&state_content).expect("parse state");

    // Build id → task_id mapping
    let documents = state["documents"].as_object().expect("documents map");
    let mut id_to_task: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (uuid, doc) in documents {
        if let Some(task_id) = doc["task_id"].as_str() {
            id_to_task.insert(uuid.clone(), task_id.to_string());
        }
    }

    // Map processing order to task_ids
    let task_order: Vec<String> = processing_started
        .iter()
        .filter_map(|id| id_to_task.get(id).cloned())
        .collect();

    assert_eq!(
        task_order.len(),
        4,
        "all 4 tasks should have task_ids in state, got: {task_order:?}"
    );

    // Verify dependency ordering:
    // A and B must come before C; C must come before D
    let pos_a = task_order.iter().position(|t| t == "task-a").expect("task-a processed");
    let pos_b = task_order.iter().position(|t| t == "task-b").expect("task-b processed");
    let pos_c = task_order.iter().position(|t| t == "task-c").expect("task-c processed");
    let pos_d = task_order.iter().position(|t| t == "task-d").expect("task-d processed");

    assert!(
        pos_a < pos_c,
        "task-a (pos {pos_a}) must be processed before task-c (pos {pos_c})"
    );
    assert!(
        pos_b < pos_c,
        "task-b (pos {pos_b}) must be processed before task-c (pos {pos_c})"
    );
    assert!(
        pos_c < pos_d,
        "task-c (pos {pos_c}) must be processed before task-d (pos {pos_d})"
    );

    // Verify all documents reached Complete state
    for (_uuid, doc) in documents {
        let state = doc["state"].as_str().expect("state field");
        assert_eq!(
            state, "complete",
            "all documents should be complete, but found {state} for {:?}",
            doc["original_name"]
        );
    }

    // Verify task_id and depends_on are persisted in state
    for (_uuid, doc) in documents {
        let task_id = doc["task_id"].as_str().expect("task_id persisted");
        match task_id {
            "task-a" | "task-b" => {
                // No dependencies
                assert!(
                    doc["depends_on"].as_array().is_none_or(|a| a.is_empty()),
                    "{task_id} should have no dependencies"
                );
            }
            "task-c" => {
                let deps: Vec<&str> = doc["depends_on"]
                    .as_array()
                    .expect("depends_on array")
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect();
                assert!(deps.contains(&"task-a"), "task-c should depend on task-a");
                assert!(deps.contains(&"task-b"), "task-c should depend on task-b");
            }
            "task-d" => {
                let deps: Vec<&str> = doc["depends_on"]
                    .as_array()
                    .expect("depends_on array")
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect();
                assert!(deps.contains(&"task-c"), "task-d should depend on task-c");
            }
            other => panic!("unexpected task_id: {other}"),
        }
    }

    // Verify final stats event shows all complete
    let final_stats = events
        .iter()
        .rev()
        .find(|e| e["type"] == "stats_updated")
        .expect("should have a stats_updated event");
    assert_eq!(final_stats["todo"], 0);
    assert_eq!(final_stats["processing"], 0);
    assert_eq!(final_stats["complete"], 4);
}

// ---------------------------------------------------------------------------
// Hot integration test — calls a real LLM, verifies real file output.
// Run with: cargo test -p mu --test kanban_dag -- --ignored --nocapture
// Requires MU_OPENAI_API_KEY (or MU_OPENAI_BASE_URL for a local model).
// ---------------------------------------------------------------------------

fn build_hot_command(tempdir: &TempDir, kanban_dir: &str) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("mu"));
    let project_dir = tempdir.path().join("project");
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    command
        .current_dir(&project_dir)
        .env("MU_HOME", tempdir.path().join("home"))
        .arg("--kanban")
        .arg(kanban_dir);
    command
}

/// Create 4-task snake game board with dependency DAG and explicit tool-use instructions.
fn create_hot_snake_board(board_dir: &Path) {
    let todo_dir = board_dir.join("TODO");
    std::fs::create_dir_all(&todo_dir).expect("create TODO dir");

    let project_id = "snake-game-project";

    std::fs::write(
        todo_dir.join("task-a.md"),
        format!(
            "---\ntask_id: task-a\nproject_id: {project_id}\n---\n\
             Use the `write` tool to create a file called `constants.py` with this exact content:\n\n\
             ```python\nSCREEN_WIDTH = 40\nSCREEN_HEIGHT = 20\nSNAKE_CHAR = \"O\"\nFOOD_CHAR = \"*\"\nEMPTY_CHAR = \".\"\n```\n"
        ),
    )
    .expect("write task-a");

    std::fs::write(
        todo_dir.join("task-b.md"),
        format!(
            "---\ntask_id: task-b\nproject_id: {project_id}\n---\n\
             Use the `write` tool to create a file called `snake.py` containing a Snake class with:\n\
             - `__init__(self, x, y)` that sets `self.segments = [(x, y)]` and `self.direction = (1, 0)`\n\
             - `move(self)` that moves the head by `self.direction` and drops the tail\n\
             - `grow(self)` that duplicates the last segment\n\
             - `body` property that returns `self.segments`\n"
        ),
    )
    .expect("write task-b");

    std::fs::write(
        todo_dir.join("task-c.md"),
        format!(
            "---\ntask_id: task-c\nproject_id: {project_id}\ndepends_on: task-a, task-b\n---\n\
             Use the `write` tool to create a file called `game.py` that:\n\
             - Imports SCREEN_WIDTH, SCREEN_HEIGHT, SNAKE_CHAR, FOOD_CHAR, EMPTY_CHAR from constants\n\
             - Imports Snake from snake\n\
             - Defines a Game class with `__init__`, `update`, `render_to_string`, and `is_game_over` methods\n"
        ),
    )
    .expect("write task-c");

    std::fs::write(
        todo_dir.join("task-d.md"),
        format!(
            "---\ntask_id: task-d\nproject_id: {project_id}\ndepends_on: task-c\n---\n\
             Use the `write` tool to create a file called `main.py` that:\n\
             - Imports Game from game\n\
             - Has a `main()` function that creates a Game and runs a loop calling `update` and `render_to_string`\n\
             - Prints the rendered string each iteration\n\
             - The file should be directly runnable with `python main.py`\n"
        ),
    )
    .expect("write task-d");
}

#[tokio::test]
#[ignore] // requires real API key — run with `--ignored`
async fn kanban_hot_snake_game_produces_python_files() {
    // Skip gracefully if no API key is set
    if std::env::var("MU_OPENAI_API_KEY").is_err()
        && std::env::var("OPENAI_API_KEY").is_err()
        && std::env::var("MU_OPENAI_BASE_URL").is_err()
    {
        eprintln!(
            "skipping hot test: set MU_OPENAI_API_KEY, OPENAI_API_KEY, or MU_OPENAI_BASE_URL"
        );
        return;
    }

    let tempdir = TempDir::new().expect("tempdir");
    let board_dir = tempdir.path().join("snake-board");
    create_hot_snake_board(&board_dir);

    let output = build_hot_command(&tempdir, board_dir.to_str().expect("utf8 path"))
        .output()
        .expect("command should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("--- stdout ---\n{stdout}");
    eprintln!("--- stderr ---\n{stderr}");

    assert!(
        output.status.success(),
        "kanban headless should exit successfully"
    );

    // ── Parse events ──────────────────────────────────────────────────
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    // All 4 discovered
    let discovered: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["type"] == "document_discovered")
        .collect();
    assert_eq!(discovered.len(), 4, "all 4 documents discovered");

    // All 4 completed
    let completed: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["type"] == "processing_complete")
        .collect();
    assert_eq!(completed.len(), 4, "all 4 documents completed");

    // ── Verify dependency ordering ────────────────────────────────────
    let state_path = board_dir.join("kanban_state.json");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
            .expect("parse state");
    let documents = state["documents"].as_object().expect("documents map");

    let processing_started: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "processing_started")
        .filter_map(|e| e["id"].as_str().map(String::from))
        .collect();

    let id_to_task: std::collections::HashMap<String, String> = documents
        .iter()
        .filter_map(|(uuid, doc)| {
            doc["task_id"]
                .as_str()
                .map(|tid| (uuid.clone(), tid.to_string()))
        })
        .collect();

    let task_order: Vec<String> = processing_started
        .iter()
        .filter_map(|id| id_to_task.get(id).cloned())
        .collect();

    let pos = |name: &str| {
        task_order
            .iter()
            .position(|t| t == name)
            .unwrap_or_else(|| panic!("{name} not found in task_order: {task_order:?}"))
    };
    assert!(pos("task-a") < pos("task-c"), "A before C");
    assert!(pos("task-b") < pos("task-c"), "B before C");
    assert!(pos("task-c") < pos("task-d"), "C before D");

    // ── Verify shared project directory ─────────────────────────────
    // All tasks share project_id "snake-game-project" → single RESULT dir
    let project_dir = board_dir.join("RESULT/snake-game-project");
    assert!(
        project_dir.is_dir(),
        "shared project dir should exist at {}",
        project_dir.display()
    );

    // Only one top-level entry in RESULT/
    let result_entries: Vec<_> = std::fs::read_dir(board_dir.join("RESULT"))
        .expect("read RESULT")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        result_entries.len(),
        1,
        "RESULT/ should have exactly 1 project directory, found: {:?}",
        result_entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    // All 4 Python files live in the same directory
    let constants = project_dir.join("constants.py");
    assert!(constants.exists(), "constants.py should exist in {}", project_dir.display());
    let constants_src = std::fs::read_to_string(&constants).expect("read constants.py");
    assert!(constants_src.contains("SCREEN_WIDTH"), "constants.py should define SCREEN_WIDTH");
    assert!(constants_src.contains("SCREEN_HEIGHT"), "constants.py should define SCREEN_HEIGHT");
    assert!(constants_src.contains("SNAKE_CHAR"), "constants.py should define SNAKE_CHAR");

    let snake = project_dir.join("snake.py");
    assert!(snake.exists(), "snake.py should exist in {}", project_dir.display());
    let snake_src = std::fs::read_to_string(&snake).expect("read snake.py");
    assert!(snake_src.contains("class Snake"), "snake.py should define class Snake");
    assert!(snake_src.contains("segments"), "snake.py should use segments");

    let game = project_dir.join("game.py");
    assert!(game.exists(), "game.py should exist in {}", project_dir.display());
    let game_src = std::fs::read_to_string(&game).expect("read game.py");
    assert!(game_src.contains("class Game"), "game.py should define class Game");
    assert!(game_src.contains("import") || game_src.contains("from"), "game.py should have imports");

    let main = project_dir.join("main.py");
    assert!(main.exists(), "main.py should exist in {}", project_dir.display());
    let main_src = std::fs::read_to_string(&main).expect("read main.py");
    assert!(main_src.contains("import") || main_src.contains("from"), "main.py should have imports");
    assert!(main_src.contains("main") || main_src.contains("Game"), "main.py should reference Game");

    // ── Verify per-task sessions in .sessions/ ───────────────────────
    let sessions_dir = project_dir.join(".sessions");
    assert!(sessions_dir.is_dir(), ".sessions/ should exist in project dir");

    let session_dirs: Vec<_> = std::fs::read_dir(&sessions_dir)
        .expect("read .sessions")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        session_dirs.len(),
        4,
        ".sessions/ should have 4 per-task subdirs, found: {:?}",
        session_dirs.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    // Each per-task session dir should have session.jsonl and SUMMARY.md
    for entry in &session_dirs {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            dir.join("session.jsonl").exists(),
            "{name}: session.jsonl should exist"
        );
        assert!(
            dir.join("SUMMARY.md").exists(),
            "{name}: SUMMARY.md should exist"
        );
    }

    // ── Verify TODO/ and PROCESSING/ are empty ────────────────────────
    let todo_remaining: Vec<_> = std::fs::read_dir(board_dir.join("TODO"))
        .expect("read TODO")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert!(
        todo_remaining.is_empty(),
        "TODO/ should be empty after completion, found: {todo_remaining:?}"
    );

    let processing_remaining: Vec<_> = std::fs::read_dir(board_dir.join("PROCESSING"))
        .expect("read PROCESSING")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert!(
        processing_remaining.is_empty(),
        "PROCESSING/ should be empty after completion, found: {processing_remaining:?}"
    );

    eprintln!("hot test passed — all 4 Python files in shared project dir");
}

// ---------------------------------------------------------------------------
// Hot fan-out/fan-in integration test.
// DAG:  A → B, C, D (parallel) → E
// Run with: cargo test -p mu --test kanban_dag -- --ignored --nocapture
// ---------------------------------------------------------------------------

/// Create 5-task board exercising the fan-out/fan-in pattern:
///   A (root)
///   ├── B (depends_on: a)
///   ├── C (depends_on: a)
///   ├── D (depends_on: a)
///   └── E (depends_on: b, c, d)
fn create_fan_out_fan_in_board(board_dir: &Path) {
    let todo_dir = board_dir.join("TODO");
    std::fs::create_dir_all(&todo_dir).expect("create TODO dir");

    let project_id = "fanout-project";

    std::fs::write(
        todo_dir.join("a.md"),
        format!(
            "---\ntask_id: a\nproject_id: {project_id}\n---\n\
             Use the `write` tool to create a file called `shared_config.py` with this exact content:\n\n\
             ```python\nAPP_NAME = \"FanOutDemo\"\nVERSION = \"1.0\"\nMAX_WORKERS = 3\n```\n"
        ),
    )
    .expect("write a");

    std::fs::write(
        todo_dir.join("b.md"),
        format!(
            "---\ntask_id: b\nproject_id: {project_id}\ndepends_on: a\n---\n\
             Use the `write` tool to create a file called `worker_alpha.py` with a function \
             `run_alpha()` that prints \"alpha done\" and returns the string \"alpha\". \
             Import APP_NAME from shared_config at the top.\n"
        ),
    )
    .expect("write b");

    std::fs::write(
        todo_dir.join("c.md"),
        format!(
            "---\ntask_id: c\nproject_id: {project_id}\ndepends_on: a\n---\n\
             Use the `write` tool to create a file called `worker_beta.py` with a function \
             `run_beta()` that prints \"beta done\" and returns the string \"beta\". \
             Import VERSION from shared_config at the top.\n"
        ),
    )
    .expect("write c");

    std::fs::write(
        todo_dir.join("d.md"),
        format!(
            "---\ntask_id: d\nproject_id: {project_id}\ndepends_on: a\n---\n\
             Use the `write` tool to create a file called `worker_gamma.py` with a function \
             `run_gamma()` that prints \"gamma done\" and returns the string \"gamma\". \
             Import MAX_WORKERS from shared_config at the top.\n"
        ),
    )
    .expect("write d");

    std::fs::write(
        todo_dir.join("e.md"),
        format!(
            "---\ntask_id: e\nproject_id: {project_id}\ndepends_on: b, c, d\n---\n\
             Use the `write` tool to create a file called `aggregator.py` that:\n\
             - Imports run_alpha from worker_alpha, run_beta from worker_beta, run_gamma from worker_gamma\n\
             - Defines a function `aggregate()` that calls all three and returns a list of their results\n\
             - Prints the aggregated list when run as `__main__`\n"
        ),
    )
    .expect("write e");
}

#[tokio::test]
#[ignore] // requires real API key — run with `--ignored`
async fn kanban_hot_fan_out_fan_in() {
    // Skip gracefully if no API key is set
    if std::env::var("MU_OPENAI_API_KEY").is_err()
        && std::env::var("OPENAI_API_KEY").is_err()
        && std::env::var("MU_OPENAI_BASE_URL").is_err()
    {
        eprintln!(
            "skipping hot test: set MU_OPENAI_API_KEY, OPENAI_API_KEY, or MU_OPENAI_BASE_URL"
        );
        return;
    }

    let tempdir = TempDir::new().expect("tempdir");
    let board_dir = tempdir.path().join("fanout-board");
    create_fan_out_fan_in_board(&board_dir);

    let output = build_hot_command(&tempdir, board_dir.to_str().expect("utf8 path"))
        .output()
        .expect("command should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("--- stdout ---\n{stdout}");
    eprintln!("--- stderr ---\n{stderr}");

    assert!(
        output.status.success(),
        "kanban headless should exit successfully"
    );

    // ── Parse events ──────────────────────────────────────────────────
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    // All 5 discovered
    let discovered: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["type"] == "document_discovered")
        .collect();
    assert_eq!(discovered.len(), 5, "all 5 documents discovered");

    // All 5 completed
    let completed: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["type"] == "processing_complete")
        .collect();
    assert_eq!(completed.len(), 5, "all 5 documents completed");

    // ── Verify dependency ordering ────────────────────────────────────
    let state_path = board_dir.join("kanban_state.json");
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
            .expect("parse state");
    let documents = state["documents"].as_object().expect("documents map");

    let processing_started: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "processing_started")
        .filter_map(|e| e["id"].as_str().map(String::from))
        .collect();

    let id_to_task: std::collections::HashMap<String, String> = documents
        .iter()
        .filter_map(|(uuid, doc)| {
            doc["task_id"]
                .as_str()
                .map(|tid| (uuid.clone(), tid.to_string()))
        })
        .collect();

    let task_order: Vec<String> = processing_started
        .iter()
        .filter_map(|id| id_to_task.get(id).cloned())
        .collect();

    assert_eq!(
        task_order.len(),
        5,
        "all 5 tasks should appear in processing order, got: {task_order:?}"
    );

    let pos = |name: &str| {
        task_order
            .iter()
            .position(|t| t == name)
            .unwrap_or_else(|| panic!("{name} not found in task_order: {task_order:?}"))
    };

    // A must precede B, C, D (fan-out)
    assert!(pos("a") < pos("b"), "A before B");
    assert!(pos("a") < pos("c"), "A before C");
    assert!(pos("a") < pos("d"), "A before D");
    // B, C, D must all precede E (fan-in)
    assert!(pos("b") < pos("e"), "B before E");
    assert!(pos("c") < pos("e"), "C before E");
    assert!(pos("d") < pos("e"), "D before E");

    // ── Verify all documents reached Complete state ───────────────────
    for (_uuid, doc) in documents {
        let state = doc["state"].as_str().expect("state field");
        assert_eq!(
            state, "complete",
            "all documents should be complete, but found {state} for {:?}",
            doc["original_name"]
        );
    }

    // ── Verify shared project directory & output files ────────────────
    let project_dir = board_dir.join("RESULT/fanout-project");
    assert!(
        project_dir.is_dir(),
        "shared project dir should exist at {}",
        project_dir.display()
    );

    let shared_config = project_dir.join("shared_config.py");
    assert!(shared_config.exists(), "shared_config.py should exist");
    let src = std::fs::read_to_string(&shared_config).expect("read shared_config.py");
    assert!(src.contains("APP_NAME"), "shared_config.py should define APP_NAME");
    assert!(src.contains("MAX_WORKERS"), "shared_config.py should define MAX_WORKERS");

    let worker_alpha = project_dir.join("worker_alpha.py");
    assert!(worker_alpha.exists(), "worker_alpha.py should exist");
    let src = std::fs::read_to_string(&worker_alpha).expect("read worker_alpha.py");
    assert!(src.contains("run_alpha"), "worker_alpha.py should define run_alpha");

    let worker_beta = project_dir.join("worker_beta.py");
    assert!(worker_beta.exists(), "worker_beta.py should exist");
    let src = std::fs::read_to_string(&worker_beta).expect("read worker_beta.py");
    assert!(src.contains("run_beta"), "worker_beta.py should define run_beta");

    let worker_gamma = project_dir.join("worker_gamma.py");
    assert!(worker_gamma.exists(), "worker_gamma.py should exist");
    let src = std::fs::read_to_string(&worker_gamma).expect("read worker_gamma.py");
    assert!(src.contains("run_gamma"), "worker_gamma.py should define run_gamma");

    let aggregator = project_dir.join("aggregator.py");
    assert!(aggregator.exists(), "aggregator.py should exist");
    let src = std::fs::read_to_string(&aggregator).expect("read aggregator.py");
    assert!(src.contains("aggregate"), "aggregator.py should define aggregate");
    assert!(
        src.contains("import") || src.contains("from"),
        "aggregator.py should import from workers"
    );

    // ── Verify per-task sessions ──────────────────────────────────────
    let sessions_dir = project_dir.join(".sessions");
    assert!(sessions_dir.is_dir(), ".sessions/ should exist in project dir");

    let session_dirs: Vec<_> = std::fs::read_dir(&sessions_dir)
        .expect("read .sessions")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        session_dirs.len(),
        5,
        ".sessions/ should have 5 per-task subdirs, found: {:?}",
        session_dirs.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );

    for entry in &session_dirs {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            dir.join("session.jsonl").exists(),
            "{name}: session.jsonl should exist"
        );
        assert!(
            dir.join("SUMMARY.md").exists(),
            "{name}: SUMMARY.md should exist"
        );
    }

    // ── Verify TODO/ and PROCESSING/ are empty ────────────────────────
    let todo_remaining: Vec<_> = std::fs::read_dir(board_dir.join("TODO"))
        .expect("read TODO")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert!(
        todo_remaining.is_empty(),
        "TODO/ should be empty after completion, found: {todo_remaining:?}"
    );

    let processing_remaining: Vec<_> = std::fs::read_dir(board_dir.join("PROCESSING"))
        .expect("read PROCESSING")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert!(
        processing_remaining.is_empty(),
        "PROCESSING/ should be empty after completion, found: {processing_remaining:?}"
    );

    // MU_TEST_PERSIST=1 keeps the output directory for manual inspection
    if std::env::var("MU_TEST_PERSIST").is_ok() {
        let kept = tempdir.keep();
        eprintln!("test output persisted at: {}", kept.display());
    }

    eprintln!("hot fan-out/fan-in test passed — all 5 Python files in shared project dir");
}
