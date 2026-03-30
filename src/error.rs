use std::path::PathBuf;

#[derive(thiserror::Error, Debug)]
pub enum DoyranError {
    #[error("pass '{pass}' prerequisite not met: {reason}")]
    PrerequisiteNotMet { pass: String, reason: String },

    #[error("tool not found: {tool} (install with: {hint})")]
    ToolNotFound { tool: String, hint: String },

    #[error("agent '{agent}' failed after {elapsed}s: {reason}")]
    AgentFailed {
        agent: String,
        elapsed: u64,
        reason: String,
    },

    #[error("agent '{agent}' timed out after {timeout}s")]
    AgentTimeout { agent: String, timeout: u64 },

    #[error("JSON schema validation failed for {file}: {errors:?}")]
    SchemaValidation { file: PathBuf, errors: Vec<String> },

    #[error("pass '{pass}' timed out after {timeout}s")]
    PassTimeout { pass: String, timeout: u64 },

    #[error("checkpoint not found for pass '{pass}'")]
    CheckpointNotFound { pass: String },

    #[error("config error: {0}")]
    Config(String),

    #[error("command '{command}' failed with exit code {code}: {stderr}")]
    CommandFailed {
        command: String,
        code: i32,
        stderr: String,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    TomlParse(#[from] toml::de::Error),
}

pub type Result<T> = std::result::Result<T, DoyranError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerequisite_not_met_display_message() {
        let err = DoyranError::PrerequisiteNotMet {
            pass: "poc".into(),
            reason: "merged findings not found".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("poc"));
        assert!(msg.contains("prerequisite not met"));
        assert!(msg.contains("merged findings not found"));
    }

    #[test]
    fn tool_not_found_display_message() {
        let err = DoyranError::ToolNotFound {
            tool: "forge".into(),
            hint: "curl -L https://foundry.paradigm.xyz | bash".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("forge"));
        assert!(msg.contains("tool not found"));
        assert!(msg.contains("curl -L https://foundry.paradigm.xyz | bash"));
    }

    #[test]
    fn agent_failed_display_message() {
        let err = DoyranError::AgentFailed {
            agent: "red".into(),
            elapsed: 120,
            reason: "non-zero exit".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("red"));
        assert!(msg.contains("120s"));
        assert!(msg.contains("non-zero exit"));
    }

    #[test]
    fn agent_timeout_display_message() {
        let err = DoyranError::AgentTimeout {
            agent: "blue".into(),
            timeout: 3600,
        };
        let msg = err.to_string();
        assert!(msg.contains("blue"));
        assert!(msg.contains("timed out"));
        assert!(msg.contains("3600s"));
    }

    #[test]
    fn config_error_display_message() {
        let err = DoyranError::Config("missing required field".into());
        let msg = err.to_string();
        assert!(msg.contains("config error"));
        assert!(msg.contains("missing required field"));
    }

    #[test]
    fn command_failed_display_message() {
        let err = DoyranError::CommandFailed {
            command: "forge build".into(),
            code: 1,
            stderr: "compilation failed".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("forge build"));
        assert!(msg.contains("1"));
        assert!(msg.contains("compilation failed"));
    }

    #[test]
    fn schema_validation_display_message() {
        let err = DoyranError::SchemaValidation {
            file: PathBuf::from("findings.json"),
            errors: vec!["missing field: severity".into()],
        };
        let msg = err.to_string();
        assert!(msg.contains("findings.json"));
        assert!(msg.contains("missing field: severity"));
    }
}
