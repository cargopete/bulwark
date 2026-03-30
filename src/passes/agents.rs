use crate::error::{DoyranError, Result};
use crate::findings::merge;
use crate::findings::Finding;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::{self, ClaudeSession};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::path::Path;
use std::time::Duration;
use tokio::task::JoinSet;

/// Pass 2: Multi-Agent Adversarial Analysis.
///
/// Three independent Claude Code sessions run in parallel (RED/BLUE/GOLD).
/// Agents cannot see each other's output. After all complete,
/// findings are merged and deduplicated.
pub async fn run(ctx: &PipelineContext, agent_filter: Option<&str>) -> Result<String> {
    // Preflight: verify Pass 1 outputs exist
    if !ctx.workspace.recon_summary().exists() {
        return Err(DoyranError::PrerequisiteNotMet {
            pass: "agents".into(),
            reason: "Pass 1 recon outputs not found. Run Pass 1 first.".into(),
        });
    }

    let claude_bin = ctx.config.resolve_tool("claude")?;
    let all_agents = &ctx.config.passes.agents.agents;

    // Filter to a single agent if requested
    let agents: Vec<String> = if let Some(filter) = agent_filter {
        let filter_lower = filter.to_lowercase();
        if !all_agents.iter().any(|a| a.to_lowercase() == filter_lower) {
            return Err(DoyranError::PrerequisiteNotMet {
                pass: "agents".into(),
                reason: format!(
                    "unknown agent '{}' — available: {}",
                    filter,
                    all_agents.join(", ")
                ),
            });
        }
        vec![filter_lower]
    } else {
        all_agents.clone()
    };

    let max_turns = ctx.config.passes.agents.max_turns;
    let timeout = Duration::from_secs(ctx.config.passes.agents.timeout_minutes * 60);
    let prompts_dir = ctx.doyran_root.join(&ctx.config.prompts.dir);

    // Verify prompts exist
    for agent in &agents {
        let prompt_file = prompts_dir.join(format!("{agent}-agent.md"));
        if !prompt_file.exists() {
            return Err(DoyranError::PrerequisiteNotMet {
                pass: "agents".into(),
                reason: format!("prompt not found: {}", prompt_file.display()),
            });
        }
    }

    eprintln!("  Max turns per agent: {max_turns}");
    eprintln!("  Timeout: {} minutes", timeout.as_secs() / 60);
    eprintln!("  Launching {} agents in parallel...\n", agents.len());

    // Set up progress bars
    let multi = MultiProgress::new();
    let spinner_style = ProgressStyle::with_template("  {spinner:.cyan} {prefix}: {msg} ({elapsed})")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");

    // Launch agents in parallel
    let mut set = JoinSet::new();
    let agent_count = agents.len();

    for agent_name in &agents {
        let ctx = ctx.clone();
        let name = agent_name.clone();
        let claude_bin = claude_bin.clone();
        let prompts_dir = prompts_dir.clone();
        let pb = multi.add(ProgressBar::new_spinner());
        pb.set_style(spinner_style.clone());
        pb.set_prefix(name.to_uppercase());
        pb.set_message("running...");
        pb.enable_steady_tick(Duration::from_millis(120));

        set.spawn(async move {
            let result = run_single_agent(&ctx, &name, &claude_bin, &prompts_dir, max_turns).await;
            match &result {
                Ok(count) => pb.finish_with_message(format!("{count} findings")),
                Err(e) => pb.finish_with_message(format!("failed: {e}")),
            }
            (name, result)
        });
    }

    // Wait for all agents with timeout
    let agent_results = tokio::time::timeout(timeout, async {
        let mut results = Vec::new();
        while let Some(res) = set.join_next().await {
            match res {
                Ok((name, Ok(count))) => {
                    results.push((name, count));
                }
                Ok((name, Err(e))) => {
                    eprintln!(
                        "\n  {} {} agent failed: {}",
                        style("⚠").yellow(),
                        name.to_uppercase(),
                        e
                    );
                    results.push((name, 0));
                }
                Err(e) => {
                    eprintln!("\n  {} Agent task panicked: {e}", style("✗").red());
                }
            }
        }
        results
    })
    .await
    .map_err(|_| DoyranError::PassTimeout {
        pass: "agents".into(),
        timeout: timeout.as_secs(),
    })?;

    eprintln!();

    // Check if all agents failed
    let total_findings: usize = agent_results.iter().map(|(_, c)| *c).sum();
    if total_findings == 0 && agent_results.iter().all(|(_, c)| *c == 0) {
        eprintln!(
            "  {} All agents produced 0 findings. Check logs in {}",
            style("⚠").yellow(),
            ctx.workspace.findings_logs_dir().display()
        );
    }

    // Merge and deduplicate
    eprintln!("  Merging and deduplicating findings...");
    let merge_result = merge_agent_outputs(ctx, &agents)?;

    eprintln!();
    for (name, count) in &agent_results {
        eprintln!(
            "  {} {} agent: {} findings",
            style("✓").green(),
            name.to_uppercase(),
            count
        );
    }
    eprintln!();
    eprintln!(
        "  {} After merge: {} unique findings ({} duplicates removed)",
        style("✓").green(),
        merge_result.findings.len(),
        merge_result.duplicates_removed
    );

    if merge_result.severity_disagreements > 0 {
        eprintln!(
            "  {} Severity disagreements: {} findings flagged for review",
            style("⚠").yellow(),
            merge_result.severity_disagreements
        );
    }

    Ok(format!(
        "{} unique findings from {} agents",
        merge_result.findings.len(),
        agent_count
    ))
}

async fn run_single_agent(
    ctx: &PipelineContext,
    agent_name: &str,
    claude_bin: &Path,
    prompts_dir: &Path,
    max_turns: u32,
) -> Result<usize> {
    let prompt_file = prompts_dir.join(format!("{agent_name}-agent.md"));
    let prompt = std::fs::read_to_string(&prompt_file)?;
    let output_file = ctx.workspace.agent_raw_output(agent_name);
    let log_file = ctx.workspace.agent_log(agent_name);

    let session = ClaudeSession {
        claude_bin: claude_bin.to_path_buf(),
        prompt,
        max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: log_file.clone(),
        model: Some(ctx.config.model.clone()),
    };

    let result = session.run().await?;

    if result.exit_code != 0 {
        eprintln!(
            "  {} {} agent exited with code {}",
            style("⚠").yellow(),
            agent_name.to_uppercase(),
            result.exit_code
        );
    }

    // Check if agent wrote the expected output file
    let findings_count = if output_file.exists() {
        match std::fs::read_to_string(&output_file) {
            Ok(content) => match serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                Ok(arr) => arr.len(),
                Err(_) => {
                    // Invalid JSON — try extraction from log
                    extract_and_save(&log_file, &output_file)?
                }
            },
            Err(_) => 0,
        }
    } else {
        // Agent didn't write output — try extraction from log
        extract_and_save(&log_file, &output_file)?
    };

    Ok(findings_count)
}

/// Fallback: extract findings JSON from a Claude log file.
fn extract_and_save(log_file: &Path, output_file: &Path) -> Result<usize> {
    let findings = claude::extract_findings_from_log(log_file)?;
    let count = findings.len();
    let content = serde_json::to_string_pretty(&findings)?;
    std::fs::write(output_file, content)?;
    Ok(count)
}

fn merge_agent_outputs(
    ctx: &PipelineContext,
    agents: &[String],
) -> Result<merge::MergeResult> {
    let mut sources = Vec::new();

    for agent_name in agents {
        let output_file = ctx.workspace.agent_raw_output(agent_name);
        if !output_file.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&output_file)?;
        let findings: Vec<Finding> = match serde_json::from_str(&content) {
            Ok(f) => f,
            Err(_) => {
                // Try to parse as generic JSON array and filter to items with severity
                let arr: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap_or_default();
                arr.into_iter()
                    .filter(|v| v.get("severity").is_some())
                    .filter_map(|v| serde_json::from_value(v).ok())
                    .collect()
            }
        };
        sources.push((agent_name.as_str(), findings));
    }

    let result = merge::merge_and_dedup(sources);

    // Write merged output
    let merged_path = ctx.workspace.merged_findings();
    let content = serde_json::to_string_pretty(&result.findings)?;
    std::fs::write(&merged_path, content)?;

    Ok(result)
}
