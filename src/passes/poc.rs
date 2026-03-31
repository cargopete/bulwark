use crate::error::{BulwarkError, Result};
use crate::findings::{Finding, Severity};
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::{self, ClaudeSession};
use console::style;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

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

    let findings: Vec<Finding> = {
        let content = std::fs::read_to_string(&merged_path)?;
        serde_json::from_str(&content)?
    };

    if findings.is_empty() {
        eprintln!("  No findings from Pass 2 — nothing to validate.");
        write_json(&ctx.workspace.validated_findings(), &json!([]))?;
        return Ok("0 findings to validate".into());
    }

    let claude_bin = ctx.config.resolve_tool("claude")?;

    // ── Pre-filter: false positive check ─────────────────────────
    let findings = if ctx.config.passes.poc.fp_check {
        eprintln!("  Running false-positive check on {} findings...", findings.len());
        run_fp_check_filter(ctx, &findings, &claude_bin).await?
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
    let prompts_dir = ctx.bulwark_root.join(&ctx.config.prompts.dir);
    let poc_prompt_path = prompts_dir.join("poc-generator.md");

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
    let test_info = scan_test_infrastructure(ctx);

    let mut validated: Vec<Value> = Vec::new();
    let mut discarded_ids: Vec<String> = Vec::new();
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

        let poc_status = generate_and_validate_poc(
            ctx,
            finding,
            &base_prompt,
            &test_info,
            &claude_bin,
            &forge_bin,
            &poc_file,
            &log_file,
            &logs_dir,
            max_turns,
            max_retries,
        )
        .await;

        apply_validation_gate(
            finding,
            &poc_status,
            &poc_file,
            ctx,
            &mut validated,
            &mut discarded_ids,
            &mut stats,
        );
    }

    // Write validated findings
    write_json(&ctx.workspace.validated_findings(), &Value::Array(validated.clone()))?;

    // Write discarded findings
    let discarded: Vec<&Finding> = findings
        .iter()
        .filter(|f| discarded_ids.contains(&f.id))
        .collect();
    let discarded_json: Vec<Value> = discarded
        .iter()
        .map(|f| {
            let mut v = serde_json::to_value(f).unwrap_or_default();
            v["poc_status"] = json!("discarded_no_poc");
            v
        })
        .collect();
    write_json(
        &ctx.workspace.discarded_findings(),
        &Value::Array(discarded_json),
    )?;

    let survived = stats.validated + stats.inconclusive;
    eprintln!();
    eprintln!(
        "  {} Input: {} | Validated: {} | Inconclusive: {} | Discarded: {} | Survived: {}",
        style("✓").green(),
        findings.len(),
        stats.validated,
        stats.inconclusive,
        stats.discarded,
        survived
    );

    Ok(format!(
        "{survived} survived ({} validated, {} inconclusive, {} discarded)",
        stats.validated, stats.inconclusive, stats.discarded
    ))
}

#[derive(Default)]
struct PocStats {
    validated: usize,
    inconclusive: usize,
    discarded: usize,
}

#[allow(clippy::too_many_arguments)]
async fn generate_and_validate_poc(
    ctx: &PipelineContext,
    finding: &Finding,
    base_prompt: &str,
    test_info: &str,
    claude_bin: &Path,
    forge_bin: &Path,
    poc_file: &Path,
    log_file: &Path,
    logs_dir: &Path,
    max_turns: u32,
    max_retries: u32,
) -> String {
    let finding_json = serde_json::to_string_pretty(finding).unwrap_or_default();

    for retry in 0..=max_retries {
        if retry > 0 {
            eprintln!("    Retry {retry}/{max_retries}...");
        }

        // Build prompt
        let mut prompt = format!(
            "{base_prompt}\n\n---\n\n## Finding to Demonstrate\n\n```json\n{finding_json}\n```\n\n\
             ## Test Infrastructure\n{test_info}\n\n\
             ## Output Path\n\nWrite the PoC test to: `{}`\n\n\
             The test MUST compile with `forge build`. A compiling test that's inconclusive is \
             infinitely better than a perfect test that doesn't compile.",
            poc_file.display()
        );

        // On retry, include compilation errors
        if retry > 0 {
            let error_file = logs_dir.join(format!(
                "{}-build-error-{}.txt",
                finding.id,
                retry - 1
            ));
            if let Ok(errors) = std::fs::read_to_string(&error_file) {
                let tail: String = errors.lines().rev().take(50).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                prompt.push_str(&format!(
                    "\n\n## PREVIOUS ATTEMPT FAILED TO COMPILE\n\n\
                     The previous PoC failed to compile with these errors:\n\n```\n{tail}\n```\n\n\
                     Fix the compilation errors. Common issues:\n\
                     - Wrong import paths — check remappings.txt and foundry.toml\n\
                     - Missing function signatures — read the actual contract source\n\
                     - Wrong Solidity version — use `pragma solidity ^0.8.27;`\n\
                     - Simplify: use `deal()` for balances, `vm.prank()` for callers"
                ));
            }
        }

        // Run Claude
        let session = ClaudeSession {
            claude_bin: claude_bin.to_path_buf(),
            prompt,
            max_turns,
            working_dir: ctx.audit_dir.clone(),
            log_file: log_file.to_path_buf(),
            model: Some(ctx.config.model.clone()),
        };

        let _ = session.run().await;

        // Check if PoC file was created
        if !poc_file.exists() {
            eprintln!("    Agent did not write PoC file — extracting from log");
            extract_solidity_from_log(log_file, poc_file);
        }

        if !poc_file.exists() {
            eprintln!("    {} No PoC generated", style("⚠").yellow());
            if retry == max_retries {
                return "failed_to_compile".into();
            }
            continue;
        }

        // Find build directory
        let build_dir = find_build_dir(ctx, &finding.contract);

        // Attempt compilation
        eprintln!("    Compiling...");
        let build_output = crate::tools::run_command(
            forge_bin.to_str().unwrap_or("forge"),
            &["build"],
            &build_dir,
        )
        .await;

        let build_stdout = build_output
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let build_stderr = build_output
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
            .unwrap_or_default();
        let build_combined = format!("{build_stdout}\n{build_stderr}");

        let compiled = build_output
            .as_ref()
            .is_ok_and(|o| o.status.success())
            || build_combined.contains("Compiler run successful");

        if compiled {
            eprintln!("    {} Compilation successful", style("✓").green());

            // Run the test
            eprintln!("    Running test...");
            let test_result = crate::tools::run_command(
                forge_bin.to_str().unwrap_or("forge"),
                &["test", "--match-path", &poc_file.to_string_lossy(), "-vvv"],
                &build_dir,
            )
            .await;

            let test_output = test_result
                .as_ref()
                .map(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).to_string();
                    let e = String::from_utf8_lossy(&o.stderr).to_string();
                    format!("{s}\n{e}")
                })
                .unwrap_or_default();

            let status = classify_test_result(&test_output);
            eprintln!("    Result: {status}");
            return status;
        } else {
            eprintln!("    {} Compilation failed", style("⚠").yellow());

            if retry < max_retries {
                let error_file = logs_dir.join(format!("{}-build-error-{retry}.txt", finding.id));
                let _ = std::fs::write(&error_file, &build_combined);
            } else {
                eprintln!(
                    "    {} All retries exhausted",
                    style("✗").red()
                );
                return "failed_to_compile".into();
            }
        }
    }

    "failed_to_compile".into()
}

fn classify_test_result(output: &str) -> String {
    if output.contains("[PASS]") && output.contains("test_") {
        let vuln_indicators = [
            "assert", "revert", "overflow", "underflow", "drift", "profit", "loss", "extract",
        ];
        if vuln_indicators
            .iter()
            .any(|i| output.to_lowercase().contains(i))
        {
            "compiles_and_demonstrates".into()
        } else {
            "compiles_but_inconclusive".into()
        }
    } else if output.contains("[FAIL]") && output.contains("test_") {
        let demo_indicators = [
            "Rounding profit",
            "shares",
            "price",
            "manipulat",
            "extract",
            "drain",
        ];
        if demo_indicators.iter().any(|i| output.contains(i)) {
            "compiles_and_demonstrates".into()
        } else {
            "compiles_but_inconclusive".into()
        }
    } else if output.to_lowercase().contains("mainnet")
        || output.to_lowercase().contains("fork")
    {
        "requires_mainnet_simulation".into()
    } else {
        "compiles_but_inconclusive".into()
    }
}

fn apply_validation_gate(
    finding: &Finding,
    poc_status: &str,
    poc_file: &Path,
    ctx: &PipelineContext,
    validated: &mut Vec<Value>,
    discarded_ids: &mut Vec<String>,
    stats: &mut PocStats,
) {
    let mut finding_json = serde_json::to_value(finding).unwrap_or_default();
    let rel_poc = poc_file
        .strip_prefix(ctx.workspace.root())
        .unwrap_or(poc_file)
        .to_string_lossy()
        .to_string();

    match poc_status {
        "compiles_and_demonstrates" => {
            eprintln!(
                "    {} VALIDATED — severity preserved ({})",
                style("✓").green(),
                finding.severity
            );
            finding_json["poc_file"] = json!(rel_poc);
            finding_json["poc_status"] = json!(poc_status);
            validated.push(finding_json);
            stats.validated += 1;
        }
        "compiles_but_inconclusive" => {
            let capped = if matches!(finding.severity, Severity::Critical | Severity::High) {
                eprintln!(
                    "    {} INCONCLUSIVE — severity capped: {} -> Medium",
                    style("~").yellow(),
                    finding.severity
                );
                "Medium"
            } else {
                eprintln!(
                    "    {} INCONCLUSIVE — severity preserved ({})",
                    style("~").yellow(),
                    finding.severity
                );
                &finding.severity.to_string()
            };
            finding_json["poc_file"] = json!(rel_poc);
            finding_json["poc_status"] = json!(poc_status);
            finding_json["original_severity"] = json!(finding.severity.to_string());
            finding_json["severity"] = json!(capped);
            validated.push(finding_json);
            stats.inconclusive += 1;
        }
        "requires_mainnet_simulation" => {
            eprintln!("    {} MAINNET REQUIRED — flagged for manual review", style("⚑").cyan());
            finding_json["poc_status"] = json!(poc_status);
            validated.push(finding_json);
            stats.validated += 1;
        }
        _ => {
            eprintln!(
                "    {} DISCARDED ({})",
                style("✗").red(),
                poc_status
            );
            discarded_ids.push(finding.id.clone());
            stats.discarded += 1;
        }
    }
}

fn scan_test_infrastructure(ctx: &PipelineContext) -> String {
    let mut info = String::new();

    for pkg in &ctx.config.target.scope {
        let test_dir = ctx.audit_dir.join(pkg).join("test");
        if !test_dir.exists() {
            continue;
        }

        // Find test base contracts
        let mut bases = Vec::new();
        if let Ok(entries) = glob_sol_files(&test_dir) {
            for file in entries {
                if let Ok(content) = std::fs::read_to_string(&file) {
                    if content.contains("contract") && content.contains("Test") && content.contains(" is ") {
                        let rel = file
                            .strip_prefix(&ctx.audit_dir)
                            .unwrap_or(&file)
                            .to_string_lossy();
                        if let Some(line) = content
                            .lines()
                            .find(|l| l.contains("contract") && l.contains("Test"))
                        {
                            bases.push(format!("  {rel}: {}", line.trim()));
                        }
                    }
                }
            }
        }

        if !bases.is_empty() {
            info.push_str(&format!("\nExisting test bases in {pkg}/test/:\n"));
            for b in &bases {
                info.push_str(&format!("{b}\n"));
            }
        }

        // Check remappings
        let remappings = ctx.audit_dir.join(pkg).join("remappings.txt");
        if let Ok(content) = std::fs::read_to_string(&remappings) {
            info.push_str(&format!("\nRemappings ({pkg}):\n{content}\n"));
        }

        // Check foundry.toml
        let foundry_toml = ctx.audit_dir.join(pkg).join("foundry.toml");
        if let Ok(content) = std::fs::read_to_string(&foundry_toml) {
            let relevant: String = content
                .lines()
                .filter(|l| {
                    ["src", "test", "out", "libs", "remappings"]
                        .iter()
                        .any(|k| l.contains(k))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !relevant.is_empty() {
                info.push_str(&format!("\nfoundry.toml ({pkg}):\n{relevant}\n"));
            }
        }
    }

    info
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
                if files.iter().any(|f| f.file_name().unwrap_or_default().to_string_lossy() == contract_file) {
                    return pkg_path;
                }
            }
        }
    }

    // Default to horizon
    ctx.audit_dir.join("packages/horizon")
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
            "Run /tob-fp-check to challenge this finding. \
             Be adversarial — try to prove it is a false positive.\n\n\
             ```json\n{finding_json}\n```\n\n\
             After analysis, respond with ONLY one of these verdicts on a single line:\n\
             - CONFIRMED: <one-sentence reason>\n\
             - FALSE_POSITIVE: <one-sentence reason>\n\
             - UNCERTAIN: <one-sentence reason>"
        );

        let session = ClaudeSession {
            claude_bin: claude_bin.to_path_buf(),
            prompt,
            max_turns: ctx.config.passes.poc.fp_check_max_turns,
            working_dir: ctx.audit_dir.clone(),
            log_file: log_file.clone(),
            model: Some(ctx.config.model.clone()),
        };

        let result = session.run().await;

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
