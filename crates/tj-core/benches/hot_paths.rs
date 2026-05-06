//! Criterion benchmarks for the hottest paths the MCP server walks every
//! tool call: rebuild_state, ingest_new_events, pack::assemble, FTS search.
//!
//! These exist to (a) put numbers on the B2 incremental-indexing win and
//! (b) catch regressions before they ship. CI runs `cargo bench --no-run`
//! so the harness must compile; full runs happen locally on a quiet box
//! or in a dedicated bench job.

use std::io::Write;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use tempfile::TempDir;
use tj_core::{
    db,
    event::{Author, Event, EventType, Source},
    pack,
};

const PROJECT_HASH: &str = "deadbeefdeadbeef";

/// Materialize an N-event JSONL file spread across 100 distinct tasks.
fn synthetic_jsonl(n: usize) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let jsonl = dir.path().join("events.jsonl");
    let sqlite = dir.path().join("s.sqlite");
    let mut f = std::fs::File::create(&jsonl).unwrap();
    for i in 0..n {
        let task_id = format!("tj-b{:03}", i % 100);
        let kind = match i % 4 {
            0 => EventType::Open,
            1 => EventType::Decision,
            2 => EventType::Finding,
            _ => EventType::Evidence,
        };
        let mut e = Event::new(
            &task_id,
            kind,
            Author::User,
            Source::Cli,
            format!("event {i} for task {task_id}"),
        );
        if matches!(kind, EventType::Open) {
            e.meta = serde_json::json!({"title": format!("Task {}", i % 100)});
        }
        writeln!(f, "{}", serde_json::to_string(&e).unwrap()).unwrap();
    }
    drop(f);
    (dir, jsonl, sqlite)
}

fn bench_rebuild_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("rebuild_state");
    for &n in &[1_000usize, 10_000] {
        group.bench_function(format!("{n}_events"), |b| {
            b.iter_batched(
                || synthetic_jsonl(n),
                |(_dir, jsonl, sqlite)| {
                    let conn = db::open(&sqlite).unwrap();
                    db::rebuild_state(&conn, &jsonl, PROJECT_HASH).unwrap();
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn bench_pack_assemble_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("pack_assemble_cold");
    for &n in &[1_000usize, 10_000] {
        let (_dir, jsonl, sqlite) = synthetic_jsonl(n);
        let conn = db::open(&sqlite).unwrap();
        db::rebuild_state(&conn, &jsonl, PROJECT_HASH).unwrap();
        group.bench_function(format!("{n}_events"), |b| {
            b.iter(|| {
                // Invalidate the cache so each iteration is a cold compute.
                conn.execute("DELETE FROM task_pack_cache", []).unwrap();
                pack::assemble(&conn, "tj-b000", pack::PackMode::Compact).unwrap();
            });
        });
    }
    group.finish();
}

fn bench_search_fts(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_fts");
    for &n in &[1_000usize, 10_000] {
        let (_dir, jsonl, sqlite) = synthetic_jsonl(n);
        let conn = db::open(&sqlite).unwrap();
        db::rebuild_state(&conn, &jsonl, PROJECT_HASH).unwrap();
        group.bench_function(format!("{n}_events"), |b| {
            b.iter(|| {
                let mut stmt = conn
                    .prepare(
                        "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT 50",
                    )
                    .unwrap();
                let _: Vec<String> = stmt
                    .query_map(rusqlite::params!["event"], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_rebuild_state,
    bench_pack_assemble_cold,
    bench_search_fts
);
criterion_main!(benches);
