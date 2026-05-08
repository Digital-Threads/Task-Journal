//! Main TUI application — manages screens and terminal lifecycle.
//!
//! Default mode (`task-journal ui`): browses tasks of the current
//! project from SQLite — open ones first, then closed. Enter renders a
//! task's compact resume-pack.
//!
//! Legacy mode (`task-journal ui --chats`): the older session-browser
//! over `~/.claude/projects/*.jsonl`. Useful for spelunking raw chat
//! history; default mode is the right answer 95% of the time.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::Path;
use tj_core::session::{discovery, parser};

use super::chat_view::ChatView;
use super::session_list::SessionList;
use super::task_detail::TaskDetail;
use super::task_list::TaskList;

pub enum Screen {
    /// Default: browse task-journal tasks for the current project.
    TaskList,
    /// Render a task's compact resume-pack.
    TaskDetail,
    /// Legacy: browse Claude Code chat sessions.
    SessionList,
    /// Legacy: read a chat session message-by-message.
    Chat,
}

pub struct App {
    pub screen: Screen,
    pub task_list: Option<TaskList>,
    pub task_detail: Option<TaskDetail>,
    pub session_list: Option<SessionList>,
    pub chat_view: Option<ChatView>,
    pub should_quit: bool,
}

impl App {
    /// Build the default task-oriented App from current cwd /
    /// project_path. Opens the SQLite for this project_hash and
    /// loads the task list.
    pub fn new(project_path: &Path) -> Result<Self> {
        let project_hash = tj_core::project_hash::from_path(project_path)?;
        let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
        let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));

        let conn = tj_core::db::open(&state_path)?;
        if events_path.exists() {
            tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
        }
        let tasks = tj_core::db::list_tasks_by_project(&conn, &project_hash)?;

        let project_str = project_path.to_string_lossy().into_owned();

        Ok(App {
            screen: Screen::TaskList,
            task_list: Some(TaskList::new(tasks, project_str)),
            task_detail: None,
            session_list: None,
            chat_view: None,
            should_quit: false,
        })
    }

    /// Legacy chat-session browser. Same behavior as pre-v0.3 default.
    pub fn new_chats(project_path: &Path) -> Result<Self> {
        let proj_dir = discovery::find_project_dir(project_path)?;
        let sessions = match proj_dir {
            Some(ref d) => discovery::list_sessions(d)?,
            None => vec![],
        };

        // Filter classifier sessions (one per ingested event in v0.2.9+).
        const CLASSIFIER_PROMPT_PREFIX: &str =
            "You classify chat chunks for an AI-coding-agent task journal";
        let mut items = Vec::new();
        for path in &sessions {
            match parser::parse_session(path) {
                Ok(parsed) => {
                    if parsed
                        .first_user_text()
                        .map(|t| t.trim_start().starts_with(CLASSIFIER_PROMPT_PREFIX))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    items.push(parsed);
                }
                Err(_) => continue,
            }
        }

        let project_str = project_path.to_string_lossy().into_owned();

        Ok(App {
            screen: Screen::SessionList,
            task_list: None,
            task_detail: None,
            session_list: Some(SessionList::new(items, project_str)),
            chat_view: None,
            should_quit: false,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.main_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        result
    }

    fn main_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            terminal.draw(|frame| match &mut self.screen {
                Screen::TaskList => {
                    if let Some(ref mut tl) = self.task_list {
                        tl.render(frame);
                    }
                }
                Screen::TaskDetail => {
                    if let Some(ref td) = self.task_detail {
                        td.render(frame);
                    }
                }
                Screen::SessionList => {
                    if let Some(ref sl) = self.session_list {
                        sl.render(frame);
                    }
                }
                Screen::Chat => {
                    if let Some(ref cv) = self.chat_view {
                        cv.render(frame);
                    }
                }
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        self.should_quit = true;
                    }

                    match &self.screen {
                        Screen::TaskList => self.handle_task_list_input(key.code),
                        Screen::TaskDetail => self.handle_task_detail_input(key.code),
                        Screen::SessionList => self.handle_session_list_input(key.code),
                        Screen::Chat => self.handle_chat_input(key.code),
                    }
                }
            }

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    fn handle_task_list_input(&mut self, key: KeyCode) {
        let Some(ref mut tl) = self.task_list else {
            return;
        };
        match key {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => tl.previous(),
            KeyCode::Down | KeyCode::Char('j') => tl.next(),
            KeyCode::Home => tl.first(),
            KeyCode::End => tl.last(),
            KeyCode::PageUp => {
                for _ in 0..10 {
                    tl.previous();
                }
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    tl.next();
                }
            }
            KeyCode::Enter => {
                if let Some(task) = tl.selected().cloned() {
                    self.open_task_detail(&task);
                }
            }
            _ => {}
        }
    }

    fn open_task_detail(&mut self, task: &tj_core::db::TaskRow) {
        // Pull a compact resume-pack — same renderer the CLI uses.
        // Failures (missing state, schema mismatch) fall back to a
        // diagnostic body so the TUI doesn't crash on edge cases.
        let body = match self.assemble_pack(&task.task_id) {
            Ok(s) => s,
            Err(e) => format!("(failed to assemble pack: {e:#})"),
        };
        self.task_detail = Some(TaskDetail::new(
            task.task_id.clone(),
            task.title.clone(),
            task.status.clone(),
            body,
        ));
        self.screen = Screen::TaskDetail;
    }

    fn assemble_pack(&self, task_id: &str) -> anyhow::Result<String> {
        // The state SQLite already exists (App::new opened it).
        // Re-resolve through paths to avoid storing the connection
        // on App and dealing with !Send across the render loop.
        let cwd = std::env::current_dir()?;
        let project_hash = tj_core::project_hash::from_path(&cwd)?;
        let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
        let conn = tj_core::db::open(&state_path)?;
        let pack = tj_core::pack::assemble(&conn, task_id, tj_core::pack::PackMode::Compact)?;
        Ok(pack.text)
    }

    fn handle_task_detail_input(&mut self, key: KeyCode) {
        let Some(ref mut td) = self.task_detail else {
            return;
        };
        match key {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Backspace => {
                self.task_detail = None;
                self.screen = Screen::TaskList;
            }
            KeyCode::Up | KeyCode::Char('k') => td.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => td.scroll_down(1),
            KeyCode::PageUp => td.scroll_up(20),
            KeyCode::PageDown => td.scroll_down(20),
            KeyCode::Home => td.scroll_top(),
            KeyCode::End => td.scroll_bottom(),
            _ => {}
        }
    }

    // --- Legacy chat-session handlers (preserved for `--chats` mode) ---

    fn handle_session_list_input(&mut self, key: KeyCode) {
        let Some(ref mut sl) = self.session_list else {
            return;
        };
        if sl.filter_mode {
            match key {
                KeyCode::Esc => sl.clear_filter(),
                KeyCode::Enter => sl.accept_filter(),
                KeyCode::Backspace => sl.filter_pop(),
                KeyCode::Char(ch) => sl.filter_push(ch),
                _ => {}
            }
            return;
        }
        match key {
            KeyCode::Char('q') | KeyCode::Esc => {
                if !sl.filter_text.is_empty() {
                    sl.clear_filter();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('/') => sl.enter_filter_mode(),
            KeyCode::Up | KeyCode::Char('k') => sl.previous(),
            KeyCode::Down | KeyCode::Char('j') => sl.next(),
            KeyCode::Home => sl.first(),
            KeyCode::End => sl.last(),
            KeyCode::PageUp => {
                for _ in 0..10 {
                    sl.previous();
                }
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    sl.next();
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = sl.selected_session_index() {
                    let session = &sl.sessions[idx];
                    self.chat_view = Some(ChatView::from_session(session));
                    self.screen = Screen::Chat;
                }
            }
            _ => {}
        }
    }

    fn handle_chat_input(&mut self, key: KeyCode) {
        let Some(ref mut cv) = self.chat_view else {
            return;
        };
        match key {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Backspace => {
                self.screen = Screen::SessionList;
                self.chat_view = None;
            }
            KeyCode::Up | KeyCode::Char('k') => cv.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => cv.scroll_down(1),
            KeyCode::PageUp => cv.scroll_up(20),
            KeyCode::PageDown => cv.scroll_down(20),
            KeyCode::Home => cv.scroll_top(),
            KeyCode::End => cv.scroll_bottom(),
            _ => {}
        }
    }
}
