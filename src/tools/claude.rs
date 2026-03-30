use crate::error::Result;
use std::path::{Path, PathBuf};
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
}

/// Result of a completed Claude session.
pub struct ClaudeOutput {
    pub exit_code: i32,
    pub log_file: PathBuf,
}

impl ClaudeSession {
    /// Run the Claude session, writing stdout+stderr to the log file.
    pub async fn run(&self) -> Result<ClaudeOutput> {
        let mut cmd = Command::new(&self.claude_bin);
        cmd.arg("-p")
            .arg(&self.prompt)
            .arg("--max-turns")
            .arg(self.max_turns.to_string())
            .arg("--verbose");

        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }

        let output = cmd
            .current_dir(&self.working_dir)
            .output()
            .await?;

        // Write combined output to log file
        let mut log_content = Vec::new();
        log_content.extend_from_slice(&output.stdout);
        if !output.stderr.is_empty() {
            log_content.extend_from_slice(b"\n--- STDERR ---\n");
            log_content.extend_from_slice(&output.stderr);
        }
        std::fs::write(&self.log_file, &log_content)?;

        Ok(ClaudeOutput {
            exit_code: output.status.code().unwrap_or(-1),
            log_file: self.log_file.clone(),
        })
    }
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
