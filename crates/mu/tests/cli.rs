#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

use std::process::Command;

use mu_agent::SessionStore;
use mu_ai::{Message, Role};
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

async fn mock_openai(text: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(openai_stream_body(text)),
        )
        .mount(&server)
        .await;
    server
}

fn build_command(tempdir: &TempDir, server: &MockServer) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("mu"));
    let project_dir = tempdir.path().join("project");
    if let Err(error) = std::fs::create_dir_all(&project_dir) {
        panic!("failed to create project dir: {error}");
    }
    command
        .current_dir(project_dir)
        .env("MU_HOME", tempdir.path().join("home"))
        .env("MU_PROVIDER", "openai-compatible")
        .env("MU_MODEL", "gpt-4o-mini")
        .env("MU_OPENAI_API_KEY", "test-key")
        .env("MU_OPENAI_BASE_URL", server.uri());
    command
}

#[tokio::test]
async fn print_mode_outputs_assistant_text() {
    let tempdir = match TempDir::new() {
        Ok(value) => value,
        Err(error) => panic!("tempdir should exist: {error}"),
    };
    let server = mock_openai("hello from mu").await;
    let output = match build_command(&tempdir, &server)
        .arg("--print")
        .arg("hello")
        .output()
    {
        Ok(value) => value,
        Err(error) => panic!("command should run: {error}"),
    };

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "hello from mu");
}

#[tokio::test]
async fn json_mode_outputs_agent_events() {
    let tempdir = match TempDir::new() {
        Ok(value) => value,
        Err(error) => panic!("tempdir should exist: {error}"),
    };
    let server = mock_openai("streamed").await;
    let output = match build_command(&tempdir, &server)
        .arg("--json")
        .arg("hello")
        .output()
    {
        Ok(value) => value,
        Err(error) => panic!("command should run: {error}"),
    };

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"type\":\"agent_start\""));
    assert!(stdout.contains("\"type\":\"text_delta\""));
    assert!(stdout.contains("\"delta\":\"streamed\""));
}

#[tokio::test]
async fn print_mode_can_continue_existing_session() {
    let tempdir = match TempDir::new() {
        Ok(value) => value,
        Err(error) => panic!("tempdir should exist: {error}"),
    };
    let server = mock_openai("continued").await;
    let session_path = tempdir.path().join("sessions/manual.jsonl");
    let store = SessionStore::from_path(session_path.clone());
    if let Err(error) = store.append(None, &Message::text(Role::User, "continue")) {
        panic!("failed to seed session: {error}");
    }

    let output = match build_command(&tempdir, &server)
        .arg("--print")
        .arg("--session")
        .arg(session_path)
        .output()
    {
        Ok(value) => value,
        Err(error) => panic!("command should run: {error}"),
    };

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "continued");
}
