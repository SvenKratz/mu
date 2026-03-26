#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mu_agent::AgentEvent;
use mu_ai::{ContentPart, Message};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FooterData {
    pub cwd: String,
    pub session_name: String,
    pub model: String,
    pub status: String,
    pub queued_steering: usize,
    pub queued_follow_up: usize,
}

impl FooterData {
    pub fn render_text(&self) -> String {
        format!(
            "{} | session={} | model={} | status={} | queued={}/{}",
            self.cwd,
            self.session_name,
            self.model,
            self.status,
            self.queued_steering,
            self.queued_follow_up
        )
    }

    pub fn render_line(&self) -> Line<'_> {
        let status_color = match self.status.as_str() {
            "idle" => Color::Green,
            "streaming" | "running" => Color::Yellow,
            _ if self.status.starts_with("kanban") => Color::Magenta,
            _ => Color::White,
        };
        Line::from(vec![
            Span::styled(" model:", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} ", self.model),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("session:", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} ", self.session_name),
                Style::default().fg(Color::White),
            ),
            Span::styled("status:", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} ", self.status),
                Style::default().fg(status_color),
            ),
            Span::styled("queue:", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}/{}", self.queued_steering, self.queued_follow_up),
                Style::default().fg(Color::White),
            ),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayKind {
    Info,
    ModelPicker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayItem {
    pub label: String,
    pub value: String,
}

impl OverlayItem {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlaySelection {
    pub kind: OverlayKind,
    pub item: OverlayItem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayState {
    pub title: String,
    pub kind: OverlayKind,
    pub items: Vec<OverlayItem>,
    pub focused: bool,
    pub hidden: bool,
    selected_index: Option<usize>,
}

impl OverlayState {
    pub fn new(title: impl Into<String>, items: Vec<String>) -> Self {
        Self {
            title: title.into(),
            kind: OverlayKind::Info,
            items: items
                .into_iter()
                .map(|item| OverlayItem::new(item.clone(), item))
                .collect(),
            focused: true,
            hidden: false,
            selected_index: None,
        }
    }

    pub fn selectable(
        title: impl Into<String>,
        kind: OverlayKind,
        items: Vec<OverlayItem>,
        selected_value: Option<&str>,
    ) -> Self {
        let selected_index = if items.is_empty() {
            None
        } else {
            selected_value
                .and_then(|value| items.iter().position(|item| item.value == value))
                .or(Some(0))
        };

        Self {
            title: title.into(),
            kind,
            items,
            focused: true,
            hidden: false,
            selected_index,
        }
    }

    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }

    pub fn focus(&mut self) {
        self.focused = true;
    }

    pub fn unfocus(&mut self) {
        self.focused = false;
    }

    pub fn is_visible(&self) -> bool {
        !self.hidden
    }

    pub fn is_selectable(&self) -> bool {
        self.selected_index.is_some()
    }

    pub fn selected_item(&self) -> Option<&OverlayItem> {
        self.selected_index.and_then(|index| self.items.get(index))
    }

    pub fn select_next(&mut self) {
        let Some(index) = self.selected_index else {
            return;
        };
        if self.items.is_empty() {
            return;
        }
        self.selected_index = Some((index + 1) % self.items.len());
    }

    pub fn select_previous(&mut self) {
        let Some(index) = self.selected_index else {
            return;
        };
        if self.items.is_empty() {
            return;
        }
        self.selected_index = Some(if index == 0 {
            self.items.len() - 1
        } else {
            index - 1
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedMessage {
    pub role: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
}

impl RenderedMessage {
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            text: text.into(),
            timestamp: Utc::now(),
        }
    }
}

const SLASH_COMMANDS: &[&str] = &[
    "compact", "exit", "kanban", "kui", "model", "new", "quit", "resume", "session", "tree",
];

const SLASH_SUBCOMMANDS: &[&str] = &["kanban retry", "kanban status", "kanban stop"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommand {
    Model(Option<String>),
    New,
    Resume(Option<String>),
    Session,
    Tree(Option<String>),
    Compact(Option<String>),
    Kanban(String),
    KanbanUi(String),
    Quit,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    None,
    Prompt(String),
    Command(SlashCommand),
    OverlaySelection(OverlaySelection),
}

pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let mut parts = trimmed[1..].splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or_default();
    let remainder = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    Some(match command {
        "model" => SlashCommand::Model(remainder.map(ToString::to_string)),
        "new" => SlashCommand::New,
        "resume" => SlashCommand::Resume(remainder.map(ToString::to_string)),
        "session" => SlashCommand::Session,
        "tree" => SlashCommand::Tree(remainder.map(ToString::to_string)),
        "compact" => SlashCommand::Compact(remainder.map(ToString::to_string)),
        "kanban" => SlashCommand::Kanban(
            remainder
                .map(ToString::to_string)
                .unwrap_or_else(|| "kanban-board".to_string()),
        ),
        "kui" | "kanban-ui" => SlashCommand::KanbanUi(
            remainder
                .map(ToString::to_string)
                .unwrap_or_else(|| "kanban-board".to_string()),
        ),
        "quit" | "exit" => SlashCommand::Quit,
        _ => SlashCommand::Unknown(trimmed.to_string()),
    })
}

pub struct App {
    pub messages: Vec<RenderedMessage>,
    pub input: String,
    pub footer: FooterData,
    pub overlay: Option<OverlayState>,
    streaming_assistant_index: Option<usize>,
}

impl App {
    pub fn new(footer: FooterData) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            footer,
            overlay: None,
            streaming_assistant_index: None,
        }
    }

    pub fn push_message(&mut self, role: impl Into<String>, text: impl Into<String>) {
        self.messages.push(RenderedMessage::new(role, text));
    }

    pub fn open_overlay(&mut self, title: impl Into<String>, items: Vec<String>) {
        self.overlay = Some(OverlayState::new(title, items));
    }

    pub fn open_selectable_overlay(
        &mut self,
        title: impl Into<String>,
        kind: OverlayKind,
        items: Vec<OverlayItem>,
        selected_value: Option<&str>,
    ) {
        self.overlay = Some(OverlayState::selectable(title, kind, items, selected_value));
    }

    pub fn close_overlay(&mut self) {
        self.overlay = None;
    }

    fn slash_suggestion(&self) -> Option<&'static str> {
        let partial = self.input.strip_prefix('/')?;
        if partial.is_empty() {
            return None;
        }
        if partial.contains(char::is_whitespace) {
            // After a space, match sub-commands (e.g. "kanban r" → "kanban retry")
            SLASH_SUBCOMMANDS
                .iter()
                .find(|cmd| cmd.starts_with(partial) && cmd.len() > partial.len())
                .copied()
        } else {
            SLASH_COMMANDS
                .iter()
                .find(|cmd| cmd.starts_with(partial) && cmd.len() > partial.len())
                .copied()
        }
    }

    pub fn submit(&mut self) -> AppAction {
        let input = self.input.trim().to_string();
        self.input.clear();
        if input.is_empty() {
            return AppAction::None;
        }
        if let Some(command) = parse_slash_command(&input) {
            return AppAction::Command(command);
        }
        AppAction::Prompt(input)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<AppAction> {
        if matches!(key.code, KeyCode::Char('q')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(AppAction::Command(SlashCommand::Quit));
        }

        if let Some(overlay) = &mut self.overlay {
            if overlay.is_visible() {
                match key.code {
                    KeyCode::Esc => {
                        self.close_overlay();
                        return Some(AppAction::None);
                    }
                    KeyCode::Up | KeyCode::Char('k') if overlay.is_selectable() => {
                        overlay.select_previous();
                        return None;
                    }
                    KeyCode::Down | KeyCode::Char('j') if overlay.is_selectable() => {
                        overlay.select_next();
                        return None;
                    }
                    KeyCode::Enter if overlay.is_selectable() => {
                        let selection =
                            overlay
                                .selected_item()
                                .cloned()
                                .map(|item| OverlaySelection {
                                    kind: overlay.kind.clone(),
                                    item,
                                });
                        self.close_overlay();
                        return selection.map(AppAction::OverlaySelection);
                    }
                    _ => {}
                }
                return None;
            }
        }

        match key.code {
            KeyCode::Tab | KeyCode::Right => {
                if let Some(cmd) = self.slash_suggestion() {
                    self.input = format!("/{cmd} ");
                }
                None
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(character);
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Enter => Some(self.submit()),
            KeyCode::Esc => {
                self.input.clear();
                Some(AppAction::None)
            }
            _ => None,
        }
    }

    pub fn apply_agent_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta { delta } => {
                if let Some(index) = self.streaming_assistant_index {
                    if let Some(message) = self.messages.get_mut(index) {
                        message.text.push_str(delta);
                    }
                } else {
                    self.messages
                        .push(RenderedMessage::new("assistant", delta.clone()));
                    self.streaming_assistant_index = self.messages.len().checked_sub(1);
                }
                self.footer.status = "streaming".to_string();
            }
            AgentEvent::ToolCall { call } => {
                self.messages.push(RenderedMessage::new(
                    "assistant",
                    format!("tool call {} {}", call.name, call.arguments),
                ));
                self.streaming_assistant_index = None;
            }
            AgentEvent::ToolResult {
                tool_name,
                result,
                is_error,
                ..
            } => {
                let prefix = if *is_error {
                    "tool error"
                } else {
                    "tool result"
                };
                self.messages.push(RenderedMessage::new(
                    "tool",
                    format!("{prefix} {tool_name}: {result}"),
                ));
                self.footer.status = "tool finished".to_string();
            }
            AgentEvent::MessageComplete { message } => {
                self.merge_completed_message(message);
                self.streaming_assistant_index = None;
                self.footer.status = "idle".to_string();
            }
            AgentEvent::QueueUpdated {
                steering,
                follow_up,
            } => {
                self.footer.queued_steering = *steering;
                self.footer.queued_follow_up = *follow_up;
            }
            AgentEvent::Compaction { .. } => {
                self.footer.status = "compacted".to_string();
            }
            AgentEvent::AgentStart { .. } => {
                self.footer.status = "running".to_string();
            }
            AgentEvent::AgentEnd { .. } => {
                self.footer.status = "idle".to_string();
            }
            AgentEvent::TurnStart { .. }
            | AgentEvent::ToolCallDelta { .. }
            | AgentEvent::Usage { .. }
            | AgentEvent::TurnEnd { .. } => {}
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());

        let messages = self
            .messages
            .iter()
            .map(|message| {
                let role_color = match message.role.as_str() {
                    "user" => Color::Green,
                    "assistant" => Color::Blue,
                    "tool" => Color::Yellow,
                    "system" | "kanban" => Color::Magenta,
                    _ => Color::Cyan,
                };
                let text_style = if message.role == "tool"
                    && message.text.starts_with("tool error")
                {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("[{}] ", message.role),
                        Style::default()
                            .fg(role_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(message.text.clone(), text_style),
                ]))
            })
            .collect::<Vec<_>>();
        let list = List::new(messages).block(
            Block::default()
                .title(Span::styled(
                    " Messages ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(list, layout[0]);

        // Input prompt with cwd prefix
        let prompt = format!("{} > ", self.footer.cwd);
        let mut input_spans = vec![
            Span::styled(
                prompt,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(self.input.clone()),
        ];
        if let Some(cmd) = self.slash_suggestion() {
            let suffix = &cmd[self.input.len() - 1..]; // skip the leading '/'
            input_spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
        }
        let input_line = Line::from(input_spans);
        let input = Paragraph::new(input_line)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(input, layout[1]);

        let footer = Paragraph::new(self.footer.render_line())
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        frame.render_widget(footer, layout[2]);

        if let Some(overlay) = &self.overlay {
            if overlay.is_visible() {
                let area = centered_rect(70, 60, frame.area());
                frame.render_widget(Clear, area);
                let block = Block::default()
                    .title(overlay.title.clone())
                    .borders(Borders::ALL)
                    .border_style(if overlay.focused {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    });
                if overlay.is_selectable() {
                    let items = overlay
                        .items
                        .iter()
                        .map(|item| ListItem::new(item.label.clone()))
                        .collect::<Vec<_>>();
                    let list = List::new(items)
                        .block(block)
                        .highlight_style(
                            Style::default()
                                .bg(Color::Yellow)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol("> ");
                    let mut state = ListState::default();
                    state.select(overlay.selected_index);
                    frame.render_stateful_widget(list, area, &mut state);
                } else {
                    let content = Text::from(
                        overlay
                            .items
                            .iter()
                            .map(|item| Line::from(item.label.clone()))
                            .collect::<Vec<_>>(),
                    );
                    let widget = Paragraph::new(content)
                        .block(block)
                        .wrap(Wrap { trim: false });
                    frame.render_widget(widget, area);
                }
            }
        }
    }

    fn merge_completed_message(&mut self, message: &Message) {
        let plain_text = message.plain_text();
        if !plain_text.is_empty() {
            if let Some(index) = self.streaming_assistant_index {
                if let Some(rendered) = self.messages.get_mut(index) {
                    rendered.text = plain_text.clone();
                    rendered.role = role_label(message).to_string();
                }
            } else {
                self.messages
                    .push(RenderedMessage::new(role_label(message), plain_text));
            }
        }

        for part in &message.content {
            if let ContentPart::ToolCall(call) = part {
                self.messages.push(RenderedMessage::new(
                    "assistant",
                    format!("tool call {} {}", call.name, call.arguments),
                ));
            }
        }
    }
}

fn role_label(message: &Message) -> &'static str {
    match message.role {
        mu_ai::Role::System => "system",
        mu_ai::Role::User => "user",
        mu_ai::Role::Assistant => "assistant",
        mu_ai::Role::Tool => "tool",
    }
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100u16.saturating_sub(height_percent)) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100u16.saturating_sub(height_percent)) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100u16.saturating_sub(width_percent)) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100u16.saturating_sub(width_percent)) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        parse_slash_command, App, AppAction, FooterData, OverlayItem, OverlayKind,
        OverlaySelection, OverlayState, SlashCommand,
    };

    fn app() -> App {
        App::new(FooterData {
            cwd: "/tmp/project".to_string(),
            session_name: "current".to_string(),
            model: "gpt-4o-mini".to_string(),
            status: "idle".to_string(),
            queued_steering: 0,
            queued_follow_up: 0,
        })
    }

    #[test]
    fn editor_handles_input_and_submit() {
        let mut app = app();
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, Some(AppAction::Prompt("hi".to_string())));
        assert!(app.input.is_empty());
    }

    #[test]
    fn routes_slash_commands() {
        assert_eq!(
            parse_slash_command("/compact summarize"),
            Some(SlashCommand::Compact(Some("summarize".to_string())))
        );
        assert_eq!(parse_slash_command("/quit"), Some(SlashCommand::Quit));
    }

    #[test]
    fn renders_footer_text() {
        let app = app();
        assert!(app.footer.render_text().contains("session=current"));
        assert!(app.footer.render_text().contains("model=gpt-4o-mini"));
    }

    #[test]
    fn overlay_focus_and_dismissal_work() {
        let mut overlay = OverlayState::new("Models", vec!["one".to_string()]);
        assert!(overlay.is_visible());
        overlay.unfocus();
        assert!(!overlay.focused);
        overlay.set_hidden(true);
        assert!(!overlay.is_visible());

        let mut app = app();
        app.open_overlay("Models", vec!["one".to_string()]);
        let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, Some(AppAction::None));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn selectable_overlay_highlights_and_selects_items() {
        let mut app = app();
        app.open_selectable_overlay(
            "Models",
            OverlayKind::ModelPicker,
            vec![
                OverlayItem::new(
                    "gpt-4o-mini (openai-compatible)",
                    "openai-compatible\tgpt-4o-mini",
                ),
                OverlayItem::new("gpt-5.4 (openai-compatible)", "openai-compatible\tgpt-5.4"),
            ],
            Some("openai-compatible\tgpt-4o-mini"),
        );

        assert!(matches!(
            app.overlay.as_ref().and_then(OverlayState::selected_item),
            Some(item) if item.value == "openai-compatible\tgpt-4o-mini"
        ));

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            action,
            Some(AppAction::OverlaySelection(OverlaySelection {
                kind: OverlayKind::ModelPicker,
                item: OverlayItem::new("gpt-5.4 (openai-compatible)", "openai-compatible\tgpt-5.4",),
            }))
        );
        assert!(app.overlay.is_none());
    }

    #[test]
    fn slash_suggestion_matches_prefix() {
        let mut app = app();
        app.input = "/mo".to_string();
        assert_eq!(app.slash_suggestion(), Some("model"));
    }

    #[test]
    fn slash_suggestion_none_for_exact_match() {
        let mut app = app();
        app.input = "/quit".to_string();
        assert_eq!(app.slash_suggestion(), None);
    }

    #[test]
    fn slash_suggestion_none_for_non_slash() {
        let mut app = app();
        app.input = "hello".to_string();
        assert_eq!(app.slash_suggestion(), None);
    }

    #[test]
    fn tab_completes_slash_command() {
        let mut app = app();
        app.input = "/mo".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "/model ");
    }

    #[test]
    fn right_arrow_completes_slash_command() {
        let mut app = app();
        app.input = "/re".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.input, "/resume ");
    }

    #[test]
    fn tab_noop_without_suggestion() {
        let mut app = app();
        app.input = "hello".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn slash_suggestion_picks_first_alphabetical_match() {
        let mut app = app();
        app.input = "/k".to_string();
        // "kanban" comes before "kui" alphabetically in the sorted list
        assert_eq!(app.slash_suggestion(), Some("kanban"));
    }

    #[test]
    fn slash_suggestion_subcommand_after_space() {
        let mut app = app();
        app.input = "/kanban r".to_string();
        assert_eq!(app.slash_suggestion(), Some("kanban retry"));
    }

    #[test]
    fn tab_completes_subcommand() {
        let mut app = app();
        app.input = "/kanban st".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input, "/kanban status ");
    }

    #[test]
    fn no_suggestion_for_arbitrary_arg() {
        let mut app = app();
        app.input = "/kanban my-board".to_string();
        assert_eq!(app.slash_suggestion(), None);
    }

    #[test]
    fn ctrl_q_maps_to_quit_even_with_overlay_open() {
        let mut app = app();
        app.open_overlay("Models", vec!["one".to_string()]);

        let action = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

        assert_eq!(action, Some(AppAction::Command(SlashCommand::Quit)));
        assert!(app.overlay.is_some());
    }
}
