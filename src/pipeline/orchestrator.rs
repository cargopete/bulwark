use crate::config::Config;
use crate::error::{DoyranError, Result};
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
        doyran_root: PathBuf,
        start: u8,
        end: u8,
        agent_filter: Option<String>,
    ) -> Result<Self> {
        let workspace = Workspace::new(&audit_dir, &config.workspace.path);

        Ok(Self {
            ctx: PipelineContext {
                config,
                workspace,
                audit_dir,
                doyran_root,
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
        Ok(results)
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

    fn preflight(&self) -> Result<()> {
        if !self.ctx.audit_dir.exists() {
            return Err(DoyranError::PrerequisiteNotMet {
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
    ╔═══════════════════════════════════════════════════════════╗
    ║                                                           ║
    ║   ██████╗  ██████╗ ██╗   ██╗██████╗  █████╗ ███╗   ██╗   ║
    ║   ██╔══██╗██╔═══██╗╚██╗ ██╔╝██╔══██╗██╔══██╗████╗  ██║   ║
    ║   ██║  ██║██║   ██║ ╚████╔╝ ██████╔╝███████║██╔██╗ ██║   ║
    ║   ██║  ██║██║   ██║  ╚██╔╝  ██╔══██╗██╔══██║██║╚██╗██║   ║
    ║   ██████╔╝╚██████╔╝   ██║   ██║  ██║██║  ██║██║ ╚████║   ║
    ║   ╚═════╝  ╚═════╝    ╚═╝   ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝   ║
    ║                                                           ║
    ║   v2 — Multi-Pass Smart Contract Audit Pipeline           ║
    ║   The Graph Protocol                                      ║
    ║                                                           ║
    ╚═══════════════════════════════════════════════════════════╝
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
