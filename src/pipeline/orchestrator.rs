use crate::budget::Ledger;
use crate::config::Config;
use crate::error::{BulwarkError, Result};
use crate::pipeline::checkpoint::PipelineStatus;
use crate::pipeline::pass::{PassNumber, PassResult, PassStatus, PipelineContext};
use crate::tools::claude;
use crate::workspace::Workspace;
use console::style;
use std::path::{Path, PathBuf};
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

        // Starting at pass 1 is a new run; a later start continues the last run's spend.
        let max = config.budget.max_usd;
        let ledger = Ledger::open(
            workspace.cost_ledger(),
            max.is_finite().then_some(max),
            start <= 1,
        )?;

        Ok(Self {
            ctx: PipelineContext {
                config,
                workspace,
                audit_dir,
                bulwark_root,
                bulwark_install,
                ledger,
            },
            start_pass: start,
            end_pass: end,
            agent_filter,
        })
    }

    pub fn ledger(&self) -> std::sync::Arc<Ledger> {
        self.ctx.ledger.clone()
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
            if claude::auth_failed() {
                return Err(BulwarkError::ClaudeAuth(
                    "fix the login and resume with --resume".into(),
                ));
            }
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
                warn!(
                    pass = num,
                    name = pass_num.name(),
                    "skipping — Claude not authenticated"
                );
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
                style(format!("━━━ Pass {}: {} ━━━", num, pass_num.description()))
                    .blue()
                    .bold()
            );

            let pass_start = Instant::now();
            info!(pass = num, name = pass_num.name(), "starting");
            // Structured progress line read by the bulwark-cloud Python orchestrator.
            println!("[pass:{num}] [status:running]");
            let result = self.execute_pass(*pass_num).await;
            let duration = pass_start.elapsed().as_secs();

            let pass_result = match result {
                Ok(summary) => {
                    info!(pass = num, duration, "completed");
                    println!("[pass:{num}] [status:done] [duration_s:{duration}]");
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
                    println!("[pass:{num}] [status:failed] [duration_s:{duration}]");
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
        match crate::submission::write_all(
            &self.ctx.config,
            &self.ctx.audit_dir,
            self.ctx.workspace.root(),
        ) {
            Ok(drafts) if !drafts.is_empty() => eprintln!(
                "  {} {} submission draft(s) in {}",
                style("✓").green(),
                drafts.len(),
                self.ctx.workspace.root().join("submissions").display()
            ),
            Ok(_) => {}
            Err(e) => eprintln!(
                "  {} Submission drafts not written: {e}",
                style("⚠").yellow()
            ),
        }
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

        let drafts = self.ctx.workspace.root().join("submissions");
        if drafts.is_dir() {
            let dest = reports_dir.join(format!("submissions-{timestamp}"));
            if std::fs::create_dir_all(&dest).is_ok() {
                for e in std::fs::read_dir(&drafts).into_iter().flatten().flatten() {
                    let _ = std::fs::copy(e.path(), dest.join(e.file_name()));
                }
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

    /// Copy the project's context files into audit_dir so agents can read them.
    ///
    /// They come from the `context/` beside the config in use. The install's copies describe
    /// The Graph, so they are read only when no project config is mounted. On a run from Pass 1 a file the project does not supply is removed, so a
    /// stale one from an earlier run is never read and AUDIT_CONTEXT.md and PROPERTIES.md are
    /// derived afresh; a resumed run keeps what its Pass 1 derived.
    fn stage_context_files(&self) {
        let project_ctx = if self.ctx.bulwark_root.join("bulwark.toml").exists() {
            self.ctx.bulwark_root.join("context")
        } else {
            self.ctx.bulwark_install.join("context")
        };
        stage_context(&project_ctx, &self.ctx.audit_dir, self.start_pass <= 1);
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
        eprintln!("\n{}\n", style("━━━ Pipeline Complete ━━━").blue().bold());

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

        eprintln!("\n  Total: {}m {}s", total_secs / 60, total_secs % 60);
        eprintln!("  Workspace: {}", self.ctx.workspace.root().display());
        let spent = self.ctx.ledger.spent();
        match self.ctx.ledger.remaining() {
            Some(left) => eprintln!(
                "  Claude spend: ${spent:.2} of ${:.2} (${left:.2} left), see {}",
                spent + left,
                self.ctx.workspace.cost_ledger().display()
            ),
            None => eprintln!("  Claude spend: ${spent:.2}, no budget set"),
        }
        eprintln!();
    }
}

fn stage_context(context_dir: &Path, audit_dir: &Path, fresh: bool) {
    for filename in [
        "AUDIT_CONTEXT.md",
        "PROPERTIES.md",
        "KNOWN_ISSUES.md",
        "ATTACK_PATTERNS.md",
    ] {
        let dest = audit_dir.join(filename);
        let src = context_dir.join(filename);
        if src.exists() {
            if let Err(e) = std::fs::copy(&src, &dest) {
                warn!("could not stage {filename}: {e}");
            }
        } else if fresh && dest.exists() {
            let _ = std::fs::remove_file(&dest);
            debug!("removed stale {filename}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_context_is_removed_only_on_a_fresh_run() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, audit) = (tmp.path().join("context"), tmp.path().join("audit"));
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::create_dir_all(&audit).unwrap();
        std::fs::write(ctx.join("AUDIT_CONTEXT.md"), "target").unwrap();
        std::fs::write(audit.join("PROPERTIES.md"), "derived").unwrap();

        stage_context(&ctx, &audit, false);
        assert!(audit.join("PROPERTIES.md").exists());

        stage_context(&ctx, &audit, true);
        assert!(!audit.join("PROPERTIES.md").exists());
        assert_eq!(
            std::fs::read_to_string(audit.join("AUDIT_CONTEXT.md")).unwrap(),
            "target"
        );
    }
}
