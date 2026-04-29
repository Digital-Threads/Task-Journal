use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

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
}

impl EventType {
    pub const ALL: &'static [Self] = &[
        Self::Open, Self::Hypothesis, Self::Finding, Self::Evidence,
        Self::Decision, Self::Rejection, Self::Constraint, Self::Correction,
        Self::Reopen, Self::Supersede, Self::Close, Self::Redirect,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Author { User, Agent, Classifier, Hook }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source { Chat, Hook, Manual, Cli }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus { Confirmed, Suggested }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength { Weak, Medium, Strong }

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
        assert_eq!(serde_json::to_string(&Author::Classifier).unwrap(), "\"classifier\"");
        assert_eq!(serde_json::to_string(&Source::Hook).unwrap(), "\"hook\"");
        assert_eq!(serde_json::to_string(&EventStatus::Suggested).unwrap(), "\"suggested\"");
        assert_eq!(serde_json::to_string(&EvidenceStrength::Strong).unwrap(), "\"strong\"");
    }
}
