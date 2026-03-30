use crate::error::Result;
use std::path::{Path, PathBuf};

/// Manages the audit workspace directory structure.
///
/// All inter-pass communication flows through files in this workspace.
/// Each pass reads from previous pass directories and writes to its own.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(audit_dir: &Path, workspace_rel: &str) -> Self {
        Self {
            root: audit_dir.join(workspace_rel),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── Pass directories ──────────────────────────────────────────

    pub fn recon_dir(&self) -> PathBuf {
        self.root.join("recon")
    }

    pub fn findings_dir(&self) -> PathBuf {
        self.root.join("findings")
    }

    pub fn findings_logs_dir(&self) -> PathBuf {
        self.root.join("findings/logs")
    }

    pub fn pocs_dir(&self) -> PathBuf {
        self.root.join("pocs")
    }

    pub fn fuzzing_dir(&self) -> PathBuf {
        self.root.join("fuzzing")
    }

    pub fn formal_dir(&self) -> PathBuf {
        self.root.join("formal")
    }

    pub fn review_dir(&self) -> PathBuf {
        self.root.join("review")
    }

    // ── Key files ─────────────────────────────────────────────────

    pub fn pipeline_status_file(&self) -> PathBuf {
        self.root.join("pipeline-status.json")
    }

    pub fn recon_summary(&self) -> PathBuf {
        self.recon_dir().join("recon-summary.json")
    }

    pub fn merged_findings(&self) -> PathBuf {
        self.findings_dir().join("merged-deduplicated.json")
    }

    pub fn validated_findings(&self) -> PathBuf {
        self.pocs_dir().join("validated-findings.json")
    }

    pub fn discarded_findings(&self) -> PathBuf {
        self.pocs_dir().join("discarded-findings.json")
    }

    pub fn final_report_md(&self) -> PathBuf {
        self.root.join("final-report.md")
    }

    pub fn final_report_json(&self) -> PathBuf {
        self.root.join("final-report.json")
    }

    // ── Agent output files ────────────────────────────────────────

    pub fn agent_raw_output(&self, agent: &str) -> PathBuf {
        self.findings_dir().join(format!("{agent}-agent-raw.json"))
    }

    pub fn agent_log(&self, agent: &str) -> PathBuf {
        self.findings_logs_dir().join(format!("{agent}-agent.log"))
    }

    pub fn property_verifications(&self) -> PathBuf {
        self.findings_dir().join("property-verifications.json")
    }

    // ── Initialisation ────────────────────────────────────────────

    /// Create all workspace directories. Idempotent.
    pub fn init(&self) -> Result<()> {
        let dirs = [
            self.recon_dir(),
            self.findings_dir(),
            self.findings_logs_dir(),
            self.pocs_dir(),
            self.fuzzing_dir().join("invariant-tests"),
            self.fuzzing_dir().join("fuzzing-campaign-results"),
            self.formal_dir(),
            self.review_dir(),
        ];

        for dir in &dirs {
            std::fs::create_dir_all(dir)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn workspace_new_constructs_correct_root() {
        let audit_dir = Path::new("/home/auditor/audits/graph");
        let ws = Workspace::new(audit_dir, "audit-workspace");

        assert_eq!(
            ws.root(),
            Path::new("/home/auditor/audits/graph/audit-workspace")
        );
    }

    #[test]
    fn directory_accessors_return_expected_paths() {
        let ws = Workspace::new(Path::new("/audit"), "ws");
        let root = Path::new("/audit/ws");

        assert_eq!(ws.recon_dir(), root.join("recon"));
        assert_eq!(ws.findings_dir(), root.join("findings"));
        assert_eq!(ws.findings_logs_dir(), root.join("findings/logs"));
        assert_eq!(ws.pocs_dir(), root.join("pocs"));
        assert_eq!(ws.fuzzing_dir(), root.join("fuzzing"));
        assert_eq!(ws.formal_dir(), root.join("formal"));
        assert_eq!(ws.review_dir(), root.join("review"));
    }

    #[test]
    fn key_file_paths_are_correct() {
        let ws = Workspace::new(Path::new("/audit"), "ws");
        let root = Path::new("/audit/ws");

        assert_eq!(ws.pipeline_status_file(), root.join("pipeline-status.json"));
        assert_eq!(ws.recon_summary(), root.join("recon/recon-summary.json"));
        assert_eq!(
            ws.merged_findings(),
            root.join("findings/merged-deduplicated.json")
        );
        assert_eq!(
            ws.validated_findings(),
            root.join("pocs/validated-findings.json")
        );
        assert_eq!(
            ws.discarded_findings(),
            root.join("pocs/discarded-findings.json")
        );
        assert_eq!(ws.final_report_md(), root.join("final-report.md"));
        assert_eq!(ws.final_report_json(), root.join("final-report.json"));
    }

    #[test]
    fn agent_output_paths_include_agent_name() {
        let ws = Workspace::new(Path::new("/audit"), "ws");

        assert_eq!(
            ws.agent_raw_output("red"),
            Path::new("/audit/ws/findings/red-agent-raw.json")
        );
        assert_eq!(
            ws.agent_raw_output("blue"),
            Path::new("/audit/ws/findings/blue-agent-raw.json")
        );
        assert_eq!(
            ws.agent_raw_output("gold"),
            Path::new("/audit/ws/findings/gold-agent-raw.json")
        );

        assert_eq!(
            ws.agent_log("red"),
            Path::new("/audit/ws/findings/logs/red-agent.log")
        );
        assert_eq!(
            ws.agent_log("blue"),
            Path::new("/audit/ws/findings/logs/blue-agent.log")
        );
    }

    #[test]
    fn init_creates_all_directories() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = Workspace::new(tmp.path(), "ws");

        ws.init().expect("init should succeed");

        assert!(ws.recon_dir().exists());
        assert!(ws.findings_dir().exists());
        assert!(ws.findings_logs_dir().exists());
        assert!(ws.pocs_dir().exists());
        assert!(ws.fuzzing_dir().join("invariant-tests").exists());
        assert!(ws.fuzzing_dir().join("fuzzing-campaign-results").exists());
        assert!(ws.formal_dir().exists());
        assert!(ws.review_dir().exists());
    }

    #[test]
    fn init_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = Workspace::new(tmp.path(), "ws");

        ws.init().expect("first init");
        ws.init().expect("second init should also succeed");

        assert!(ws.recon_dir().exists());
    }

    #[test]
    fn property_verifications_path() {
        let ws = Workspace::new(Path::new("/audit"), "ws");
        assert_eq!(
            ws.property_verifications(),
            Path::new("/audit/ws/findings/property-verifications.json")
        );
    }
}
