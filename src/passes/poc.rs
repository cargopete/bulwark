use crate::budget;
use crate::error::{BulwarkError, Result};
use crate::findings::Finding;
use crate::passes::poc_evidence::{
    self, GAIN_MARKER, LOSS_MARKER, NOT_REPRODUCIBLE_MARKER, PocMode,
};
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::{self, ClaudeSession};
use console::style;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Part of the pass's money set aside for fp-check; the rest goes to PoCs.
const FP_CHECK_SHARE: f64 = 0.2;

/// Pass 3: PoC Generation & Validation — "No PoC, no finding" gate.
///
/// For each finding from Pass 2:
///   1. Generate a Foundry PoC via Claude
///   2. Attempt compilation with forge build
///   3. Run with forge test, classify result
///   4. Apply validation gate (discard if doesn't compile)
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let merged_path = ctx.workspace.merged_findings();
    if !merged_path.exists() {
        return Err(BulwarkError::PrerequisiteNotMet {
            pass: "poc".into(),
            reason: "Pass 2 findings not found. Run Pass 2 first.".into(),
        });
    }

    let mut findings: Vec<Finding> = {
        let content = std::fs::read_to_string(&merged_path)?;
        serde_json::from_str(&content)?
    };
    // Highest severity first, so a tight budget is spent where the payouts are.
    findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    let allowance = ctx.allowance("poc");

    if findings.is_empty() {
        eprintln!("  No findings from Pass 2 — nothing to validate.");
        write_json(&ctx.workspace.validated_findings(), &json!([]))?;
        return Ok("0 findings to validate".into());
    }

    let claude_bin = ctx.config.resolve_tool("claude")?;

    let findings = match &ctx.config.program {
        Some(program) => {
            super::scope::gate(ctx, program, findings, &allowance, &claude_bin).await?
        }
        None => findings,
    };
    if findings.is_empty() {
        eprintln!("  All findings rejected by the scope check — nothing to validate.");
        write_json(&ctx.workspace.validated_findings(), &json!([]))?;
        return Ok("0 findings in scope".into());
    }

    let findings = crate::known::suppress(ctx, findings, &allowance, &claude_bin).await?;
    if findings.is_empty() {
        eprintln!("  All findings match known issues — nothing to validate.");
        write_json(&ctx.workspace.validated_findings(), &json!([]))?;
        return Ok("0 findings not already known".into());
    }

    // ── Pre-filter: false positive check ─────────────────────────
    let findings = if ctx.config.passes.poc.fp_check {
        eprintln!(
            "  Running false-positive check on {} findings...",
            findings.len()
        );
        let fp_cap = allowance
            .left()
            .map(|l| l * FP_CHECK_SHARE / findings.len() as f64);
        run_fp_check_filter(ctx, &findings, &claude_bin, fp_cap).await?
    } else {
        findings
    };

    if findings.is_empty() {
        eprintln!("  All findings filtered by fp-check — nothing to validate.");
        write_json(&ctx.workspace.validated_findings(), &json!([]))?;
        return Ok("0 findings survived fp-check".into());
    }
    let forge_bin = ctx.config.resolve_tool("forge")?;
    let max_turns = ctx.config.passes.poc.max_turns;
    let max_retries = ctx.config.passes.poc.max_retries;
    let poc_prompt_path = ctx.resolve_prompt_path("poc-generator.md");

    if !poc_prompt_path.exists() {
        return Err(BulwarkError::PrerequisiteNotMet {
            pass: "poc".into(),
            reason: format!("prompt not found: {}", poc_prompt_path.display()),
        });
    }

    let base_prompt = std::fs::read_to_string(&poc_prompt_path)?;
    let pocs_dir = ctx.workspace.pocs_dir();
    let logs_dir = pocs_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    eprintln!("  {} findings to process", findings.len());
    eprintln!("  Max turns: {max_turns}, Max retries: {max_retries}");

    // Scan test infrastructure
    let test_info = super::scan_test_infrastructure(ctx);

    // Load failed packages from recon summary so we can identify unverifiable findings
    let failed_packages: Vec<String> = {
        let recon_summary = ctx.workspace.recon_summary();
        std::fs::read_to_string(&recon_summary)
            .ok()
            .and_then(|c| serde_json::from_str::<Value>(&c).ok())
            .and_then(|v| v.get("failed_packages").and_then(|p| p.as_array()).cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    if !failed_packages.is_empty() {
        eprintln!(
            "  {} Note: {} package(s) failed to build — PoC compile failures may be unverifiable, not false positives",
            style("⚠").yellow(),
            failed_packages.len()
        );
    }

    let (mode, rpc_url) = poc_mode(&ctx.config.passes.poc)?;
    match &mode {
        PocMode::Fork { rpc_env, block } => {
            eprintln!("  Fork mode: ${rpc_env} pinned at block {block}")
        }
        PocMode::Local => eprintln!(
            "  {} Local mode: no fork configured, so no PoC can count as demonstrated",
            style("⚠").yellow()
        ),
    }
    let poc_run = PocRun {
        claude_bin: &claude_bin,
        forge_bin: &forge_bin,
        base_prompt: &base_prompt,
        test_info: &test_info,
        logs_dir: &logs_dir,
        max_turns,
        max_retries,
        mode: &mode,
        rpc_url: rpc_url.as_deref(),
    };

    let mut validated: Vec<Value> = Vec::new();
    let mut unverifiable: Vec<Value> = Vec::new();
    let mut discarded: Vec<Value> = Vec::new();
    let mut stats = PocStats::default();

    for (idx, finding) in findings.iter().enumerate() {
        eprintln!(
            "\n  [{}/{}] {} — {} ({})",
            idx + 1,
            findings.len(),
            finding.id,
            finding.title,
            finding.severity
        );

        let poc_file = pocs_dir.join(format!("{}.t.sol", finding.id));
        let log_file = logs_dir.join(format!("{}.log", finding.id));

        let finding_cap = allowance.split(findings.len() - idx);
        let outcome =
            generate_and_validate_poc(ctx, &poc_run, finding, &poc_file, &log_file, finding_cap)
                .await;
        let record_file = pocs_dir.join(format!("{}.poc.json", finding.id));
        if !outcome.record.is_null() {
            write_json(&record_file, &outcome.record)?;
        }

        apply_validation_gate(
            finding,
            &outcome,
            &poc_file,
            ctx,
            &failed_packages,
            &mut validated,
            &mut unverifiable,
            &mut discarded,
            &mut stats,
        );
    }

    // Write validated findings
    write_json(
        &ctx.workspace.validated_findings(),
        &Value::Array(validated.clone()),
    )?;

    // Write unverifiable findings (PoC couldn't compile due to known build failures)
    write_json(
        &ctx.workspace.unverifiable_findings(),
        &Value::Array(unverifiable.clone()),
    )?;

    write_json(
        &ctx.workspace.discarded_findings(),
        &Value::Array(discarded),
    )?;

    let survived = stats.validated + stats.inconclusive + stats.unverifiable;
    eprintln!();
    eprintln!(
        "  {} Input: {} | Validated: {} | Inconclusive: {} | Unverifiable: {} | Discarded: {} | Survived: {}",
        style("✓").green(),
        findings.len(),
        stats.validated,
        stats.inconclusive,
        stats.unverifiable,
        stats.discarded,
        survived
    );

    Ok(format!(
        "{survived} survived ({} validated, {} inconclusive, {} unverifiable, {} discarded)",
        stats.validated, stats.inconclusive, stats.unverifiable, stats.discarded
    ))
}

#[derive(Default)]
struct PocStats {
    validated: usize,
    inconclusive: usize,
    unverifiable: usize,
    discarded: usize,
}

fn poc_mode(cfg: &crate::config::PocPassConfig) -> Result<(PocMode, Option<String>)> {
    let Some(env) = &cfg.fork_rpc_env else {
        return Ok((PocMode::Local, None));
    };
    let block = cfg
        .fork_block
        .ok_or_else(|| BulwarkError::PrerequisiteNotMet {
            pass: "poc".into(),
            reason: "fork_rpc_env is set but fork_block is not; pin the block".into(),
        })?;
    let url = std::env::var(env).map_err(|_| BulwarkError::PrerequisiteNotMet {
        pass: "poc".into(),
        reason: format!("fork_rpc_env names ${env}, which is not set"),
    })?;
    Ok((
        PocMode::Fork {
            rpc_env: env.clone(),
            block,
        },
        Some(url),
    ))
}

struct PocRun<'a> {
    claude_bin: &'a Path,
    forge_bin: &'a Path,
    base_prompt: &'a str,
    test_info: &'a str,
    logs_dir: &'a Path,
    max_turns: u32,
    max_retries: u32,
    mode: &'a PocMode,
    rpc_url: Option<&'a str>,
}

/// Outcome of one finding's PoC attempts; `record` is written as `<id>.poc.json`.
struct PocOutcome {
    status: String,
    reasons: Vec<String>,
    record: Value,
}

async fn generate_and_validate_poc(
    ctx: &PipelineContext,
    run: &PocRun<'_>,
    finding: &Finding,
    poc_file: &Path,
    log_file: &Path,
    finding_cap: Option<f64>,
) -> PocOutcome {
    let finding_json = serde_json::to_string_pretty(finding).unwrap_or_default();
    let rel_poc = format!("test/pocs/{}.t.sol", finding.id);
    let (fork_args, shown_fork_args) = run.mode.forge_args(run.rpc_url);
    let shown_command = std::iter::once(format!("forge test --match-path {rel_poc} --json -vv"))
        .chain(shown_fork_args)
        .collect::<Vec<_>>()
        .join(" ");
    let mode_text = match run.mode {
        PocMode::Fork { block, .. } => format!(
            "Tests run against a fork pinned at block {block}. Attack the DEPLOYED contracts at \
             their real addresses (see AUDIT_CONTEXT.md); do not deploy fresh copies."
        ),
        PocMode::Local => "No fork is configured: tests run on a local deployment.".to_string(),
    };

    let mut feedback: Option<String> = None;
    let mut last = PocOutcome {
        status: "failed_to_compile".into(),
        reasons: vec!["no PoC generated".into()],
        record: Value::Null,
    };

    // A leftover file from an earlier run would otherwise be graded as this run's work.
    let _ = std::fs::remove_file(poc_file);

    let spent_before = ctx.ledger.spent();
    for retry in 0..=run.max_retries {
        let attempt_cap = finding_cap.map(|c| c - (ctx.ledger.spent() - spent_before));
        if budget::check_cap(attempt_cap).is_err() {
            eprintln!("    {} Run budget exhausted", style("⚑").cyan());
            if retry == 0 {
                last.status = "not_attempted".into();
                last.reasons = vec!["run budget exhausted before a PoC was attempted".into()];
            }
            break;
        }
        if retry > 0 {
            eprintln!("    Retry {retry}/{}...", run.max_retries);
        }
        let before = std::fs::read_to_string(poc_file).ok();

        let mut prompt = format!(
            "{}\n\n---\n\n## Finding to Demonstrate\n\n```json\n{finding_json}\n```\n\n\
             ## Test Infrastructure\n{}\n\n## Execution\n\n{mode_text}\n\n\
             ## Output Path\n\nWrite the PoC test to: `{}`",
            run.base_prompt,
            run.test_info,
            poc_file.display()
        );
        if let Some(fb) = feedback.take() {
            prompt.push_str(&fb);
        }

        let session = ClaudeSession {
            claude_bin: run.claude_bin.to_path_buf(),
            prompt: format!("{prompt}{}", ctx.persona_section("poc")),
            max_turns: run.max_turns,
            working_dir: ctx.audit_dir.clone(),
            log_file: log_file.to_path_buf(),
            model: Some(ctx.config.model_for("poc")),
            allowed_tools: vec![],
            timeout_minutes: Some(45),
            label: format!("poc:{}", finding.id),
            budget_usd: attempt_cap,
            ledger: Some(ctx.ledger.clone()),
        };
        let _ = session.run_with_spinner("Generating PoC...").await;

        if !poc_file.exists() {
            eprintln!("    Agent did not write PoC file — extracting from log");
            extract_solidity_from_log(log_file, poc_file);
        }
        let source = match std::fs::read_to_string(poc_file) {
            Ok(s) if Some(&s) != before.as_ref() => s,
            _ => {
                eprintln!("    {} No new PoC written", style("⚠").yellow());
                continue;
            }
        };

        let build_dir = find_build_dir(ctx, &finding.contract);
        let forge_poc_dir = build_dir.join("test/pocs");
        let _ = std::fs::create_dir_all(&forge_poc_dir);
        let poc_in_project = forge_poc_dir.join(format!("{}.t.sol", finding.id));
        if let Err(e) = std::fs::copy(poc_file, &poc_in_project) {
            eprintln!(
                "    {} Failed to copy PoC into project: {e}",
                style("⚠").yellow()
            );
        }

        eprintln!("    Compiling...");
        let forge = run.forge_bin.to_str().unwrap_or("forge");
        let build_output = crate::tools::run_command(forge, &["build"], &build_dir).await;
        let build_combined = build_output
            .as_ref()
            .map(|o| {
                format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )
            })
            .unwrap_or_default();
        let compiled = build_output.as_ref().is_ok_and(|o| o.status.success())
            || build_combined.contains("Compiler run successful");

        if !compiled {
            eprintln!("    {} Compilation failed", style("⚠").yellow());
            let _ = std::fs::write(
                run.logs_dir
                    .join(format!("{}-build-error-{retry}.txt", finding.id)),
                &build_combined,
            );
            // A file that fails to compile breaks later passes' forge builds.
            let _ = std::fs::remove_file(&poc_in_project);
            feedback = Some(compile_feedback(&build_combined));
            last.reasons = vec!["did not compile".into()];
            continue;
        }
        eprintln!("    {} Compilation successful", style("✓").green());

        eprintln!("    Running test...");
        let mut args: Vec<&str> = vec!["test", "--match-path", &rel_poc, "--json", "-vv"];
        args.extend(fork_args.iter().map(String::as_str));
        let test_stdout = crate::tools::run_command(forge, &args, &build_dir)
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let _ = std::fs::write(
            run.logs_dir
                .join(format!("{}-test-{retry}.json", finding.id)),
            &test_stdout,
        );

        let tests = poc_evidence::parse_forge_json(&test_stdout);
        let lint = poc_evidence::lint(&source);
        let grade = poc_evidence::grade(&source, tests.as_deref(), &lint, run.mode);
        eprintln!("    Result: {}", grade.status);
        for r in &grade.reasons {
            eprintln!("      - {r}");
        }

        last = PocOutcome {
            status: grade.status.to_string(),
            reasons: grade.reasons.clone(),
            record: json!({
                "id": finding.id,
                "status": grade.status,
                "reasons": grade.reasons,
                "mode": run.mode,
                "command": shown_command,
                "build_dir": build_dir.strip_prefix(&ctx.audit_dir).unwrap_or(&build_dir),
                "tests": tests.as_deref().unwrap_or_default(),
                "capital": lint.capital,
                "lint_flags": lint.flags,
                "attempts": retry + 1,
            }),
        };

        if !grade.retryable {
            return last;
        }

        // Traces only reach the model, not the grade; -vvv text is what it can read.
        let mut trace_args: Vec<&str> = vec!["test", "--match-path", &rel_poc, "-vvv"];
        trace_args.extend(fork_args.iter().map(String::as_str));
        let traces = crate::tools::run_command(forge, &trace_args, &build_dir)
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        feedback = Some(grade_feedback(&grade.reasons, &traces));
    }

    last
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn compile_feedback(build_output: &str) -> String {
    format!(
        "\n\n## PREVIOUS ATTEMPT FAILED TO COMPILE\n\n```\n{}\n```\n\n\
         Fix the compilation errors. Check import paths against remappings.txt and \
         foundry.toml, and function signatures against the contract source.",
        tail(build_output, 50)
    )
}

/// Retry guidance after a test that compiled but did not prove impact. It must never
/// invite weakening or flipping the impact measurement until something passes.
fn grade_feedback(reasons: &[String], traces: &str) -> String {
    let reasons = reasons
        .iter()
        .map(|r| format!("- {r}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\n## PREVIOUS ATTEMPT DID NOT PROVE IMPACT\n\n{reasons}\n\n\
         Forge output:\n\n```\n{}\n```\n\n\
         You may fix setup mistakes: wrong preconditions, wrong signatures, missing approvals, \
         a flagged cheatcode replaced by a real protocol call. You must NOT change what is \
         measured: the impact is attacker balance gained or victim balance lost, computed as \
         after minus before and logged via `console.log(\"{GAIN_MARKER}\", x)` / \
         `console.log(\"{LOSS_MARKER}\", x)`. Do not flip or loosen assertions to make the test pass.\n\n\
         If the source shows the attack cannot work, put \
         `// {NOT_REPRODUCIBLE_MARKER}: <the line or check that blocks it>` at the top of the file \
         and stop. That is a correct answer, not a failure.",
        tail(traces, 80)
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_validation_gate(
    finding: &Finding,
    outcome: &PocOutcome,
    poc_file: &Path,
    ctx: &PipelineContext,
    failed_packages: &[String],
    validated: &mut Vec<Value>,
    unverifiable: &mut Vec<Value>,
    discarded: &mut Vec<Value>,
    stats: &mut PocStats,
) {
    let poc_status = outcome.status.as_str();
    let mut finding_json = serde_json::to_value(finding).unwrap_or_default();
    let rel = |p: &Path| {
        p.strip_prefix(ctx.workspace.root())
            .unwrap_or(p)
            .to_string_lossy()
            .to_string()
    };
    let rel_poc = rel(poc_file);
    finding_json["poc_reasons"] = json!(outcome.reasons);
    if !outcome.record.is_null() {
        finding_json["poc_record"] = json!(rel(
            &poc_file.with_file_name(format!("{}.poc.json", finding.id))
        ));
    }

    // Reclassify compile failures when we know a package didn't build.
    // A PoC that can't compile because SubgraphService is missing is NOT the same as
    // a PoC that can't compile because the exploit logic is wrong.
    let effective_status = if poc_status == "failed_to_compile" && !failed_packages.is_empty() {
        "unverifiable_build_failure"
    } else {
        poc_status
    };

    match effective_status {
        "compiles_and_demonstrates" => {
            eprintln!(
                "    {} VALIDATED — severity preserved ({})",
                style("✓").green(),
                finding.severity
            );
            finding_json["poc_file"] = json!(rel_poc);
            finding_json["poc_status"] = json!(effective_status);
            validated.push(finding_json);
            stats.validated += 1;
        }
        "compiles_but_inconclusive" => {
            // Preserve original severity — inconclusive means we couldn't prove it,
            // not that it's wrong. The adversarial reviewer (Pass 6) can downgrade
            // if it finds a concrete reason. Silently capping Critical→Medium here
            // caused genuine findings to be underreported.
            eprintln!(
                "    {} INCONCLUSIVE — severity preserved ({})",
                style("~").yellow(),
                finding.severity
            );
            finding_json["poc_file"] = json!(rel_poc);
            finding_json["poc_status"] = json!(effective_status);
            validated.push(finding_json);
            stats.inconclusive += 1;
        }
        "requires_mainnet_simulation" => {
            eprintln!(
                "    {} MAINNET REQUIRED — flagged for manual review",
                style("⚑").cyan()
            );
            finding_json["poc_status"] = json!(effective_status);
            validated.push(finding_json);
            stats.inconclusive += 1;
        }
        "not_attempted" => {
            eprintln!(
                "    {} NOT ATTEMPTED — run budget exhausted",
                style("⚑").cyan()
            );
            finding_json["poc_status"] = json!("not_attempted");
            finding_json["unverifiable_reason"] =
                json!("Run budget ran out before a PoC was attempted. Not disproven.");
            unverifiable.push(finding_json);
            stats.unverifiable += 1;
        }
        "unverifiable_build_failure" => {
            eprintln!(
                "    {} UNVERIFIABLE — PoC could not compile due to build failure in: {}",
                style("⚑").cyan(),
                failed_packages.join(", ")
            );
            eprintln!(
                "      Finding preserved for manual review — fix the build and re-run to get a verdict."
            );
            finding_json["poc_status"] = json!("unverifiable_build_failure");
            finding_json["unverifiable_reason"] = json!(format!(
                "PoC compilation failed — the following packages did not build and may be required: {}. \
                 This finding has NOT been disproven. Fix the build error and re-run Pass 3 for a definitive verdict.",
                failed_packages.join(", ")
            ));
            unverifiable.push(finding_json);
            stats.unverifiable += 1;
        }
        _ => {
            eprintln!("    {} DISCARDED ({})", style("✗").red(), effective_status);
            finding_json["poc_status"] = json!(effective_status);
            discarded.push(finding_json);
            stats.discarded += 1;
        }
    }
}

fn find_build_dir(ctx: &PipelineContext, contract: &str) -> PathBuf {
    let contract_file = if contract.ends_with(".sol") {
        contract.to_string()
    } else {
        format!("{contract}.sol")
    };

    for pkg in &ctx.config.target.scope {
        let pkg_path = ctx.audit_dir.join(pkg);
        let contracts_dir = pkg_path.join("contracts");
        if contracts_dir.exists() {
            if let Ok(files) = glob_sol_files(&contracts_dir) {
                if files
                    .iter()
                    .any(|f| f.file_name().unwrap_or_default().to_string_lossy() == contract_file)
                {
                    return pkg_path;
                }
            }
        }
    }

    // Fall back to primary build directory from config scope
    ctx.build_dir()
}

fn glob_sol_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir_for_sol(dir, &mut files);
    Ok(files)
}

fn walk_dir_for_sol(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir_for_sol(&path, out);
        } else if path.extension().is_some_and(|e| e == "sol") {
            out.push(path);
        }
    }
}

/// Extract the largest Solidity code block from a Claude log file.
fn extract_solidity_from_log(log_path: &Path, output_path: &Path) {
    let Ok(content) = std::fs::read_to_string(log_path) else {
        return;
    };

    // Find blocks starting with SPDX or pragma
    let mut best: Option<&str> = None;

    // Look for code between ``` markers
    let mut in_block = false;
    let mut block_start = 0;

    for (i, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("```") && !in_block {
            in_block = true;
            // Find the byte offset for the next line
            let offset: usize = content.lines().take(i + 1).map(|l| l.len() + 1).sum();
            block_start = offset;
        } else if line.trim_start().starts_with("```") && in_block {
            in_block = false;
            let offset: usize = content.lines().take(i).map(|l| l.len() + 1).sum();
            let block = &content[block_start..offset];
            if (block.contains("SPDX-License-Identifier") || block.contains("pragma solidity"))
                && block.len() > best.map_or(0, |b| b.len())
            {
                best = Some(block);
            }
        }
    }

    // Fallback: search for SPDX blocks without markdown fences
    if best.is_none() {
        if let Some(pos) = content.find("// SPDX-License-Identifier") {
            let block = &content[pos..];
            // Take until next ``` or end
            let end = block.find("```").unwrap_or(block.len());
            best = Some(&block[..end]);
        }
    }

    if let Some(sol_code) = best {
        let cleaned = sol_code
            .replace("```solidity", "")
            .replace("```", "")
            .trim()
            .to_string();
        let _ = std::fs::write(output_path, cleaned);
    }
}

/// Run fp-check on each finding to filter out false positives before PoC generation.
///
/// Design: fails open — if the skill is missing or a check errors, the finding passes through.
async fn run_fp_check_filter(
    ctx: &PipelineContext,
    findings: &[Finding],
    claude_bin: &Path,
    cap_each: Option<f64>,
) -> Result<Vec<Finding>> {
    if !claude::is_skill_available("tob-fp-check") {
        eprintln!(
            "    {} fp-check skill not installed, passing all findings through",
            style("⚠").yellow()
        );
        return Ok(findings.to_vec());
    }

    let logs_dir = ctx.workspace.pocs_dir().join("fp-check-logs");
    std::fs::create_dir_all(&logs_dir)?;

    let mut survivors = Vec::new();
    let mut filtered_count = 0;

    for (idx, finding) in findings.iter().enumerate() {
        eprintln!(
            "    [{}/{}] fp-check: {} — {}",
            idx + 1,
            findings.len(),
            finding.id,
            finding.title
        );

        let finding_json = serde_json::to_string_pretty(finding).unwrap_or_default();
        let log_file = logs_dir.join(format!("{}-fp-check.log", finding.id));

        let prompt = format!(
            "Run /tob-fp-check on this finding.\n\n\
             ```json\n{finding_json}\n```\n\n\
             Your job is to verify whether the vulnerable code path described could \
             plausibly exist and be reachable. Read the relevant source files before deciding.\n\n\
             Verdict rules — be conservative about calling FALSE_POSITIVE:\n\
             - CONFIRMED: the vulnerable code pattern exists in source and the attack path \
               is plausible given the access control.\n\
             - FALSE_POSITIVE: you have found CONCRETE code evidence that the attack \
               is impossible — e.g. the function does not exist, a require() definitively \
               blocks the path, or the math is provably safe by inspection. Do NOT use \
               this verdict merely because you cannot immediately construct a PoC — that \
               is what Pass 3 is for.\n\
             - UNCERTAIN: the code is complex, ambiguous, or you cannot determine \
               reachability within your turn budget. Default to this when in doubt.\n\n\
             Respond with ONLY one verdict on a single line:\n\
             CONFIRMED: <one-sentence reason>\n\
             FALSE_POSITIVE: <one-sentence reason>\n\
             UNCERTAIN: <one-sentence reason>"
        );

        let session = ClaudeSession {
            claude_bin: claude_bin.to_path_buf(),
            prompt,
            max_turns: ctx.config.passes.poc.fp_check_max_turns,
            working_dir: ctx.audit_dir.clone(),
            log_file: log_file.clone(),
            model: Some(ctx.config.model_for("fp_check")),
            allowed_tools: vec![],
            timeout_minutes: Some(15),
            label: format!("fp_check:{}", finding.id),
            budget_usd: cap_each,
            ledger: Some(ctx.ledger.clone()),
        };

        let result = session
            .run_with_spinner(&format!("Analysing {}...", finding.id))
            .await;

        // Parse result: check if output contains FALSE_POSITIVE
        // Fail open: on error, keep the finding
        let is_fp = result.is_ok()
            && std::fs::read_to_string(&log_file)
                .unwrap_or_default()
                .contains("FALSE_POSITIVE");

        if is_fp {
            eprintln!("      {} FALSE POSITIVE — filtered", style("✗").red());
            filtered_count += 1;
        } else {
            eprintln!("      {} survived", style("✓").green());
            survivors.push(finding.clone());
        }
    }

    eprintln!(
        "    fp-check: {filtered_count} filtered, {} survived",
        survivors.len()
    );
    Ok(survivors)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(path, content)?;
    Ok(())
}
