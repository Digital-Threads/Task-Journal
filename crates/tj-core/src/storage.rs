use crate::event::Event;
use anyhow::Context;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub struct JsonlWriter {
    path: PathBuf,
    inner: BufWriter<File>,
}

impl JsonlWriter {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {path:?} for append"))?;
        Ok(Self {
            path,
            inner: BufWriter::new(file),
        })
    }

    pub fn append(&mut self, event: &Event) -> anyhow::Result<()> {
        let line = serde_json::to_string(event).context("serialize event")?;
        self.inner
            .write_all(line.as_bytes())
            .context("write event line")?;
        self.inner.write_all(b"\n").context("write newline")?;
        Ok(())
    }

    /// Flush user buffers to OS, then fsync the underlying file so the bytes
    /// survive a crash. Call after every batch of appends that must be durable.
    pub fn flush_durable(&mut self) -> anyhow::Result<()> {
        self.inner.flush().context("flush BufWriter")?;
        self.inner
            .get_ref()
            .sync_all()
            .context("fsync events file")?;
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
}
