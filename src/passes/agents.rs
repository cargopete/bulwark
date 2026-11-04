use crate::coverage::{self, Evidence, Shallow, Thresholds};
use crate::dashboard::status::parse_events;
use crate::error::{BulwarkError, Result};
use crate::findings::Finding;
use crate::findings::merge;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::{self, ClaudeSession};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Pass 2: Multi-Agent Adversarial Analysis.
///
/// Three independent Claude Code sessions run in parallel (RED/BLUE/GOLD).
/// Agents cannot see each other's output. After all complete,
/// findings are merged and deduplicated.
pub async fn run(ctx: &PipelineContext, agent_filter: Option<&str>) -> Result<String> {
    // Preflight: verify Pass 1 outputs exist
    if !ctx.workspace.recon_summary().exists() {
        return Err(BulwarkError::PrerequisiteNotMet {
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
            return Err(BulwarkError::PrerequisiteNotMet {
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

    let unmatchable = coverage::Scope::from_config(&ctx.config).drop_unmatchable(&ctx.audit_dir);
    if !unmatchable.is_empty() {
        eprintln!(
            "  {} Not holding agents to asset names no source file matches: {}",
            style("→").cyan(),
            unmatchable.join(", ")
        );
    }

    let max_turns = ctx.config.passes.agents.max_turns;
    let timeout = Duration::from_secs(ctx.config.passes.agents.timeout_minutes * 60);

    // Verify prompts exist (using project-first resolution)
    for agent in &agents {
        let prompt_file = ctx.resolve_prompt_path(&format!("{agent}-agent.md"));
        if !prompt_file.exists() {
            return Err(BulwarkError::PrerequisiteNotMet {
                pass: "agents".into(),
                reason: format!("prompt not found: {}", prompt_file.display()),
            });
        }
    }

    let mut jobs: Vec<Job> = Vec::new();
    for agent in &agents {
        jobs.push(Job {
            name: agent.clone(),
            prompt: std::fs::read_to_string(ctx.resolve_prompt_path(&format!("{agent}-agent.md")))?
                + &ctx.persona_section(agent),
            model: ctx.config.model_for(agent),
        });
    }
    if agent_filter.is_none() && !ctx.config.passes.agents.components.is_empty() {
        let hunter = std::fs::read_to_string(ctx.resolve_prompt_path("hunter-agent.md"))?;
        for c in &ctx.config.passes.agents.components {
            let name = format!("hunt-{}", c.name);
            jobs.push(Job {
                prompt: format!(
                    "{hunter}{}{}",
                    ctx.persona_section("hunter"),
                    component_section(ctx, &name, &c.files)
                ),
                name,
                model: ctx.config.model_for("hunter"),
            });
        }
    }
    let names: Vec<String> = jobs.iter().map(|j| j.name.clone()).collect();
    let max_parallel = ctx.config.passes.agents.max_parallel.max(1);

    eprintln!("  Max turns per agent: {max_turns}");
    eprintln!("  Timeout: {} minutes", timeout.as_secs() / 60);
    eprintln!(
        "  Launching {} agents, {max_parallel} at a time: {}\n",
        jobs.len(),
        names.join(", ")
    );

    // Set up progress bars
    let multi = MultiProgress::new();
    let spinner_style =
        ProgressStyle::with_template("  {spinner:.cyan} {prefix}: {msg} ({elapsed})")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");

    // Launch agents in parallel
    let mut set = JoinSet::new();
    let agent_count = jobs.len();
    let slots = Arc::new(Semaphore::new(max_parallel));
    // Variant analysis draws on the same allowance after the agents, so it takes one share.
    let allowance = ctx.allowance("agents");
    let variant_share = usize::from(ctx.config.passes.agents.variant_analysis);
    let agent_cap = allowance.split(agent_count + variant_share);

    for job in jobs {
        let ctx = ctx.clone();
        let name = job.name.clone();
        let claude_bin = claude_bin.clone();
        let slots = slots.clone();
        let pb = multi.add(ProgressBar::new_spinner());
        pb.set_style(spinner_style.clone());
        pb.set_prefix(name.to_uppercase());
        pb.set_message("queued");
        pb.enable_steady_tick(Duration::from_millis(120));

        set.spawn(async move {
            let _slot = slots
                .acquire_owned()
                .await
                .expect("semaphore is never closed");
            pb.set_message("running...");
            let result = run_single_agent(&ctx, &job, &claude_bin, max_turns, agent_cap).await;
            match &result {
                Ok(AgentOutcome {
                    findings,
                    shallow: Some(_),
                }) => pb.finish_with_message(format!("{findings} findings, shallow")),
                Ok(o) => pb.finish_with_message(format!("{} findings", o.findings)),
                Err(e) => pb.finish_with_message(format!("failed: {e}")),
            }
            (name, result)
        });
    }

    // Wait for all agents with timeout
    let mut shallow = Vec::new();
    let agent_results = tokio::time::timeout(timeout, async {
        let mut results = Vec::new();
        while let Some(res) = set.join_next().await {
            match res {
                Ok((name, Ok(outcome))) => {
                    if let Some(missing) = outcome.shallow {
                        shallow.push(Shallow {
                            agent: name.clone(),
                            missing,
                        });
                    }
                    results.push((name, outcome.findings));
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
    .map_err(|_| BulwarkError::PassTimeout {
        pass: "agents".into(),
        timeout: timeout.as_secs(),
    })?;

    eprintln!();

    if !shallow.is_empty() {
        let who: Vec<String> = shallow.iter().map(|s| s.agent.to_uppercase()).collect();
        eprintln!(
            "  {} Shallow sessions, still short of evidence after a resume: {}",
            style("⚠").yellow(),
            who.join(", ")
        );
    }
    write_shallow(ctx, &names, shallow)?;

    // Check if all agents failed
    let total_findings: usize = agent_results.iter().map(|(_, c)| *c).sum();
    if total_findings == 0 && agent_results.iter().all(|(_, c)| *c == 0) {
        eprintln!(
            "  {} All agents produced 0 findings. Check logs in {}",
            style("⚠").yellow(),
            ctx.workspace.findings_logs_dir().display()
        );
    }

    // Agents' experiments would break every later build if one fails to compile; keep them for triage.
    let scratch = ctx.build_dir().join("test/scratch");
    if scratch.exists() {
        let kept = ctx.workspace.findings_dir().join("agent-tests");
        let _ = std::fs::remove_dir_all(&kept);
        std::fs::rename(&scratch, &kept)?;
        eprintln!("  Agent experiments moved to {}", kept.display());
    }

    // Merge and deduplicate
    eprintln!("  Merging and deduplicating findings...");
    let merge_result = merge_agent_outputs(ctx, &names)?;

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
    let total_from_agents: usize = agent_results.iter().map(|(_, c)| *c).sum();
    let displayed_deduped = total_from_agents.saturating_sub(merge_result.findings.len());
    eprintln!(
        "  {} After merge: {} unique findings ({} duplicates removed)",
        style("✓").green(),
        merge_result.findings.len(),
        displayed_deduped
    );

    if merge_result.severity_disagreements > 0 {
        eprintln!(
            "  {} Severity disagreements: {} findings flagged for review",
            style("⚠").yellow(),
            merge_result.severity_disagreements
        );
    }

    // ── Variant analysis post-processing ─────────────────────────
    if ctx.config.passes.agents.variant_analysis {
        eprintln!("\n  Running variant analysis on high/critical findings...");
        match run_variant_analysis(ctx, &merge_result.findings, &allowance).await {
            Ok(count) if count > 0 => {
                eprintln!(
                    "  {} variant-analysis found {count} additional instances",
                    style("✓").green()
                );
            }
            Ok(_) => {
                eprintln!("  {} No additional variants found", style("~").yellow());
            }
            Err(e) => {
                eprintln!("  {} variant-analysis skipped: {e}", style("⚠").yellow());
            }
        }
    }

    Ok(format!(
        "{} unique findings from {} agents",
        merge_result.findings.len(),
        agent_count
    ))
}

struct Job {
    name: String,
    prompt: String,
    model: String,
}

/// Runtime instructions for a component hunter: its files and where its output goes.
fn component_section(ctx: &PipelineContext, name: &str, files: &[String]) -> String {
    let list: String = files.iter().map(|f| format!("- `{f}`\n")).collect();
    let rel = |p: std::path::PathBuf| {
        p.strip_prefix(&ctx.audit_dir)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| p.display().to_string())
    };
    format!(
        "\n\n## Runtime instructions\n\nYour component is `{name}`. Its files:\n\n{list}\n\
         Write findings to `{}` and coverage to `{}`.\n",
        rel(ctx.workspace.agent_raw_output(name)),
        rel(ctx.workspace.agent_coverage(name)),
    )
}

async fn run_single_agent(
    ctx: &PipelineContext,
    job: &Job,
    claude_bin: &Path,
    max_turns: u32,
    budget_usd: Option<f64>,
) -> Result<AgentOutcome> {
    let agent_name = job.name.as_str();
    let mut prompt = job.prompt.clone();
    if let Some(program) = &ctx.config.program {
        prompt.push_str(&program.prompt_section());
    }
    let output_file = ctx.workspace.agent_raw_output(agent_name);
    let log_file = ctx.workspace.agent_log(agent_name);

    let mut session = ClaudeSession {
        claude_bin: claude_bin.to_path_buf(),
        prompt,
        max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: log_file.clone(),
        model: Some(job.model.clone()),
        allowed_tools: vec![],
        timeout_minutes: None, // Pass 2 uses its own tokio::time::timeout wrapper
        label: format!("agent:{agent_name}"),
        budget_usd,
        ledger: Some(ctx.ledger.clone()),
        resume: None,
    };

    let mut scope = coverage::Scope::from_config(&ctx.config);
    scope.drop_unmatchable(&ctx.audit_dir);
    let thresholds = Thresholds::from_config(&ctx.config.passes.agents);
    let mut shallow = None;
    for attempt in 0..2 {
        let result = session.run().await?;
        if result.exit_code != 0 {
            eprintln!(
                "  {} {} agent exited with code {}",
                style("⚠").yellow(),
                agent_name.to_uppercase(),
                result.exit_code
            );
        }
        let gaps = coverage::missing(&scope, agent_name, &evidence(ctx, agent_name), &thresholds);
        if gaps.is_empty() {
            break;
        }
        if attempt == 1 {
            eprintln!(
                "  {} {} agent is shallow: {}",
                style("⚠").yellow(),
                agent_name.to_uppercase(),
                gaps.join("; ")
            );
            shallow = Some(gaps);
            break;
        }
        eprintln!(
            "  {} {} agent is short of evidence, resuming: {}",
            style("⚠").yellow(),
            agent_name.to_uppercase(),
            gaps.join("; ")
        );
        // A session that never started has nothing to resume, so it reruns from the top.
        if let Some(id) = result.session_id {
            session.prompt = coverage::resume_prompt(&gaps);
            session.resume = Some(id);
        }
    }

    // Check if agent wrote the expected output file
    let findings_count = if output_file.exists() {
        match std::fs::read_to_string(&output_file) {
            Ok(content) => match serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                // Count only items that look like findings (have a severity field),
                // not property verification status objects
                Ok(arr) => arr.iter().filter(|v| v.get("severity").is_some()).count(),
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

    Ok(AgentOutcome {
        findings: findings_count,
        shallow,
    })
}

struct AgentOutcome {
    findings: usize,
    /// What the session still owed after its resume.
    shallow: Option<Vec<String>>,
}

fn evidence(ctx: &PipelineContext, agent: &str) -> Evidence {
    let live = claude::live_log(&ctx.workspace.agent_log(agent));
    let forge_test_runs = std::fs::read_to_string(live)
        .map(|t| parse_events(&t).forge_test_runs)
        .unwrap_or(0);
    Evidence::read(&ctx.workspace, agent, forge_test_runs)
}

/// Records from earlier runs survive for agents this run did not touch, as with `--agent`.
fn write_shallow(ctx: &PipelineContext, ran: &[String], shallow: Vec<Shallow>) -> Result<()> {
    let path = ctx.workspace.shallow_sessions();
    let mut all: Vec<Shallow> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    all.retain(|s| !ran.contains(&s.agent));
    all.extend(shallow);
    std::fs::write(&path, serde_json::to_string_pretty(&all)?)?;
    Ok(())
}

/// Fallback: extract findings JSON from a Claude log file.
fn extract_and_save(log_file: &Path, output_file: &Path) -> Result<usize> {
    let findings = claude::extract_findings_from_log(log_file)?;
    let count = findings.len();
    let content = serde_json::to_string_pretty(&findings)?;
    std::fs::write(output_file, content)?;
    Ok(count)
}

/// Run variant-analysis on high/critical findings to find pattern matches elsewhere.
///
/// Gracefully degrades: skips if skill not installed or Claude not available.
async fn run_variant_analysis(
    ctx: &PipelineContext,
    findings: &[Finding],
    allowance: &crate::budget::Allowance,
) -> crate::error::Result<usize> {
    use crate::error::BulwarkError;
    use crate::findings::Severity;

    if !claude::is_skill_available("tob-variant-analysis") {
        return Err(BulwarkError::PrerequisiteNotMet {
            pass: "agents".into(),
            reason: "tob-variant-analysis skill not installed".into(),
        });
    }

    let claude_bin = ctx.config.resolve_tool("claude")?;
    let logs_dir = ctx.workspace.findings_logs_dir();

    // Only run on High/Critical findings to save tokens
    let high_findings: Vec<&Finding> = findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::High | Severity::Critical))
        .collect();

    if high_findings.is_empty() {
        return Ok(0);
    }

    eprintln!(
        "    Checking {} high/critical findings for variants...",
        high_findings.len()
    );

    let mut all_variants: Vec<serde_json::Value> = Vec::new();
    let scope_dirs = ctx
        .config
        .target
        .scope
        .iter()
        .map(|s| format!("{s}/contracts/"))
        .collect::<Vec<_>>()
        .join(" and ");

    for (idx, finding) in high_findings.iter().enumerate() {
        let finding_json = serde_json::to_string_pretty(finding).unwrap_or_default();
        let log_file = logs_dir.join(format!("{}-variant.log", finding.id));
        let variant_file = ctx
            .workspace
            .findings_dir()
            .join(format!("variants-{}.json", finding.id));

        let prompt = format!(
            "Run /tob-variant-analysis to search for other instances of this vulnerability \
             pattern in the codebase.\n\n\
             Finding:\n```json\n{finding_json}\n```\n\n\
             Search all Solidity files in {scope_dirs} for the same pattern.\n\n\
             If you find variants, output them as a JSON array to: {}",
            variant_file.display()
        );

        let session = ClaudeSession {
            claude_bin: claude_bin.clone(),
            prompt: format!("{prompt}{}", ctx.persona_section("variant_analysis")),
            max_turns: ctx.config.passes.agents.variant_max_turns,
            working_dir: ctx.audit_dir.clone(),
            log_file,
            model: Some(ctx.config.model_for("variant_analysis")),
            allowed_tools: vec![],
            timeout_minutes: Some(20),
            label: format!("variant:{}", finding.id),
            budget_usd: allowance.split(high_findings.len() - idx),
            ledger: Some(ctx.ledger.clone()),
            resume: None,
        };

        let _ = session
            .run_with_spinner(&format!("Checking {}...", finding.id))
            .await;

        // Check for variant output
        if variant_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&variant_file) {
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                    all_variants.extend(arr);
                }
            }
        }
    }

    // Write consolidated variant findings
    if !all_variants.is_empty() {
        let path = ctx.workspace.findings_dir().join("variant-analysis.json");
        let content = serde_json::to_string_pretty(&all_variants)?;
        std::fs::write(&path, content)?;
    }

    Ok(all_variants.len())
}

fn merge_agent_outputs(ctx: &PipelineContext, agents: &[String]) -> Result<merge::MergeResult> {
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
                let arr: Vec<serde_json::Value> =
                    serde_json::from_str(&content).unwrap_or_default();
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
