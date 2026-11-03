use crate::budget::{self, Ledger};
use crate::error::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

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
    /// Hard wall-clock timeout for the subprocess. If set, the command is
    /// wrapped with the system `timeout` utility and killed after this many
    /// minutes. Exit code 124 means timeout was hit.
    pub timeout_minutes: Option<u32>,
    /// Ledger name for this session, e.g. `poc:F-003`.
    pub label: String,
    /// Dollar cap passed as `--max-budget-usd`; `None` is unlimited.
    pub budget_usd: Option<f64>,
    pub ledger: Option<Arc<Ledger>>,
}

/// Result of a completed Claude session.
pub struct ClaudeOutput {
    pub exit_code: i32,
    pub log_file: PathBuf,
    pub cost_usd: Option<f64>,
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

impl ClaudeSession {
    /// Run the Claude session, writing stdout+stderr to the log file.
    pub async fn run(&self) -> Result<ClaudeOutput> {
        budget::check_cap(self.budget_usd)?;
        let reservation = match &self.ledger {
            Some(l) => Some(l.reserve(&self.label, self.model.clone(), self.budget_usd)?),
            None => None,
        };
        // If a timeout is configured, wrap with system `timeout` utility so the
        // subprocess is hard-killed after the wall-clock limit.
        let mut cmd = if let Some(mins) = self.timeout_minutes {
            let mut c = Command::new("timeout");
            c.arg(format!("{}m", mins)).arg(&self.claude_bin);
            c
        } else {
            Command::new(&self.claude_bin)
        };

        cmd.arg("-p")
            .arg(&self.prompt)
            .arg("--max-turns")
            .arg(self.max_turns.to_string())
            .arg("--verbose")
            .arg("--output-format")
            .arg("json");

        if let Some(cap) = self.budget_usd {
            let cli_cap = cap * budget::CLI_CAP_FRACTION;
            cmd.arg("--max-budget-usd").arg(format!("{cli_cap:.2}"));
        }

        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }

        if !self.allowed_tools.is_empty() {
            for tool in &self.allowed_tools {
                cmd.arg("--allowedTools").arg(tool);
            }
        }

        let output = match cmd.current_dir(&self.working_dir).output().await {
            Ok(o) => o,
            Err(e) => {
                // Never started, so nothing was spent.
                if let (Some(l), Some(i)) = (&self.ledger, reservation) {
                    l.settle(i, Some(0.0), None, "spawn_failed")?;
                }
                return Err(e.into());
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed = parse_session_json(&stdout);

        // The log keeps the session's final text, which the passes parse; raw JSON sits beside it.
        let mut log_content = Vec::new();
        match &parsed {
            Some(r) => log_content.extend_from_slice(r.text.as_bytes()),
            None => log_content.extend_from_slice(&output.stdout),
        }
        if !output.stderr.is_empty() {
            log_content.extend_from_slice(b"\n--- STDERR ---\n");
            log_content.extend_from_slice(&output.stderr);
        }
        std::fs::write(&self.log_file, &log_content)?;
        let mut raw = self.log_file.clone().into_os_string();
        raw.push(".json");
        std::fs::write(raw, &output.stdout)?;

        let cost_usd = parsed.as_ref().and_then(|r| r.cost_usd);
        if let (Some(l), Some(i)) = (&self.ledger, reservation) {
            let outcome = parsed.as_ref().map_or("no_result", |r| r.outcome.as_str());
            l.settle(i, cost_usd, parsed.as_ref().and_then(|r| r.turns), outcome)?;
        }

        Ok(ClaudeOutput {
            exit_code: output.status.code().unwrap_or(-1),
            log_file: self.log_file.clone(),
            cost_usd,
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
            Ok(output) if output.exit_code == 124 => {
                pb.finish_with_message(format!("{label} TIMED OUT"));
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
/// Skills are markdown files installed to `~/.claude/commands/<name>.md`.
/// In the Docker container, the install-skills.sh script copies them there.
pub fn is_skill_available(skill_name: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = format!("{home}/.claude/commands/{skill_name}.md");
    std::path::Path::new(&path).exists()
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
    arr[0].as_object().is_some_and(|obj| obj.contains_key("severity"))
}

#[cfg(test)]
mod tests {
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
        let arr: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"severity": "High", "title": "Bug"}]"#,
        )
        .unwrap();
        assert!(looks_like_findings(&arr));
    }

    #[test]
    fn looks_like_findings_false_for_empty_array() {
        let arr: Vec<serde_json::Value> = vec![];
        assert!(!looks_like_findings(&arr));
    }

    #[test]
    fn looks_like_findings_false_for_objects_without_severity() {
        let arr: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"name": "something", "value": 42}]"#,
        )
        .unwrap();
        assert!(!looks_like_findings(&arr));
    }

    #[test]
    fn is_skill_available_returns_false_for_nonexistent_skill() {
        assert!(!is_skill_available("nonexistent-skill-that-should-never-exist-12345"));
    }

    #[test]
    fn is_skill_available_returns_true_for_existing_skill() {
        let dir = tempfile::TempDir::new().unwrap();
        let commands_dir = dir.path().join(".claude/commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(commands_dir.join("test-skill.md"), "# Test Skill").unwrap();

        // Temporarily override HOME (unsafe since Rust 1.85 — process-wide)
        let original_home = std::env::var("HOME").unwrap_or_default();
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        let result = is_skill_available("test-skill");
        unsafe {
            std::env::set_var("HOME", &original_home);
        }
        assert!(result);
    }

    #[test]
    fn auth_status_display() {
        assert_eq!(AuthStatus::ApiKey.to_string(), "API key");
        assert_eq!(AuthStatus::Login.to_string(), "Claude login");
        assert_eq!(AuthStatus::NotAuthenticated.to_string(), "not authenticated");
    }

    #[test]
    fn auth_status_is_authenticated() {
        assert!(AuthStatus::ApiKey.is_authenticated());
        assert!(AuthStatus::Login.is_authenticated());
        assert!(!AuthStatus::NotAuthenticated.is_authenticated());
    }
}
