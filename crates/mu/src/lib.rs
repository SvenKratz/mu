#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use mu_agent::instructions::{load_instruction_files, render_instruction_text};
use mu_agent::kanban::state::KanbanState;
use mu_agent::kanban::KanbanRunner;
use mu_agent::{
    default_tools, list_session_files, Agent, AgentConfig, KanbanCommand, KanbanEvent, QueueMode,
    SessionStore,
};
use mu_kanban_ui::KanbanUiConfig;
use mu_ai::{load_custom_models, ModelRegistry, ModelSpec, ProviderId, RouterProvider};
use mu_tui::{
    App, AppAction, FooterData, OverlayItem, OverlayKind, OverlaySelection, SlashCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Parser)]
#[command(name = "mu", version, about = "Mu terminal coding agent")]
pub struct Cli {
    #[arg(long)]
    pub print: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(short = 'c', long = "continue")]
    pub continue_most_recent: bool,
    #[arg(short = 'r', long)]
    pub resume: bool,
    #[arg(long = "no-session")]
    pub no_session: bool,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub kanban: Option<String>,
    #[arg(long, help = "Run kanban board headless with API server and structured logging")]
    pub headless: Option<String>,
    #[arg()]
    pub prompts: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Settings {
    provider: Option<String>,
    model: Option<String>,
    system_prompt: Option<String>,
    max_turns: Option<usize>,
    auto_compact_threshold: Option<usize>,
}

impl Settings {
    fn merge(self, override_settings: Self) -> Self {
        Self {
            provider: override_settings.provider.or(self.provider),
            model: override_settings.model.or(self.model),
            system_prompt: override_settings.system_prompt.or(self.system_prompt),
            max_turns: override_settings.max_turns.or(self.max_turns),
            auto_compact_threshold: override_settings
                .auto_compact_threshold
                .or(self.auto_compact_threshold),
        }
    }
}

#[derive(Clone)]
struct Runtime {
    cwd: PathBuf,
    mu_home: PathBuf,
    settings: Settings,
    registry: ModelRegistry,
    provider: Arc<RouterProvider>,
    system_prompt: String,
}

impl Runtime {
    async fn build_agent(&self, model: ModelSpec, session_path: PathBuf) -> Result<Arc<Agent>> {
        let agent = Agent::new(AgentConfig {
            system_prompt: self.system_prompt.clone(),
            model,
            provider: self.provider.clone(),
            tools: default_tools(&self.cwd),
            working_directory: self.cwd.clone(),
            session_store: SessionStore::from_path(session_path),
            max_turns: self.settings.max_turns.unwrap_or(12),
            auto_compact_threshold: self.settings.auto_compact_threshold.unwrap_or(48),
        })
        .await?;
        Ok(Arc::new(agent))
    }

    fn resolve_model(
        &self,
        provider_override: Option<ProviderId>,
        model_override: Option<&str>,
    ) -> Result<ModelSpec> {
        let provider = if let Some(provider) = provider_override {
            provider
        } else if let Ok(value) = std::env::var("MU_PROVIDER") {
            ProviderId::from_str(&value)?
        } else if let Some(value) = &self.settings.provider {
            ProviderId::from_str(value)?
        } else {
            ProviderId::OpenAiCompatible
        };

        let requested_model = model_override
            .map(ToString::to_string)
            .or_else(|| std::env::var("MU_MODEL").ok())
            .or_else(|| self.settings.model.clone());

        if let Some(model) = requested_model {
            if let Some(found) = self.registry.find(&provider, &model) {
                return Ok(found);
            }
            let context_window = default_context_window(&provider);
            let max_output = default_max_output(&provider);
            return Ok(ModelSpec::new(
                provider,
                model.clone(),
                model,
                context_window,
                max_output,
            ));
        }

        self.registry
            .default_for(&provider)
            .ok_or_else(|| anyhow!("no default model found for provider {provider}"))
    }

    fn workspace_session_root(&self) -> PathBuf {
        self.mu_home
            .join("agent/sessions")
            .join(sanitize_path(&self.cwd))
    }

    fn new_session_path(&self) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs();
        self.workspace_session_root()
            .join(format!("{timestamp}.jsonl"))
    }

    fn list_sessions(&self) -> Result<Vec<PathBuf>> {
        list_session_files(&self.workspace_session_root()).map_err(Into::into)
    }

    fn latest_session(&self) -> Result<Option<PathBuf>> {
        let mut sessions = self.list_sessions()?;
        sessions.sort_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH)
        });
        Ok(sessions.pop())
    }

    fn resolve_session_selector(&self, selector: &str) -> Result<Option<PathBuf>> {
        let path = PathBuf::from(selector);
        if path.exists() {
            return Ok(Some(path));
        }

        let sessions = self.list_sessions()?;
        Ok(sessions.into_iter().find(|candidate| {
            candidate
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem == selector)
                .unwrap_or(false)
                || candidate.to_string_lossy().contains(selector)
        }))
    }
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    run_with_cli(cli).await
}

pub async fn run_with_cli(cli: Cli) -> Result<()> {
    let runtime = build_runtime()?;
    let mut current_model = runtime.resolve_model(None, None)?;

    if let Some(ref dir) = cli.headless {
        return run_headless(&runtime, current_model, dir).await;
    }

    if let Some(ref kanban_dir) = cli.kanban {
        return run_kanban_headless(&runtime, current_model, kanban_dir).await;
    }

    let session_path = resolve_initial_session_path(&runtime, &cli)?;
    let mut agent = runtime
        .build_agent(current_model.clone(), session_path.clone())
        .await?;

    if cli.json {
        run_json_mode(agent, cli.prompts).await?;
        return Ok(());
    }

    if cli.print || !cli.prompts.is_empty() {
        let text = run_print_mode(agent, cli.prompts).await?;
        println!("{text}");
        return Ok(());
    }

    if cli.resume {
        let sessions = runtime.list_sessions()?;
        if !sessions.is_empty() {
            let listed = sessions
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            let footer = FooterData {
                cwd: runtime.cwd.display().to_string(),
                session_name: session_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("current")
                    .to_string(),
                model: current_model.id.0.clone(),
                status: "idle".to_string(),
                queued_steering: 0,
                queued_follow_up: 0,
            };
            let mut app = App::new(footer);
            populate_app_from_agent(&mut app, &agent).await;
            app.open_overlay("Sessions", listed);
            run_interactive_loop(&runtime, &mut current_model, &mut agent, &mut app).await?;
            return Ok(());
        }
    }

    let footer = FooterData {
        cwd: runtime.cwd.display().to_string(),
        session_name: session_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("current")
            .to_string(),
        model: current_model.id.0.clone(),
        status: "idle".to_string(),
        queued_steering: 0,
        queued_follow_up: 0,
    };
    let mut app = App::new(footer);
    populate_app_from_agent(&mut app, &agent).await;
    run_interactive_loop(&runtime, &mut current_model, &mut agent, &mut app).await
}

async fn run_print_mode(agent: Arc<Agent>, prompts: Vec<String>) -> Result<String> {
    let response = if prompts.is_empty() {
        agent.continue_from_current().await?
    } else {
        agent.prompt(prompts.join("\n")).await?
    };
    Ok(response.plain_text())
}

async fn run_json_mode(agent: Arc<Agent>, prompts: Vec<String>) -> Result<()> {
    let mut receiver = agent.subscribe();
    let mut run_handle = spawn_agent_run(agent.clone(), prompts);

    loop {
        tokio::select! {
            message = receiver.recv() => {
                match message {
                    Ok(event) => {
                        println!("{}", serde_json::to_string(&event)?);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            result = &mut run_handle => {
                result.context("json run task panicked")??;
                while let Ok(event) = receiver.try_recv() {
                    println!("{}", serde_json::to_string(&event)?);
                }
                break;
            }
        }
    }

    Ok(())
}

async fn run_kanban_headless(runtime: &Runtime, model: ModelSpec, dir: &str) -> Result<()> {
    let kanban_root = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        runtime.cwd.join(dir)
    };

    let config_template = AgentConfig {
        system_prompt: String::new(),
        model,
        provider: runtime.provider.clone(),
        tools: Vec::new(),
        working_directory: PathBuf::new(),
        session_store: SessionStore::from_path(PathBuf::new()),
        max_turns: runtime.settings.max_turns.unwrap_or(12),
        auto_compact_threshold: runtime.settings.auto_compact_threshold.unwrap_or(48),
    };

    let (runner, mut event_rx, _event_tx, _command_tx) =
        KanbanRunner::new(kanban_root, config_template)?;
    let task = tokio::spawn(async move {
        let mut runner = runner;
        runner.run().await.map_err(|e| anyhow::anyhow!("{e}"))
    });

    // Pin the task so we can poll it in select!
    tokio::pin!(task);

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                match event {
                    Ok(ref ev) => {
                        println!("{}", serde_json::to_string(ev)?);
                        if let KanbanEvent::StatsUpdated(ref stats) = ev {
                            if stats.total_documents > 0
                                && stats.todo == 0
                                && stats.processing == 0
                                && stats.feedback == 0
                                && stats.refining == 0
                            {
                                task.abort();
                                if stats.errored > 0 {
                                    return Err(anyhow!(
                                        "kanban completed with {} errored task(s)",
                                        stats.errored
                                    ));
                                }
                                return Ok(());
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            result = &mut task => {
                match result {
                    Ok(Ok(())) => break,
                    Ok(Err(e)) => return Err(e),
                    Err(e) if e.is_cancelled() => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    Ok(())
}

// ── Headless mode ──────────────────────────────────────────────────────────

mod ansi {
    pub const RED: &str = "\x1b[31m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE: &str = "\x1b[34m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const CYAN: &str = "\x1b[36m";
    pub const DIM: &str = "\x1b[2m";
    pub const BOLD: &str = "\x1b[1m";
    pub const RESET: &str = "\x1b[0m";
}

/// Formats KanbanEvents as colored, greppable terminal log lines.
///
/// Designed for iterative debugging:
/// - Fixed-width event labels for easy grep (`DISCOVER`, `MOVE`, `ERROR`, …)
/// - Timestamps in local time for quick correlation
/// - Document names in quotes so you can `grep '"my-task"'`
/// - Errors print multi-line detail (doc_id, error message) for copy-paste
/// - Processing durations on completion events
struct HeadlessOutput {
    names: HashMap<String, String>,
    started_at: HashMap<String, Instant>,
    last_stats_line: String,
}

impl HeadlessOutput {
    fn new() -> Self {
        Self {
            names: HashMap::new(),
            started_at: HashMap::new(),
            last_stats_line: String::new(),
        }
    }

    fn doc_label(&self, id: &str) -> String {
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.chars().take(12).collect())
    }

    fn print_event(&mut self, event: &KanbanEvent) {
        use ansi::*;
        let ts = chrono::Local::now().format("%H:%M:%S");

        match event {
            KanbanEvent::DocumentDiscovered { id, name } => {
                self.names.insert(id.clone(), name.clone());
                eprintln!("{DIM}{ts}{RESET} {CYAN}DISCOVER{RESET} \"{name}\"");
            }
            KanbanEvent::StateChanged { id, from, to } => {
                let label = self.doc_label(id);
                let color = match to.as_str() {
                    "complete" => GREEN,
                    "error" => RED,
                    "processing" => BLUE,
                    "feedback" => YELLOW,
                    "todo" => DIM,
                    _ => RESET,
                };
                eprintln!(
                    "{DIM}{ts}{RESET} {YELLOW}MOVE    {RESET} \"{label}\": {from} {DIM}→{RESET} {color}{to}{RESET}"
                );
            }
            KanbanEvent::ProcessingStarted { id } => {
                let label = self.doc_label(id);
                self.started_at.insert(id.clone(), Instant::now());
                eprintln!("{DIM}{ts}{RESET} {BLUE}PROCESS {RESET} \"{label}\": started");
            }
            KanbanEvent::ProcessingComplete { id } => {
                let label = self.doc_label(id);
                let elapsed = self
                    .started_at
                    .remove(id)
                    .map(|t| {
                        let secs = t.elapsed().as_secs();
                        if secs >= 60 {
                            format!(" ({}m{}s)", secs / 60, secs % 60)
                        } else {
                            format!(" ({secs}s)")
                        }
                    })
                    .unwrap_or_default();
                eprintln!(
                    "{DIM}{ts}{RESET} {GREEN}DONE    {RESET} \"{label}\": completed{elapsed}"
                );
            }
            KanbanEvent::FeedbackRequested { id, question } => {
                let label = self.doc_label(id);
                eprintln!(
                    "{DIM}{ts}{RESET} {MAGENTA}FEEDBACK{RESET} \"{label}\": {question}"
                );
            }
            KanbanEvent::StatsUpdated(stats) => {
                let line = stats.status_line();
                if line != self.last_stats_line {
                    self.last_stats_line = line.clone();
                    eprintln!("{DIM}{ts} STATS    {line}{RESET}");
                }
            }
            KanbanEvent::Error { id, message } => {
                if let Some(doc_id) = id {
                    let label = self.doc_label(doc_id);
                    eprintln!(
                        "{DIM}{ts}{RESET} {RED}{BOLD}ERROR   {RESET} \"{label}\": {RED}{message}{RESET}"
                    );
                    eprintln!("                  {DIM}doc_id: {doc_id}{RESET}");
                } else {
                    eprintln!(
                        "{DIM}{ts}{RESET} {RED}{BOLD}ERROR   {RESET} {RED}{message}{RESET}"
                    );
                }
            }
            KanbanEvent::StatusResponse { documents } => {
                eprintln!(
                    "{DIM}{ts}{RESET} {DIM}STATUS  {RESET} {} document(s)",
                    documents.len()
                );
                for doc in documents {
                    let color = match doc.state.as_str() {
                        "complete" => GREEN,
                        "error" => RED,
                        "processing" => BLUE,
                        "feedback" => MAGENTA,
                        _ => DIM,
                    };
                    let elapsed = doc
                        .elapsed_secs
                        .map(|s| {
                            if s >= 60 {
                                format!(" ({}m{}s)", s / 60, s % 60)
                            } else {
                                format!(" ({s}s)")
                            }
                        })
                        .unwrap_or_default();
                    let err = doc
                        .error
                        .as_ref()
                        .map(|e| format!(" {DIM}— {e}{RESET}"))
                        .unwrap_or_default();
                    eprintln!(
                        "                  {color}{:12}{RESET} \"{}\"{elapsed}{err}",
                        doc.state, doc.name
                    );
                }
            }
        }
    }
}

async fn run_headless(runtime: &Runtime, model: ModelSpec, dir: &str) -> Result<()> {
    let kanban_root = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        runtime.cwd.join(dir)
    };

    // Headless kanban tasks need more turns than interactive mode since
    // implementation tasks routinely read, write, and edit multiple files.
    let max_turns = runtime.settings.max_turns.unwrap_or(50);

    let config_template = AgentConfig {
        system_prompt: String::new(),
        model: model.clone(),
        provider: runtime.provider.clone(),
        tools: Vec::new(),
        working_directory: PathBuf::new(),
        session_store: SessionStore::from_path(PathBuf::new()),
        max_turns,
        auto_compact_threshold: runtime.settings.auto_compact_threshold.unwrap_or(48),
    };

    let (runner, mut event_rx, event_tx, command_tx) =
        KanbanRunner::new(kanban_root.clone(), config_template)?;

    // Start the API server
    let addr: std::net::SocketAddr = "127.0.0.1:3141"
        .parse()
        .context("invalid bind address")?;

    let actual_addr = mu_kanban_ui::start_server(KanbanUiConfig {
        addr,
        kanban_root: kanban_root.clone(),
        event_tx: event_tx.clone(),
        command_tx,
    })
    .await?;

    // Startup banner
    eprintln!(
        "\n{dim}── mu headless ─────────────────────────────────────{reset}\n\
         {dim}   kanban:{reset}  {path}\n\
         {dim}   api:{reset}     http://{addr}\n\
         {dim}   model:{reset}   {model}\n\
         {dim}   logs:{reset}    {path}/logs/kanban.jsonl\n\
         {dim}───────────────────────────────────────────────────{reset}\n",
        dim = ansi::DIM,
        reset = ansi::RESET,
        path = kanban_root.display(),
        addr = actual_addr,
        model = model.id.0,
    );

    let mut output = HeadlessOutput::new();

    // Pre-populate doc names from existing state so events for
    // previously-discovered documents show names instead of truncated IDs.
    if let Ok(state) = KanbanState::load_or_create(kanban_root.clone()) {
        for (id, doc) in &state.documents {
            output.names.insert(id.clone(), doc.original_name.clone());
        }
    }

    let run_start = Instant::now();

    // Completion requires 3 consecutive stable stats to avoid exiting before
    // newly-created subtask files are discovered by the next scan cycle.
    let mut stable_complete_count: u32 = 0;
    let mut last_stable_total: usize = 0;

    let task = tokio::spawn(async move {
        let mut runner = runner;
        runner.run().await.map_err(|e| anyhow::anyhow!("{e}"))
    });
    tokio::pin!(task);

    // Ctrl+C handler for clean shutdown
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                let elapsed = run_start.elapsed().as_secs();
                eprintln!(
                    "\n{dim}── interrupted after {elapsed}s ──{reset}",
                    dim = ansi::DIM,
                    reset = ansi::RESET,
                );
                task.abort();
                break;
            }
            event = event_rx.recv() => {
                match event {
                    Ok(ref ev) => {
                        output.print_event(ev);
                        if let KanbanEvent::StatsUpdated(ref stats) = ev {
                            let looks_complete = stats.total_documents > 0
                                && stats.todo == 0
                                && stats.processing == 0
                                && stats.feedback == 0
                                && stats.refining == 0;

                            if looks_complete && stats.total_documents == last_stable_total {
                                stable_complete_count += 1;
                            } else if looks_complete {
                                // First time at this total, start counting
                                stable_complete_count = 1;
                            } else {
                                stable_complete_count = 0;
                            }
                            last_stable_total = stats.total_documents;

                            // Require 3 consecutive stable stats (~6s) to confirm
                            // no new subtasks are being created
                            if stable_complete_count >= 3 {
                                let elapsed = run_start.elapsed().as_secs();
                                let (color, label) = if stats.errored > 0 {
                                    (ansi::RED, "completed with errors")
                                } else {
                                    (ansi::GREEN, "all tasks completed")
                                };
                                eprintln!(
                                    "\n{color}── {label}: {done} done, {err} errored ({elapsed}s) ──{reset}",
                                    done = stats.complete,
                                    err = stats.errored,
                                    reset = ansi::RESET,
                                );
                                task.abort();
                                if stats.errored > 0 {
                                    return Err(anyhow!(
                                        "kanban completed with {} errored task(s)",
                                        stats.errored
                                    ));
                                }
                                return Ok(());
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            result = &mut task => {
                match result {
                    Ok(Ok(())) => break,
                    Ok(Err(e)) => return Err(e),
                    Err(e) if e.is_cancelled() => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    Ok(())
}

async fn run_interactive_loop(
    runtime: &Runtime,
    current_model: &mut ModelSpec,
    agent: &mut Arc<Agent>,
    app: &mut App,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let loop_result = interactive_loop(runtime, current_model, agent, app, &mut terminal).await;
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    loop_result
}

struct KanbanHandle {
    task: JoinHandle<Result<(), anyhow::Error>>,
    event_rx: tokio::sync::broadcast::Receiver<KanbanEvent>,
    event_tx: tokio::sync::broadcast::Sender<KanbanEvent>,
    command_tx: tokio::sync::mpsc::Sender<KanbanCommand>,
    #[allow(dead_code)]
    board_name: String,
    kanban_root: PathBuf,
}

struct KanbanUiHandle {
    #[allow(dead_code)]
    addr: std::net::SocketAddr,
    /// If the web UI auto-started the kanban runner, this holds the runner's handle.
    auto_started_runner: bool,
}

async fn interactive_loop(
    runtime: &Runtime,
    current_model: &mut ModelSpec,
    agent: &mut Arc<Agent>,
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let mut receiver = agent.subscribe();
    let mut running_prompt: Option<JoinHandle<Result<mu_ai::Message, anyhow::Error>>> = None;
    let mut kanban_handle: Option<KanbanHandle> = None;
    let mut kanban_ui_handle: Option<KanbanUiHandle> = None;

    loop {
        while let Ok(event) = receiver.try_recv() {
            app.apply_agent_event(&event);
        }

        // Drain kanban events
        {
            let finished = if let Some(kb) = &mut kanban_handle {
                while let Ok(event) = kb.event_rx.try_recv() {
                    apply_kanban_event(app, &event);
                }
                kb.task.is_finished()
            } else {
                false
            };
            if finished {
                if let Some(kb) = kanban_handle.take() {
                    let result = kb.task.await.context("kanban task panicked")?;
                    if let Err(err) = result {
                        app.push_message("system", format!("kanban stopped: {err}"));
                    }
                    app.footer.status = "idle".to_string();
                }
            }
        }

        terminal.draw(|frame| app.render(frame))?;

        if let Some(handle) = &mut running_prompt {
            if handle.is_finished() {
                let result = handle.await.context("prompt task panicked")?;
                result?;
                running_prompt = None;
            }
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if let Some(action) = app.handle_key(key) {
                    match action {
                        AppAction::None => {}
                        AppAction::Prompt(input) => {
                            if running_prompt.is_some() {
                                agent.queue_message(QueueMode::Steering, input).await;
                            } else {
                                running_prompt = Some(spawn_prompt(agent.clone(), input));
                            }
                        }
                        AppAction::Command(command) => {
                            if handle_command(
                                runtime,
                                current_model,
                                agent,
                                app,
                                command,
                                &mut receiver,
                                &mut kanban_handle,
                                &mut kanban_ui_handle,
                            )
                            .await?
                            {
                                break;
                            }
                        }
                        AppAction::OverlaySelection(selection) => {
                            handle_overlay_selection(runtime, current_model, agent, app, selection)
                                .await?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn apply_kanban_event(app: &mut App, event: &KanbanEvent) {
    match event {
        KanbanEvent::DocumentDiscovered { id, name } => {
            app.push_message("kanban", format!("new document: {name} ({id})"));
        }
        KanbanEvent::StateChanged { id, from, to } => {
            app.push_message("kanban", format!("{id}: {from} -> {to}"));
        }
        KanbanEvent::ProcessingStarted { id } => {
            app.push_message("kanban", format!("processing: {id}"));
            app.footer.status = format!("kanban: processing {}", &id[..8.min(id.len())]);
        }
        KanbanEvent::ProcessingComplete { id } => {
            app.push_message("kanban", format!("complete: {id}"));
        }
        KanbanEvent::FeedbackRequested { id, question } => {
            app.push_message("kanban", format!("feedback needed [{id}]: {question}"));
        }
        KanbanEvent::StatsUpdated(stats) => {
            app.footer.status = format!("kanban: {}", stats.status_line());
        }
        KanbanEvent::Error { id, message } => {
            let prefix = id
                .as_ref()
                .map(|i| format!("[{i}] "))
                .unwrap_or_default();
            app.push_message("kanban", format!("{prefix}error: {message}"));
        }
        KanbanEvent::StatusResponse { documents } => {
            let lines: Vec<String> = documents
                .iter()
                .map(|d| {
                    let elapsed = d
                        .elapsed_secs
                        .map(|s| {
                            if s >= 60 {
                                format!(" ({}m{}s)", s / 60, s % 60)
                            } else {
                                format!(" ({s}s)")
                            }
                        })
                        .unwrap_or_default();
                    let err = d
                        .error
                        .as_ref()
                        .map(|e| format!(" - {e}"))
                        .unwrap_or_default();
                    format!("[{}] {}{}{}", d.state, d.name, elapsed, err)
                })
                .collect();
            if lines.is_empty() {
                app.open_overlay("Kanban Status", vec!["No documents".to_string()]);
            } else {
                app.open_overlay("Kanban Status", lines);
            }
        }
    }
}

async fn handle_command(
    runtime: &Runtime,
    current_model: &mut ModelSpec,
    agent: &mut Arc<Agent>,
    app: &mut App,
    command: SlashCommand,
    receiver: &mut tokio::sync::broadcast::Receiver<mu_agent::AgentEvent>,
    kanban_handle: &mut Option<KanbanHandle>,
    kanban_ui_handle: &mut Option<KanbanUiHandle>,
) -> Result<bool> {
    match command {
        SlashCommand::Model(Some(model_id)) => {
            let provider = current_model.provider.clone();
            let resolved = runtime.resolve_model(Some(provider), Some(&model_id))?;
            agent.set_model(resolved.clone()).await;
            *current_model = resolved.clone();
            app.footer.model = resolved.id.0;
            app.footer.status = "model updated".to_string();
        }
        SlashCommand::Model(None) => {
            let (items, selected_value) = model_overlay_items(&runtime.registry, current_model);
            app.open_selectable_overlay(
                "Models",
                OverlayKind::ModelPicker,
                items,
                Some(selected_value.as_str()),
            );
        }
        SlashCommand::New => {
            let session_path = runtime.new_session_path();
            *agent = runtime
                .build_agent(current_model.clone(), session_path.clone())
                .await?;
            *receiver = agent.subscribe();
            app.messages.clear();
            app.footer.session_name = file_name(&session_path);
            app.footer.status = "new session".to_string();
        }
        SlashCommand::Resume(selector) => {
            if let Some(selector) = selector {
                let Some(session_path) = runtime.resolve_session_selector(&selector)? else {
                    app.open_overlay("Sessions", vec![format!("No session matched {selector}")]);
                    return Ok(false);
                };
                *agent = runtime
                    .build_agent(current_model.clone(), session_path.clone())
                    .await?;
                *receiver = agent.subscribe();
                app.messages.clear();
                populate_app_from_agent(app, agent).await;
                app.footer.session_name = file_name(&session_path);
                app.footer.status = "session resumed".to_string();
            } else {
                let sessions = runtime
                    .list_sessions()?
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>();
                app.open_overlay("Sessions", sessions);
            }
        }
        SlashCommand::Session => {
            app.open_overlay(
                "Session",
                vec![
                    format!("cwd: {}", runtime.cwd.display()),
                    format!("session: {}", agent.session_store().path().display()),
                ],
            );
        }
        SlashCommand::Tree(node) => {
            if let Some(node) = node {
                agent.branch_to(&node).await?;
                app.messages.clear();
                populate_app_from_agent(app, agent).await;
                app.footer.status = format!("branched to {node}");
            } else {
                let entries = agent
                    .session_tree()
                    .await?
                    .into_iter()
                    .map(|entry| {
                        format!(
                            "{} parent={} {}",
                            entry.id,
                            entry.parent_id.unwrap_or_else(|| "-".to_string()),
                            entry.message.plain_text()
                        )
                    })
                    .collect::<Vec<_>>();
                app.open_overlay("Tree", entries);
            }
        }
        SlashCommand::Compact(note) => {
            let summary = agent.compact(note).await?;
            if let Some(summary) = summary {
                app.open_overlay("Compaction", vec![summary]);
                app.messages.clear();
                populate_app_from_agent(app, agent).await;
            }
        }
        SlashCommand::Kanban(folder_arg) => {
            // Handle "/kanban stop" sub-command
            if folder_arg == "stop" {
                if let Some(kb) = kanban_handle.take() {
                    kb.task.abort();
                    app.push_message("kanban", "kanban mode stopped");
                    app.footer.status = "idle".to_string();
                } else {
                    app.push_message("system", "no kanban board is running");
                }
                return Ok(false);
            }

            // Handle "/kanban retry" sub-command
            if folder_arg == "retry" {
                if let Some(kb) = kanban_handle.as_ref() {
                    let _ = kb.command_tx.send(KanbanCommand::RetryAllErrored);
                    app.push_message("kanban", "retrying all errored items");
                } else {
                    app.push_message("system", "no kanban board is running");
                }
                return Ok(false);
            }

            // Handle "/kanban status" sub-command
            if folder_arg == "status" {
                if let Some(kb) = kanban_handle.as_ref() {
                    let _ = kb.command_tx.send(KanbanCommand::RequestStatus);
                } else {
                    app.push_message("system", "no kanban board is running");
                }
                return Ok(false);
            }

            // Check if already running
            if kanban_handle.is_some() {
                app.push_message(
                    "system",
                    "kanban board already running. Use /kanban stop first.",
                );
                return Ok(false);
            }

            let kanban_root = if Path::new(&folder_arg).is_absolute() {
                PathBuf::from(&folder_arg)
            } else {
                runtime.cwd.join(&folder_arg)
            };

            let config_template = AgentConfig {
                system_prompt: String::new(), // overridden per-document
                model: current_model.clone(),
                provider: runtime.provider.clone(),
                tools: Vec::new(), // overridden per-document
                working_directory: PathBuf::new(), // overridden per-document
                session_store: SessionStore::from_path(PathBuf::new()), // overridden
                max_turns: runtime.settings.max_turns.unwrap_or(12),
                auto_compact_threshold: runtime
                    .settings
                    .auto_compact_threshold
                    .unwrap_or(48),
            };

            match KanbanRunner::new(kanban_root.clone(), config_template) {
                Ok((runner, event_rx, event_tx, command_tx)) => {
                    let board_name = folder_arg.clone();
                    let kanban_root_clone = kanban_root.clone();
                    let task = tokio::spawn(async move {
                        let mut runner = runner;
                        runner.run().await.map_err(Into::into)
                    });

                    *kanban_handle = Some(KanbanHandle {
                        task,
                        event_rx,
                        event_tx,
                        command_tx,
                        board_name: board_name.clone(),
                        kanban_root: kanban_root_clone,
                    });

                    app.push_message(
                        "kanban",
                        format!("started board: {board_name} ({})", kanban_root.display()),
                    );
                    app.footer.status = format!("kanban: {board_name}");
                }
                Err(err) => {
                    app.push_message("system", format!("failed to start kanban: {err}"));
                }
            }
        }
        SlashCommand::KanbanUi(folder_arg) => {
            // Handle "/kanban-ui stop"
            if folder_arg == "stop" {
                if let Some(ui) = kanban_ui_handle.take() {
                    app.push_message("kanban", "kanban web UI stopped");
                    if ui.auto_started_runner {
                        if let Some(kb) = kanban_handle.take() {
                            kb.task.abort();
                            app.push_message("kanban", "kanban runner stopped");
                        }
                    }
                    app.footer.status = "idle".to_string();
                } else {
                    app.push_message("system", "no kanban UI is running");
                }
                return Ok(false);
            }

            // Check if UI already running
            if kanban_ui_handle.is_some() {
                app.push_message(
                    "system",
                    "kanban UI already running. Use /kanban-ui stop first.",
                );
                return Ok(false);
            }

            let kanban_root = if Path::new(&folder_arg).is_absolute() {
                PathBuf::from(&folder_arg)
            } else {
                runtime.cwd.join(&folder_arg)
            };

            // Auto-start kanban runner if none is active
            let auto_started = if kanban_handle.is_none() {
                let config_template = AgentConfig {
                    system_prompt: String::new(),
                    model: current_model.clone(),
                    provider: runtime.provider.clone(),
                    tools: Vec::new(),
                    working_directory: PathBuf::new(),
                    session_store: SessionStore::from_path(PathBuf::new()),
                    max_turns: runtime.settings.max_turns.unwrap_or(12),
                    auto_compact_threshold: runtime
                        .settings
                        .auto_compact_threshold
                        .unwrap_or(48),
                };

                match KanbanRunner::new(kanban_root.clone(), config_template) {
                    Ok((runner, event_rx, event_tx, command_tx)) => {
                        let board_name = folder_arg.clone();
                        let kanban_root_clone = kanban_root.clone();
                        let task = tokio::spawn(async move {
                            let mut runner = runner;
                            runner.run().await.map_err(Into::into)
                        });
                        *kanban_handle = Some(KanbanHandle {
                            task,
                            event_rx,
                            event_tx,
                            command_tx,
                            board_name: board_name.clone(),
                            kanban_root: kanban_root_clone,
                        });
                        app.push_message(
                            "kanban",
                            format!("auto-started runner: {board_name}"),
                        );
                        true
                    }
                    Err(err) => {
                        app.push_message(
                            "system",
                            format!("failed to start kanban runner: {err}"),
                        );
                        return Ok(false);
                    }
                }
            } else {
                false
            };

            // Start web server using the runner's event/command channels
            if let Some(kb) = kanban_handle.as_ref() {
                let addr: std::net::SocketAddr = ([127, 0, 0, 1], 3141).into();
                let ui_config = KanbanUiConfig {
                    addr,
                    kanban_root: kb.kanban_root.clone(),
                    event_tx: kb.event_tx.clone(),
                    command_tx: kb.command_tx.clone(),
                };

                match mu_kanban_ui::start_server(ui_config).await {
                    Ok(actual_addr) => {
                        let url = format!("http://{actual_addr}");
                        *kanban_ui_handle = Some(KanbanUiHandle {
                            addr: actual_addr,
                            auto_started_runner: auto_started,
                        });
                        app.push_message(
                            "kanban",
                            format!("web UI started at {url}"),
                        );
                        app.footer.status = format!("kanban-ui: {url}");

                        // Auto-open browser
                        #[cfg(target_os = "macos")]
                        {
                            let _ = std::process::Command::new("open")
                                .arg(&url)
                                .spawn();
                        }
                        #[cfg(target_os = "linux")]
                        {
                            let _ = std::process::Command::new("xdg-open")
                                .arg(&url)
                                .spawn();
                        }
                    }
                    Err(err) => {
                        app.push_message(
                            "system",
                            format!("failed to start kanban UI: {err}"),
                        );
                        if auto_started {
                            if let Some(kb) = kanban_handle.take() {
                                kb.task.abort();
                            }
                        }
                    }
                }
            }
        }
        SlashCommand::Quit => return Ok(true),
        SlashCommand::Unknown(command) => {
            app.open_overlay("Unknown command", vec![command]);
        }
    }

    Ok(false)
}

async fn handle_overlay_selection(
    runtime: &Runtime,
    current_model: &mut ModelSpec,
    agent: &mut Arc<Agent>,
    app: &mut App,
    selection: OverlaySelection,
) -> Result<()> {
    match selection.kind {
        OverlayKind::Info => {}
        OverlayKind::ModelPicker => {
            let (provider, model_id) = decode_model_selection(&selection.item.value)?;
            let resolved = runtime.resolve_model(Some(provider), Some(&model_id))?;
            agent.set_model(resolved.clone()).await;
            *current_model = resolved.clone();
            app.footer.model = resolved.id.0;
            app.footer.status = "model updated".to_string();
        }
    }

    Ok(())
}

fn spawn_prompt(
    agent: Arc<Agent>,
    input: String,
) -> JoinHandle<Result<mu_ai::Message, anyhow::Error>> {
    tokio::spawn(async move { agent.prompt(input).await.map_err(Into::into) })
}

fn spawn_agent_run(
    agent: Arc<Agent>,
    prompts: Vec<String>,
) -> JoinHandle<Result<mu_ai::Message, anyhow::Error>> {
    tokio::spawn(async move {
        if prompts.is_empty() {
            agent.continue_from_current().await.map_err(Into::into)
        } else {
            agent.prompt(prompts.join("\n")).await.map_err(Into::into)
        }
    })
}

async fn populate_app_from_agent(app: &mut App, agent: &Arc<Agent>) {
    let state = agent.state().await;
    for message in state.messages {
        if !message.plain_text().is_empty() {
            app.push_message(role_name(&message), message.plain_text());
        }
        for part in message.content {
            if let mu_ai::ContentPart::ToolCall(call) = part {
                app.push_message(
                    "assistant",
                    format!("tool call {} {}", call.name, call.arguments),
                );
            }
        }
    }
}

fn role_name(message: &mu_ai::Message) -> &'static str {
    match message.role {
        mu_ai::Role::System => "system",
        mu_ai::Role::User => "user",
        mu_ai::Role::Assistant => "assistant",
        mu_ai::Role::Tool => "tool",
    }
}

fn build_runtime() -> Result<Runtime> {
    let cwd = std::env::current_dir().context("failed to get current directory")?;
    let mu_home = mu_home_path()?;
    let settings = load_settings(&cwd, &mu_home)?;
    let custom_models = load_custom_models(&mu_home.join("agent/models.toml"))?;
    let registry = ModelRegistry::new(custom_models);
    let instruction_files = load_instruction_files(&cwd, None)?;
    let instruction_text = render_instruction_text(&instruction_files);
    let base_prompt = settings
        .system_prompt
        .clone()
        .unwrap_or_else(|| "You are Mu, a pragmatic Rust coding agent.".to_string());
    let system_prompt = if instruction_text.trim().is_empty() {
        base_prompt
    } else {
        format!("{base_prompt}\n\nProject instructions:\n{instruction_text}")
    };
    Ok(Runtime {
        cwd,
        mu_home,
        settings,
        registry,
        provider: Arc::new(RouterProvider::default()),
        system_prompt,
    })
}

fn resolve_initial_session_path(runtime: &Runtime, cli: &Cli) -> Result<PathBuf> {
    if cli.no_session {
        return Ok(std::env::temp_dir().join(format!("mu-ephemeral-{}.jsonl", process_nonce())));
    }

    if let Some(selector) = &cli.session {
        let session = runtime
            .resolve_session_selector(selector)?
            .unwrap_or_else(|| PathBuf::from(selector));
        return Ok(session);
    }

    if cli.continue_most_recent || cli.resume {
        if let Some(session) = runtime.latest_session()? {
            return Ok(session);
        }
    }

    Ok(runtime.new_session_path())
}

fn mu_home_path() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("MU_HOME") {
        return Ok(PathBuf::from(value));
    }
    let Some(home) = dirs::home_dir() else {
        return Err(anyhow!("failed to determine home directory"));
    };
    Ok(home.join(".mu"))
}

fn load_settings(cwd: &Path, mu_home: &Path) -> Result<Settings> {
    let global = load_settings_file(&mu_home.join("agent/settings.toml"))?;
    let local = load_settings_file(&cwd.join(".mu/settings.toml"))?;
    Ok(global.merge(local))
}

fn load_settings_file(path: &Path) -> Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read settings file {}", path.display()))?;
    toml::from_str(&raw)
        .with_context(|| format!("failed to parse settings file {}", path.display()))
}

fn sanitize_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | ' ' => '_',
            other => other,
        })
        .collect()
}

fn process_nonce() -> String {
    let pid = std::process::id();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos();
    format!("{pid}-{now}")
}

fn default_context_window(provider: &ProviderId) -> u32 {
    match provider {
        ProviderId::OpenAiCompatible => 1_000_000,
        ProviderId::Anthropic => 200_000,
    }
}

fn default_max_output(provider: &ProviderId) -> u32 {
    match provider {
        ProviderId::OpenAiCompatible => 100_000,
        ProviderId::Anthropic => 8_192,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("session")
        .to_string()
}

fn model_overlay_items(
    registry: &ModelRegistry,
    current_model: &ModelSpec,
) -> (Vec<OverlayItem>, String) {
    let selected_value = encode_model_selection(current_model);
    let items = registry
        .list()
        .into_iter()
        .map(|model| {
            OverlayItem::new(
                format!("{} ({})", model.id.0, model.provider),
                encode_model_selection(&model),
            )
        })
        .collect::<Vec<_>>();
    (items, selected_value)
}

fn encode_model_selection(model: &ModelSpec) -> String {
    format!("{}\t{}", model.provider, model.id.0)
}

fn decode_model_selection(value: &str) -> Result<(ProviderId, String)> {
    let Some((provider, model_id)) = value.split_once('\t') else {
        return Err(anyhow!("invalid model selection payload"));
    };
    Ok((ProviderId::from_str(provider)?, model_id.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mu_ai::{ModelRegistry, ModelSpec, ProviderId};
    use tempfile::TempDir;

    use super::{encode_model_selection, load_settings, model_overlay_items, sanitize_path};

    #[test]
    fn sanitizes_workspace_paths_for_session_dirs() {
        assert_eq!(
            sanitize_path(Path::new("/tmp/my project")),
            "_tmp_my_project"
        );
    }

    #[test]
    fn merges_global_and_local_settings() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let home = tempdir.path().join("home");
        let cwd = tempdir.path().join("repo");
        if let Err(error) = std::fs::create_dir_all(home.join("agent")) {
            panic!("home should exist: {error}");
        }
        if let Err(error) = std::fs::create_dir_all(cwd.join(".mu")) {
            panic!("cwd should exist: {error}");
        }
        if let Err(error) = std::fs::write(
            home.join("agent/settings.toml"),
            "model = \"global\"\nprovider = \"openai-compatible\"\n",
        ) {
            panic!("write should succeed: {error}");
        }
        if let Err(error) = std::fs::write(cwd.join(".mu/settings.toml"), "model = \"local\"\n") {
            panic!("write should succeed: {error}");
        }

        let settings = match load_settings(&cwd, &home) {
            Ok(value) => value,
            Err(error) => panic!("settings should load: {error}"),
        };
        assert_eq!(settings.model, Some("local".to_string()));
        assert_eq!(settings.provider, Some("openai-compatible".to_string()));
    }

    #[test]
    fn model_overlay_marks_current_model() {
        let registry = ModelRegistry::new(Vec::new());
        let current_model =
            ModelSpec::new(ProviderId::OpenAiCompatible, "gpt-5.4", "GPT-5.4", 400_000, 100_000);

        let (items, selected_value) = model_overlay_items(&registry, &current_model);

        assert!(items.iter().any(|item| item.value == selected_value));
        assert_eq!(selected_value, encode_model_selection(&current_model));
    }
}
