//! Where a project's chronicle lives. The module map sits in one journal,
//! the home: the main checkout of a repository. Tasks linked to modules sit
//! in the home's journal or in the journals of its git worktrees, the
//! members. A member records itself in a registry next to the journals, and
//! the chronicle events it writes carry `meta.chronicle_home`, so a lost
//! registry is rebuilt from the journals themselves.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// The `meta` key a member's chronicle events carry: the home's hash.
pub const HOME_KEY: &str = "chronicle_home";

pub struct Chronicle {
    /// The home journal's project hash.
    pub home_hash: String,
    /// The home's state: the modules, and the home's own tasks.
    pub home: Connection,
    /// The states of the other journals that may hold linked tasks.
    pub members: Vec<Connection>,
    /// The current project's hash when it is a member (a worktree).
    pub member_hash: Option<String>,
    data: PathBuf,
}

impl Chronicle {
    /// The chronicle of the project at `dir`, every journal ingested.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        Self::open_in(&crate::paths::data_dir()?, dir)
    }

    /// [`Chronicle::open`] with the data dir given.
    pub fn open_in(data: &Path, dir: &Path) -> anyhow::Result<Self> {
        let current = crate::project_hash::from_path(dir)?;
        let home_hash = match crate::project_hash::chronicle_home(dir) {
            Some(root) => crate::project_hash::from_path(root)?,
            None => current.clone(),
        };
        let member_hash = (home_hash != current).then_some(current);
        if let Some(member) = &member_hash {
            register(data, &home_hash, member)?;
        }

        let home = open_journal(data, &home_hash)?;
        let mut members = Vec::new();
        for hash in member_hashes(data, &home_hash)? {
            if hash != home_hash && data.join("events").join(format!("{hash}.jsonl")).exists() {
                members.push(open_journal(data, &hash)?);
            }
        }

        Ok(Self {
            home_hash,
            home,
            members,
            member_hash,
            data: data.to_path_buf(),
        })
    }

    /// A chronicle of one journal that holds both the modules and the tasks.
    pub fn single(conn: Connection, hash: &str) -> Self {
        Self {
            home_hash: hash.to_string(),
            home: conn,
            members: Vec::new(),
            member_hash: None,
            data: PathBuf::new(),
        }
    }

    /// Every state that holds tasks, the home first.
    pub fn journals(&self) -> impl Iterator<Item = &Connection> {
        std::iter::once(&self.home).chain(self.members.iter())
    }

    /// The home's journal file: where module events go.
    pub fn home_events(&self) -> PathBuf {
        self.data
            .join("events")
            .join(format!("{}.jsonl", self.home_hash))
    }

    /// Mark an event a member writes about modules with its home, so the
    /// home finds the member again even without the registry.
    pub fn stamp(&self, meta: &mut serde_json::Value) {
        if self.member_hash.is_some() {
            meta[HOME_KEY] = serde_json::Value::String(self.home_hash.clone());
        }
    }

    /// Re-read the home's journal after writing to it.
    pub fn refresh_home(&self) -> anyhow::Result<()> {
        crate::db::ingest_new_events(&self.home, self.home_events(), &self.home_hash)?;

        Ok(())
    }
}

fn open_journal(data: &Path, hash: &str) -> anyhow::Result<Connection> {
    let conn = crate::db::open(data.join("state").join(format!("{hash}.sqlite")))?;
    crate::db::ingest_new_events(
        &conn,
        data.join("events").join(format!("{hash}.jsonl")),
        hash,
    )?;

    Ok(conn)
}

fn registry(data: &Path, home: &str) -> PathBuf {
    data.join("chronicle").join(format!("{home}.members"))
}

/// The journals known to link tasks to `home`'s modules. Without a registry
/// file the journals are scanned for members' marks once, and the result kept.
fn member_hashes(data: &Path, home: &str) -> anyhow::Result<Vec<String>> {
    let path = registry(data, home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let found = scan(data, home)?;
            std::fs::create_dir_all(path.parent().expect("registry has a parent"))?;
            std::fs::write(
                &path,
                found.iter().map(|h| format!("{h}\n")).collect::<String>(),
            )?;
            return Ok(found);
        }
        Err(e) => return Err(e.into()),
    };

    let mut out: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|h| h == line) {
            out.push(line.to_string());
        }
    }

    Ok(out)
}

fn register(data: &Path, home: &str, member: &str) -> anyhow::Result<()> {
    if member_hashes(data, home)?.iter().any(|h| h == member) {
        return Ok(());
    }

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(registry(data, home))?;
    f.write_all(format!("{member}\n").as_bytes())?;

    Ok(())
}

/// Journals whose lines carry `home` as their chronicle home.
fn scan(data: &Path, home: &str) -> anyhow::Result<Vec<String>> {
    let needle = format!("\"{HOME_KEY}\":\"{home}\"");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(data.join("events")) else {
        return Ok(out);
    };

    for entry in entries {
        let path = entry?.path();
        let is_journal = path.extension().is_some_and(|x| x == "jsonl");
        if is_journal && std::fs::read_to_string(&path).is_ok_and(|t| t.contains(&needle)) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::event::{Author, Event, EventType, Source};
    use crate::modules::tests_support::{close_task, open_task};
    use crate::modules::{module_event, notes_meta, ModuleFields};

    /// A git repository with one linked worktree, and a data dir for journals.
    struct Repo {
        _dir: tempfile::TempDir,
        data: PathBuf,
        main: PathBuf,
        worktree: PathBuf,
    }

    fn repo() -> Repo {
        let dir = tempfile::TempDir::new().unwrap();
        let main = dir.path().join("repo");
        std::fs::create_dir_all(&main).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&main)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        let worktree = dir.path().join("wt");
        git(&["worktree", "add", "-q", worktree.to_str().unwrap()]);

        Repo {
            data: dir.path().join("data"),
            _dir: dir,
            main,
            worktree,
        }
    }

    fn write(data: &Path, project: &Path, events: &[Event]) -> String {
        let hash = crate::project_hash::from_path(project).unwrap();
        let mut w =
            crate::storage::JsonlWriter::open(data.join("events").join(format!("{hash}.jsonl")))
                .unwrap();
        for e in events {
            w.append(e).unwrap();
        }

        hash
    }

    fn stars() -> Event {
        module_event(
            "stars",
            &ModuleFields {
                name: Some("Stars".into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// The home holds the module and a task; the worktree holds another task
    /// of the same module, with a decision and a history note.
    fn populated() -> Repo {
        let r = repo();
        let home = write(
            &r.data,
            &r.main,
            &[
                stars(),
                open_task("tj-home", &["stars"]),
                close_task("tj-home", serde_json::json!({})),
            ],
        );

        let mut open = open_task("tj-wt", &["stars"]);
        open.meta[HOME_KEY] = serde_json::json!(home);
        let decision = Event::new(
            "tj-wt",
            EventType::Decision,
            Author::Agent,
            Source::Chat,
            "Cache the feed".into(),
        );
        let mut close = close_task(
            "tj-wt",
            serde_json::json!({"module_notes": notes_meta(&[("stars".into(), "Feed cached in the worktree".into())])}),
        );
        close.meta[HOME_KEY] = serde_json::json!(home);
        write(&r.data, &r.worktree, &[open, decision, close]);

        r
    }

    #[test]
    fn a_worktree_reads_the_home_map_and_the_whole_history() {
        let r = populated();

        let chr = Chronicle::open_in(&r.data, &r.worktree).unwrap();

        assert_eq!(
            chr.home_hash,
            crate::project_hash::from_path(&r.main).unwrap()
        );
        assert!(chr.member_hash.is_some());
        let page = crate::modules::page(&chr, "stars").unwrap();
        for needle in [
            "tj-home",
            "tj-wt",
            "Feed cached in the worktree",
            "Cache the feed",
        ] {
            assert!(page.contains(needle), "missing {needle}:\n{page}");
        }
        let map = crate::modules::map(&chr).unwrap();
        assert_eq!(map[0].task_count, 2);
    }

    #[test]
    fn the_home_sees_its_worktrees_tasks_once_they_registered() {
        let r = populated();
        Chronicle::open_in(&r.data, &r.worktree).unwrap();

        let chr = Chronicle::open_in(&r.data, &r.main).unwrap();

        assert!(chr.member_hash.is_none());
        assert!(crate::modules::page(&chr, "stars")
            .unwrap()
            .contains("tj-wt"));
    }

    #[test]
    fn a_lost_registry_is_rebuilt_from_the_journals() {
        let r = populated();
        // The worktree never opened its chronicle, so it never registered:
        // only the marks in its journal tell the home about it.
        assert!(!r.data.join("chronicle").exists());

        let chr = Chronicle::open_in(&r.data, &r.main).unwrap();

        assert!(crate::modules::page(&chr, "stars")
            .unwrap()
            .contains("tj-wt"));
        assert!(
            r.data.join("chronicle").exists(),
            "the rebuilt registry is kept"
        );
    }

    #[test]
    fn a_project_without_worktrees_is_its_own_home() {
        let r = repo();
        let home = write(&r.data, &r.main, &[stars()]);

        let chr = Chronicle::open_in(&r.data, &r.main).unwrap();

        assert_eq!(chr.home_hash, home);
        assert!(chr.member_hash.is_none());
        assert!(chr.members.is_empty());
    }

    #[test]
    fn only_a_member_stamps_its_events_with_the_home() {
        let r = populated();
        let mut meta = serde_json::json!({});

        Chronicle::open_in(&r.data, &r.main)
            .unwrap()
            .stamp(&mut meta);
        assert!(meta.get(HOME_KEY).is_none());

        let chr = Chronicle::open_in(&r.data, &r.worktree).unwrap();
        chr.stamp(&mut meta);
        assert_eq!(meta[HOME_KEY], chr.home_hash);
    }
}
