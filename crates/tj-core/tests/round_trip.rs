use tempfile::TempDir;
use tj_core::db;
use tj_core::event::{Author, Event, EventType, Source};
use tj_core::storage::JsonlWriter;

#[test]
fn full_round_trip_writes_events_and_rebuilds_state() {
    let d = TempDir::new().unwrap();
    let events_path = d.path().join("events.jsonl");
    let db_path = d.path().join("s.sqlite");
    let project_hash = "deadbeefdeadbeef";

    let mut writer = JsonlWriter::open(&events_path).unwrap();
    let mut open_e = Event::new(
        "tj-r",
        EventType::Open,
        Author::User,
        Source::Cli,
        "x".into(),
    );
    open_e.meta = serde_json::json!({"title": "Round trip"});
    let dec = Event::new(
        "tj-r",
        EventType::Decision,
        Author::Agent,
        Source::Chat,
        "Adopt Rust".into(),
    );
    let close = Event::new(
        "tj-r",
        EventType::Close,
        Author::User,
        Source::Cli,
        "done".into(),
    );
    writer.append(&open_e).unwrap();
    writer.append(&dec).unwrap();
    writer.append(&close).unwrap();
    writer.flush_durable().unwrap();
    drop(writer);

    let conn = db::open(&db_path).unwrap();
    let n = db::rebuild_state(&conn, &events_path, project_hash).unwrap();
    assert_eq!(n, 3);

    let (status, closed_at): (String, Option<String>) = conn
        .query_row(
            "SELECT status, closed_at FROM tasks WHERE task_id=?1",
            ["tj-r"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "closed");
    assert!(closed_at.is_some());
}
