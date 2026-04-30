//! Curated event sequences → expected pack output. Updates require
//! deliberate review (these protect the user-facing contract).

use tempfile::TempDir;
use tj_core::db;
use tj_core::event::{Author, Event, EventType, EvidenceStrength, Source};
use tj_core::pack::{assemble, PackMode};

#[test]
fn fixture_a_compact_pack_for_simple_task() {
    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let ph = "feedface";

    let events = build_fixture_a();
    for e in &events {
        db::upsert_task_from_event(&conn, e, ph).unwrap();
        db::index_event(&conn, e).unwrap();
    }

    let pack = assemble(&conn, "tj-fa", PackMode::Compact).unwrap();
    insta_assert_contains(&pack.text, "# Add OAuth login");
    insta_assert_contains(&pack.text, "status: open");
    insta_assert_contains(&pack.text, "Active decisions");
    insta_assert_contains(&pack.text, "Adopt PKCE flow");
    insta_assert_contains(&pack.text, "Recent events");
    assert_eq!(pack.metadata.source_event_count, 5);
}

fn build_fixture_a() -> Vec<Event> {
    let mut events = Vec::new();
    let mut open_e = Event::new(
        "tj-fa",
        EventType::Open,
        Author::User,
        Source::Cli,
        "Add OAuth login".into(),
    );
    open_e.meta = serde_json::json!({"title": "Add OAuth login"});
    events.push(open_e);
    events.push(Event::new(
        "tj-fa",
        EventType::Hypothesis,
        Author::Agent,
        Source::Chat,
        "PKCE vs implicit grant".into(),
    ));
    let mut ev = Event::new(
        "tj-fa",
        EventType::Evidence,
        Author::Agent,
        Source::Chat,
        "OAuth 2.1 deprecates implicit".into(),
    );
    ev.evidence_strength = Some(EvidenceStrength::Strong);
    events.push(ev);
    events.push(Event::new(
        "tj-fa",
        EventType::Decision,
        Author::Agent,
        Source::Chat,
        "Adopt PKCE flow".into(),
    ));
    events.push(Event::new(
        "tj-fa",
        EventType::Rejection,
        Author::Agent,
        Source::Chat,
        "Implicit grant: deprecated, no refresh".into(),
    ));
    events
}

#[test]
fn fixture_b_full_pack_with_supersede_and_correction() {
    let d = TempDir::new().unwrap();
    let conn = db::open(d.path().join("s.sqlite")).unwrap();
    let ph = "feedface";

    let events = build_fixture_b();
    for e in &events {
        db::upsert_task_from_event(&conn, e, ph).unwrap();
        db::index_event(&conn, e).unwrap();
    }

    let pack = assemble(&conn, "tj-fb", PackMode::Full).unwrap();
    insta_assert_contains(&pack.text, "# Stack choice for journal");
    insta_assert_contains(&pack.text, "Lifecycle");
    insta_assert_contains(&pack.text, "opened");
    insta_assert_contains(&pack.text, "closed");
    insta_assert_contains(&pack.text, "Active decisions");
    insta_assert_contains(&pack.text, "Adopt Rust");
    insta_assert_contains(&pack.text, "Rejected");
    insta_assert_contains(&pack.text, "TypeScript");
    insta_assert_contains(&pack.text, "Evidence");

    // The superseded TS decision must NOT appear under "Active decisions".
    let active_section_start = pack.text.find("## Active decisions").unwrap();
    let active_section_end = pack.text[active_section_start..]
        .find("\n## ")
        .map(|i| active_section_start + i)
        .unwrap_or(pack.text.len());
    let active_section = &pack.text[active_section_start..active_section_end];
    assert!(
        !active_section.contains("Adopt TypeScript"),
        "superseded TS decision must NOT appear under Active decisions:\n{active_section}"
    );

    assert_eq!(pack.metadata.source_event_count, 12);
}

fn build_fixture_b() -> Vec<Event> {
    let mut events = Vec::new();
    let mut open_e = Event::new(
        "tj-fb",
        EventType::Open,
        Author::User,
        Source::Cli,
        "Stack choice".into(),
    );
    open_e.meta = serde_json::json!({"title": "Stack choice for journal"});
    events.push(open_e);
    events.push(Event::new(
        "tj-fb",
        EventType::Hypothesis,
        Author::Agent,
        Source::Chat,
        "TS vs Rust".into(),
    ));
    events.push(Event::new(
        "tj-fb",
        EventType::Constraint,
        Author::User,
        Source::Chat,
        "Single static binary".into(),
    ));
    let mut ev1 = Event::new(
        "tj-fb",
        EventType::Evidence,
        Author::Agent,
        Source::Chat,
        "Hook startup 380ms node, 12ms rust".into(),
    );
    ev1.evidence_strength = Some(EvidenceStrength::Strong);
    events.push(ev1);
    let ts_dec = Event::new(
        "tj-fb",
        EventType::Decision,
        Author::Agent,
        Source::Chat,
        "Adopt TypeScript".into(),
    );
    let ts_dec_id = ts_dec.event_id.clone();
    events.push(ts_dec);
    let mut sup = Event::new(
        "tj-fb",
        EventType::Supersede,
        Author::Agent,
        Source::Chat,
        "TS decision replaced".into(),
    );
    sup.supersedes = Some(ts_dec_id);
    events.push(sup);
    events.push(Event::new(
        "tj-fb",
        EventType::Decision,
        Author::Agent,
        Source::Chat,
        "Adopt Rust".into(),
    ));
    events.push(Event::new(
        "tj-fb",
        EventType::Rejection,
        Author::Agent,
        Source::Chat,
        "TypeScript: loses single-binary distribution".into(),
    ));
    let mistake = Event::new(
        "tj-fb",
        EventType::Finding,
        Author::Classifier,
        Source::Hook,
        "Migration looks complete (was wrong)".into(),
    );
    let mistake_id = mistake.event_id.clone();
    events.push(mistake);
    let mut corr = Event::new(
        "tj-fb",
        EventType::Correction,
        Author::User,
        Source::Cli,
        "Migration was NOT complete; reverted finding".into(),
    );
    corr.corrects = Some(mistake_id);
    events.push(corr);
    events.push(Event::new(
        "tj-fb",
        EventType::Finding,
        Author::Agent,
        Source::Chat,
        "Migration completed for real after fix".into(),
    ));
    events.push(Event::new(
        "tj-fb",
        EventType::Close,
        Author::User,
        Source::Cli,
        "Done".into(),
    ));
    events
}

fn insta_assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "missing {needle:?} in:\n{haystack}"
    );
}
