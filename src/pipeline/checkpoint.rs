use crate::error::Result;
use crate::pipeline::pass::{PassResult, PassStatus};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Persistent pipeline status, written to pipeline-status.json after each pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStatus {
    pub passes: HashMap<String, PassRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassRecord {
    pub status: PassStatus,
    pub duration: u64,
    pub completed: String,
}

impl PipelineStatus {
    /// Load from file, or create empty if file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let status: Self = serde_json::from_str(&content)?;
            Ok(status)
        } else {
            Ok(Self {
                passes: HashMap::new(),
            })
        }
    }

    /// Record a pass result and write to disk.
    pub fn record(&mut self, result: &PassResult, path: &Path) -> Result<()> {
        self.passes.insert(
            result.name.clone(),
            PassRecord {
                status: result.status,
                duration: result.duration_secs,
                completed: Utc::now().to_rfc3339(),
            },
        );
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Check if a pass has completed successfully.
    pub fn is_completed(&self, pass_name: &str) -> bool {
        self.passes
            .get(pass_name)
            .is_some_and(|r| r.status == PassStatus::Completed)
    }
}
