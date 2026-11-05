//! Bounty program scope: assets, the impact table, and what the program will not pay for.

use crate::findings::Severity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramConfig {
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default = "default_severity_system")]
    pub severity_system: String,
    #[serde(default)]
    pub max_payout_usd: Option<u64>,
    /// "fixed", or "pct_of_funds_at_risk" with `reward_pct` and `max_payout_usd` as the cap.
    #[serde(default = "default_reward_formula")]
    pub reward_formula: String,
    #[serde(default)]
    pub reward_pct: Option<f64>,
    #[serde(default = "default_true")]
    pub requires_poc: bool,
    #[serde(default)]
    pub requires_kyc: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub impacts: Vec<Impact>,
    /// Maximum payout per severity from the program page, e.g. `High = 15000`.
    #[serde(default)]
    pub rewards: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub out_of_scope: OutOfScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    /// Contract name as it appears in source, e.g. "Vault". Empty after an import.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub chain_id: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Impact {
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutOfScope {
    /// The program's own exclusions, in its words.
    #[serde(default)]
    pub program_specific: Vec<String>,
}

fn default_platform() -> String {
    "immunefi".into()
}
fn default_severity_system() -> String {
    "v2.3".into()
}
fn default_reward_formula() -> String {
    "fixed".into()
}
fn default_true() -> bool {
    true
}
fn default_kind() -> String {
    "smart_contract".into()
}

/// Immunefi v2.3 default exclusions, always applied.
pub const DEFAULT_EXCLUSIONS: &[(&str, &str)] = &[
    (
        "D-1",
        "Attacks that require access to privileged addresses (governance, owner, admin, operator roles) or leaked keys",
    ),
    ("D-2", "Centralization risks"),
    ("D-3", "Sybil attacks"),
    (
        "D-4",
        "Basic economic or governance attacks, such as a 51% attack",
    ),
    (
        "D-5",
        "Incorrect data supplied by third-party oracles (oracle manipulation and flash-loan attacks remain in scope)",
    ),
    ("D-6", "Lack of liquidity"),
    ("D-7", "Best-practice recommendations and feature requests"),
    ("D-8", "Issues only in test or configuration files"),
    (
        "D-9",
        "Findings without a demonstrated impact on funds or protocol operation",
    ),
];

impl ProgramConfig {
    /// Impact ids are positional: `I-1` is the first `[[program.impacts]]` row.
    pub fn impact(&self, id: &str) -> Option<&Impact> {
        let n: usize = id.strip_prefix("I-")?.parse().ok()?;
        self.impacts.get(n.checked_sub(1)?)
    }

    pub fn exclusions(&self) -> Vec<(String, String)> {
        DEFAULT_EXCLUSIONS
            .iter()
            .map(|(id, t)| (id.to_string(), t.to_string()))
            .chain(
                self.out_of_scope
                    .program_specific
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (format!("P-{}", i + 1), t.clone())),
            )
            .collect()
    }

    /// Whether a finding's contract (a file name or a contract name) is a listed asset.
    pub fn has_addresses(&self) -> bool {
        self.assets
            .iter()
            .any(|a| a.address.as_deref().is_some_and(|s| !s.trim().is_empty()))
    }

    /// With no named assets every contract counts.
    pub fn has_asset(&self, contract: &str) -> bool {
        if self.assets.iter().all(|a| a.name.is_empty()) {
            return true;
        }
        let file = contract.rsplit('/').next().unwrap_or(contract);
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        self.assets.iter().any(|a| a.name == stem)
    }

    /// The impact table alone, for passes that judge severity.
    pub fn impact_table(&self) -> String {
        let mut s = String::from("\n\n## Bounty Program Impact Table\n\n");
        for (i, imp) in self.impacts.iter().enumerate() {
            s.push_str(&format!("- I-{} [{}] {}\n", i + 1, imp.severity, imp.text));
        }
        s
    }

    /// The scope section appended to Pass 2 agent prompts.
    pub fn prompt_section(&self) -> String {
        let mut s = String::from(
            "\n\n---\n\n## Bounty Program Scope\n\n\
             Only findings that fit this program are worth anything. Severity is set by the \
             impact you cite, not by your own rating.\n\n### Impacts in scope\n\n",
        );
        for (i, imp) in self.impacts.iter().enumerate() {
            s.push_str(&format!("- I-{} [{}] {}\n", i + 1, imp.severity, imp.text));
        }
        let named: Vec<&str> = self
            .assets
            .iter()
            .map(|a| a.name.as_str())
            .filter(|n| !n.is_empty())
            .collect();
        if !named.is_empty() {
            s.push_str("\n### Assets in scope\n\n");
            for n in named {
                s.push_str(&format!("- {n}\n"));
            }
        }
        s.push_str("\n### Out of scope (not paid, do not report)\n\n");
        for (id, t) in self.exclusions() {
            s.push_str(&format!("- {id} {t}\n"));
        }
        s.push_str(
            "\nEvery finding MUST include `\"impact\": \"I-<n>\"` naming the one impact above it \
             achieves, and `\"asset\": \"<name>\"` naming the in-scope contract it is on. A finding \
             that fits no listed impact, or falls under an exclusion, is not a finding here; leave it out.\n",
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program() -> ProgramConfig {
        toml::from_str(
            r#"
slug = "example"
[[assets]]
name = "Vault"
[[impacts]]
severity = "Critical"
text = "Direct theft of any user funds"
[[impacts]]
severity = "High"
text = "Temporary freezing of funds"
[out_of_scope]
program_specific = ["Frontrunning, including back-running and sandwich attacks"]
"#,
        )
        .unwrap()
    }

    #[test]
    fn impacts_are_numbered_from_one() {
        let p = program();
        assert_eq!(p.impact("I-1").unwrap().severity, Severity::Critical);
        assert_eq!(p.impact("I-2").unwrap().severity, Severity::High);
        assert!(p.impact("I-0").is_none());
        assert!(p.impact("I-3").is_none());
        assert!(p.impact("critical").is_none());
    }

    #[test]
    fn program_exclusions_follow_the_defaults() {
        let ex = program().exclusions();
        assert_eq!(ex.len(), DEFAULT_EXCLUSIONS.len() + 1);
        assert_eq!(ex.last().unwrap().0, "P-1");
    }

    #[test]
    fn asset_matches_file_or_contract_name() {
        let p = program();
        assert!(p.has_asset("Vault"));
        assert!(p.has_asset("Vault.sol"));
        assert!(p.has_asset("src/core/Vault.sol"));
        assert!(p.has_asset("contracts/Vault.vy"));
        assert!(!p.has_asset("VaultHelper.sol"));
    }

    #[test]
    fn source_scoped_program_has_no_addresses() {
        let mut p = program();
        assert!(!p.has_addresses());
        p.assets[0].address = Some(" ".into());
        assert!(!p.has_addresses());
        p.assets[0].address = Some("0xabc".into());
        assert!(p.has_addresses());
    }

    #[test]
    fn prompt_lists_ids_agents_must_cite() {
        let s = program().prompt_section();
        assert!(s.contains("I-1 [Critical] Direct theft"));
        assert!(s.contains("P-1 Frontrunning"));
        assert!(s.contains("\"impact\": \"I-<n>\""));
    }
}
