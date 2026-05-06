//! Main TUI application — manages screens and terminal lifecycle.

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

pub enum Screen {
    List,
    Chat,
}

pub struct App {
    pub screen: Screen,
    pub session_list: SessionList,
    pub chat_view: Option<ChatView>,
    pub should_quit: bool,
}

impl App {
    pub fn new(project_path: &Path) -> Result<Self> {
        let proj_dir = discovery::find_project_dir(project_path)?;
        let sessions = match proj_dir {
            Some(ref d) => discovery::list_sessions(d)?,
            None => vec![],
        };

        // Parse session metadata (lightweight — just first user msg + timestamps).
        let mut items = Vec::new();
        for path in &sessions {
            match parser::parse_session(path) {
                Ok(parsed) => items.push(parsed),
                Err(_) => continue,
            }
        }

        let project_str = project_path.to_string_lossy().into_owned();

        Ok(App {
            screen: Screen::List,
            session_list: SessionList::new(items, project_str),
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
            terminal.draw(|frame| {
                match &self.screen {
                    Screen::List => self.session_list.render(frame),
                    Screen::Chat => {
                        if let Some(ref cv) = self.chat_view {
                            cv.render(frame);
                        }
                    }
                }
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Global: Ctrl+C or q quits.
                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                        self.should_quit = true;
                    }

                    match &self.screen {
                        Screen::List => self.handle_list_input(key.code),
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

    fn handle_list_input(&mut self, key: KeyCode) {
        // When in filter/search mode, intercept keys for the search input.
        if self.session_list.filter_mode {
            match key {
                KeyCode::Esc => {
                    self.session_list.clear_filter();
                }
                KeyCode::Enter => {
                    self.session_list.accept_filter();
                }
                KeyCode::Backspace => {
                    self.session_list.filter_pop();
                }
                KeyCode::Char(ch) => {
                    self.session_list.filter_push(ch);
                }
                _ => {}
            }
            return;
        }

        // Normal list navigation mode.
        match key {
            KeyCode::Char('q') | KeyCode::Esc => {
                // If a filter is active but we're not in filter_mode, clear it first.
                if !self.session_list.filter_text.is_empty() {
                    self.session_list.clear_filter();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('/') => {
                self.session_list.enter_filter_mode();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.session_list.previous();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.session_list.next();
            }
            KeyCode::Home => {
                self.session_list.first();
            }
            KeyCode::End => {
                self.session_list.last();
            }
            KeyCode::PageUp => {
                for _ in 0..10 {
                    self.session_list.previous();
                }
            }
            KeyCode::PageDown => {
                for _ in 0..10 {
                    self.session_list.next();
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = self.session_list.selected_session_index() {
                    let session = &self.session_list.sessions[idx];
                    self.chat_view = Some(ChatView::from_session(session));
                    self.screen = Screen::Chat;
                }
            }
            _ => {}
        }
    }

    fn handle_chat_input(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Backspace => {
                self.screen = Screen::List;
                self.chat_view = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(ref mut cv) = self.chat_view {
                    cv.scroll_up(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(ref mut cv) = self.chat_view {
                    cv.scroll_down(1);
                }
            }
            KeyCode::PageUp => {
                if let Some(ref mut cv) = self.chat_view {
                    cv.scroll_up(20);
                }
            }
            KeyCode::PageDown => {
                if let Some(ref mut cv) = self.chat_view {
                    cv.scroll_down(20);
                }
            }
            KeyCode::Home => {
                if let Some(ref mut cv) = self.chat_view {
                    cv.scroll_top();
                }
            }
            KeyCode::End => {
                if let Some(ref mut cv) = self.chat_view {
                    cv.scroll_bottom();
                }
            }
            _ => {}
        }
    }
}
