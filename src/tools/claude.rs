use crate::budget::{self, Ledger};
use crate::error::{BulwarkError, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::Instant;

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Configuration for a headless Claude Code session.
pub struct ClaudeSession {
    pub claude_bin: PathBuf,
    pub prompt: String,
    pub max_turns: u32,
    pub working_dir: PathBuf,
    pub log_file: PathBuf,
    /// Model to use (e.g. "haiku", "sonnet", "opus"). Passed as `--model`.
    pub model: Option<String>,
    /// Tools the subprocess is allowed to use (passed as `--allowedTools`).
    /// Empty means no explicit restriction.
    pub allowed_tools: Vec<String>,
    /// Wall-clock limit in minutes; the session is killed and reports `TIMEOUT_EXIT`.
    pub timeout_minutes: Option<u32>,
    /// Ledger name for this session, e.g. `poc:F-003`.
    pub label: String,
    /// Dollar cap passed as `--max-budget-usd`; `None` is unlimited.
    pub budget_usd: Option<f64>,
    pub ledger: Option<Arc<Ledger>>,
    /// Session to continue with `--resume`; `prompt` is then the message sent to it.
    pub resume: Option<String>,
}

/// Result of a completed Claude session.
pub struct ClaudeOutput {
    pub exit_code: i32,
    pub log_file: PathBuf,
    pub cost_usd: Option<f64>,
    /// From the stream's `init` event; `None` if the session never started.
    pub session_id: Option<String>,
    /// Launches including stall restarts; 1 for a session that never stalled.
    pub attempts: u32,
}

/// The `type: "result"` record of `--output-format json`, which may be the whole
/// output or the last element of a message array.
#[derive(Debug, Default, PartialEq)]
pub struct SessionResult {
    pub text: String,
    pub cost_usd: Option<f64>,
    pub turns: Option<u64>,
    pub outcome: String,
}

pub fn parse_session_json(stdout: &str) -> Option<SessionResult> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let result = match &v {
        serde_json::Value::Array(items) => items
            .iter()
            .rev()
            .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("result"))?,
        obj => obj,
    };
    Some(SessionResult {
        // Error results carry `errors` and no `result`.
        text: match result.get("result").and_then(|r| r.as_str()) {
            Some(r) => r.to_string(),
            None => result
                .get("errors")
                .and_then(|e| e.as_array())
                .map(|e| {
                    e.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        },
        cost_usd: result.get("total_cost_usd").and_then(|c| c.as_f64()),
        turns: result.get("num_turns").and_then(|n| n.as_u64()),
        outcome: result
            .get("subtype")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string(),
    })
}

/// Set by the first session refused for a bad login; every later session would fail the same way.
static AUTH_FAILED: AtomicBool = AtomicBool::new(false);

pub fn auth_failed() -> bool {
    AUTH_FAILED.load(Ordering::Relaxed)
}

fn auth_failure(events: &[&str]) -> bool {
    events.iter().any(|e| {
        serde_json::from_str::<serde_json::Value>(e)
            .is_ok_and(|v| v.get("error").and_then(|x| x.as_str()) == Some("authentication_failed"))
    })
}

/// A session silent for this long is killed and restarted. Claude Code caps a Bash call at ten
/// minutes, so a live session always emits something well inside it.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);
pub const MAX_STALL_RESTARTS: u32 = 2;
/// Exit code when the wall-clock timeout ends a session, as `timeout(1)` reports it.
pub const TIMEOUT_EXIT: i32 = 124;
/// Exit code when every attempt stalled.
pub const STALLED_EXIT: i32 = 125;
const STALL_RESUME_PROMPT: &str =
    "You were interrupted. Carry on with the task from where you left off.";

/// The raw event stream a session appends to beside its log.
pub fn live_log(log_file: &Path) -> PathBuf {
    with_suffix(log_file, ".live.jsonl")
}

fn init_session_id(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        if v.get("type")?.as_str()? != "system" || v.get("subtype")?.as_str()? != "init" {
            return None;
        }
        Some(v.get("session_id")?.as_str()?.to_string())
    })
}

enum Ending {
    Exited(i32),
    Stalled,
    TimedOut,
}

struct Attempt {
    lines: Vec<String>,
    stderr: Vec<u8>,
    ending: Ending,
}

impl ClaudeSession {
    fn command(&self, resume: Option<&str>) -> Command {
        let mut cmd = Command::new(&self.claude_bin);
        if let Some(id) = resume {
            cmd.arg("--resume").arg(id);
        }
        // The prompt goes on stdin: Linux refuses any single argument over 128KB.
        cmd.arg("-p")
            .arg("--max-turns")
            .arg(self.max_turns.to_string())
            .arg("--verbose")
            .arg("--output-format")
            .arg("stream-json");

        if let Some(cap) = self.budget_usd {
            let cli_cap = cap * budget::CLI_CAP_FRACTION;
            cmd.arg("--max-budget-usd").arg(format!("{cli_cap:.2}"));
        }
        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }
        for tool in &self.allowed_tools {
            cmd.arg("--allowedTools").arg(tool);
        }
        cmd.current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    async fn attempt(
        &self,
        stall: Duration,
        deadline: Option<Instant>,
        live: &Path,
        resume: Option<&str>,
        prompt: &str,
    ) -> std::io::Result<Attempt> {
        let mut child = self.command(resume).spawn()?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let prompt = prompt.to_owned();
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
        });
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            buf
        });
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
        let mut live_log = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(live)
            .await?;

        let mut lines = Vec::new();
        let ending = loop {
            let wait = match deadline {
                Some(d) => stall.min(d.saturating_duration_since(Instant::now())),
                None => stall,
            };
            match tokio::time::timeout(wait, stdout.next_line()).await {
                Ok(Ok(Some(line))) => {
                    live_log.write_all(line.as_bytes()).await?;
                    live_log.write_all(b"\n").await?;
                    lines.push(line);
                }
                Ok(_) => {
                    let status = child.wait().await?;
                    break Ending::Exited(status.code().unwrap_or(-1));
                }
                Err(_) => {
                    let _ = child.kill().await;
                    break match deadline {
                        Some(d) if Instant::now() >= d => Ending::TimedOut,
                        _ => Ending::Stalled,
                    };
                }
            }
        };
        let stderr = stderr_task.await.unwrap_or_default();
        Ok(Attempt {
            lines,
            stderr,
            ending,
        })
    }

    /// Run the Claude session, writing stdout+stderr to the log file.
    pub async fn run(&self) -> Result<ClaudeOutput> {
        self.run_watched(STALL_TIMEOUT).await
    }

    async fn run_watched(&self, stall: Duration) -> Result<ClaudeOutput> {
        budget::check_cap(self.budget_usd)?;
        if auth_failed() {
            return Err(BulwarkError::ClaudeAuth(
                "an earlier session was refused".into(),
            ));
        }
        let reservation = match &self.ledger {
            Some(l) => Some(l.reserve(&self.label, self.model.clone(), self.budget_usd)?),
            None => None,
        };
        let deadline = self
            .timeout_minutes
            .map(|m| Instant::now() + Duration::from_secs(u64::from(m) * 60));
        let live = live_log(&self.log_file);
        // A resumed session's earlier events stay, so the log covers the whole session.
        if self.resume.is_none() {
            let _ = std::fs::remove_file(&live);
        }

        let mut stalls = 0;
        let mut session_id = self.resume.clone();
        let mut resume = self.resume.clone();
        let mut prompt = self.prompt.as_str();
        let attempt = loop {
            let attempt = match self
                .attempt(stall, deadline, &live, resume.as_deref(), prompt)
                .await
            {
                Ok(a) => a,
                Err(e) => {
                    if let (Some(l), Some(i)) = (&self.ledger, reservation) {
                        l.settle(i, Some(0.0), None, "spawn_failed")?;
                    }
                    return Err(e.into());
                }
            };
            if let Some(id) = init_session_id(&attempt.lines) {
                session_id = Some(id);
            }
            if matches!(attempt.ending, Ending::Stalled) && stalls < MAX_STALL_RESTARTS {
                stalls += 1;
                // Without an id the session never started, so there is nothing to resume.
                let how = match &session_id {
                    Some(id) => {
                        resume = Some(id.clone());
                        prompt = STALL_RESUME_PROMPT;
                        "resuming"
                    }
                    None => "restarting",
                };
                eprintln!(
                    "    {} {} silent for {}s, {how} ({stalls}/{MAX_STALL_RESTARTS})",
                    style("⚠").yellow(),
                    self.label,
                    stall.as_secs()
                );
                continue;
            }
            break attempt;
        };

        // Stream lines that parse form the same array `--output-format json --verbose` prints.
        let events: Vec<&str> = attempt
            .lines
            .iter()
            .map(String::as_str)
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
            .collect();
        let stdout = format!("[{}]", events.join(","));
        let parsed = parse_session_json(&stdout);

        // The log keeps the session's final text, which the passes parse; raw JSON sits beside it.
        let mut log_content = Vec::new();
        match &parsed {
            Some(r) => log_content.extend_from_slice(r.text.as_bytes()),
            None => log_content.extend_from_slice(attempt.lines.join("\n").as_bytes()),
        }
        if !attempt.stderr.is_empty() {
            log_content.extend_from_slice(b"\n--- STDERR ---\n");
            log_content.extend_from_slice(&attempt.stderr);
        }
        std::fs::write(&self.log_file, &log_content)?;
        std::fs::write(with_suffix(&self.log_file, ".json"), &stdout)?;

        let cost_usd = parsed.as_ref().and_then(|r| r.cost_usd);
        let exit_code = match attempt.ending {
            Ending::Exited(c) => c,
            Ending::TimedOut => TIMEOUT_EXIT,
            Ending::Stalled => STALLED_EXIT,
        };
        let refused = auth_failure(&events);
        if let (Some(l), Some(i)) = (&self.ledger, reservation) {
            let outcome = match (&parsed, exit_code) {
                _ if refused => "auth_failed",
                (Some(r), _) => r.outcome.as_str(),
                (None, STALLED_EXIT) => "stalled",
                (None, TIMEOUT_EXIT) => "timed_out",
                (None, _) => "no_result",
            };
            l.settle(i, cost_usd, parsed.as_ref().and_then(|r| r.turns), outcome)?;
        }

        if refused {
            AUTH_FAILED.store(true, Ordering::Relaxed);
            let why = parsed.map(|r| r.text).unwrap_or_default();
            return Err(BulwarkError::ClaudeAuth(why));
        }

        Ok(ClaudeOutput {
            exit_code,
            log_file: self.log_file.clone(),
            cost_usd,
            session_id,
            attempts: stalls + 1,
        })
    }

    /// Run the Claude session with an animated spinner showing elapsed time.
    ///
    /// Identical to [`run`] but displays a progress spinner on stderr so the
    /// user can tell the subprocess is still alive during long-running passes.
    pub async fn run_with_spinner(&self, label: &str) -> Result<ClaudeOutput> {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("    {spinner:.cyan} {msg} ({elapsed})")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(120));

        let result = self.run().await;

        match &result {
            Ok(output) if output.exit_code == 0 => {
                pb.finish_with_message(format!("{label} done"));
            }
            Ok(output) if output.exit_code == TIMEOUT_EXIT => {
                pb.finish_with_message(format!("{label} TIMED OUT"));
            }
            Ok(output) if output.exit_code == STALLED_EXIT => {
                pb.finish_with_message(format!("{label} STALLED"));
            }
            Ok(_output) => {
                pb.finish_with_message(format!("{label} done (with errors)"));
            }
            Err(_) => {
                pb.finish_with_message(format!("{label} failed"));
            }
        }

        result
    }
}

/// Check if a Claude Code slash command (skill) is available.
///
/// A skill directory `~/.claude/skills/<name>/SKILL.md`, or a command file
/// `~/.claude/commands/<name>.md`. install-skills.sh writes the former.
pub fn is_skill_available(skill_name: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let skill = format!("{home}/.claude/skills/{skill_name}/SKILL.md");
    let command = format!("{home}/.claude/commands/{skill_name}.md");
    std::path::Path::new(&skill).exists() || std::path::Path::new(&command).exists()
}

/// Check if Claude Code is authenticated (API key, credentials file, or `claude auth status`).
pub fn check_auth() -> AuthStatus {
    // Check API key
    if std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .is_some_and(|k| !k.is_empty())
    {
        return AuthStatus::ApiKey;
    }

    // Check for credentials file (older auth flow)
    let home = std::env::var("HOME").unwrap_or_default();
    let cred_paths = [
        format!("{home}/.claude/.credentials.json"),
        format!("{home}/.claude-auth/.credentials.json"),
    ];
    for path in &cred_paths {
        if Path::new(path).exists() {
            return AuthStatus::Login;
        }
    }

    // Check via `claude auth status` (Max/Team/Enterprise OAuth)
    if let Ok(output) = std::process::Command::new("claude")
        .args(["auth", "status"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("\"loggedIn\": true") || stdout.contains("\"loggedIn\":true") {
                return AuthStatus::Login;
            }
        }
    }

    AuthStatus::NotAuthenticated
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthStatus {
    ApiKey,
    Login,
    NotAuthenticated,
}

impl AuthStatus {
    pub fn is_authenticated(&self) -> bool {
        !matches!(self, AuthStatus::NotAuthenticated)
    }
}

impl std::fmt::Display for AuthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey => write!(f, "API key"),
            Self::Login => write!(f, "Claude login"),
            Self::NotAuthenticated => write!(f, "not authenticated"),
        }
    }
}

/// Attempt to extract a JSON array of findings from a Claude log file.
///
/// This is the fallback when an agent doesn't write to the expected output path.
/// Searches for the largest JSON array containing objects with a "severity" field.
pub fn extract_findings_from_log(log_path: &Path) -> Result<Vec<serde_json::Value>> {
    let content = std::fs::read_to_string(log_path)?;
    let mut best: Vec<serde_json::Value> = Vec::new();

    // Scan for JSON array candidates using bracket matching
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Try to find a matching ] by parsing from this position
            let slice = &content[i..];
            // Try increasingly large substrings until we find valid JSON
            if let Some(arr) = try_parse_json_array(slice) {
                if arr.len() > best.len() && looks_like_findings(&arr) {
                    best = arr;
                }
            }
        }
        i += 1;
    }

    Ok(best)
}

pub(crate) fn try_parse_json_array(s: &str) -> Option<Vec<serde_json::Value>> {
    // Find the matching ] bracket
    let mut depth = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, ch) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    let candidate = &s[..=i];
                    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(candidate) {
                        return Some(arr);
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn looks_like_findings(arr: &[serde_json::Value]) -> bool {
    if arr.is_empty() {
        return false;
    }
    // Check if first element has "severity" field
    arr[0]
        .as_object()
        .is_some_and(|obj| obj.contains_key("severity"))
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_expired_login_is_an_auth_failure() {
        let refused = r#"{"type":"assistant","message":{"content":[]},"error":"authentication_failed","is_api_error_message":true}"#;
        let fine = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"authentication_failed"}]}}"#;
        assert!(auth_failure(&[fine, refused]));
        assert!(!auth_failure(&[fine]));
    }
    use super::*;
    use std::io::Write;

    #[test]
    fn session_json_object_and_array_forms() {
        let obj = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","total_cost_usd":0.42,"num_turns":7}"#;
        let r = parse_session_json(obj).unwrap();
        assert_eq!(r.text, "done");
        assert_eq!(r.cost_usd, Some(0.42));
        assert_eq!(r.turns, Some(7));
        assert_eq!(r.outcome, "success");

        let arr = format!(r#"[{{"type":"system"}},{{"type":"assistant"}},{obj}]"#);
        assert_eq!(parse_session_json(&arr).unwrap(), r);
    }

    #[test]
    fn budget_error_keeps_cost_and_errors() {
        let out = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"errors":["budget reached"],"total_cost_usd":3.51}"#;
        let r = parse_session_json(out).unwrap();
        assert_eq!(r.outcome, "error_max_budget_usd");
        assert_eq!(r.cost_usd, Some(3.51));
        assert_eq!(r.text, "budget reached");
    }

    #[test]
    fn real_verbose_budget_stop_from_cli_2_1_317() {
        let out = r#"[{"type":"system","subtype":"init","session_id":"x"},
            {"type":"assistant","message":{}},
            {"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":null,
             "errors":["Reached maximum budget ($0.001)"],"total_cost_usd":0.0030603550000000003,"num_turns":1}]"#;
        let r = parse_session_json(out).unwrap();
        assert_eq!(r.outcome, "error_max_budget_usd");
        assert_eq!(r.text, "Reached maximum budget ($0.001)");
        assert!((r.cost_usd.unwrap() - 0.00306).abs() < 1e-5);
    }

    #[test]
    fn non_json_is_none() {
        assert!(parse_session_json("Error: not logged in").is_none());
    }

    #[test]
    fn extract_findings_from_log_with_embedded_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("agent.log");

        let log_content = r#"
Starting analysis...
Some debug output here
[{"severity": "High", "title": "Reentrancy"}, {"severity": "Medium", "title": "Overflow"}]
More output after JSON
"#;
        let mut f = std::fs::File::create(&log_path).unwrap();
        f.write_all(log_content.as_bytes()).unwrap();

        let findings = extract_findings_from_log(&log_path).unwrap();
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0]["severity"], "High");
        assert_eq!(findings[1]["title"], "Overflow");
    }

    #[test]
    fn extract_findings_picks_largest_array() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("agent.log");

        let log_content = r#"
[{"severity": "Low", "title": "Minor"}]
some text
[{"severity": "High", "title": "Big"}, {"severity": "Medium", "title": "Med"}, {"severity": "Low", "title": "Small"}]
"#;
        let mut f = std::fs::File::create(&log_path).unwrap();
        f.write_all(log_content.as_bytes()).unwrap();

        let findings = extract_findings_from_log(&log_path).unwrap();
        assert_eq!(findings.len(), 3);
    }

    #[test]
    fn extract_findings_empty_log_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("agent.log");

        std::fs::write(&log_path, "no json here at all").unwrap();

        let findings = extract_findings_from_log(&log_path).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn try_parse_json_array_with_valid_json() {
        let input = r#"[{"a": 1}, {"b": 2}]"#;
        let result = try_parse_json_array(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn try_parse_json_array_with_nested_brackets() {
        let input = r#"[{"data": [1, 2, 3]}, {"more": {"nested": [4]}}]"#;
        let result = try_parse_json_array(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn try_parse_json_array_with_trailing_text() {
        let input = r#"[{"a": 1}] some trailing garbage"#;
        let result = try_parse_json_array(input);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn try_parse_json_array_invalid_returns_none() {
        let input = "not json at all";
        assert!(try_parse_json_array(input).is_none());
    }

    #[test]
    fn try_parse_json_array_unclosed_bracket_returns_none() {
        let input = r#"[{"a": 1}"#;
        assert!(try_parse_json_array(input).is_none());
    }

    #[test]
    fn looks_like_findings_true_for_objects_with_severity() {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(r#"[{"severity": "High", "title": "Bug"}]"#).unwrap();
        assert!(looks_like_findings(&arr));
    }

    #[test]
    fn looks_like_findings_false_for_empty_array() {
        let arr: Vec<serde_json::Value> = vec![];
        assert!(!looks_like_findings(&arr));
    }

    #[test]
    fn looks_like_findings_false_for_objects_without_severity() {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(r#"[{"name": "something", "value": 42}]"#).unwrap();
        assert!(!looks_like_findings(&arr));
    }

    // macOS can hold the first exec of a freshly written script for seconds, so a test that needs
    // the child to speak in time gives it a wide window.
    fn fake_claude(dir: &Path, body: &str) -> ClaudeSession {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        ClaudeSession {
            claude_bin: bin,
            prompt: "p".into(),
            max_turns: 1,
            working_dir: dir.to_path_buf(),
            log_file: dir.join("s.log"),
            model: None,
            allowed_tools: vec![],
            timeout_minutes: None,
            label: "test".into(),
            budget_usd: None,
            ledger: None,
            resume: None,
        }
    }

    #[tokio::test]
    async fn streamed_result_is_parsed() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = fake_claude(
            dir.path(),
            r#"echo '{"type":"system","subtype":"init"}'
echo '{"type":"result","subtype":"success","result":"done","total_cost_usd":0.5,"num_turns":3}'"#,
        );
        let out = s.run_watched(Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.cost_usd, Some(0.5));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("s.log")).unwrap(),
            "done"
        );
        let live = std::fs::read_to_string(dir.path().join("s.log.live.jsonl")).unwrap();
        assert_eq!(live.lines().count(), 2);
    }

    #[tokio::test]
    async fn silent_session_is_restarted_then_reported_stalled() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = fake_claude(dir.path(), "exec sleep 30");
        let out = s.run_watched(Duration::from_secs(1)).await.unwrap();
        assert_eq!(out.exit_code, STALLED_EXIT);
        assert_eq!(out.attempts, MAX_STALL_RESTARTS + 1);
    }

    #[tokio::test]
    async fn stall_restart_resumes_the_session_it_saw() {
        let dir = tempfile::TempDir::new().unwrap();
        let args = dir.path().join("args");
        let s = fake_claude(
            dir.path(),
            &format!(
                r#"echo "$@" >> {}
case "$1" in
  --resume) echo '{{"type":"result","subtype":"success","result":"resumed"}}' ;;
  *) echo '{{"type":"system","subtype":"init","session_id":"abc-123"}}'; exec sleep 30 ;;
esac"#,
                args.display()
            ),
        );
        let out = s.run_watched(Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.session_id.as_deref(), Some("abc-123"));
        let calls = std::fs::read_to_string(&args).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].starts_with("-p --max-turns"));
        assert!(calls[1].starts_with("--resume abc-123 -p --max-turns"));
        // The resumed attempt appends to the stream rather than replacing it.
        let live = std::fs::read_to_string(dir.path().join("s.log.live.jsonl")).unwrap();
        assert_eq!(live.lines().count(), 2);
    }

    #[tokio::test]
    async fn a_prompt_too_large_for_argv_reaches_the_cli() {
        let dir = tempfile::TempDir::new().unwrap();
        let got = dir.path().join("stdin");
        let mut s = fake_claude(
            dir.path(),
            &format!(
                r#"cat > {}
echo '{{"type":"result","subtype":"success","result":"ok"}}'"#,
                got.display()
            ),
        );
        s.prompt = "x".repeat(4 << 20);
        let out = s.run_watched(Duration::from_secs(10)).await.unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(std::fs::metadata(&got).unwrap().len(), 4 << 20);
    }

    #[tokio::test]
    async fn steady_output_keeps_a_session_alive() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = fake_claude(
            dir.path(),
            r#"for i in 1 2 3 4; do echo '{"type":"assistant"}'; sleep 0.5; done
echo '{"type":"result","subtype":"success","result":"ok"}'"#,
        );
        let out = s.run_watched(Duration::from_secs(5)).await.unwrap();
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn is_skill_available_returns_false_for_nonexistent_skill() {
        assert!(!is_skill_available(
            "nonexistent-skill-that-should-never-exist-12345"
        ));
    }

    #[test]
    fn is_skill_available_returns_true_for_existing_skill() {
        let dir = tempfile::TempDir::new().unwrap();
        let commands_dir = dir.path().join(".claude/commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(commands_dir.join("test-skill.md"), "# Test Skill").unwrap();
        let skill_dir = dir.path().join(".claude/skills/tob-test-dir-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: tob-test-dir-skill\n---",
        )
        .unwrap();

        // Temporarily override HOME (unsafe since Rust 1.85 — process-wide)
        let original_home = std::env::var("HOME").unwrap_or_default();
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        let result = is_skill_available("test-skill") && is_skill_available("tob-test-dir-skill");
        unsafe {
            std::env::set_var("HOME", &original_home);
        }
        assert!(result);
    }

    #[test]
    fn auth_status_display() {
        assert_eq!(AuthStatus::ApiKey.to_string(), "API key");
        assert_eq!(AuthStatus::Login.to_string(), "Claude login");
        assert_eq!(
            AuthStatus::NotAuthenticated.to_string(),
            "not authenticated"
        );
    }

    #[test]
    fn auth_status_is_authenticated() {
        assert!(AuthStatus::ApiKey.is_authenticated());
        assert!(AuthStatus::Login.is_authenticated());
        assert!(!AuthStatus::NotAuthenticated.is_authenticated());
    }
}
