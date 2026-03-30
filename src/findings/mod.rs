pub mod merge;
pub mod schema;

use serde::{Deserialize, Serialize};

/// A single audit finding, matching schemas/finding.schema.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub source: String,
    pub severity: Severity,
    pub confidence: Confidence,
    pub title: String,
    pub contract: String,
    pub function: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property_violated: Option<String>,

    pub attack_scenario: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poc_file: Option<String>,

    #[serde(default = "default_poc_status")]
    pub poc_status: PocStatus,

    #[serde(default)]
    pub dedup_hash: String,

    /// Which agents found this (populated during merge).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found_by: Vec<String>,

    /// Populated during merge when agents disagree on severity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_disagreements: Option<Vec<Severity>>,

    /// Catch-all for extra fields agents might include.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
pub enum Severity {
    Informational,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn rank(self) -> u8 {
        match self {
            Self::Informational => 1,
            Self::Low => 2,
            Self::Medium => 3,
            Self::High => 4,
            Self::Critical => 5,
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Informational => write!(f, "Informational"),
            Self::Low => write!(f, "Low"),
            Self::Medium => write!(f, "Medium"),
            Self::High => write!(f, "High"),
            Self::Critical => write!(f, "Critical"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "Low"),
            Self::Medium => write!(f, "Medium"),
            Self::High => write!(f, "High"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PocStatus {
    CompilesAndDemonstrates,
    CompilesButInconclusive,
    FailedToCompile,
    InfeasibleConditions,
    RequiresMainnetSimulation,
    NotAttempted,
}

impl std::fmt::Display for PocStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompilesAndDemonstrates => write!(f, "compiles_and_demonstrates"),
            Self::CompilesButInconclusive => write!(f, "compiles_but_inconclusive"),
            Self::FailedToCompile => write!(f, "failed_to_compile"),
            Self::InfeasibleConditions => write!(f, "infeasible_conditions"),
            Self::RequiresMainnetSimulation => write!(f, "requires_mainnet_simulation"),
            Self::NotAttempted => write!(f, "not_attempted"),
        }
    }
}

fn default_poc_status() -> PocStatus {
    PocStatus::NotAttempted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ordering_informational_through_critical() {
        assert!(Severity::Informational < Severity::Low);
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }

    #[test]
    fn severity_rank_values() {
        assert_eq!(Severity::Informational.rank(), 1);
        assert_eq!(Severity::Low.rank(), 2);
        assert_eq!(Severity::Medium.rank(), 3);
        assert_eq!(Severity::High.rank(), 4);
        assert_eq!(Severity::Critical.rank(), 5);
    }

    #[test]
    fn finding_round_trip_serialization() {
        let finding = Finding {
            id: "F-001".into(),
            source: "red".into(),
            severity: Severity::High,
            confidence: Confidence::Medium,
            title: "Reentrancy in withdraw".into(),
            contract: "Token.sol".into(),
            function: "withdraw()".into(),
            lines: vec![42, 55],
            property_violated: Some("no-reentrancy".into()),
            attack_scenario: "attacker calls withdraw recursively".into(),
            poc_file: Some("test/poc_001.t.sol".into()),
            poc_status: PocStatus::CompilesAndDemonstrates,
            dedup_hash: "abc123".into(),
            found_by: vec!["red".into(), "blue".into()],
            severity_disagreements: Some(vec![Severity::Medium, Severity::High]),
            extra: serde_json::Map::new(),
        };

        let json = serde_json::to_string(&finding).expect("serialize");
        let deserialized: Finding = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.id, "F-001");
        assert_eq!(deserialized.severity, Severity::High);
        assert_eq!(deserialized.confidence, Confidence::Medium);
        assert_eq!(deserialized.title, "Reentrancy in withdraw");
        assert_eq!(deserialized.lines, vec![42, 55]);
        assert_eq!(deserialized.property_violated, Some("no-reentrancy".into()));
        assert_eq!(deserialized.poc_status, PocStatus::CompilesAndDemonstrates);
        assert_eq!(deserialized.found_by, vec!["red", "blue"]);
        assert_eq!(
            deserialized.severity_disagreements,
            Some(vec![Severity::Medium, Severity::High])
        );
    }

    #[test]
    fn poc_status_serialization_uses_snake_case() {
        let json = serde_json::to_value(PocStatus::CompilesAndDemonstrates).unwrap();
        assert_eq!(json.as_str().unwrap(), "compiles_and_demonstrates");

        let json = serde_json::to_value(PocStatus::CompilesButInconclusive).unwrap();
        assert_eq!(json.as_str().unwrap(), "compiles_but_inconclusive");

        let json = serde_json::to_value(PocStatus::FailedToCompile).unwrap();
        assert_eq!(json.as_str().unwrap(), "failed_to_compile");

        let json = serde_json::to_value(PocStatus::InfeasibleConditions).unwrap();
        assert_eq!(json.as_str().unwrap(), "infeasible_conditions");

        let json = serde_json::to_value(PocStatus::RequiresMainnetSimulation).unwrap();
        assert_eq!(json.as_str().unwrap(), "requires_mainnet_simulation");

        let json = serde_json::to_value(PocStatus::NotAttempted).unwrap();
        assert_eq!(json.as_str().unwrap(), "not_attempted");
    }

    #[test]
    fn default_poc_status_is_not_attempted() {
        assert_eq!(default_poc_status(), PocStatus::NotAttempted);
    }

    #[test]
    fn severity_display_matches_variant_names() {
        assert_eq!(Severity::Informational.to_string(), "Informational");
        assert_eq!(Severity::Low.to_string(), "Low");
        assert_eq!(Severity::Medium.to_string(), "Medium");
        assert_eq!(Severity::High.to_string(), "High");
        assert_eq!(Severity::Critical.to_string(), "Critical");
    }

    #[test]
    fn finding_with_defaults_omits_optional_fields() {
        let finding = Finding {
            id: "F-001".into(),
            source: "test".into(),
            severity: Severity::Low,
            confidence: Confidence::Low,
            title: "Minor".into(),
            contract: "C.sol".into(),
            function: "f()".into(),
            lines: vec![],
            property_violated: None,
            attack_scenario: "none".into(),
            poc_file: None,
            poc_status: PocStatus::NotAttempted,
            dedup_hash: "x".into(),
            found_by: vec![],
            severity_disagreements: None,
            extra: serde_json::Map::new(),
        };

        let json = serde_json::to_string(&finding).unwrap();
        // Optional/empty fields with skip_serializing_if should be absent
        assert!(!json.contains("\"lines\""));
        assert!(!json.contains("\"property_violated\""));
        assert!(!json.contains("\"poc_file\""));
        assert!(!json.contains("\"found_by\""));
        assert!(!json.contains("\"severity_disagreements\""));
    }
}
