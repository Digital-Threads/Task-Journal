use crate::event::Event;
use anyhow::Context;
use fd_lock::RwLock as FdLock;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Append-only writer for the events JSONL log. Holds an advisory
/// cross-platform file lock around each append + fsync, so that
/// concurrent producers (auto-capture hook + manual `task-journal
/// event` + MCP server) cannot interleave bytes — `O_APPEND` alone
/// is not atomic on Windows.
///
/// The trade-off: every append takes one syscall to acquire the
/// lock and one more to release it. For a journal — which sees a
/// handful of events per minute — this overhead is negligible and
/// far cheaper than recovery from a corrupt JSONL line.
pub struct JsonlWriter {
    path: PathBuf,
    lock: FdLock<File>,
}

impl JsonlWriter {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
        }
        // `read(true)` is required on Windows: `fd_lock` calls
        // `LockFileEx` on the underlying handle, which fails with
        // `os error 5 (Access is denied)` if the file was opened
        // append-only — the API needs GENERIC_READ access on the
        // handle. Linux's flock() doesn't care, so the omission was
        // silent on POSIX. See windows-rs / fd_lock notes.
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {path:?} for append"))?;
        Ok(Self {
            path,
            lock: FdLock::new(file),
        })
    }

    pub fn append(&mut self, event: &Event) -> anyhow::Result<()> {
        let line = serde_json::to_string(event).context("serialize event")?;
        let mut guard = self.lock.write().context("acquire exclusive file lock")?;
        guard
            .write_all(line.as_bytes())
            .context("write event line")?;
        guard.write_all(b"\n").context("write newline")?;
        Ok(())
    }

    /// Force the file's bytes through to durable storage. Holds the
    /// exclusive lock so no concurrent writer can sneak an append
    /// between us and the fsync.
    pub fn flush_durable(&mut self) -> anyhow::Result<()> {
        let guard = self.lock.write().context("acquire exclusive file lock")?;
        guard.sync_all().context("fsync events file")?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use tempfile::TempDir;

    fn sample_event(text: &str) -> Event {
        Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            text.into(),
        )
    }

    #[test]
    fn append_three_events_yields_three_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");

        let mut w = JsonlWriter::open(&path).unwrap();
        w.append(&sample_event("a")).unwrap();
        w.append(&sample_event("b")).unwrap();
        w.append(&sample_event("c")).unwrap();
        w.flush_durable().unwrap();
        drop(w);

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let _: Event = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn reopen_appends_not_truncates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");

        {
            let mut w = JsonlWriter::open(&path).unwrap();
            w.append(&sample_event("a")).unwrap();
            w.flush_durable().unwrap();
        }
        {
            let mut w = JsonlWriter::open(&path).unwrap();
            w.append(&sample_event("b")).unwrap();
            w.flush_durable().unwrap();
        }

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2);
    }

    #[test]
    fn concurrent_appends_do_not_interleave_bytes() {
        // Eight threads, each owning its own JsonlWriter (own File handle
        // + own fd_lock::RwLock instance) on the same path, race to write
        // 100 events apiece. The exclusive advisory lock must serialize
        // them so every line is a parseable Event with no torn writes.
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let path = Arc::new(dir.path().join("events.jsonl"));

        let mut handles = Vec::with_capacity(8);
        for thread_idx in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                let mut w = JsonlWriter::open(&*path).unwrap();
                for i in 0..100 {
                    let mut e = Event::new(
                        format!("tj-t{thread_idx}"),
                        EventType::Open,
                        Author::User,
                        Source::Cli,
                        format!("thread {thread_idx} event {i}"),
                    );
                    e.meta = serde_json::json!({"thread": thread_idx, "i": i});
                    w.append(&e).unwrap();
                }
                w.flush_durable().unwrap();
            }));
        }
        for h in handles {
            h.join().expect("writer thread panicked");
        }

        let body = std::fs::read_to_string(&*path).unwrap();
        let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 800, "expected 800 lines, got {}", lines.len());
        for (idx, line) in lines.iter().enumerate() {
            serde_json::from_str::<Event>(line)
                .unwrap_or_else(|e| panic!("line {idx} not a valid Event: {e}\n  line: {line}"));
        }
    }
}
