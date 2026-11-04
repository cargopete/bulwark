//! Run state for the dashboard, read fresh from the workspace on every request.

use crate::budget::Entry;
use crate::coverage::{self, Thresholds};
pub use crate::coverage::{Coverage, coverage_stats};
use crate::pipeline::checkpoint::PipelineStatus;
use crate::workspace::Workspace;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const SILENT_RED_SECS: u64 = 15 * 60;
pub const SILENT_AMBER_SECS: u64 = 5 * 60;

const PERSONA_STAGES: [&str; 10] = [
    "red",
    "blue",
    "gold",
    "hunter",
    "poc",
    "variant_analysis",
    "scv_scan",
    "fuzzing",
    "formal",
    "review",
];
const PASSES: [&str; 6] = ["recon", "agents", "poc", "fuzzing", "formal", "review"];
const LIVE_SUFFIX: &str = ".live.jsonl";
const LAST_TOOL_CHARS: usize = 140;
const LAST_SAID_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Green,
    Amber,
    Red,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Done,
    Stalled,
    Failed,
}

/// What a session's event stream says it has done so far.
#[derive(Debug, Default, Serialize)]
pub struct Activity {
    pub model: Option<String>,
    pub turns: u64,
    pub tool_calls: BTreeMap<String, u64>,
    pub total_tool_calls: u64,
    pub bash_commands: u64,
    pub forge_test_runs: u64,
    pub skills: Vec<String>,
    pub permission_denials: u64,
    /// Each stall restart appends a fresh `init` to the same live log.
    pub restarts: u64,
    pub last_tool: Option<String>,
    /// The latest narration between tool calls, flattened to one line.
    pub last_said: Option<String>,
    #[serde(skip)]
    pub result: Option<SessionEnd>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SessionEnd {
    pub subtype: String,
    pub num_turns: Option<u64>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub permission_denials: u64,
}

pub fn parse_events(text: &str) -> Activity {
    let mut a = Activity::default();
    let mut inits = 0u64;
    let mut message_ids = BTreeSet::new();
    let mut denied_in_stream = 0u64;
    for line in text.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match ev.get("type").and_then(Value::as_str) {
            Some("system") if ev.get("subtype").and_then(Value::as_str) == Some("init") => {
                inits += 1;
                // A session waiting on background tasks ends a turn with a `result`, then resumes
                // with a fresh `init`: that is a continuation, not the end and not a restart.
                if inits > 1 && a.result.take().is_none() {
                    a.restarts += 1;
                }
                if let Some(m) = ev.get("model").and_then(Value::as_str) {
                    a.model = Some(m.to_string());
                }
            }
            Some("assistant") => {
                let msg = &ev["message"];
                // The stream can split one message across events that share an id.
                match msg.get("id").and_then(Value::as_str) {
                    Some(id) => {
                        if message_ids.insert(id.to_string()) {
                            a.turns += 1;
                        }
                    }
                    None => a.turns += 1,
                }
                for block in content_blocks(msg) {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => record_tool(&mut a, block),
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !text.is_empty() {
                                a.last_said = Some(truncate(&text, LAST_SAID_CHARS));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                denied_in_stream += content_blocks(&ev["message"])
                    .filter(|b| is_permission_denial(b))
                    .count() as u64;
            }
            Some("result") => {
                let denials = ev
                    .get("permission_denials")
                    .and_then(Value::as_array)
                    .map_or(0, |d| d.len() as u64);
                a.result = Some(SessionEnd {
                    subtype: ev
                        .get("subtype")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    num_turns: ev.get("num_turns").and_then(Value::as_u64),
                    cost_usd: ev.get("total_cost_usd").and_then(Value::as_f64),
                    duration_ms: ev.get("duration_ms").and_then(Value::as_u64),
                    permission_denials: denials,
                });
            }
            _ => {}
        }
    }
    let reported = a.result.as_ref().map_or(0, |r| r.permission_denials);
    a.permission_denials = denied_in_stream.max(reported);
    a
}

fn content_blocks(msg: &Value) -> impl Iterator<Item = &Value> {
    msg.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn record_tool(a: &mut Activity, block: &Value) {
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let input = &block["input"];
    *a.tool_calls.entry(name.clone()).or_default() += 1;
    a.total_tool_calls += 1;
    if name == "Bash" {
        a.bash_commands += 1;
        let cmd = input.get("command").and_then(Value::as_str).unwrap_or("");
        if cmd.contains("forge test") {
            a.forge_test_runs += 1;
        }
    }
    if name == "Skill" {
        if let Some(s) = input.get("skill").and_then(Value::as_str) {
            a.skills.push(s.to_string());
        }
    }
    a.last_tool = Some(tool_summary(block));
}

/// One line for a tool call: its name and the input that says what it was for.
pub fn tool_summary(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
    let input = &block["input"];
    let detail = [
        "command",
        "skill",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
    ]
    .iter()
    .find_map(|k| input.get(*k).and_then(Value::as_str))
    .unwrap_or("");
    let line = detail.lines().next().unwrap_or("");
    let summary = if line.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {line}")
    };
    truncate(&summary, LAST_TOOL_CHARS)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect::<String>() + "..."
    } else {
        s.to_string()
    }
}

/// Claude Code reports a refused tool as an error result naming the missing permission.
fn is_permission_denial(block: &Value) -> bool {
    if block.get("type").and_then(Value::as_str) != Some("tool_result")
        || block.get("is_error").and_then(Value::as_bool) != Some(true)
    {
        return false;
    }
    let text = match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => return false,
    };
    text.contains("requested permissions") || text.contains("haven't granted")
}

/// What a Pass 2 agent left behind, which is how a finished agent is judged.
#[derive(Debug, Clone)]
pub struct AgentEvidence {
    /// `hunt-*` agents, which work one component and must test their hypotheses.
    pub hunter: bool,
    pub findings_file: bool,
    pub coverage: Option<Coverage>,
    pub thresholds: Thresholds,
}

/// The inputs a session's health depends on, kept apart from I/O so the rules can be tested.
/// Duration and tool counts are deliberately absent: a thorough hunter can finish in minutes.
#[derive(Debug, Clone)]
pub struct HealthInput {
    pub state: State,
    pub silent_s: u64,
    pub permission_denials: u64,
    pub forge_test_runs: u64,
    pub agent: Option<AgentEvidence>,
}

pub fn classify(h: &HealthInput) -> (Health, Vec<String>) {
    let mut red = Vec::new();
    let mut amber = Vec::new();
    match h.state {
        State::Running => {
            if h.silent_s > SILENT_RED_SECS {
                red.push(format!("silent {}m", h.silent_s / 60));
            } else if h.silent_s > SILENT_AMBER_SECS {
                amber.push(format!("silent {}m", h.silent_s / 60));
            }
        }
        State::Stalled => red.push("stalled".into()),
        State::Failed => red.push("failed".into()),
        State::Done => {}
    }
    if let (false, Some(a)) = (h.state == State::Running, &h.agent) {
        if !a.findings_file {
            red.push("no findings file".into());
        }
        match &a.coverage {
            None => red.push("no coverage file".into()),
            Some(c) => {
                if c.files == 0 {
                    red.push("coverage lists no files".into());
                } else if c.files_read_in_full < c.files {
                    red.push(format!(
                        "{} of {} files not read in full",
                        c.files - c.files_read_in_full,
                        c.files
                    ));
                }
                let hyps = c.hypotheses.unwrap_or(0);
                let min = a.thresholds.min_hypotheses;
                if a.hunter && hyps < min {
                    red.push(format!("{hyps} hypotheses, under {min}"));
                }
            }
        }
        if a.hunter && h.forge_test_runs < a.thresholds.min_forge_tests {
            amber.push("no forge test".into());
        }
    }
    // Often harmless (a refused heredoc followed by a Write), so worth a look, not a restart.
    if h.permission_denials > 0 {
        amber.push(format!("{} permission denials", h.permission_denials));
    }
    if !red.is_empty() {
        red.extend(amber);
        (Health::Red, red)
    } else if !amber.is_empty() {
        (Health::Amber, amber)
    } else {
        (Health::Green, vec![])
    }
}

// ── The /api/status document ──────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct Status {
    pub generated_at: String,
    pub workspace: String,
    pub run_started_at: Option<String>,
    pub overall: Overall,
    pub budget: Budget,
    pub passes: Vec<PassView>,
    pub sessions: Vec<SessionView>,
    pub agents: Vec<AgentView>,
    pub findings: FindingsView,
    /// Files that exist but could not be read; whatever they feed is shown as unknown.
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Overall {
    pub health: Health,
    pub reasons: Vec<String>,
    pub running: usize,
    pub finished: usize,
}

#[derive(Debug, Serialize)]
pub struct Budget {
    pub max_usd: Option<f64>,
    pub spent_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct PassView {
    pub number: usize,
    pub name: &'static str,
    /// pending, running, completed, failed, skipped, not_implemented
    pub state: String,
    pub duration_s: Option<u64>,
    pub sessions: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub label: String,
    pub persona: Option<Persona>,
    pub pass: &'static str,
    pub model: Option<String>,
    pub state: State,
    pub health: Health,
    pub reasons: Vec<String>,
    pub started_at: Option<String>,
    pub elapsed_s: u64,
    pub silent_s: Option<u64>,
    #[serde(flatten)]
    pub activity: Activity,
    pub outcome: Option<String>,
    pub cost_usd: Option<f64>,
    pub final_turns: Option<u64>,
    pub log: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentView {
    pub name: String,
    pub findings: Option<usize>,
    pub coverage: Option<Coverage>,
}

#[derive(Debug, Serialize)]
pub struct FindingsView {
    /// "validated" (Pass 3) or "merged" (Pass 2), whichever is furthest along.
    pub source: Option<&'static str>,
    pub items: Option<Vec<FindingRow>>,
}

#[derive(Debug, Serialize)]
pub struct FindingRow {
    pub id: String,
    pub severity: String,
    pub confidence: String,
    pub title: String,
    pub contract: String,
    pub source: String,
    pub poc_status: String,
    pub scope_pin: String,
}

#[derive(Deserialize)]
struct CostFile {
    max_usd: Option<f64>,
    #[serde(default)]
    entries: Vec<Entry>,
}

struct LiveLog {
    rel: String,
    modified: DateTime<Utc>,
    activity: Activity,
}

#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub owed: coverage::Scope,
    pub thresholds: Thresholds,
    /// Stage to narrating persona, resolved from the config; empty without one.
    pub personas: HashMap<String, Persona>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Persona {
    pub key: String,
    pub name: String,
}

impl Scope {
    pub fn from_config(c: &crate::config::Config) -> Self {
        Self {
            owed: coverage::Scope::from_config(c),
            thresholds: Thresholds::from_config(&c.passes.agents),
            personas: PERSONA_STAGES
                .iter()
                .map(|s| s.to_string())
                .chain(c.passes.agents.agents.iter().cloned())
                .filter_map(|stage| {
                    let key = c.persona_for(&stage)?;
                    let name = crate::config::Config::persona_name(&key).to_string();
                    Some((stage, Persona { key, name }))
                })
                .collect(),
        }
    }

    pub fn persona(&self, label: &str) -> Option<Persona> {
        self.personas.get(stage_for(label)?).cloned()
    }
}

pub fn collect(ws: &Workspace, scope: &Scope, now: DateTime<Utc>) -> Status {
    let mut errors = Vec::new();
    let root = ws.root();

    let cost: Option<CostFile> = read_json(&ws.cost_ledger(), &mut errors);
    let entries = cost.as_ref().map(|c| c.entries.as_slice()).unwrap_or(&[]);
    // Retries reuse a label; the last reservation is the live one.
    let mut by_label: BTreeMap<String, &Entry> = BTreeMap::new();
    for e in entries {
        by_label.insert(e.label.clone(), e);
    }

    let mut live: BTreeMap<String, LiveLog> = BTreeMap::new();
    for path in find_live_logs(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            errors.push(format!("unreadable {}", path.display()));
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map(DateTime::<Utc>::from)
            .unwrap_or(now);
        let rel_str = rel.display().to_string();
        let log_rel = rel_str.strip_suffix(LIVE_SUFFIX).unwrap_or(&rel_str);
        live.insert(
            label_for(Path::new(log_rel)),
            LiveLog {
                rel: rel_str.clone(),
                modified,
                activity: parse_events(&text),
            },
        );
    }

    let run_start = entries
        .iter()
        .filter_map(|e| parse_time(&e.at))
        .min()
        .or_else(|| live.values().map(|l| l.modified).min());

    let labels: BTreeSet<String> = by_label.keys().chain(live.keys()).cloned().collect();
    let mut sessions: Vec<SessionView> = labels
        .into_iter()
        .map(|label| {
            let entry = by_label.get(&label).copied();
            let log = live.remove(&label);
            session_view(ws, scope, label, entry, log, now)
        })
        .collect();
    sessions.sort_by(|a, b| {
        pass_index(a.pass)
            .cmp(&pass_index(b.pass))
            .then(a.started_at.cmp(&b.started_at))
            .then(a.label.cmp(&b.label))
    });

    let pipeline: Option<PipelineStatus> = read_json(&ws.pipeline_status_file(), &mut errors);
    let passes = pass_views(pipeline.as_ref(), &sessions, run_start, entries, now);
    let agents = agent_views(ws, scope, &sessions, &mut errors);
    let findings = findings_view(ws, &mut errors);

    let worst = sessions
        .iter()
        .map(|s| s.health)
        .max()
        .unwrap_or(Health::Green);
    let reasons = sessions
        .iter()
        .filter(|s| s.health == worst && worst != Health::Green)
        .map(|s| format!("{}: {}", s.label, s.reasons.join(", ")))
        .collect();
    let running = sessions
        .iter()
        .filter(|s| s.state == State::Running)
        .count();

    Status {
        generated_at: now.to_rfc3339(),
        workspace: root.display().to_string(),
        run_started_at: run_start.map(|t| t.to_rfc3339()),
        overall: Overall {
            health: worst,
            reasons,
            running,
            finished: sessions.len() - running,
        },
        budget: Budget {
            max_usd: cost.as_ref().and_then(|c| c.max_usd),
            spent_usd: cost
                .as_ref()
                .map(|c| c.entries.iter().map(|e| e.cost_usd).sum()),
        },
        passes,
        sessions,
        agents,
        findings,
        errors,
    }
}

fn session_view(
    ws: &Workspace,
    scope: &Scope,
    label: String,
    entry: Option<&Entry>,
    log: Option<LiveLog>,
    now: DateTime<Utc>,
) -> SessionView {
    let started = entry.and_then(|e| parse_time(&e.at));
    let last_event = log.as_ref().map(|l| l.modified);
    let (rel, activity) = match log {
        Some(l) => (Some(l.rel), l.activity),
        None => (None, Activity::default()),
    };
    let end = activity.result.clone();

    // The ledger settles only when the process exits; a `result` mid-stream can be a pause.
    let outcome = match entry.map(|e| e.outcome.as_str()) {
        Some("running") => None,
        Some(o) => Some(o.to_string()),
        None => end.as_ref().map(|r| r.subtype.clone()),
    };
    let state = match outcome.as_deref() {
        None => State::Running,
        Some("success") | Some("error_max_turns") => State::Done,
        Some("stalled") | Some("timed_out") => State::Stalled,
        Some(_) => State::Failed,
    };

    let start = started.or(last_event).unwrap_or(now);
    let elapsed_s = match (state, &end) {
        (State::Running, _) => secs(now - start),
        (
            _,
            Some(SessionEnd {
                duration_ms: Some(ms),
                ..
            }),
        ) => ms / 1000,
        _ => secs(last_event.unwrap_or(now) - start),
    };
    let silent_s = (state == State::Running).then(|| secs(now - last_event.unwrap_or(start)));

    let pass = pass_for(&label);
    let agent = label.strip_prefix("agent:");
    let (health, reasons) = classify(&HealthInput {
        state,
        silent_s: silent_s.unwrap_or(0),
        permission_denials: activity.permission_denials,
        forge_test_runs: activity.forge_test_runs,
        agent: agent.map(|a| AgentEvidence {
            hunter: a.starts_with("hunt-"),
            findings_file: ws.agent_raw_output(a).exists(),
            // A broken coverage file counts as none; agent_views reports the parse error.
            coverage: read_json::<Value>(&ws.agent_coverage(a), &mut Vec::new())
                .as_ref()
                .map(|v| coverage_stats(v, |f| scope.owed.counts(a, f))),
            thresholds: scope.thresholds,
        }),
    });

    SessionView {
        persona: scope.persona(&label),
        pass,
        model: activity
            .model
            .clone()
            .or_else(|| entry.and_then(|e| e.model.clone())),
        state,
        health,
        reasons,
        started_at: started.map(|t| t.to_rfc3339()),
        elapsed_s,
        silent_s,
        outcome,
        cost_usd: end
            .as_ref()
            .and_then(|r| r.cost_usd)
            .or_else(|| entry.filter(|e| e.measured).map(|e| e.cost_usd)),
        final_turns: end
            .as_ref()
            .and_then(|r| r.num_turns)
            .or_else(|| entry.and_then(|e| e.turns)),
        log: rel,
        activity,
        label,
    }
}

fn pass_views(
    pipeline: Option<&PipelineStatus>,
    sessions: &[SessionView],
    run_start: Option<DateTime<Utc>>,
    entries: &[Entry],
    now: DateTime<Utc>,
) -> Vec<PassView> {
    let last_active = sessions.iter().map(|s| pass_index(s.pass)).max();
    PASSES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let in_pass: Vec<&SessionView> = sessions.iter().filter(|s| s.pass == *name).collect();
            // pipeline-status.json survives across runs; a record older than this run is stale.
            let record = pipeline.and_then(|p| p.passes.get(*name)).filter(|r| {
                match (run_start, parse_time(&r.completed)) {
                    (Some(start), Some(done)) => done >= start,
                    _ => true,
                }
            });
            let (state, duration_s) = match record {
                Some(r) => (
                    serde_json::to_value(r.status)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "completed".into()),
                    Some(r.duration),
                ),
                None if in_pass.is_empty() => ("pending".into(), None),
                None => {
                    let state = if in_pass.iter().any(|s| s.state == State::Running)
                        || last_active == Some(i)
                    {
                        "running"
                    } else {
                        "completed"
                    };
                    let first = entries
                        .iter()
                        .filter(|e| pass_for(&e.label) == *name)
                        .filter_map(|e| parse_time(&e.at))
                        .min();
                    (state.into(), first.map(|t| secs(now - t)))
                }
            };
            PassView {
                number: i + 1,
                name,
                state,
                duration_s,
                sessions: in_pass.len(),
            }
        })
        .collect()
}

fn agent_views(
    ws: &Workspace,
    scope: &Scope,
    sessions: &[SessionView],
    errors: &mut Vec<String>,
) -> Vec<AgentView> {
    let mut names: BTreeSet<String> = sessions
        .iter()
        .filter_map(|s| s.label.strip_prefix("agent:"))
        .map(str::to_string)
        .collect();
    if let Ok(dir) = std::fs::read_dir(ws.findings_dir()) {
        for f in dir.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            if let Some(a) = n
                .strip_suffix("-agent-raw.json")
                .or_else(|| n.strip_suffix("-agent-coverage.json"))
            {
                names.insert(a.to_string());
            }
        }
    }
    names
        .into_iter()
        .map(|name| {
            let findings: Option<Vec<Value>> = read_json(&ws.agent_raw_output(&name), errors);
            let coverage: Option<Value> = read_json(&ws.agent_coverage(&name), errors);
            AgentView {
                findings: findings.map(|f| f.len()),
                coverage: coverage
                    .as_ref()
                    .map(|v| coverage_stats(v, |f| scope.owed.counts(&name, f))),
                name,
            }
        })
        .collect()
}

fn findings_view(ws: &Workspace, errors: &mut Vec<String>) -> FindingsView {
    let (source, path) = if ws.validated_findings().exists() {
        ("validated", ws.validated_findings())
    } else if ws.merged_findings().exists() {
        ("merged", ws.merged_findings())
    } else {
        return FindingsView {
            source: None,
            items: Some(vec![]),
        };
    };
    let items: Option<Vec<Value>> = read_json(&path, errors);
    FindingsView {
        source: Some(source),
        items: items.map(|v| v.iter().map(finding_row).collect()),
    }
}

fn finding_row(f: &Value) -> FindingRow {
    let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let found_by: Vec<&str> = f
        .get("found_by")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    FindingRow {
        id: s("id"),
        severity: s("severity"),
        confidence: s("confidence"),
        title: s("title"),
        contract: s("contract"),
        source: if found_by.is_empty() {
            s("source")
        } else {
            found_by.join(", ")
        },
        poc_status: f
            .get("poc_status")
            .and_then(Value::as_str)
            .unwrap_or("not_attempted")
            .to_string(),
        scope_pin: s("scope_pin"),
    }
}

/// `None` when the file is absent; a file that is present but broken is also recorded in `errors`.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path, errors: &mut Vec<String>) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            errors.push(format!("{}: {e}", path.display()));
            None
        }
    }
}

fn find_live_logs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            // Forge projects under the workspace carry deep lib/ and out/ trees with no logs in them.
            if ft.is_dir() && depth < 4 {
                let n = e.file_name();
                if !matches!(n.to_str(), Some("lib" | "out" | "cache" | "node_modules")) {
                    stack.push((p, depth + 1));
                }
            } else if ft.is_file() && p.to_string_lossy().ends_with(LIVE_SUFFIX) {
                out.push(p);
            }
        }
    }
    out
}

/// The live log a session writes to, found by mapping each log back to its label.
pub fn live_log_for(ws: &Workspace, label: &str) -> Option<PathBuf> {
    let root = ws.root();
    find_live_logs(root).into_iter().find(|p| {
        let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
        let log_rel = rel.strip_suffix(LIVE_SUFFIX).unwrap_or(&rel);
        label_for(Path::new(log_rel)) == label
    })
}

/// The ledger label a log path belongs to; mirrors where each pass puts its session logs.
pub fn label_for(log: &Path) -> String {
    let file = log.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = file.strip_suffix(".log").unwrap_or(file);
    let parent = log
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    match stem {
        "scv-scan" => return "scv_scan".into(),
        "scope-check" => return "scope_check".into(),
        "adversarial-review" => return "review".into(),
        "invariant-generation" => return "fuzzing".into(),
        "halmos-generation" => return "formal".into(),
        _ => {}
    }
    if parent == "known" {
        return format!("known_{stem}");
    }
    if let Some(a) = stem.strip_suffix("-agent") {
        return format!("agent:{a}");
    }
    if let Some(id) = stem.strip_suffix("-fp-check") {
        return format!("fp_check:{id}");
    }
    if let Some(id) = stem.strip_suffix("-variant") {
        return format!("variant:{id}");
    }
    if parent == "pocs/logs" {
        return format!("poc:{stem}");
    }
    stem.to_string()
}

/// The config stage a session label narrates as, for persona lookup.
pub fn stage_for(label: &str) -> Option<&str> {
    let (head, rest) = label.split_once(':').unwrap_or((label, ""));
    match head {
        "agent" if rest.starts_with("hunt-") => Some("hunter"),
        "agent" => Some(rest),
        "poc" => Some("poc"),
        "variant" => Some("variant_analysis"),
        "scv_scan" | "fuzzing" | "formal" | "review" if rest.is_empty() => Some(head),
        _ => None,
    }
}

pub fn pass_for(label: &str) -> &'static str {
    let head = label.split(':').next().unwrap_or(label);
    match head {
        "scv_scan" | "recon" => "recon",
        "agent" | "variant" => "agents",
        "poc" | "fp_check" | "scope_check" => "poc",
        "fuzzing" => "fuzzing",
        "formal" => "formal",
        "review" => "review",
        h if h.starts_with("known") => "poc",
        _ => "other",
    }
}

fn pass_index(pass: &str) -> usize {
    PASSES
        .iter()
        .position(|p| *p == pass)
        .unwrap_or(PASSES.len())
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn secs(d: chrono::TimeDelta) -> u64 {
    d.num_seconds().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, input: Value) -> String {
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": name, "input": input}
        ]}})
        .to_string()
    }

    fn stream(lines: &[String]) -> String {
        lines.join("\n") + "\n"
    }

    fn init(model: &str) -> String {
        json!({"type": "system", "subtype": "init", "model": model}).to_string()
    }

    fn hunter(coverage: Option<Coverage>) -> HealthInput {
        HealthInput {
            state: State::Done,
            silent_s: 0,
            permission_denials: 0,
            forge_test_runs: 13,
            agent: Some(AgentEvidence {
                hunter: true,
                findings_file: true,
                coverage,
                thresholds: Thresholds::default(),
            }),
        }
    }

    fn covered(files: usize, read: usize, hypotheses: Option<usize>) -> Option<Coverage> {
        Some(Coverage {
            files,
            files_read_in_full: read,
            hypotheses,
        })
    }

    #[test]
    fn events_count_tools_tests_skills_and_turns() {
        let text = stream(&[
            init("claude-opus-5-5"),
            tool(
                "Bash",
                json!({"command": "forge test --mt testExploit -vvv"}),
            ),
            tool("Bash", json!({"command": "ls src"})),
            tool("Skill", json!({"skill": "tob-scv-scan"})),
            "not json".into(),
            tool("Read", json!({"file_path": "src/Vault.sol"})),
        ]);
        let a = parse_events(&text);
        assert_eq!(a.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(a.turns, 4);
        assert_eq!(a.total_tool_calls, 4);
        assert_eq!(a.tool_calls["Bash"], 2);
        assert_eq!(a.bash_commands, 2);
        assert_eq!(a.forge_test_runs, 1);
        assert_eq!(a.skills, vec!["tob-scv-scan"]);
        assert_eq!(a.last_tool.as_deref(), Some("Read: src/Vault.sol"));
        assert_eq!(a.restarts, 0);
        assert!(a.result.is_none());
    }

    #[test]
    fn split_message_counts_as_one_turn() {
        let part = |t: &str| {
            json!({"type": "assistant", "message": {"id": "msg_1", "content": [{"type": t, "text": "x"}]}})
                .to_string()
        };
        assert_eq!(
            parse_events(&stream(&[part("text"), part("thinking")])).turns,
            1
        );
    }

    #[test]
    fn denials_come_from_the_stream_and_the_result() {
        let denied = json!({"type": "user", "message": {"content": [{
            "type": "tool_result", "is_error": true,
            "content": "Claude requested permissions to use Skill, but you haven't granted it yet."
        }]}})
        .to_string();
        let a = parse_events(&stream(&[init("m"), denied.clone(), denied]));
        assert_eq!(a.permission_denials, 2);

        let result = json!({"type": "result", "subtype": "success", "num_turns": 7,
            "total_cost_usd": 1.25, "duration_ms": 90_000,
            "permission_denials": [{"tool_name": "Skill"}, {"tool_name": "Bash"}, {"tool_name": "Write"}]})
        .to_string();
        let a = parse_events(&stream(&[init("m"), init("m"), result]));
        assert_eq!(a.permission_denials, 3);
        assert_eq!(a.restarts, 1);
        let end = a.result.unwrap();
        assert_eq!(end.subtype, "success");
        assert_eq!(end.num_turns, Some(7));
        assert_eq!(end.duration_ms, Some(90_000));
    }

    #[test]
    fn a_result_followed_by_init_is_a_pause_not_an_end_or_restart() {
        let pause =
            json!({"type": "result", "subtype": "success", "result": "Waiting."}).to_string();
        let a = parse_events(&stream(&[init("m"), pause.clone(), init("m")]));
        assert!(a.result.is_none());
        assert_eq!(a.restarts, 0);
        let a = parse_events(&stream(&[init("m"), pause, init("m"), init("m")]));
        assert_eq!(a.restarts, 1);
    }

    #[test]
    fn long_last_tool_is_one_truncated_line() {
        let cmd = format!("{}\nsecond line", "x".repeat(300));
        let a = parse_events(&tool("Bash", json!({"command": cmd})));
        let last = a.last_tool.unwrap();
        assert!(!last.contains('\n'));
        assert_eq!(last.chars().count(), LAST_TOOL_CHARS + 3);
    }

    #[test]
    fn short_session_with_evidence_of_work_is_green() {
        // The live case: 4 minutes, 17 tool calls, every file read, 21 hypotheses, 13 forge tests.
        assert_eq!(
            classify(&hunter(covered(6, 6, Some(21)))),
            (Health::Green, vec![])
        );
    }

    #[test]
    fn missing_or_thin_evidence_is_red() {
        let no_findings = HealthInput {
            agent: Some(AgentEvidence {
                findings_file: false,
                ..hunter(covered(6, 6, Some(21))).agent.unwrap()
            }),
            ..hunter(None)
        };
        let cases = [
            (no_findings, "no findings file"),
            (hunter(None), "no coverage file"),
            (hunter(covered(6, 6, Some(14))), "14 hypotheses, under 15"),
            (
                hunter(covered(6, 4, Some(21))),
                "2 of 6 files not read in full",
            ),
            (hunter(covered(0, 0, Some(21))), "coverage lists no files"),
        ];
        for (h, why) in cases {
            let (health, reasons) = classify(&h);
            assert_eq!(health, Health::Red, "{h:?}");
            assert!(reasons.iter().any(|r| r == why), "{reasons:?}");
        }
    }

    #[test]
    fn persona_agents_need_no_hypotheses_or_forge_tests() {
        let persona = HealthInput {
            forge_test_runs: 0,
            agent: Some(AgentEvidence {
                hunter: false,
                findings_file: true,
                coverage: covered(9, 9, None),
                thresholds: Thresholds::default(),
            }),
            ..hunter(None)
        };
        assert_eq!(classify(&persona).0, Health::Green);
    }

    #[test]
    fn untested_hunter_and_denials_are_amber() {
        let untested = HealthInput {
            forge_test_runs: 0,
            ..hunter(covered(6, 6, Some(21)))
        };
        assert_eq!(
            classify(&untested),
            (Health::Amber, vec!["no forge test".into()])
        );
        let denied = HealthInput {
            permission_denials: 2,
            ..hunter(covered(6, 6, Some(21)))
        };
        assert_eq!(
            classify(&denied),
            (Health::Amber, vec!["2 permission denials".into()])
        );
    }

    #[test]
    fn evidence_is_not_judged_while_running_or_outside_pass_2() {
        let running = HealthInput {
            state: State::Running,
            ..hunter(None)
        };
        assert_eq!(classify(&running).0, Health::Green);
        let scoped = HealthInput {
            agent: None,
            ..hunter(None)
        };
        assert_eq!(classify(&scoped).0, Health::Green);
    }

    #[test]
    fn stalled_and_failed_sessions_are_red() {
        for state in [State::Stalled, State::Failed] {
            let h = HealthInput {
                state,
                agent: None,
                ..hunter(None)
            };
            assert_eq!(classify(&h).0, Health::Red);
        }
    }

    #[test]
    fn silence_goes_amber_then_red() {
        let at = |silent_s| {
            classify(&HealthInput {
                state: State::Running,
                silent_s,
                ..hunter(None)
            })
            .0
        };
        assert_eq!(at(SILENT_AMBER_SECS), Health::Green);
        assert_eq!(at(SILENT_AMBER_SECS + 1), Health::Amber);
        assert_eq!(at(SILENT_RED_SECS + 1), Health::Red);
    }

    #[test]
    fn log_paths_map_to_ledger_labels_and_passes() {
        let cases = [
            ("findings/logs/red-agent.log", "agent:red", "agents"),
            (
                "findings/logs/hunt-fees-agent.log",
                "agent:hunt-fees",
                "agents",
            ),
            (
                "findings/logs/RED-003-variant.log",
                "variant:RED-003",
                "agents",
            ),
            ("findings/logs/scope-check.log", "scope_check", "poc"),
            ("recon/scv-scan.log", "scv_scan", "recon"),
            ("pocs/logs/F-001.log", "poc:F-001", "poc"),
            (
                "pocs/fp-check-logs/F-001-fp-check.log",
                "fp_check:F-001",
                "poc",
            ),
            ("known/extract.log", "known_extract", "poc"),
            (
                "fuzzing/logs/invariant-generation.log",
                "fuzzing",
                "fuzzing",
            ),
            ("formal/logs/halmos-generation.log", "formal", "formal"),
            ("review/logs/adversarial-review.log", "review", "review"),
        ];
        for (path, label, pass) in cases {
            assert_eq!(label_for(Path::new(path)), label);
            assert_eq!(pass_for(label), pass);
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn collect_joins_ledger_logs_and_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), "ws");
        let now = Utc::now();
        let started = (now - chrono::TimeDelta::minutes(40)).to_rfc3339();
        let cost = json!({"max_usd": null, "entries": [
            {"label": "agent:red", "model": "claude-opus-5-5", "cost_usd": 0.0, "measured": false,
             "cap_usd": null, "turns": null, "outcome": "running", "at": started},
            {"label": "agent:blue", "model": "claude-opus-5-5", "cost_usd": 0.4, "measured": true,
             "cap_usd": null, "turns": 5, "outcome": "success", "at": started},
        ]});
        write(&ws.cost_ledger(), &cost.to_string());
        write(
            &ws.findings_logs_dir().join("red-agent.log.live.jsonl"),
            &stream(&[
                init("claude-opus-5-5"),
                tool("Bash", json!({"command": "forge test"})),
                // Paused on background tasks: the ledger still says running.
                json!({"type": "result", "subtype": "success"}).to_string(),
            ]),
        );
        let done = json!({"type": "result", "subtype": "success", "num_turns": 5,
            "total_cost_usd": 0.4, "duration_ms": 120_000, "permission_denials": []});
        write(
            &ws.findings_logs_dir().join("blue-agent.log.live.jsonl"),
            &stream(&[init("claude-opus-5-5"), done.to_string()]),
        );
        write(&ws.agent_raw_output("red"), "[{\"title\": \"x\"}]");
        write(
            &ws.findings_dir().join("red-agent-coverage.json"),
            "{\"files\": [], \"hypotheses\": [{}]}",
        );
        write(&ws.merged_findings(), "not json");

        let s = collect(&ws, &Scope::default(), now);
        let red = s.sessions.iter().find(|x| x.label == "agent:red").unwrap();
        assert_eq!(red.state, State::Running);
        assert_eq!(red.health, Health::Green);
        assert_eq!(red.activity.forge_test_runs, 1);
        assert!(red.elapsed_s >= 40 * 60);

        let blue = s.sessions.iter().find(|x| x.label == "agent:blue").unwrap();
        assert_eq!(blue.state, State::Done);
        assert_eq!(blue.health, Health::Red);
        assert!(blue.reasons.iter().any(|r| r == "no findings file"));

        assert_eq!(s.overall.health, Health::Red);
        assert_eq!(s.overall.running, 1);
        let agents = &s.passes[1];
        assert_eq!((agents.state.as_str(), agents.sessions), ("running", 2));
        assert_eq!(s.passes[0].state, "pending");

        let red_agent = s.agents.iter().find(|a| a.name == "red").unwrap();
        assert_eq!(red_agent.findings, Some(1));
        assert_eq!(red_agent.coverage.as_ref().unwrap().hypotheses, Some(1));
        // A broken file is reported, not shown as an empty list.
        assert_eq!(s.findings.source, Some("merged"));
        assert!(s.findings.items.is_none());
        assert_eq!(s.errors.len(), 1);
    }

    #[test]
    fn stale_pass_records_from_an_earlier_run_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), "ws");
        let now = Utc::now();
        let status = json!({"passes": {
            "recon": {"status": "completed", "duration": 30, "completed": (now - chrono::TimeDelta::days(2)).to_rfc3339()},
        }});
        write(&ws.pipeline_status_file(), &status.to_string());
        let cost = json!({"entries": [{"label": "scv_scan", "model": null, "cost_usd": 0.0,
            "measured": false, "cap_usd": null, "turns": null, "outcome": "running",
            "at": (now - chrono::TimeDelta::minutes(1)).to_rfc3339()}]});
        write(&ws.cost_ledger(), &cost.to_string());
        let s = collect(&ws, &Scope::default(), now);
        assert_eq!(s.passes[0].state, "running");
        assert_eq!(s.passes[0].duration_s, Some(60));
    }

    #[test]
    fn labels_map_to_persona_stages() {
        let cases = [
            ("agent:red", Some("red")),
            ("agent:gold", Some("gold")),
            ("agent:hunt-fees", Some("hunter")),
            ("poc:F-001", Some("poc")),
            ("variant:RED-003", Some("variant_analysis")),
            ("scv_scan", Some("scv_scan")),
            ("review", Some("review")),
            ("fp_check:F-001", None),
            ("known_match", None),
        ];
        for (label, stage) in cases {
            assert_eq!(stage_for(label), stage, "{label}");
        }
    }

    #[test]
    fn last_said_is_the_latest_narration_on_one_line() {
        let say = |t: &str| {
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}})
                .to_string()
        };
        let a = parse_events(&stream(&[
            say("First."),
            say("Now the  escrow.\nIt thaws."),
            tool("Read", json!({"file_path": "x"})),
        ]));
        assert_eq!(a.last_said.as_deref(), Some("Now the escrow. It thaws."));
        let a = parse_events(&say(&"y".repeat(500)));
        assert_eq!(a.last_said.unwrap().chars().count(), LAST_SAID_CHARS + 3);
    }
}
