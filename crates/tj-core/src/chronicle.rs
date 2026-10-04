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
    /// The current project's hash when it is a member (a worktree).
    pub member_hash: Option<String>,
    /// The other journals known to link tasks to the home's modules. Opened
    /// one at a time, and only by the views that gather the whole history.
    members: Vec<String>,
    data: PathBuf,
}

impl Chronicle {
    /// The chronicle of the project at `dir`: its home's state, caught up.
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
        let home = open_journal(data, &home_hash)?;
        let members = member_hashes(data, &home_hash)?
            .into_iter()
            .filter(|h| *h != home_hash)
            .collect();

        Ok(Self {
            home_hash,
            home,
            member_hash,
            members,
            data: data.to_path_buf(),
        })
    }

    /// A chronicle of one journal that holds both the modules and the tasks.
    pub fn single(conn: Connection, hash: &str) -> Self {
        Self {
            home_hash: hash.to_string(),
            home: conn,
            member_hash: None,
            members: Vec::new(),
            data: PathBuf::new(),
        }
    }

    pub fn member_hashes(&self) -> &[String] {
        &self.members
    }

    /// Visit each member journal's state in turn: opened, caught up with its
    /// journal and closed again, so a repository with hundreds of worktrees
    /// holds one at a time. A member that fails is skipped, never fatal.
    pub fn for_each_member(&self, mut visit: impl FnMut(&Connection) -> anyhow::Result<()>) {
        for hash in &self.members {
            if !self
                .data
                .join("events")
                .join(format!("{hash}.jsonl"))
                .exists()
            {
                continue;
            }

            let result = open_journal(&self.data, hash).and_then(|conn| visit(&conn));
            if let Err(e) = result {
                tracing::warn!(member = hash.as_str(), "skipping a chronicle member: {e:#}");
            }
        }
    }

    /// The home's journal file: where module events go.
    pub fn home_events(&self) -> PathBuf {
        self.data
            .join("events")
            .join(format!("{}.jsonl", self.home_hash))
    }

    /// Mark an event a member writes about modules with its home, and list
    /// the member under its home: from now on its tasks join the history.
    pub fn stamp(&self, meta: &mut serde_json::Value) {
        let Some(member) = &self.member_hash else {
            return;
        };

        meta[HOME_KEY] = serde_json::Value::String(self.home_hash.clone());
        // The mark is in the journal, so a lost registration is rebuilt.
        if let Err(e) = register(&self.data, &self.home_hash, member) {
            tracing::warn!(
                member = member.as_str(),
                "could not register a chronicle member: {e:#}"
            );
        }
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
        let is_member = is_journal
            && std::fs::read_to_string(&path).is_ok_and(|text| {
                // Only the mark a member writes counts: `meta.chronicle_home`
                // of an event, not the same text inside any other value.
                text.lines()
                    .filter(|line| line.contains(&needle))
                    .any(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .is_ok_and(|e| e["meta"][HOME_KEY] == home)
                    })
            });
        if is_member {
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
        assert!(chr.member_hashes().is_empty());
    }

    fn registry_of(r: &Repo) -> String {
        let home = crate::project_hash::from_path(&r.main).unwrap();

        std::fs::read_to_string(r.data.join("chronicle").join(format!("{home}.members")))
            .unwrap_or_default()
    }

    #[test]
    fn a_worktree_registers_only_once_it_links_a_task() {
        // Every Loom task gets a worktree; most never link anything. Only a
        // worktree whose tasks join the history is worth visiting.
        let r = repo();
        write(&r.data, &r.main, &[stars()]);
        let member = crate::project_hash::from_path(&r.worktree).unwrap();

        let chr = Chronicle::open_in(&r.data, &r.worktree).unwrap();
        assert!(
            !registry_of(&r).contains(&member),
            "registered on a mere open"
        );

        chr.stamp(&mut serde_json::json!({}));
        assert!(registry_of(&r).contains(&member));
    }

    #[test]
    fn a_broken_member_is_skipped_not_fatal() {
        let r = populated();
        Chronicle::open_in(&r.data, &r.worktree)
            .unwrap()
            .stamp(&mut serde_json::json!({}));
        let bogus = "deadbeefdeadbeef";
        std::fs::write(r.data.join("events").join(format!("{bogus}.jsonl")), "{}\n").unwrap();
        std::fs::write(
            r.data.join("state").join(format!("{bogus}.sqlite")),
            "not a database",
        )
        .unwrap();
        let home = crate::project_hash::from_path(&r.main).unwrap();
        let registry = r.data.join("chronicle").join(format!("{home}.members"));
        std::fs::write(
            &registry,
            format!("{}{bogus}\n", std::fs::read_to_string(&registry).unwrap()),
        )
        .unwrap();

        let chr = Chronicle::open_in(&r.data, &r.main).unwrap();

        assert!(crate::modules::page(&chr, "stars")
            .unwrap()
            .contains("tj-wt"));
        assert_eq!(crate::modules::map(&chr).unwrap()[0].task_count, 2);
    }

    #[test]
    fn a_mark_inside_free_text_does_not_join_a_chronicle() {
        // An agent can put any JSON in a decision's alternatives; only the
        // top-level mark a member writes counts.
        let r = repo();
        let home = write(&r.data, &r.main, &[stars()]);
        let mut open = open_task("tj-stranger", &["stars"]);
        open.meta["alternatives"] = serde_json::json!([{ HOME_KEY: home }]);
        let stranger = tempfile::TempDir::new().unwrap();
        write(&r.data, stranger.path(), &[open]);

        let chr = Chronicle::open_in(&r.data, &r.main).unwrap();

        assert!(!crate::modules::page(&chr, "stars")
            .unwrap()
            .contains("tj-stranger"));
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
