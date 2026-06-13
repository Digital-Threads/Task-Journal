use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Open,
    Hypothesis,
    Finding,
    Evidence,
    Decision,
    Rejection,
    Constraint,
    Correction,
    Reopen,
    Supersede,
    Close,
    Redirect,
    Rename,
}

impl EventType {
    pub const ALL: &'static [Self] = &[
        Self::Open,
        Self::Hypothesis,
        Self::Finding,
        Self::Evidence,
        Self::Decision,
        Self::Rejection,
        Self::Constraint,
        Self::Correction,
        Self::Reopen,
        Self::Supersede,
        Self::Close,
        Self::Redirect,
        Self::Rename,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Agent,
    Classifier,
    Hook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Chat,
    Hook,
    Manual,
    Cli,
    Dream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Confirmed,
    Suggested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Weak,
    Medium,
    Strong,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Refs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub event_id: String,
    pub schema_version: String,
    pub task_id: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub timestamp: String,
    pub author: Author,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_strength: Option<EvidenceStrength>,
    pub text: String,
    #[serde(default)]
    pub refs: Refs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub status: EventStatus,
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl Event {
    pub fn new(
        task_id: impl Into<String>,
        event_type: EventType,
        author: Author,
        source: Source,
        text: String,
    ) -> Self {
        Event {
            event_id: ulid::Ulid::new().to_string(),
            schema_version: crate::SCHEMA_VERSION.to_string(),
            task_id: task_id.into(),
            event_type,
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            author,
            source,
            confidence: None,
            evidence_strength: None,
            text,
            refs: Refs::default(),
            corrects: None,
            supersedes: None,
            status: EventStatus::Confirmed,
            meta: serde_json::json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_serializes_to_snake_case() {
        let t = EventType::Decision;
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "\"decision\"");
    }

    #[test]
    fn event_type_round_trip_all_variants() {
        for ty in EventType::ALL {
            let s = serde_json::to_string(&ty).unwrap();
            let back: EventType = serde_json::from_str(&s).unwrap();
            assert_eq!(*ty, back);
        }
    }

    #[test]
    fn author_source_status_strength_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&Author::Classifier).unwrap(),
            "\"classifier\""
        );
        assert_eq!(serde_json::to_string(&Source::Hook).unwrap(), "\"hook\"");
        assert_eq!(
            serde_json::to_string(&EventStatus::Suggested).unwrap(),
            "\"suggested\""
        );
        assert_eq!(
            serde_json::to_string(&EvidenceStrength::Strong).unwrap(),
            "\"strong\""
        );
    }

    #[test]
    fn source_dream_serializes_to_snake_case() {
        let j = serde_json::to_string(&Source::Dream).unwrap();
        assert_eq!(j, "\"dream\"");
        let back: Source = serde_json::from_str("\"dream\"").unwrap();
        assert_eq!(back, Source::Dream);
    }

    #[test]
    fn event_new_assigns_ulid_and_now() {
        let a = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Manual,
            "first".into(),
        );
        let b = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Manual,
            "second".into(),
        );
        assert_ne!(a.event_id, b.event_id);
        assert_eq!(a.event_id.len(), 26);
        // ULID = 48-bit timestamp (10 base32 chars) + 80-bit random (16 base32 chars).
        // Random portion is independent per call, so only the timestamp prefix is monotonic.
        assert!(
            a.event_id[..10] <= b.event_id[..10],
            "ULID timestamp prefix must be monotonic"
        );
        assert_eq!(a.schema_version, "1.0");
        assert_eq!(a.status, EventStatus::Confirmed);
        chrono::DateTime::parse_from_rfc3339(&a.timestamp).expect("RFC3339");
    }

    #[test]
    fn event_round_trip_all_fields() {
        let e = Event {
            event_id: "01HZX5K8000000000000000000".to_string(),
            schema_version: "1.0".to_string(),
            task_id: "tj-7f3a".to_string(),
            event_type: EventType::Decision,
            timestamp: "2026-05-14T12:00:00+04:00".to_string(),
            author: Author::Agent,
            source: Source::Chat,
            confidence: Some(0.92),
            evidence_strength: Some(EvidenceStrength::Strong),
            text: "Adopt Rust + rmcp.".to_string(),
            refs: Refs {
                commits: vec!["a3f2dd".into()],
                files: vec!["Cargo.toml".into()],
                events: vec![],
            },
            corrects: None,
            supersedes: None,
            status: EventStatus::Confirmed,
            meta: serde_json::json!({}),
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(e.event_id, back.event_id);
        assert_eq!(e.event_type, back.event_type);
        assert_eq!(e.refs.commits, back.refs.commits);
        assert_eq!(e.confidence, back.confidence);
    }
}
