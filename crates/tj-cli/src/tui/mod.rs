//! Interactive TUI for browsing task-journal data (default) and the
//! underlying Claude Code chat sessions (legacy `--chats` mode).

pub mod app;
pub mod chat_view;
pub mod session_list;
pub mod task_detail;
pub mod task_list;

/// First 8 characters of a session id, cut on a char boundary.
fn short_session_id(id: &str) -> String {
    id.chars().take(8).collect()
}
