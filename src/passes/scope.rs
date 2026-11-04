//! Scope check at the start of Pass 3: a finding must sit on a listed asset, cite a listed
//! impact, and fall under no exclusion, or it is rejected before any PoC money is spent.

use crate::budget::{self, Allowance};
use crate::error::Result;
use crate::findings::Finding;
use crate::pipeline::pass::PipelineContext;
use crate::program::ProgramConfig;
use crate::tools::claude::{ClaudeSession, try_parse_json_array};
use console::style;
use serde_json::{Value, json};
use std::path::Path;

/// Part of Pass 3's money the classifier may use; it is one short session.
const CLASSIFIER_SHARE: f64 = 0.05;

#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub rule: String,
    pub by: &'static str,
    pub reason: String,
}

/// Deterministic checks. On success the finding's severity becomes its impact's severity.
pub fn rule_check(program: &ProgramConfig, f: &mut Finding) -> std::result::Result<(), Rejection> {
    let reject = |rule: &str, reason: String| Rejection {
        rule: rule.into(),
        by: "rule",
        reason,
    };
    let path = f.contract.to_lowercase();
    if path.ends_with(".t.sol")
        || path.ends_with(".s.sol")
        || path.split('/').any(|p| p == "test" || p == "script")
    {
        return Err(reject(
            "D-8",
            format!("{} is a test or script file", f.contract),
        ));
    }
    let asset = f
        .extra
        .get("asset")
        .and_then(|a| a.as_str())
        .unwrap_or(&f.contract)
        .to_string();
    if !program.has_asset(&asset) || !program.has_asset(&f.contract) {
        return Err(reject(
            "asset",
            format!("{} is not a listed asset", f.contract),
        ));
    }
    let cited = f.extra.get("impact").and_then(|i| i.as_str()).unwrap_or("");
    let Some(impact) = program.impact(cited) else {
        return Err(reject(
            "impact",
            if cited.is_empty() {
                "cites no listed impact".into()
            } else {
                format!("cites {cited}, which is not in the impact table")
            },
        ));
    };
    if impact.severity != f.severity {
        f.extra
            .insert("agent_severity".into(), json!(f.severity.to_string()));
        f.severity = impact.severity;
    }
    Ok(())
}

/// Run the gate; returns the findings that survive and writes `findings/rejected.json`.
pub async fn gate(
    ctx: &PipelineContext,
    program: &ProgramConfig,
    findings: Vec<Finding>,
    allowance: &Allowance,
    claude_bin: &Path,
) -> Result<Vec<Finding>> {
    let mut rejected: Vec<Value> = Vec::new();
    let mut survivors = Vec::new();
    for mut f in findings {
        match rule_check(program, &mut f) {
            Ok(()) => survivors.push(f),
            Err(r) => rejected.push(rejected_json(&f, &r)),
        }
    }
    eprintln!(
        "  Scope check: {} rejected by rule, {} to the classifier",
        rejected.len(),
        survivors.len()
    );

    if !survivors.is_empty() {
        let cap = allowance
            .left()
            .map(|l| (l * CLASSIFIER_SHARE).max(budget::MIN_SESSION_USD).min(l));
        match classify(ctx, program, &survivors, claude_bin, cap).await {
            Some(verdicts) => {
                let mut kept = Vec::new();
                for f in survivors {
                    match verdicts.iter().find(|v| v.0 == f.id) {
                        Some((_, rule, reason)) => rejected.push(rejected_json(
                            &f,
                            &Rejection {
                                rule: rule.clone(),
                                by: "classifier",
                                reason: reason.clone(),
                            },
                        )),
                        None => kept.push(f),
                    }
                }
                survivors = kept;
            }
            None => {
                // Fail open: an unchecked finding still reaches a human, a dropped one does not.
                eprintln!(
                    "  {} Scope classifier gave no verdicts; exclusions unchecked",
                    style("⚠").yellow()
                );
                for f in &mut survivors {
                    f.extra.insert("scope_check".into(), json!("unchecked"));
                }
            }
        }
    }

    for r in &rejected {
        eprintln!(
            "    {} {} rejected ({}): {}",
            style("✗").red(),
            r["id"].as_str().unwrap_or("?"),
            r["scope_rejection"]["rule"].as_str().unwrap_or("?"),
            r["scope_rejection"]["reason"].as_str().unwrap_or("")
        );
    }
    std::fs::write(
        ctx.workspace.rejected_findings(),
        serde_json::to_string_pretty(&rejected)?,
    )?;
    Ok(survivors)
}

fn rejected_json(f: &Finding, r: &Rejection) -> Value {
    let mut v = serde_json::to_value(f).unwrap_or_default();
    v["scope_rejection"] = json!({ "rule": r.rule, "by": r.by, "reason": r.reason });
    v
}

/// Ask a cheap model which findings fall under an exclusion. `None` means no usable answer.
async fn classify(
    ctx: &PipelineContext,
    program: &ProgramConfig,
    findings: &[Finding],
    claude_bin: &Path,
    cap: Option<f64>,
) -> Option<Vec<(String, String, String)>> {
    let rules: String = program
        .exclusions()
        .iter()
        .map(|(id, t)| format!("- {id}: {t}\n"))
        .collect();
    let items: Vec<Value> = findings
        .iter()
        .map(|f| {
            json!({
                "id": f.id, "title": f.title, "contract": f.contract,
                "function": f.function, "attack_scenario": f.attack_scenario,
            })
        })
        .collect();
    let out_file = ctx.workspace.findings_dir().join("scope-check.json");
    let _ = std::fs::remove_file(&out_file);
    let prompt = format!(
        "You are triaging bug bounty findings against a program's exclusion list. Do not read \
         any files; judge only from the descriptions.\n\n## Exclusions\n\n{rules}\n\
         A finding is excluded only if the attack as described depends on what a rule names: \
         for D-1, the attacker must hold or obtain a privileged role or key. A privileged role \
         merely existing in the protocol is not enough.\n\n## Findings\n\n```json\n{}\n```\n\n\
         Write a JSON array to `{}` with one object per finding: \
         {{\"id\": \"F-001\", \"excluded\": true|false, \"rule\": \"D-1\" or null, \"reason\": \"one sentence\"}}.",
        serde_json::to_string_pretty(&items).unwrap_or_default(),
        out_file.display()
    );
    let log_file = ctx.workspace.findings_logs_dir().join("scope-check.log");
    let session = ClaudeSession {
        claude_bin: claude_bin.to_path_buf(),
        prompt,
        max_turns: 3,
        working_dir: ctx.audit_dir.clone(),
        log_file,
        model: Some(ctx.config.model_for("scope_check")),
        allowed_tools: vec!["Write".into()],
        timeout_minutes: Some(10),
        label: "scope_check".into(),
        budget_usd: cap,
        ledger: Some(ctx.ledger.clone()),
    };
    let _ = session.run_with_spinner("Scope classifier...").await;

    let text = std::fs::read_to_string(&out_file).ok()?;
    let verdicts = first_json_array(&text)?;
    Some(parse_verdicts(&verdicts))
}

fn first_json_array(text: &str) -> Option<Vec<Value>> {
    text.match_indices('[')
        .find_map(|(i, _)| try_parse_json_array(&text[i..]))
}

/// Excluded findings as (id, rule, reason).
fn parse_verdicts(arr: &[Value]) -> Vec<(String, String, String)> {
    arr.iter()
        .filter(|v| v.get("excluded").and_then(|e| e.as_bool()) == Some(true))
        .filter_map(|v| {
            Some((
                v.get("id")?.as_str()?.to_string(),
                v.get("rule")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unspecified")
                    .to_string(),
                v.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Confidence, PocStatus, Severity};

    fn program() -> ProgramConfig {
        toml::from_str(
            r#"
[[assets]]
name = "Vault"
[[impacts]]
severity = "High"
text = "Temporary freezing of funds"
"#,
        )
        .unwrap()
    }

    fn finding(contract: &str, impact: Option<&str>) -> Finding {
        let mut extra = serde_json::Map::new();
        if let Some(i) = impact {
            extra.insert("impact".into(), json!(i));
        }
        Finding {
            id: "F-001".into(),
            source: "red-agent".into(),
            severity: Severity::Critical,
            confidence: Confidence::High,
            title: "t".into(),
            contract: contract.into(),
            function: "withdraw".into(),
            lines: vec![],
            property_violated: None,
            attack_scenario: "a".into(),
            poc_file: None,
            poc_status: PocStatus::NotAttempted,
            dedup_hash: String::new(),
            found_by: vec![],
            severity_disagreements: None,
            extra,
        }
    }

    #[test]
    fn severity_comes_from_the_cited_impact() {
        let mut f = finding("Vault.sol", Some("I-1"));
        rule_check(&program(), &mut f).unwrap();
        assert_eq!(f.severity, Severity::High);
        assert_eq!(f.extra["agent_severity"], json!("Critical"));
    }

    #[test]
    fn missing_or_unknown_impact_is_rejected() {
        let p = program();
        assert_eq!(
            rule_check(&p, &mut finding("Vault.sol", None))
                .unwrap_err()
                .rule,
            "impact"
        );
        assert_eq!(
            rule_check(&p, &mut finding("Vault.sol", Some("I-9")))
                .unwrap_err()
                .rule,
            "impact"
        );
    }

    #[test]
    fn unlisted_asset_and_test_files_are_rejected() {
        let p = program();
        assert_eq!(
            rule_check(&p, &mut finding("Router.sol", Some("I-1")))
                .unwrap_err()
                .rule,
            "asset"
        );
        assert_eq!(
            rule_check(&p, &mut finding("test/Vault.t.sol", Some("I-1")))
                .unwrap_err()
                .rule,
            "D-8"
        );
    }

    #[test]
    fn cited_asset_must_also_be_listed() {
        let mut f = finding("Vault.sol", Some("I-1"));
        f.extra.insert("asset".into(), json!("Router"));
        assert_eq!(rule_check(&program(), &mut f).unwrap_err().rule, "asset");
    }

    /// Spends real money (well under a cent on Haiku): `cargo test -- --ignored live_classifier`.
    #[tokio::test]
    #[ignore]
    async fn live_classifier_separates_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        let config: crate::config::Config = toml::from_str(
            r#"
[target]
repo = "x"
scope = ["src"]
core_contracts = []
[program]
[[program.assets]]
name = "Vault"
[[program.impacts]]
severity = "Critical"
text = "Direct theft of any user funds"
[program.out_of_scope]
program_specific = ["Frontrunning, including back-running and sandwich attacks"]
"#,
        )
        .unwrap();
        let workspace = crate::workspace::Workspace::new(dir.path(), "ws");
        workspace.init().unwrap();
        let ledger =
            crate::budget::Ledger::open(workspace.cost_ledger(), Some(0.50), true).unwrap();
        let ctx = PipelineContext {
            config: config.clone(),
            workspace,
            audit_dir: dir.path().to_path_buf(),
            bulwark_root: dir.path().to_path_buf(),
            bulwark_install: dir.path().to_path_buf(),
            ledger,
        };
        let mk = |id: &str, title: &str, scenario: &str| {
            let mut f = finding("Vault.sol", Some("I-1"));
            f.id = id.into();
            f.title = title.into();
            f.attack_scenario = scenario.into();
            f
        };
        let findings = vec![
            mk(
                "F-001",
                "Owner can sweep all deposits",
                "The owner calls sweep(owner) and receives every user's deposit; requires the owner key.",
            ),
            mk(
                "F-002",
                "Withdrawal sandwich",
                "An attacker watches the mempool for a large withdraw() and front-runs it with deposit(), then back-runs with withdraw(), capturing the price move.",
            ),
            mk(
                "F-003",
                "Missing auth on withdrawFor",
                "Any outsider calls withdrawFor(victim, attacker) and the Vault sends the victim's balance to the attacker; no role needed.",
            ),
        ];
        let program = config.program.as_ref().unwrap();
        let allowance = ctx.allowance("poc");
        let claude = which::which("claude").unwrap();
        let kept = gate(&ctx, program, findings, &allowance, &claude)
            .await
            .unwrap();
        let ids: Vec<&str> = kept.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["F-003"]);
        let rejected: Vec<Value> = serde_json::from_str(
            &std::fs::read_to_string(ctx.workspace.rejected_findings()).unwrap(),
        )
        .unwrap();
        let rules: Vec<&str> = rejected
            .iter()
            .map(|r| r["scope_rejection"]["rule"].as_str().unwrap())
            .collect();
        assert_eq!(rules, vec!["D-1", "P-1"]);
        eprintln!("spent ${:.4}", ctx.ledger.spent());
    }

    #[test]
    fn classifier_output_parses_from_prose() {
        let text = r#"Here you go:
[{"id":"F-001","excluded":true,"rule":"D-1","reason":"needs the owner key"},
 {"id":"F-002","excluded":false,"rule":null,"reason":"outsider can call it"}]
done"#;
        let v = parse_verdicts(&first_json_array(text).unwrap());
        assert_eq!(
            v,
            vec![("F-001".into(), "D-1".into(), "needs the owner key".into())]
        );
    }
}
