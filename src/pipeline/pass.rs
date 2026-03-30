use crate::config::Config;
use crate::workspace::Workspace;
use std::path::PathBuf;

/// Shared context passed to every pipeline pass.
#[derive(Clone)]
pub struct PipelineContext {
    pub config: Config,
    pub workspace: Workspace,
    pub audit_dir: PathBuf,
    pub doyran_root: PathBuf,
}

/// Result of executing a single pass.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PassResult {
    pub name: String,
    pub status: PassStatus,
    pub duration_secs: u64,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassStatus {
    Completed,
    Failed,
    Skipped,
    NotImplemented,
}

impl std::fmt::Display for PassStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "FAILED"),
            Self::Skipped => write!(f, "skipped"),
            Self::NotImplemented => write!(f, "not implemented"),
        }
    }
}

/// Pass numbers for the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PassNumber {
    Recon = 1,
    Agents = 2,
    Poc = 3,
    Fuzzing = 4,
    Formal = 5,
    Review = 6,
}

impl PassNumber {
    pub fn from_u8(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::Recon),
            2 => Some(Self::Agents),
            3 => Some(Self::Poc),
            4 => Some(Self::Fuzzing),
            5 => Some(Self::Formal),
            6 => Some(Self::Review),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Recon => "recon",
            Self::Agents => "agents",
            Self::Poc => "poc",
            Self::Fuzzing => "fuzzing",
            Self::Formal => "formal",
            Self::Review => "review",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Recon => "Reconnaissance",
            Self::Agents => "Multi-Agent Adversarial Analysis",
            Self::Poc => "PoC Generation & Validation",
            Self::Fuzzing => "Fuzzing Campaign",
            Self::Formal => "Formal Verification",
            Self::Review => "Adversarial Review",
        }
    }

    pub fn requires_claude(self) -> bool {
        matches!(self, Self::Agents | Self::Poc | Self::Review)
    }

    pub fn all() -> &'static [PassNumber] {
        &[
            Self::Recon,
            Self::Agents,
            Self::Poc,
            Self::Fuzzing,
            Self::Formal,
            Self::Review,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u8_valid_values_1_through_6() {
        assert_eq!(PassNumber::from_u8(1), Some(PassNumber::Recon));
        assert_eq!(PassNumber::from_u8(2), Some(PassNumber::Agents));
        assert_eq!(PassNumber::from_u8(3), Some(PassNumber::Poc));
        assert_eq!(PassNumber::from_u8(4), Some(PassNumber::Fuzzing));
        assert_eq!(PassNumber::from_u8(5), Some(PassNumber::Formal));
        assert_eq!(PassNumber::from_u8(6), Some(PassNumber::Review));
    }

    #[test]
    fn from_u8_invalid_values() {
        assert_eq!(PassNumber::from_u8(0), None);
        assert_eq!(PassNumber::from_u8(7), None);
        assert_eq!(PassNumber::from_u8(255), None);
    }

    #[test]
    fn name_returns_expected_strings() {
        assert_eq!(PassNumber::Recon.name(), "recon");
        assert_eq!(PassNumber::Agents.name(), "agents");
        assert_eq!(PassNumber::Poc.name(), "poc");
        assert_eq!(PassNumber::Fuzzing.name(), "fuzzing");
        assert_eq!(PassNumber::Formal.name(), "formal");
        assert_eq!(PassNumber::Review.name(), "review");
    }

    #[test]
    fn requires_claude_true_for_agents_poc_review() {
        assert!(PassNumber::Agents.requires_claude());
        assert!(PassNumber::Poc.requires_claude());
        assert!(PassNumber::Review.requires_claude());
    }

    #[test]
    fn requires_claude_false_for_recon_fuzzing_formal() {
        assert!(!PassNumber::Recon.requires_claude());
        assert!(!PassNumber::Fuzzing.requires_claude());
        assert!(!PassNumber::Formal.requires_claude());
    }

    #[test]
    fn all_returns_six_passes_in_order() {
        let all = PassNumber::all();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], PassNumber::Recon);
        assert_eq!(all[1], PassNumber::Agents);
        assert_eq!(all[2], PassNumber::Poc);
        assert_eq!(all[3], PassNumber::Fuzzing);
        assert_eq!(all[4], PassNumber::Formal);
        assert_eq!(all[5], PassNumber::Review);
    }

    #[test]
    fn pass_numbers_are_ordered() {
        assert!(PassNumber::Recon < PassNumber::Agents);
        assert!(PassNumber::Agents < PassNumber::Poc);
        assert!(PassNumber::Poc < PassNumber::Fuzzing);
        assert!(PassNumber::Fuzzing < PassNumber::Formal);
        assert!(PassNumber::Formal < PassNumber::Review);
    }

    #[test]
    fn description_returns_non_empty_strings() {
        for pass in PassNumber::all() {
            assert!(!pass.description().is_empty());
        }
    }

    #[test]
    fn pass_status_display() {
        assert_eq!(PassStatus::Completed.to_string(), "completed");
        assert_eq!(PassStatus::Failed.to_string(), "FAILED");
        assert_eq!(PassStatus::Skipped.to_string(), "skipped");
        assert_eq!(PassStatus::NotImplemented.to_string(), "not implemented");
    }
}
