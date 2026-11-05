//! Submission drafts in the Immunefi form, one per reported finding.
//!
//! Immunefi bans AI-generated reports, so a draft is scaffolding for a hand-written report:
//! the facts the pipeline measured go in plain, every sentence a model wrote is quoted and
//! marked for rewriting, and a section the run could not fill says so.

use crate::config::Config;
use crate::program::ProgramConfig;
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const REWRITE: &str = "> **[rewrite: model-written]**";

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

/// Model text, quoted paragraph by paragraph under a rewrite marker.
fn drafted(text: &str) -> String {
    if text.trim().is_empty() {
        return "**[missing]** The run produced nothing for this section; write it.\n".into();
    }
    let body: String = text
        .split("\n\n")
        .map(|p| format!("> {}\n>\n", p.trim().replace('\n', "\n> ")))
        .collect();
    format!("{REWRITE}\n>\n{body}")
}

/// Agents list line numbers from several functions; one permalink per cluster reads better
/// than a single range across the file.
fn line_ranges(lines: &[u64]) -> Vec<(u64, u64)> {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for l in sorted {
        match out.last_mut() {
            Some((_, end)) if l <= *end + 25 => *end = l,
            _ => out.push((l, l)),
        }
    }
    out
}

/// `https://github.com/o/r` from a clone URL, https or ssh.
fn web_url(repo: &str) -> Option<String> {
    let r = repo.trim_end_matches(".git");
    if let Some(rest) = r.strip_prefix("git@github.com:") {
        return Some(format!("https://github.com/{rest}"));
    }
    r.starts_with("https://").then(|| r.to_string())
}

/// The path of a finding's contract inside the audited tree, relative to its root.
fn source_path(audit_dir: &Path, contract: &str) -> Option<String> {
    let file = if contract.ends_with(".sol") {
        contract.to_string()
    } else {
        format!("{contract}.sol")
    };
    let mut stack = vec![audit_dir.to_path_buf()];
    let mut hits: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !matches!(
                    name.as_str(),
                    "node_modules" | "lib" | "out" | "cache" | ".git" | "audit-workspace"
                ) && !name.starts_with("test")
                {
                    stack.push(p);
                }
            } else if name == file {
                hits.push(p);
            }
        }
    }
    hits.sort_by_key(|p| p.components().count());
    hits.first()
        .and_then(|p| p.strip_prefix(audit_dir).ok())
        .map(|p| p.display().to_string())
}

/// The best reward the program's table allows for this finding, when it can be known.
pub fn estimate_reward(
    severity: &str,
    program: Option<&ProgramConfig>,
    funds_at_risk_usd: Option<f64>,
) -> Option<u64> {
    let p = program?;
    if p.reward_formula == "pct_of_funds_at_risk" {
        if let (Some(pct), Some(funds)) = (p.reward_pct, funds_at_risk_usd) {
            let est = (funds * pct / 100.0) as u64;
            return Some(p.max_payout_usd.map_or(est, |cap| est.min(cap)));
        }
    }
    let key = severity.to_lowercase();
    p.rewards
        .iter()
        .find(|(k, _)| k.to_lowercase() == key)
        .map(|(_, v)| *v)
        .or_else(|| (key == "critical").then_some(p.max_payout_usd).flatten())
}

pub struct Inputs<'a> {
    pub finding: &'a Value,
    pub poc: Option<&'a Value>,
    pub poc_source: Option<&'a str>,
    pub program: Option<&'a ProgramConfig>,
    pub repo: &'a str,
    pub commit: &'a str,
    pub source_path: Option<String>,
}

pub fn render(i: &Inputs) -> String {
    let f = i.finding;
    let mut out = String::new();
    let sev = s(f, "severity");
    let status = s(f, "poc_status");
    let demonstrated =
        status.starts_with("compiles_and_demonstrates") || status == "demonstrated_local";
    let funds = f.get("funds_at_risk_usd").and_then(|x| x.as_f64());
    let reward = estimate_reward(sev, i.program, funds);

    let _ = writeln!(out, "# {}\n", s(f, "title"));
    let _ = writeln!(
        out,
        "Draft for {} · severity {} · PoC `{}` ({}) · estimated reward {}\n",
        s(f, "id"),
        sev,
        if status.is_empty() { "none" } else { status },
        f.get("poc_mode")
            .and_then(|m| m.as_str())
            .unwrap_or("mode unknown"),
        reward.map_or("unknown".to_string(), |r| format!("${r}")),
    );
    if !demonstrated {
        let _ = writeln!(
            out,
            "**NOT READY:** the PoC did not demonstrate impact. Immunefi requires a working PoC; \
             prove it on a fork before writing this up.\n"
        );
    }

    let _ = writeln!(out, "## Brief/Intro\n");
    out.push_str(&drafted(s(f, "title")));

    let _ = writeln!(out, "\n## Vulnerability Details\n");
    let lines: Vec<u64> = f
        .get("lines")
        .and_then(|l| l.as_array())
        .map_or(vec![], |a| a.iter().filter_map(|x| x.as_u64()).collect());
    let _ = writeln!(
        out,
        "Location: `{}` · `{}`{}\n",
        i.source_path.as_deref().unwrap_or(s(f, "contract")),
        s(f, "function"),
        if lines.is_empty() {
            String::new()
        } else {
            format!(" · lines {lines:?}")
        }
    );
    out.push_str(&drafted(s(f, "attack_scenario")));

    let _ = writeln!(out, "\n## Impact\n");
    let impact_id = s(f, "impact");
    match i.program.and_then(|p| p.impact(impact_id)) {
        Some(row) => {
            let _ = writeln!(
                out,
                "Program impact {impact_id}: **{}** ({})\n",
                row.text, row.severity
            );
        }
        None => {
            let _ = writeln!(
                out,
                "**[missing]** No program impact row is mapped. Pick the exact row from the program page.\n"
            );
        }
    }
    if let Some(text) = f.get("impact_detail").and_then(|x| x.as_str()) {
        out.push_str(&drafted(text));
    }

    let _ = writeln!(out, "\n## Risk Breakdown\n");
    let poc = i.poc;
    let capital: Vec<String> = poc
        .and_then(|p| p.get("capital"))
        .and_then(|c| c.as_array())
        .map_or(vec![], |a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        });
    let flags: Vec<String> = poc
        .and_then(|p| p.get("lint_flags"))
        .and_then(|c| c.as_array())
        .map_or(vec![], |a| {
            a.iter()
                .map(|x| format!("{} — {}", s(x, "cheat"), s(x, "detail")))
                .collect()
        });
    let _ = writeln!(
        out,
        "- Attacker capital (declared in the PoC): {}",
        if capital.is_empty() {
            "none declared".into()
        } else {
            capital.join("; ")
        }
    );
    let _ = writeln!(
        out,
        "- Funds at risk: {}",
        funds.map_or("**[missing]** not measured in USD".into(), |f| format!(
            "${f:.0}"
        ))
    );
    if !flags.is_empty() {
        let _ = writeln!(out, "- PoC cheatcode flags to resolve before submitting:");
        for fl in &flags {
            let _ = writeln!(out, "  - {fl}");
        }
    }
    let _ = writeln!(
        out,
        "- Likelihood and preconditions: **[missing]** state them from the PoC setup."
    );

    let _ = writeln!(out, "\n## Recommendation\n");
    out.push_str(&drafted(s(f, "recommendation")));

    let _ = writeln!(out, "\n## References\n");
    match (web_url(i.repo), &i.source_path) {
        (Some(web), Some(path)) => {
            for (a, b) in line_ranges(&lines) {
                let anchor = if a == b {
                    format!("#L{a}")
                } else {
                    format!("#L{a}-L{b}")
                };
                let _ = writeln!(out, "- {web}/blob/{}/{path}{anchor}", i.commit);
            }
            if lines.is_empty() {
                let _ = writeln!(out, "- {web}/blob/{}/{path}", i.commit);
            }
        }
        _ => {
            let _ = writeln!(out, "- **[missing]** permalink to the affected code");
        }
    }

    let _ = writeln!(out, "\n## Proof of Concept\n");
    if let Some(cmd) = poc.and_then(|p| p.get("command")).and_then(|c| c.as_str()) {
        let _ = writeln!(out, "```sh\n{cmd}\n```\n");
    }
    if let Some(tests) = poc.and_then(|p| p.get("tests")).and_then(|t| t.as_array()) {
        for t in tests {
            let _ = writeln!(
                out,
                "- `{}`: {} · attacker gain {} · victim loss {}",
                s(t, "name"),
                if t.get("passed").and_then(|p| p.as_bool()).unwrap_or(false) {
                    "passed"
                } else {
                    "failed"
                },
                t.get("attacker_gain")
                    .and_then(|x| x.as_str())
                    .unwrap_or("-"),
                t.get("victim_loss").and_then(|x| x.as_str()).unwrap_or("-"),
            );
        }
        out.push('\n');
    }
    match i.poc_source {
        Some(src) => {
            let _ = writeln!(
                out,
                "Model-written test; read every line and rewrite comments before submitting.\n\n```solidity\n{}\n```",
                src.trim_end()
            );
        }
        None => {
            let _ = writeln!(out, "**[missing]** No PoC test was produced.");
        }
    }
    out
}

/// Writes `submissions/<id>.md` for every finding in the final report (or, before Pass 6, every
/// finding that survived the PoC gate). Returns (id, estimated reward), best first.
pub fn write_all(
    config: &Config,
    audit_dir: &Path,
    workspace: &Path,
) -> std::io::Result<Vec<(String, String, Option<u64>)>> {
    let read = |p: PathBuf| -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    };
    let findings: Vec<Value> = read(workspace.join("final-report.json"))
        .and_then(|r| r.get("findings").cloned())
        .or_else(|| read(workspace.join("pocs/validated-findings.json")))
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let out_dir = workspace.join("submissions");
    std::fs::create_dir_all(&out_dir)?;
    let mut index = Vec::new();
    for f in &findings {
        let id = s(f, "id");
        if id.is_empty() {
            continue;
        }
        let poc = read(workspace.join("pocs").join(format!("{id}.poc.json")));
        let poc_source =
            std::fs::read_to_string(workspace.join("pocs").join(format!("{id}.t.sol"))).ok();
        let md = render(&Inputs {
            finding: f,
            poc: poc.as_ref(),
            poc_source: poc_source.as_deref(),
            program: config.program.as_ref(),
            repo: &config.target.repo,
            commit: config.target.commit.as_deref().unwrap_or("HEAD"),
            source_path: source_path(audit_dir, s(f, "contract")),
        });
        std::fs::write(out_dir.join(format!("{id}.md")), md)?;
        let funds = f.get("funds_at_risk_usd").and_then(|x| x.as_f64());
        index.push((
            id.to_string(),
            s(f, "title").to_string(),
            estimate_reward(s(f, "severity"), config.program.as_ref(), funds),
        ));
    }
    index.sort_by_key(|r| std::cmp::Reverse(r.2));
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn program() -> ProgramConfig {
        toml::from_str(
            r#"
reward_formula = "fixed"
max_payout_usd = 1000000
[rewards]
High = 15000
[[impacts]]
severity = "Critical"
text = "Direct theft of any user funds"
[[impacts]]
severity = "High"
text = "Temporary freezing of funds"
"#,
        )
        .unwrap()
    }

    #[test]
    fn reward_comes_from_the_table_or_the_formula() {
        let p = program();
        assert_eq!(estimate_reward("High", Some(&p), None), Some(15000));
        assert_eq!(estimate_reward("Critical", Some(&p), None), Some(1_000_000));
        assert_eq!(estimate_reward("Medium", Some(&p), None), None);
        let mut pct = p.clone();
        pct.reward_formula = "pct_of_funds_at_risk".into();
        pct.reward_pct = Some(10.0);
        assert_eq!(
            estimate_reward("Critical", Some(&pct), Some(50_000_000.0)),
            Some(1_000_000)
        );
        assert_eq!(
            estimate_reward("Critical", Some(&pct), Some(200_000.0)),
            Some(20_000)
        );
    }

    #[test]
    fn draft_marks_model_text_and_names_gaps() {
        let p = program();
        let f = json!({"id": "F-001", "title": "Donation freezes withdrawals", "severity": "High",
            "contract": "VaultCore.sol", "function": "_postRedeem", "lines": [339, 361],
            "attack_scenario": "Step one.\n\nStep two.", "impact": "I-2",
            "poc_status": "compiles_and_demonstrates", "poc_mode": "fork"});
        let poc = json!({"command": "forge test --match-path test/pocs/F-001.t.sol", "capital": ["deal(USDC, attacker, 1)"],
            "tests": [{"name": "test_x()", "passed": true, "attacker_gain": "0", "victim_loss": "10"}]});
        let md = render(&Inputs {
            finding: &f,
            poc: Some(&poc),
            poc_source: Some("contract P {}"),
            program: Some(&p),
            repo: "https://github.com/o/r.git",
            commit: "abc",
            source_path: Some("contracts/vault/VaultCore.sol".into()),
        });
        assert!(md.contains("estimated reward $15000"));
        assert!(md.contains("Program impact I-2: **Temporary freezing of funds**"));
        assert!(md.contains("> Step one.") && md.contains("> Step two."));
        assert!(md.matches("[rewrite: model-written]").count() >= 2);
        assert!(
            md.contains("https://github.com/o/r/blob/abc/contracts/vault/VaultCore.sol#L339-L361")
        );
        assert!(md.contains("Recommendation\n\n**[missing]**"));
        assert!(!md.contains("NOT READY"));
    }

    #[test]
    fn an_undemonstrated_finding_is_marked_not_ready() {
        let f = json!({"id": "F-2", "title": "t", "severity": "Medium", "poc_status": "compiles_but_inconclusive"});
        let md = render(&Inputs {
            finding: &f,
            poc: None,
            poc_source: None,
            program: None,
            repo: "",
            commit: "",
            source_path: None,
        });
        assert!(md.contains("NOT READY") && md.contains("No program impact row"));
    }
}
