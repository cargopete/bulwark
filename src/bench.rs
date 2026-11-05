//! Recall benchmark: score a finished run against a contest's judged findings.
//!
//! A known bug is traced through every stage of the run, so a miss says which pass lost it:
//! never considered, considered and dismissed, reported raw, merged, past the scope and
//! known-issue gate, past the PoC gate, or in the final report.

use crate::error::{BulwarkError, Result};
use crate::workspace::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Expected {
    pub case: String,
    #[serde(default)]
    pub source: Option<String>,
    /// Judged High and Medium findings: the recall denominator.
    pub findings: Vec<Known>,
    /// Judged Lows and informationals, used only to label unmatched run findings.
    #[serde(default)]
    pub others: Vec<Known>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Known {
    pub id: String,
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub contracts: Vec<String>,
    #[serde(default)]
    pub functions: Vec<String>,
    pub root_cause: String,
}

/// Something the run produced that may describe a known bug.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub key: String,
    pub kind: Kind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An idea an agent wrote down and dismissed.
    Considered,
    /// An agent's (or the fuzzer's) reported finding, before the merge.
    Raw,
    /// A merged finding, keyed by its F-id.
    Merged,
}

/// How far a known bug got. Ordered: a later stage implies the earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Missed,
    Considered,
    Raw,
    Merged,
    PastScopeGate,
    PastPocGate,
    Final,
}

pub const STAGES: [Stage; 6] = [
    Stage::Considered,
    Stage::Raw,
    Stage::Merged,
    Stage::PastScopeGate,
    Stage::PastPocGate,
    Stage::Final,
];

/// The run's merged findings and which gates let each one through.
#[derive(Debug, Default)]
pub struct Gates {
    pub rejected: BTreeSet<String>,
    pub discarded: BTreeSet<String>,
    pub final_ids: BTreeSet<String>,
    /// Final severity by ID, after the review.
    pub final_severity: BTreeMap<String, String>,
}

const TEXT_CAP: usize = 700;

fn clip(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(TEXT_CAP) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn finding_text(v: &Value) -> String {
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    clip(&format!(
        "{} | {} :: {} | {}",
        field("title"),
        field("contract"),
        field("function"),
        if field("attack_scenario").is_empty() {
            field("impact")
        } else {
            field("attack_scenario")
        }
    ))
}

fn ids(v: Option<Value>) -> BTreeSet<String> {
    v.and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|f| f.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect()
}

fn files_matching(dir: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(&pred))
        .collect();
    out.sort();
    out
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

pub fn collect(ws: &Workspace) -> (Vec<Candidate>, Gates) {
    let mut cands = Vec::new();
    let findings = ws.findings_dir();

    for f in ids_and_values(read_json(&ws.merged_findings())) {
        cands.push(Candidate {
            key: f.0,
            kind: Kind::Merged,
            text: finding_text(&f.1),
        });
    }

    let raw_files = files_matching(&findings, |n| {
        n.ends_with("-raw.json") || (n.starts_with("red-part-") && n.ends_with("-candidates.json"))
    })
    .into_iter()
    .chain(std::iter::once(
        ws.fuzzing_dir().join("fuzzing-findings.json"),
    ));
    for file in raw_files {
        let Some(Value::Array(items)) = read_json(&file) else {
            continue;
        };
        for (i, v) in items.iter().enumerate() {
            cands.push(Candidate {
                key: format!("raw:{}:{i}", stem(&file)),
                kind: Kind::Raw,
                text: finding_text(v),
            });
        }
    }

    // Dismissed lists and coverage hypotheses have no fixed shape; the judge reads them whole.
    for file in files_matching(&findings, |n| n.ends_with("-dismissed.json")) {
        let Some(Value::Array(items)) = read_json(&file) else {
            continue;
        };
        for (i, v) in items.iter().enumerate() {
            cands.push(Candidate {
                key: format!("con:{}:{i}", stem(&file)),
                kind: Kind::Considered,
                text: clip(&v.to_string()),
            });
        }
    }
    for file in files_matching(&findings, |n| n.ends_with("-coverage.json")) {
        let hyps = read_json(&file)
            .and_then(|v| v.get("hypotheses").and_then(|h| h.as_array()).cloned())
            .unwrap_or_default();
        for (i, v) in hyps.iter().enumerate() {
            cands.push(Candidate {
                key: format!("con:{}:{i}", stem(&file)),
                kind: Kind::Considered,
                text: clip(&v.to_string()),
            });
        }
    }

    let mut rejected = ids(read_json(&ws.rejected_findings()));
    rejected.extend(ids(read_json(&ws.known_duplicates())));
    let finals =
        read_json(&ws.root().join("final-report.json")).and_then(|r| r.get("findings").cloned());
    let final_severity: BTreeMap<String, String> = finals
        .as_ref()
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some((
                f.get("id")?.as_str()?.to_string(),
                f.get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect();
    let gates = Gates {
        rejected,
        discarded: ids(read_json(&ws.discarded_findings())),
        final_ids: ids(finals),
        final_severity,
    };
    (cands, gates)
}

fn ids_and_values(v: Option<Value>) -> Vec<(String, Value)> {
    v.and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| Some((f.get("id")?.as_str()?.to_string(), f)))
        .collect()
}

/// The furthest stage reached by a known bug, given the candidates the judge matched to it.
pub fn stage_of(matched: &[&Candidate], gates: &Gates) -> Stage {
    let merged: Vec<&str> = matched
        .iter()
        .filter(|c| c.kind == Kind::Merged)
        .map(|c| c.key.as_str())
        .collect();
    let best_merged = merged
        .iter()
        .map(|id| {
            if gates.final_ids.contains(*id) {
                Stage::Final
            } else if gates.rejected.contains(*id) {
                Stage::Merged
            } else if gates.discarded.contains(*id) {
                Stage::PastScopeGate
            } else {
                // Survived the PoC gate; dropped by the review or the run stopped before the report.
                Stage::PastPocGate
            }
        })
        .max();
    if let Some(s) = best_merged {
        return s;
    }
    if matched.iter().any(|c| c.kind == Kind::Raw) {
        Stage::Raw
    } else if matched.iter().any(|c| c.kind == Kind::Considered) {
        Stage::Considered
    } else {
        Stage::Missed
    }
}

#[derive(Debug, Deserialize)]
struct Verdict {
    #[serde(default)]
    matches: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    unmatched: BTreeMap<String, String>,
}

pub fn judge_prompt(
    expected: &Expected,
    cands: &[Candidate],
    final_unmatched: &[&Candidate],
) -> String {
    let known = serde_json::to_string_pretty(&expected.findings).unwrap_or_default();
    let others = serde_json::to_string_pretty(&expected.others).unwrap_or_default();
    let list: String = cands
        .iter()
        .map(|c| format!("[{}] {}\n", c.key, c.text))
        .collect();
    let finals: Vec<&str> = final_unmatched.iter().map(|c| c.key.as_str()).collect();
    format!(
        "You are scoring an automated audit against a contest's judged findings. Do not use tools. \
         Reply with one JSON object and nothing else.\n\n\
         A candidate MATCHES a known finding only if it identifies the same root cause in the same \
         code. Same contract with a different mechanism is not a match. A vaguer description of the \
         same mechanism is a match. Candidates prefixed `con:` are ideas an agent considered and \
         dismissed; match them by the same rule.\n\n\
         ## Known findings (High and Medium)\n```json\n{known}\n```\n\n\
         ## Other judged issues (Low / informational)\n```json\n{others}\n```\n\n\
         ## Candidates\n{list}\n\
         ## Output\n\
         {{\"matches\": {{\"<known id>\": [\"<candidate key>\", ...]}}, \
         \"unmatched\": {{\"<final id>\": \"known_low\" | \"duplicate\" | \"plausible_new\" | \"invalid\"}}}}\n\n\
         List every known id under `matches`, with an empty array when nothing matches. Under \
         `unmatched`, label each of these final findings that matches no High or Medium: {finals:?}. \
         `known_low` if it matches an issue in the Low list, `duplicate` if it is a variant of a \
         known finding another candidate already matches (same root cause, another entry point), \
         `plausible_new` if it describes a real defect the contest missed, `invalid` otherwise.\n"
    )
}

/// Pull the first JSON object out of a reply that may wrap it in prose or a code fence.
fn parse_verdict(text: &str) -> Option<Verdict> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(&text[start..=end]).ok()
}

async fn ask_judge(claude: &Path, model: &str, prompt: &str) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(claude)
        .args([
            "-p",
            "--output-format",
            "json",
            "--max-turns",
            "1",
            "--model",
            model,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);
    let out = child.wait_with_output().await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    crate::tools::claude::parse_session_json(&stdout)
        .map(|r| r.text)
        .ok_or_else(|| BulwarkError::AgentFailed {
            agent: "bench-judge".into(),
            elapsed: 0,
            reason: String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(500)
                .collect(),
        })
}

fn run_cost(ws: &Workspace) -> f64 {
    read_json(&ws.cost_ledger())
        .and_then(|c| c.get("entries").and_then(|e| e.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.get("cost_usd").and_then(|c| c.as_f64()))
        .sum()
}

fn run_hours(ws: &Workspace) -> f64 {
    read_json(&ws.pipeline_status_file())
        .and_then(|s| s.get("passes").and_then(|p| p.as_object()).cloned())
        .unwrap_or_default()
        .values()
        .filter_map(|p| p.get("duration").and_then(|d| d.as_f64()))
        .sum::<f64>()
        / 3600.0
}

pub struct ScoreArgs<'a> {
    pub expected: &'a Path,
    pub workspace: &'a Path,
    pub out_dir: &'a Path,
    pub label: &'a str,
    pub claude: &'a Path,
    pub model: &'a str,
}

pub async fn score(a: ScoreArgs<'_>) -> Result<Value> {
    let expected: Expected = serde_json::from_str(&std::fs::read_to_string(a.expected)?)?;
    let ws = Workspace::new(a.workspace, "");
    let (cands, gates) = collect(&ws);
    let final_cands: Vec<&Candidate> = cands
        .iter()
        .filter(|c| c.kind == Kind::Merged && gates.final_ids.contains(&c.key))
        .collect();

    let prompt = judge_prompt(&expected, &cands, &final_cands);
    let reply = ask_judge(a.claude, a.model, &prompt).await?;
    let verdict = parse_verdict(&reply).ok_or_else(|| BulwarkError::AgentFailed {
        agent: "bench-judge".into(),
        elapsed: 0,
        reason: format!("no JSON verdict in reply: {}", clip(&reply)),
    })?;

    let by_key: BTreeMap<&str, &Candidate> = cands.iter().map(|c| (c.key.as_str(), c)).collect();
    let mut per_known = Vec::new();
    let mut stages = Vec::new();
    let mut matched_finals = BTreeSet::new();
    for k in &expected.findings {
        let keys = verdict.matches.get(&k.id).cloned().unwrap_or_default();
        let matched: Vec<&Candidate> = keys
            .iter()
            .filter_map(|key| by_key.get(key.as_str()).copied())
            .collect();
        for c in &matched {
            if c.kind == Kind::Merged && gates.final_ids.contains(&c.key) {
                matched_finals.insert(c.key.clone());
            }
        }
        let stage = stage_of(&matched, &gates);
        stages.push(stage);
        per_known.push(json!({
            "id": k.id,
            "severity": k.severity,
            "title": k.title,
            "stage": stage,
            "matched": keys,
        }));
    }

    let total = expected.findings.len().max(1) as f64;
    let reached: BTreeMap<String, usize> = STAGES
        .iter()
        .map(|s| {
            let n = stages.iter().filter(|st| *st >= s).count();
            (
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
                n,
            )
        })
        .collect();
    let unmatched_labels: BTreeMap<String, String> = final_cands
        .iter()
        .filter(|c| !matched_finals.contains(&c.key))
        .map(|c| {
            let label = verdict
                .unmatched
                .get(&c.key)
                .cloned()
                .unwrap_or_else(|| "unlabelled".into());
            (c.key.clone(), label)
        })
        .collect();
    // Judged reports rarely list Lows, so precision counts Medium and above, and a variant of a
    // bug the run already found is neither a hit nor a miss.
    let label = |c: &Candidate| unmatched_labels.get(&c.key).map(String::as_str);
    let counted: Vec<&Candidate> = final_cands
        .iter()
        .copied()
        .filter(|c| {
            let sev = gates
                .final_severity
                .get(&c.key)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            !sev.starts_with("low") && !sev.starts_with("info")
        })
        .filter(|c| label(c) != Some("duplicate"))
        .collect();
    let plausible = counted
        .iter()
        .filter(|c| label(c) == Some("plausible_new"))
        .count();
    let tp_counted = counted
        .iter()
        .filter(|c| matched_finals.contains(&c.key))
        .count();
    let n_counted = counted.len();
    let n_final = final_cands.len();
    let tp_final = matched_finals.len();
    let cost = run_cost(&ws);

    let result = json!({
        "case": expected.case,
        "label": a.label,
        "scored_at": chrono::Utc::now().to_rfc3339(),
        "known": expected.findings.len(),
        "reached": reached,
        "recall_final": reached.get("final").copied().unwrap_or(0) as f64 / total,
        "final_findings": n_final,
        "final_true_positives": tp_final,
        "final_findings_medium_plus": n_counted,
        "precision_final": if n_counted == 0 { Value::Null } else { json!((tp_counted + plausible) as f64 / n_counted as f64) },
        "unmatched_final": unmatched_labels,
        "cost_usd": cost,
        "cost_per_true_positive": if tp_final == 0 { Value::Null } else { json!(cost / tp_final as f64) },
        "hours": run_hours(&ws),
        "per_known": per_known,
        "candidates": cands.len(),
    });

    std::fs::create_dir_all(a.out_dir)?;
    let file = a
        .out_dir
        .join(format!("{}-{}.json", expected.case, a.label));
    std::fs::write(&file, serde_json::to_string_pretty(&result)?)?;
    std::fs::write(
        a.out_dir
            .join(format!("{}-{}.judge.txt", expected.case, a.label)),
        &reply,
    )?;
    Ok(result)
}

/// One markdown row per scored run, for bench/RESULTS.md.
pub fn table_row(r: &Value) -> String {
    let n = |k: &str| r["reached"][k].as_u64().unwrap_or(0);
    format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | ${:.0} | {:.1} |",
        r["label"].as_str().unwrap_or(""),
        r["case"].as_str().unwrap_or(""),
        r["known"],
        n("considered"),
        n("raw"),
        n("merged"),
        n("past_scope_gate"),
        n("past_poc_gate"),
        n("final"),
        r["final_findings"],
        r["precision_final"]
            .as_f64()
            .map(|p| format!("{:.0}%", p * 100.0))
            .unwrap_or("-".into()),
        r["cost_usd"].as_f64().unwrap_or(0.0),
        r["hours"].as_f64().unwrap_or(0.0),
    )
}

pub const TABLE_HEADER: &str = "| run | case | known H/M | considered | raw | merged | past scope | past PoC | final | final findings | precision | cost | hours |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|";

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(key: &str, kind: Kind) -> Candidate {
        Candidate {
            key: key.into(),
            kind,
            text: String::new(),
        }
    }

    #[test]
    fn stage_follows_the_furthest_merged_match() {
        let gates = Gates {
            rejected: ["F-002".into()].into(),
            discarded: ["F-003".into()].into(),
            final_ids: ["F-004".into()].into(),
            ..Default::default()
        };
        let c = |k| cand(k, Kind::Merged);
        assert_eq!(stage_of(&[&c("F-002")], &gates), Stage::Merged);
        assert_eq!(stage_of(&[&c("F-003")], &gates), Stage::PastScopeGate);
        assert_eq!(stage_of(&[&c("F-005")], &gates), Stage::PastPocGate);
        assert_eq!(stage_of(&[&c("F-002"), &c("F-004")], &gates), Stage::Final);
    }

    #[test]
    fn unmerged_matches_fall_back_to_raw_then_considered() {
        let gates = Gates::default();
        let raw = cand("raw:red-agent-raw:0", Kind::Raw);
        let con = cand("con:gold-agent-dismissed:1", Kind::Considered);
        assert_eq!(stage_of(&[&con, &raw], &gates), Stage::Raw);
        assert_eq!(stage_of(&[&con], &gates), Stage::Considered);
        assert_eq!(stage_of(&[], &gates), Stage::Missed);
    }

    #[test]
    fn verdict_survives_prose_and_fences() {
        let v = parse_verdict(
            "Here:\n```json\n{\"matches\":{\"H-01\":[\"F-001\"]},\"unmatched\":{}}\n```",
        )
        .unwrap();
        assert_eq!(v.matches["H-01"], vec!["F-001".to_string()]);
    }

    #[test]
    fn collect_reads_stages_from_a_workspace() {
        let dir = std::env::temp_dir().join(format!("bulwark-bench-{}", std::process::id()));
        let f = dir.join("findings");
        std::fs::create_dir_all(&f).unwrap();
        std::fs::create_dir_all(dir.join("pocs")).unwrap();
        let w = |p: PathBuf, s: &str| std::fs::write(p, s).unwrap();
        w(
            f.join("merged-deduplicated.json"),
            r#"[{"id":"F-001","title":"t"},{"id":"F-002","title":"u"}]"#,
        );
        w(f.join("red-agent-raw.json"), r#"[{"title":"r"}]"#);
        w(
            f.join("gold-agent-dismissed.json"),
            r#"[{"candidate":"c"}]"#,
        );
        w(
            f.join("hunt-x-agent-coverage.json"),
            r#"{"hypotheses":[{"idea":"i"}]}"#,
        );
        w(f.join("rejected.json"), r#"[{"id":"F-002"}]"#);
        w(
            dir.join("final-report.json"),
            r#"{"findings":[{"id":"F-001"}]}"#,
        );
        let (cands, gates) = collect(&Workspace::new(&dir, ""));
        let kinds: Vec<Kind> = cands.iter().map(|c| c.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Merged).count(), 2);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Raw).count(), 1);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Considered).count(), 2);
        assert!(gates.rejected.contains("F-002") && gates.final_ids.contains("F-001"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
