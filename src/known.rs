//! Known-issue suppression: findings that match an audit item, an accepted risk or a past fix
//! are worth nothing on a bounty, so they are set aside before PoC money is spent on them.

use crate::budget::{self, Allowance};
use crate::error::Result;
use crate::findings::Finding;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::{ClaudeSession, try_parse_json_array};
use crate::tools::slither::TriagedResult;
use console::style;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Shares of Pass 3's money for building records and for matching.
const EXTRACT_SHARE: f64 = 0.05;
const MATCH_SHARE: f64 = 0.05;
/// Fix commits read from history; older ones are mostly covered by the audits.
const MAX_FIX_COMMITS: usize = 200;

pub const BUG_CLASSES: &[&str] = &[
    "access_control",
    "reentrancy",
    "rounding",
    "accounting",
    "oracle_manipulation",
    "price_manipulation",
    "initialization",
    "upgrade_storage",
    "signature_replay",
    "input_validation",
    "external_call",
    "token_integration",
    "dos",
    "frontrunning",
    "griefing",
    "logic",
    "other",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnownRecord {
    pub id: String,
    /// Where it came from: a file name, `KNOWN_ISSUES.md`, or `git:<sha>`.
    pub source: String,
    pub contract: String,
    #[serde(default)]
    pub function: String,
    pub bug_class: String,
    pub mechanism: String,
    /// fixed, acknowledged or accepted.
    #[serde(default)]
    pub status: String,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    inputs_sha256: String,
    records: Vec<KnownRecord>,
}

fn known_dir(ctx: &PipelineContext) -> PathBuf {
    ctx.workspace.root().join("known")
}

/// Copy audit reports into the workspace, so the session reads them inside its working dir.
fn stage_sources(ctx: &PipelineContext) -> Result<Vec<PathBuf>> {
    let dest = known_dir(ctx).join("sources");
    std::fs::create_dir_all(&dest)?;
    let mut staged = Vec::new();
    let audits = ctx.bulwark_root.join("context").join("audits");
    if let Ok(entries) = std::fs::read_dir(&audits) {
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let ok = p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "pdf" | "md" | "txt"));
            if ok {
                let to = dest.join(p.file_name().unwrap_or_default());
                std::fs::copy(&p, &to)?;
                staged.push(to);
            }
        }
    }
    let known_issues = ctx.audit_dir.join("KNOWN_ISSUES.md");
    if known_issues.exists() {
        staged.push(known_issues);
    }
    Ok(staged)
}

async fn fix_commits(ctx: &PipelineContext) -> String {
    let n = MAX_FIX_COMMITS.to_string();
    let out = crate::tools::run_command(
        "git",
        &[
            "log",
            "--no-merges",
            "-i",
            "-E",
            "--grep=fix|audit|vuln|security|exploit|C4|sherlock|H-[0-9]|M-[0-9]",
            "--pretty=format:commit %h %s",
            "--name-only",
            "-n",
            &n,
        ],
        &ctx.audit_dir,
    )
    .await;
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn inputs_hash(files: &[PathBuf], commits: &str) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update(f.file_name().unwrap_or_default().as_encoded_bytes());
        h.update(std::fs::read(f).unwrap_or_default());
    }
    h.update(commits.as_bytes());
    format!("{:x}", h.finalize())
}

/// Known records for this target: extracted ones, then the target's triaged Slither results.
pub async fn records(
    ctx: &PipelineContext,
    allowance: &Allowance,
    claude_bin: &Path,
) -> Result<Vec<KnownRecord>> {
    let mut records = extracted(ctx, allowance, claude_bin).await?;
    let triaged: Vec<TriagedResult> = std::fs::read_to_string(ctx.workspace.slither_triaged())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if !triaged.is_empty() {
        eprintln!("  Known issues: {} triaged Slither results", triaged.len());
    }
    records.extend(triaged_records(&triaged));
    Ok(records)
}

/// Triaged Slither results are already structured, so they need no model call.
fn triaged_records(triaged: &[TriagedResult]) -> Vec<KnownRecord> {
    triaged
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let description = t
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            KnownRecord {
                id: format!("S-{}", i + 1),
                source: "slither.db.json".into(),
                contract: t.contract.clone(),
                function: t.function.clone(),
                bug_class: slither_bug_class(&t.check).into(),
                mechanism: format!(
                    "Slither {} ({}): {}",
                    t.check,
                    t.impact,
                    description.chars().take(400).collect::<String>()
                ),
                status: "accepted".into(),
            }
        })
        .collect()
}

fn slither_bug_class(check: &str) -> &'static str {
    match check {
        c if c.starts_with("reentrancy") => "reentrancy",
        c if c.starts_with("arbitrary-send") => "access_control",
        "suicidal" | "unprotected-upgrade" | "protected-vars" | "tx-origin" => "access_control",
        "missing-zero-check" => "input_validation",
        "divide-before-multiply" => "rounding",
        "calls-loop" | "costly-loop" | "msg-value-loop" | "delegatecall-loop" => "dos",
        "unused-return"
        | "unchecked-transfer"
        | "unchecked-lowlevel"
        | "unchecked-send"
        | "low-level-calls"
        | "controlled-delegatecall" => "external_call",
        c if c.starts_with("uninitialized") => "initialization",
        "erc20-interface" | "erc721-interface" => "token_integration",
        "incorrect-equality" | "timestamp" | "weak-prng" | "tautology" | "boolean-cst" => "logic",
        _ => "other",
    }
}

/// Records extracted by a model from audits, KNOWN_ISSUES.md and fix commits,
/// rebuilt only when a source changes.
async fn extracted(
    ctx: &PipelineContext,
    allowance: &Allowance,
    claude_bin: &Path,
) -> Result<Vec<KnownRecord>> {
    let files = stage_sources(ctx)?;
    let commits = fix_commits(ctx).await;
    if files.is_empty() && commits.trim().is_empty() {
        return Ok(vec![]);
    }
    let hash = inputs_hash(&files, &commits);
    let cache_path = known_dir(ctx).join("records.json");
    if let Ok(text) = std::fs::read_to_string(&cache_path) {
        if let Ok(c) = serde_json::from_str::<Cache>(&text) {
            if c.inputs_sha256 == hash {
                eprintln!("  Known issues: {} records (cached)", c.records.len());
                return Ok(c.records);
            }
        }
    }

    let rel = |p: &PathBuf| {
        p.strip_prefix(&ctx.audit_dir)
            .unwrap_or(p)
            .display()
            .to_string()
    };
    let file_list: String = files.iter().map(|f| format!("- `{}`\n", rel(f))).collect();
    let out_file = known_dir(ctx).join("records-raw.json");
    let _ = std::fs::remove_file(&out_file);
    let prompt = format!(
        "Extract every known issue from these sources into a JSON array, one object per issue. \
         Read each file in full.\n\n## Files\n\n{file_list}\n## Fix commits from git history\n\n\
         ```\n{commits}\n```\n\nFor a commit, record it only if the message or the files show \
         which contract and bug it fixed; use source `git:<sha>`.\n\nEach object: \
         {{\"source\": \"<file name or git:sha>\", \"contract\": \"<contract name>\", \
         \"function\": \"<function or empty>\", \"bug_class\": one of {classes}, \
         \"mechanism\": \"<one sentence: what goes wrong>\", \"status\": \"fixed|acknowledged|accepted\"}}.\n\n\
         Write the array to `{}`.",
        out_file.display(),
        classes = BUG_CLASSES.join(", "),
    );
    let session = ClaudeSession {
        claude_bin: claude_bin.to_path_buf(),
        prompt,
        max_turns: 30,
        working_dir: ctx.audit_dir.clone(),
        log_file: known_dir(ctx).join("extract.log"),
        model: Some(ctx.config.model_for("known_extract")),
        allowed_tools: vec![],
        timeout_minutes: Some(30),
        label: "known_extract".into(),
        budget_usd: share(allowance, EXTRACT_SHARE),
        ledger: Some(ctx.ledger.clone()),
        resume: None,
    };
    let _ = session.run_with_spinner("Extracting known issues...").await;

    let text = std::fs::read_to_string(&out_file).unwrap_or_default();
    let records: Vec<KnownRecord> = first_json_array(&text)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<KnownRecord>(with_id(v)).ok())
        .enumerate()
        .map(|(i, mut r)| {
            r.id = format!("K-{}", i + 1);
            r
        })
        .collect();
    if records.is_empty() {
        eprintln!(
            "  {} Known-issue extraction produced no records; nothing will be suppressed",
            style("⚠").yellow()
        );
        // Not cached, so the next run tries again.
        return Ok(records);
    }
    std::fs::write(
        &cache_path,
        serde_json::to_string_pretty(&Cache {
            inputs_sha256: hash,
            records: records.clone(),
        })?,
    )?;
    eprintln!("  Known issues: {} records extracted", records.len());
    Ok(records)
}

fn with_id(mut v: Value) -> Value {
    if v.get("id").is_none() {
        v["id"] = json!("");
    }
    v
}

fn share(allowance: &Allowance, fraction: f64) -> Option<f64> {
    allowance
        .left()
        .map(|l| (l * fraction).max(budget::MIN_SESSION_USD).min(l))
}

fn first_json_array(text: &str) -> Option<Vec<Value>> {
    text.match_indices('[')
        .find_map(|(i, _)| try_parse_json_array(&text[i..]))
}

fn stem(contract: &str) -> String {
    contract
        .rsplit('/')
        .next()
        .unwrap_or(contract)
        .trim_end_matches(".sol")
        .to_lowercase()
}

/// Known records on the same contract: the only ones a finding can duplicate.
pub fn candidates<'a>(f: &Finding, records: &'a [KnownRecord]) -> Vec<&'a KnownRecord> {
    let s = stem(&f.contract);
    records.iter().filter(|r| stem(&r.contract) == s).collect()
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Duplicate { of: String, reason: String },
    Near { of: Vec<String>, reason: String },
    Distinct,
}

fn parse_verdicts(arr: &[Value]) -> Vec<(String, Verdict)> {
    arr.iter()
        .filter_map(|v| {
            let id = v.get("id")?.as_str()?.to_string();
            let of: Vec<String> = match v.get("known") {
                Some(Value::String(s)) => vec![s.clone()],
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect(),
                _ => vec![],
            };
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            let verdict = match v.get("verdict").and_then(|x| x.as_str()) {
                Some("duplicate") if !of.is_empty() => Verdict::Duplicate {
                    of: of[0].clone(),
                    reason,
                },
                Some("near") if !of.is_empty() => Verdict::Near { of, reason },
                _ => Verdict::Distinct,
            };
            Some((id, verdict))
        })
        .collect()
}

/// Set aside duplicates, flag near matches; returns the findings that go on.
pub async fn suppress(
    ctx: &PipelineContext,
    findings: Vec<Finding>,
    allowance: &Allowance,
    claude_bin: &Path,
) -> Result<Vec<Finding>> {
    let records = records(ctx, allowance, claude_bin).await?;
    let dup_path = ctx.workspace.known_duplicates();
    if records.is_empty() {
        std::fs::write(&dup_path, "[]")?;
        return Ok(findings);
    }

    let pairs: Vec<Value> = findings
        .iter()
        .filter_map(|f| {
            let c = candidates(f, &records);
            (!c.is_empty()).then(|| {
                json!({
                    "finding": {"id": f.id, "title": f.title, "contract": f.contract,
                                "function": f.function, "attack_scenario": f.attack_scenario},
                    "known": c,
                })
            })
        })
        .collect();
    eprintln!(
        "  Known issues: {} of {} findings share a contract with a known record",
        pairs.len(),
        findings.len()
    );
    let verdicts = if pairs.is_empty() {
        vec![]
    } else {
        adjudicate(ctx, &pairs, allowance, claude_bin).await
    };

    let mut kept = Vec::new();
    let mut dups = Vec::new();
    for mut f in findings {
        match verdicts.iter().find(|(id, _)| *id == f.id).map(|(_, v)| v) {
            Some(Verdict::Duplicate { of, reason }) => {
                eprintln!(
                    "    {} {} duplicates {of}: {reason}",
                    style("✗").red(),
                    f.id
                );
                let mut v = serde_json::to_value(&f).unwrap_or_default();
                v["duplicate_of"] = json!(of);
                v["duplicate_reason"] = json!(reason);
                dups.push(v);
            }
            Some(Verdict::Near { of, reason }) => {
                eprintln!(
                    "    {} {} is near {}: {reason}",
                    style("~").yellow(),
                    f.id,
                    of.join(", ")
                );
                f.extra.insert("near_known".into(), json!(of));
                f.extra.insert("near_known_reason".into(), json!(reason));
                kept.push(f);
            }
            _ => kept.push(f),
        }
    }
    std::fs::write(&dup_path, serde_json::to_string_pretty(&dups)?)?;
    Ok(kept)
}

async fn adjudicate(
    ctx: &PipelineContext,
    pairs: &[Value],
    allowance: &Allowance,
    claude_bin: &Path,
) -> Vec<(String, Verdict)> {
    let out_file = known_dir(ctx).join("match.json");
    let _ = std::fs::remove_file(&out_file);
    let prompt = format!(
        "Each finding below comes with known issues on the same contract, from audits, \
         accepted risks and past fixes. Decide for each finding:\n\n\
         - `duplicate`: same root cause in the same code path as a known issue. A bounty \
           program would reject it as already known.\n\
         - `near`: same bug class or nearby code, but a different root cause, path or \
           precondition. Say exactly what differs.\n\
         - `distinct`: unrelated.\n\n\
         A known issue marked `fixed` that the finding shows still reachable is `near`, not \
         `duplicate`: an incomplete fix is a new bug. When unsure between duplicate and near, \
         choose near. Read the source if you need to.\n\n```json\n{}\n```\n\n\
         Write a JSON array to `{}`: \
         {{\"id\": \"F-001\", \"verdict\": \"duplicate|near|distinct\", \"known\": [\"K-3\"], \
         \"reason\": \"one sentence\"}}.",
        serde_json::to_string_pretty(pairs).unwrap_or_default(),
        out_file.display()
    );

    let session = ClaudeSession {
        claude_bin: claude_bin.to_path_buf(),
        prompt,
        max_turns: 20,
        working_dir: ctx.audit_dir.clone(),
        log_file: known_dir(ctx).join("match.log"),
        model: Some(ctx.config.model_for("known_match")),
        allowed_tools: vec![],
        timeout_minutes: Some(30),
        label: "known_match".into(),
        budget_usd: share(allowance, MATCH_SHARE),
        ledger: Some(ctx.ledger.clone()),
        resume: None,
    };
    let _ = session
        .run_with_spinner("Matching against known issues...")
        .await;
    // Fail open: a missed duplicate costs a PoC, a false one loses a bug.
    let text = match std::fs::read_to_string(&out_file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "  {} Known-issue matcher failed: {}: {e}; nothing suppressed",
                style("✗").red(),
                out_file.display()
            );
            return vec![];
        }
    };
    match first_json_array(&text) {
        Some(arr) => parse_verdicts(&arr),
        None => {
            eprintln!(
                "  {} Known-issue matcher gave no verdicts; nothing suppressed",
                style("⚠").yellow()
            );
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Confidence, PocStatus, Severity};

    fn record(id: &str, contract: &str) -> KnownRecord {
        KnownRecord {
            id: id.into(),
            source: "audit.pdf".into(),
            contract: contract.into(),
            function: "withdraw".into(),
            bug_class: "rounding".into(),
            mechanism: "m".into(),
            status: "fixed".into(),
        }
    }

    fn finding(contract: &str) -> Finding {
        Finding {
            id: "F-001".into(),
            source: "red-agent".into(),
            severity: Severity::High,
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
            extra: serde_json::Map::new(),
        }
    }

    /// Spends real money (Sonnet then Opus, capped at $1): `cargo test -- --ignored live_known`.
    #[tokio::test]
    #[ignore]
    async fn live_known_issue_suppression() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("KNOWN_ISSUES.md"),
            "# Known issues\n\n## KI-1 Vault.withdraw rounds shares down\nwithdraw() in Vault \
             rounds the share burn down, so a user can withdraw slightly more than their \
             shares are worth by splitting withdrawals into dust amounts. Accepted risk.\n\n\
             ## KI-2 Router fee can be set to 100%\nThe owner can set Router.fee to 100%. Accepted.\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/Vault.sol"), "contract Vault {}\n").unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "."]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "fix: Vault.deposit reentrancy via ERC777 hook (audit H-01)",
        ]);

        let config: crate::config::Config =
            toml::from_str("[target]\nrepo = \"x\"\nscope = [\"src\"]\ncore_contracts = []\n")
                .unwrap();
        let workspace = crate::workspace::Workspace::new(root, "ws");
        workspace.init().unwrap();
        let ledger = crate::budget::Ledger::open(workspace.cost_ledger(), Some(1.0), true).unwrap();
        let ctx = PipelineContext {
            config,
            workspace,
            audit_dir: root.to_path_buf(),
            bulwark_root: root.to_path_buf(),
            bulwark_install: root.to_path_buf(),
            ledger,
        };
        let mk = |id: &str, contract: &str, function: &str, scenario: &str| {
            let mut f = finding(contract);
            f.id = id.into();
            f.function = function.into();
            f.title = scenario.chars().take(60).collect();
            f.attack_scenario = scenario.into();
            f
        };
        let findings = vec![
            mk(
                "F-001",
                "Vault.sol",
                "withdraw",
                "Splitting a withdrawal into many dust withdraw() calls lets the user take more \
                than their shares are worth, because the share burn rounds down.",
            ),
            mk(
                "F-002",
                "Vault.sol",
                "redeem",
                "redeem() computes assets with the stale pre-fee totalAssets, so a redeemer \
                front of a harvest receives fees owed to other holders.",
            ),
            mk(
                "F-003",
                "Staking.sol",
                "claim",
                "claim() never zeroes the reward debt, so any staker can claim the same rewards repeatedly.",
            ),
        ];
        let allowance = ctx.allowance("poc");
        let claude = which::which("claude").unwrap();
        let kept = suppress(&ctx, findings, &allowance, &claude).await.unwrap();

        let recs: Value = serde_json::from_str(
            &std::fs::read_to_string(known_dir(&ctx).join("records.json")).unwrap(),
        )
        .unwrap();
        eprintln!(
            "records: {}",
            serde_json::to_string_pretty(&recs["records"]).unwrap()
        );
        let ids: Vec<&str> = kept.iter().map(|f| f.id.as_str()).collect();
        eprintln!("kept {ids:?}; spent ${:.3}", ctx.ledger.spent());
        assert_eq!(ids, vec!["F-002", "F-003"]);
        let dups: Vec<Value> = serde_json::from_str(
            &std::fs::read_to_string(ctx.workspace.known_duplicates()).unwrap(),
        )
        .unwrap();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0]["id"], "F-001");
    }

    #[test]
    fn candidates_match_contract_by_stem() {
        let recs = vec![
            record("K-1", "Vault"),
            record("K-2", "src/Vault.sol"),
            record("K-3", "Router"),
        ];
        let ids: Vec<&str> = candidates(&finding("contracts/Vault.sol"), &recs)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, vec!["K-1", "K-2"]);
    }

    #[test]
    fn verdicts_need_a_known_id_to_suppress() {
        let arr: Vec<Value> = serde_json::from_str(
            r#"[{"id":"F-1","verdict":"duplicate","known":["K-2"],"reason":"same"},
                {"id":"F-2","verdict":"duplicate","known":[],"reason":"vague"},
                {"id":"F-3","verdict":"near","known":"K-1","reason":"other path"},
                {"id":"F-4","verdict":"distinct"}]"#,
        )
        .unwrap();
        let v = parse_verdicts(&arr);
        assert_eq!(
            v[0].1,
            Verdict::Duplicate {
                of: "K-2".into(),
                reason: "same".into()
            }
        );
        assert_eq!(v[1].1, Verdict::Distinct);
        assert_eq!(
            v[2].1,
            Verdict::Near {
                of: vec!["K-1".into()],
                reason: "other path".into()
            }
        );
        assert_eq!(v[3].1, Verdict::Distinct);
    }

    #[test]
    fn hash_changes_with_any_source() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.md");
        std::fs::write(&a, "one").unwrap();
        let h1 = inputs_hash(std::slice::from_ref(&a), "c1");
        std::fs::write(&a, "two").unwrap();
        let h2 = inputs_hash(std::slice::from_ref(&a), "c1");
        let h3 = inputs_hash(&[a], "c2");
        assert_ne!(h1, h2);
        assert_ne!(h2, h3);
    }

    #[test]
    fn triaged_slither_results_become_accepted_records() {
        let db: Value =
            serde_json::from_str(include_str!("tools/testdata/slither.db.json")).unwrap();
        let recs = triaged_records(&crate::tools::slither::parse_triage_db(&db));
        assert_eq!(recs.len(), 4);
        let r = &recs[0];
        assert_eq!(r.id, "S-1");
        assert_eq!(r.source, "slither.db.json");
        assert_eq!(r.contract, "SyncRedeemHandler");
        assert_eq!(r.function, "__validateSharePriceTimestamp");
        assert_eq!(r.bug_class, "logic");
        assert_eq!(r.status, "accepted");
        assert!(
            r.mechanism
                .starts_with("Slither timestamp (Low): SyncRedeemHandler.")
        );
        assert!(!r.mechanism.contains('\n'));
        assert_eq!(recs[1].bug_class, "initialization");
        assert_eq!(recs[2].bug_class, "access_control");
        assert!(BUG_CLASSES.contains(&recs[3].bug_class.as_str()));
        assert_eq!(
            candidates(&finding("src/global/Global.sol"), &recs)[0].id,
            "S-3"
        );
    }

    #[test]
    fn extracted_objects_without_id_still_parse() {
        let v: Value = serde_json::from_str(
            r#"{"source":"a.pdf","contract":"Vault","bug_class":"rounding","mechanism":"m"}"#,
        )
        .unwrap();
        let r: KnownRecord = serde_json::from_value(with_id(v)).unwrap();
        assert_eq!(r.contract, "Vault");
        assert_eq!(r.function, "");
    }
}
