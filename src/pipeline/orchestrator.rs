use crate::config::Config;
use crate::error::{BulwarkError, Result};
use crate::pipeline::checkpoint::PipelineStatus;
use crate::pipeline::pass::{PassNumber, PassResult, PassStatus, PipelineContext};
use crate::tools::claude;
use crate::workspace::Workspace;
use console::style;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, error, info, warn};

pub struct Orchestrator {
    pub ctx: PipelineContext,
    pub start_pass: u8,
    pub end_pass: u8,
    pub agent_filter: Option<String>,
}

impl Orchestrator {
    pub fn new(
        config: Config,
        audit_dir: PathBuf,
        bulwark_root: PathBuf,
        start: u8,
        end: u8,
        agent_filter: Option<String>,
    ) -> Result<Self> {
        let workspace = Workspace::new(&audit_dir, &config.workspace.path);

        // BULWARK_INSTALL points to the generic install dir (baked-in prompts/schemas/context).
        // Falls back to bulwark_root so single-directory setups keep working.
        let bulwark_install = std::env::var("BULWARK_INSTALL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| bulwark_root.clone());

        Ok(Self {
            ctx: PipelineContext {
                config,
                workspace,
                audit_dir,
                bulwark_root,
                bulwark_install,
            },
            start_pass: start,
            end_pass: end,
            agent_filter,
        })
    }

    pub async fn run(&self) -> Result<Vec<PassResult>> {
        self.print_banner();
        self.preflight()?;
        self.ctx.workspace.init()?;
        self.stage_context_files();

        let status_file = self.ctx.workspace.pipeline_status_file();
        let mut status = PipelineStatus::load(&status_file)?;
        let mut results = Vec::new();

        let pipeline_start = Instant::now();

        let claude_auth = claude::check_auth();
        debug!(auth = %claude_auth, "Claude auth check");
        eprintln!(
            "  {} Claude auth: {}",
            if claude_auth.is_authenticated() {
                style("✓").green()
            } else {
                style("⚠").yellow()
            },
            claude_auth
        );
        eprintln!(
            "  {} Audit dir:   {}",
            style("→").cyan(),
            self.ctx.audit_dir.display()
        );
        eprintln!(
            "  {} Workspace:   {}",
            style("→").cyan(),
            self.ctx.workspace.root().display()
        );
        eprintln!(
            "  {} Passes:      {} through {}",
            style("→").cyan(),
            self.start_pass,
            self.end_pass
        );
        eprintln!();

        for pass_num in PassNumber::all() {
            let num = *pass_num as u8;
            if num < self.start_pass || num > self.end_pass {
                continue;
            }

            // Check if pass is enabled in config
            if !self.is_pass_enabled(*pass_num) {
                let result = PassResult {
                    name: pass_num.name().into(),
                    status: PassStatus::Skipped,
                    duration_secs: 0,
                    summary: "disabled in config".into(),
                };
                status.record(&result, &status_file)?;
                results.push(result);
                continue;
            }

            // Skip AI passes if not authenticated
            if pass_num.requires_claude() && !claude_auth.is_authenticated() {
                warn!(pass = num, name = pass_num.name(), "skipping — Claude not authenticated");
                eprintln!(
                    "  {} Skipping Pass {} ({}) — Claude not authenticated",
                    style("⚠").yellow(),
                    num,
                    pass_num.description()
                );
                let result = PassResult {
                    name: pass_num.name().into(),
                    status: PassStatus::Skipped,
                    duration_secs: 0,
                    summary: "Claude not authenticated".into(),
                };
                status.record(&result, &status_file)?;
                results.push(result);
                continue;
            }

            eprintln!(
                "\n{}\n",
                style(format!(
                    "━━━ Pass {}: {} ━━━",
                    num,
                    pass_num.description()
                ))
                .blue()
                .bold()
            );

            let pass_start = Instant::now();
            info!(pass = num, name = pass_num.name(), "starting");
            let result = self.execute_pass(*pass_num).await;
            let duration = pass_start.elapsed().as_secs();

            let pass_result = match result {
                Ok(summary) => {
                    info!(pass = num, duration, "completed");
                    eprintln!(
                        "  {} Pass {} completed in {}s",
                        style("✓").green(),
                        num,
                        duration
                    );
                    PassResult {
                        name: pass_num.name().into(),
                        status: PassStatus::Completed,
                        duration_secs: duration,
                        summary,
                    }
                }
                Err(e) => {
                    error!(pass = num, duration, err = %e, "FAILED");
                    eprintln!(
                        "  {} Pass {} FAILED after {}s: {}",
                        style("✗").red(),
                        num,
                        duration,
                        e
                    );
                    PassResult {
                        name: pass_num.name().into(),
                        status: PassStatus::Failed,
                        duration_secs: duration,
                        summary: e.to_string(),
                    }
                }
            };

            status.record(&pass_result, &status_file)?;
            let failed = pass_result.status == PassStatus::Failed;
            results.push(pass_result);

            if failed {
                break;
            }
        }

        self.print_summary(&results, pipeline_start.elapsed().as_secs());
        self.export_reports_to_host();
        Ok(results)
    }

    /// Copy final reports to the host-mounted reports directory if it exists.
    fn export_reports_to_host(&self) {
        let reports_dir = std::path::Path::new("/home/auditor/reports");
        if !reports_dir.exists() {
            return;
        }

        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let md_src = self.ctx.workspace.final_report_md();
        let json_src = self.ctx.workspace.final_report_json();

        let mut copied = vec![];

        if md_src.exists() {
            let dest = reports_dir.join(format!("audit-report-{timestamp}.md"));
            if std::fs::copy(&md_src, &dest).is_ok() {
                copied.push(dest);
            }
        }
        if json_src.exists() {
            let dest = reports_dir.join(format!("audit-report-{timestamp}.json"));
            if std::fs::copy(&json_src, &dest).is_ok() {
                copied.push(dest);
            }
        }

        if !copied.is_empty() {
            eprintln!("\n  {} Reports exported to host:", style("✓").green());
            for path in &copied {
                eprintln!("    {}", path.display());
            }
        }
    }

    async fn execute_pass(&self, pass: PassNumber) -> Result<String> {
        match pass {
            PassNumber::Recon => crate::passes::recon::run(&self.ctx).await,
            PassNumber::Agents => {
                crate::passes::agents::run(&self.ctx, self.agent_filter.as_deref()).await
            }
            PassNumber::Poc => crate::passes::poc::run(&self.ctx).await,
            PassNumber::Fuzzing => crate::passes::fuzzing::run(&self.ctx).await,
            PassNumber::Formal => crate::passes::formal::run(&self.ctx).await,
            PassNumber::Review => crate::passes::review::run(&self.ctx).await,
        }
    }

    fn is_pass_enabled(&self, pass: PassNumber) -> bool {
        let passes = &self.ctx.config.passes;
        match pass {
            PassNumber::Recon => passes.recon.enabled,
            PassNumber::Agents => passes.agents.enabled,
            PassNumber::Poc => passes.poc.enabled,
            PassNumber::Fuzzing => passes.fuzzing.enabled,
            PassNumber::Formal => passes.formal.enabled,
            PassNumber::Review => passes.review.enabled,
        }
    }

    /// Copy context files into audit_dir so Claude agents can read them.
    ///
    /// Resolution order (first match wins):
    /// 1. Already present in audit_dir (from a previous run or manual placement)
    /// 2. Project context dir: `bulwark_root/context/`
    /// 3. Install context dir: `bulwark_install/context/`
    ///
    /// `ATTACK_PATTERNS.md` always comes from the install dir — it's accumulated generic
    /// knowledge that shouldn't be overridden per-project.
    fn stage_context_files(&self) {
        let project_ctx = self.ctx.bulwark_root.join("context");
        let install_ctx = self.ctx.bulwark_install.join("context");

        let files = [
            ("AUDIT_CONTEXT.md", false),
            ("PROPERTIES.md", false),
            ("KNOWN_ISSUES.md", false),
            ("ATTACK_PATTERNS.md", true), // always from install
        ];

        for (filename, install_only) in &files {
            let dest = self.ctx.audit_dir.join(filename);
            if dest.exists() {
                continue; // already in place
            }

            if !install_only {
                let project_file = project_ctx.join(filename);
                if project_file.exists() {
                    if std::fs::copy(&project_file, &dest).is_ok() {
                        debug!("staged {filename} from project context");
                        continue;
                    }
                }
            }

            let install_file = install_ctx.join(filename);
            if install_file.exists() {
                if std::fs::copy(&install_file, &dest).is_ok() {
                    debug!("staged {filename} from install context");
                }
            }
        }
    }

    fn preflight(&self) -> Result<()> {
        if !self.ctx.audit_dir.exists() {
            return Err(BulwarkError::PrerequisiteNotMet {
                pass: "preflight".into(),
                reason: format!(
                    "audit directory not found: {}",
                    self.ctx.audit_dir.display()
                ),
            });
        }
        Ok(())
    }

    fn print_banner(&self) {
        let banner = r#"
    ╔══════════════════════════════════════════════════╗
    ║                                                  ║
    ║   ██████  ██  ██ ██     ██  ██  ██████  ██  ██   ║
    ║   ██   █  ██  ██ ██     ██  ██  ██  ██  ██ ██    ║
    ║   ██████  ██  ██ ██  █  ██  ██████  █████  ██    ║
    ║   ██   █  ██  ██ ██ ███ ██  ██  ██  ██ ██  ██    ║
    ║   ██████  ██████ ████ ████  ██  ██  ██  ██ ██    ║
    ║                                                  ║
    ║   Multi-Pass Smart Contract Audit Pipeline       ║
    ║                                                  ║
    ╚══════════════════════════════════════════════════╝
"#;
        eprintln!("{}", style(banner).cyan().bold());
    }

    fn print_summary(&self, results: &[PassResult], total_secs: u64) {
        eprintln!(
            "\n{}\n",
            style("━━━ Pipeline Complete ━━━").blue().bold()
        );

        for r in results {
            let status_str = r.status.to_string();
            let status_styled = match r.status {
                PassStatus::Completed => style(&status_str).green(),
                PassStatus::Failed => style(&status_str).red(),
                PassStatus::Skipped => style(&status_str).yellow(),
                PassStatus::NotImplemented => style(&status_str).dim(),
            };
            eprintln!(
                "  {}: {} ({}s)",
                style(&r.name).bold(),
                status_styled,
                r.duration_secs
            );
        }

        eprintln!(
            "\n  Total: {}m {}s",
            total_secs / 60,
            total_secs % 60
        );
        eprintln!(
            "  Workspace: {}",
            self.ctx.workspace.root().display()
        );
        eprintln!();
    }
}
