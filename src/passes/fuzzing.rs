use crate::error::Result;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::ClaudeSession;
use console::style;
use serde_json::{json, Value};
use std::path::Path;

/// Pass 4: Fuzzing Campaign.
///
/// 1. Claude generates invariant tests from PROPERTIES.md
/// 2. Foundry runs invariant tests
/// 3. Medusa extended fuzzing (if available)
/// 4. Echidna extended fuzzing (if available)
/// 5. Broken invariants become findings
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let fuzz_dir = ctx.workspace.fuzzing_dir();
    let invariant_dir = fuzz_dir.join("invariant-tests");
    let results_dir = fuzz_dir.join("fuzzing-campaign-results");
    let logs_dir = fuzz_dir.join("logs");
    std::fs::create_dir_all(&invariant_dir)?;
    std::fs::create_dir_all(&results_dir)?;
    std::fs::create_dir_all(&logs_dir)?;

    let forge_bin = ctx.config.resolve_tool("forge")?;
    let fuzz_runs = ctx.config.passes.fuzzing.fuzz_runs;
    let invariant_depth = ctx.config.passes.fuzzing.invariant_depth;

    // ── Step 1: Generate invariant tests ─────────────────────────────
    let tests_generated = generate_invariant_tests(ctx, &invariant_dir, &logs_dir).await;
    if tests_generated == 0 {
        eprintln!(
            "  {} No invariant tests generated — check logs",
            style("⚠").yellow()
        );
    } else {
        eprintln!(
            "  {} Generated {tests_generated} invariant test files",
            style("✓").green()
        );
    }

    // ── Step 2: Compile invariant tests ─────────────────────────────
    let build_dir = ctx.build_dir();

    let tests_exist = count_sol_files(&invariant_dir) > 0;

    // Copy generated tests into the forge project so forge can find them
    let forge_test_dir = build_dir.join("test/invariant");
    if tests_exist {
        std::fs::create_dir_all(&forge_test_dir)?;
        copy_sol_files(&invariant_dir, &forge_test_dir)?;

        let invariant_count = count_invariant_functions(&forge_test_dir);
        eprintln!(
            "  Found {invariant_count} invariant_ function(s) in test/invariant/"
        );

        eprintln!("  Compiling invariant tests...");
        let build_result = crate::tools::forge::build(&forge_bin, &build_dir).await?;
        if build_result.success {
            eprintln!("  {} Invariant tests compile", style("✓").green());
        } else {
            eprintln!("  {} Some tests failed to compile", style("⚠").yellow());
            let _ = std::fs::write(logs_dir.join("invariant-build.log"), &build_result.stderr);
        }
    }

    // ── Step 3: Foundry invariant tests ─────────────────────────────
    let mut all_findings: Vec<Value> = Vec::new();
    let mut foundry_passed = 0u32;
    let mut foundry_failed = 0u32;

    if tests_exist {
        eprintln!(
            "  Running Foundry invariant tests (fuzz-runs={fuzz_runs}, depth={invariant_depth})..."
        );

        let fuzz_runs_str = fuzz_runs.to_string();
        let depth_str = invariant_depth.to_string();
        let output = crate::tools::run_command(
            forge_bin.to_str().unwrap_or("forge"),
            &[
                "test",
                "--match-path",
                "test/invariant",
                "--fuzz-runs",
                &fuzz_runs_str,
                "--invariant-depth",
                &depth_str,
                "-vvv",
            ],
            &build_dir,
        )
        .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        let _ = std::fs::write(results_dir.join("foundry-invariant.log"), &combined);

        foundry_passed = combined.matches("[PASS]").count() as u32;
        foundry_failed = combined.matches("[FAIL]").count() as u32;

        eprintln!(
            "  Foundry: {} passed, {} failed",
            foundry_passed, foundry_failed
        );

        // Extract broken invariants as findings
        if foundry_failed > 0 {
            eprintln!(
                "  {} Broken invariants detected!",
                style("⚠").yellow()
            );
            let mut idx = 1;
            for line in combined.lines() {
                if line.contains("[FAIL]") {
                    if let Some(finding) = parse_foundry_failure(line, idx) {
                        all_findings.push(finding);
                        idx += 1;
                    }
                }
            }
        }
    } else {
        eprintln!("  {} No invariant tests to run", style("⚠").yellow());
    }

    // ── Step 4: Medusa (optional) ───────────────────────────────────
    if tests_exist && ctx.config.has_tool("medusa") {
        let medusa_bin = ctx.config.resolve_tool("medusa")?;
        let timeout_str = ctx.config.passes.fuzzing.medusa_timeout.to_string();
        eprintln!(
            "  Running Medusa extended fuzzing (timeout={timeout_str}s)..."
        );

        let output = crate::tools::run_command(
            medusa_bin.to_str().unwrap_or("medusa"),
            &["fuzz", "--target-contracts", "Invariant", "--timeout", &timeout_str],
            &build_dir,
        )
        .await?;

        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let _ = std::fs::write(results_dir.join("medusa.log"), &combined);

        let has_failures = combined.to_lowercase().contains("failed")
            || combined.to_lowercase().contains("broken")
            || combined.to_lowercase().contains("violated");

        if has_failures {
            eprintln!(
                "  {} Medusa found broken invariants — see medusa.log",
                style("⚠").yellow()
            );
        } else {
            eprintln!("  {} Medusa: all invariants held", style("✓").green());
        }
    } else if tests_exist {
        eprintln!("  Medusa not installed — skipping extended fuzzing");
    }

    // ── Step 5: Echidna (optional) ──────────────────────────────────
    if tests_exist && ctx.config.has_tool("echidna") {
        let echidna_bin = ctx.config.resolve_tool("echidna")?;
        let limit_str = ctx.config.passes.fuzzing.echidna_limit.to_string();
        eprintln!("  Running Echidna (test-limit={limit_str})...");

        let output = crate::tools::run_command(
            echidna_bin.to_str().unwrap_or("echidna"),
            &[".", "--contract", "CryticTester", "--test-limit", &limit_str],
            &build_dir,
        )
        .await?;

        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let _ = std::fs::write(results_dir.join("echidna.log"), &combined);

        let has_failures = combined.to_lowercase().contains("failed")
            || combined.to_lowercase().contains("falsified");

        if has_failures {
            eprintln!(
                "  {} Echidna found broken properties — see echidna.log",
                style("⚠").yellow()
            );
        } else {
            eprintln!("  {} Echidna: all properties held", style("✓").green());
        }
    } else if tests_exist {
        eprintln!("  Echidna not installed — skipping");
    }

    // ── Write findings ──────────────────────────────────────────────
    write_json(
        &fuzz_dir.join("fuzzing-findings.json"),
        &Value::Array(all_findings.clone()),
    )?;

    Ok(format!(
        "foundry: {} passed / {} failed, {} fuzzing findings",
        foundry_passed,
        foundry_failed,
        all_findings.len()
    ))
}

async fn generate_invariant_tests(
    ctx: &PipelineContext,
    invariant_dir: &Path,
    logs_dir: &Path,
) -> usize {
    let prompts_dir = ctx.bulwark_root.join(&ctx.config.prompts.dir);
    let prompt_path = prompts_dir.join("invariant-generator.md");

    if !prompt_path.exists() || !ctx.config.has_tool("claude") {
        eprintln!("  Claude or prompt not available — skipping test generation");
        eprintln!(
            "  Place invariant tests manually in {}",
            invariant_dir.display()
        );
        return 0;
    }

    let claude_bin = match ctx.config.resolve_tool("claude") {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let base_prompt = match std::fs::read_to_string(&prompt_path) {
        Ok(p) => p,
        Err(_) => return 0,
    };

    let test_infra = super::scan_test_infrastructure(ctx);

    let prompt = format!(
        "{base_prompt}\n\n---\n\n## Context Files\n\n\
         Read these files for protocol context:\n\
         - PROPERTIES.md (the invariants to test)\n\
         - KNOWN_ISSUES.md (focus areas)\n\
         - ATTACK_PATTERNS.md (known patterns to target)\n\
         - audit-workspace/recon/entry-points.json (function signatures)\n\
         - audit-workspace/recon/storage-layouts.json (state structure)\n\n\
         ## Project Build Infrastructure\n\n\
         CRITICAL: Read the remappings and foundry.toml below carefully. Your tests \
         MUST use these exact import paths or they will not compile.\n\
         {test_infra}\n\n\
         Before writing any test, read at least one existing test file from the list above \
         to understand the import patterns, deployment setup, and test base contracts used \
         in this project. Mirror their style exactly.\n\n\
         ## Output Directory\n\nWrite all test files to: {}\n",
        invariant_dir.display()
    );

    let session = ClaudeSession {
        claude_bin,
        prompt,
        max_turns: ctx.config.passes.fuzzing.max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: logs_dir.join("invariant-generation.log"),
        model: Some(ctx.config.passes.fuzzing.model.clone().unwrap_or_else(|| ctx.config.model.clone())),
        allowed_tools: vec![
            "Read".into(), "Write".into(), "Edit".into(),
            "Glob".into(), "Grep".into(), "Bash".into(),
        ],
        timeout_minutes: Some(60),
    };

    let _ = session.run_with_spinner("Generating invariant tests...").await;
    count_sol_files(invariant_dir)
}

fn copy_sol_files(src: &Path, dest: &Path) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol") {
                if let Some(name) = path.file_name() {
                    std::fs::copy(&path, dest.join(name))?;
                }
            }
        }
    }
    Ok(())
}

fn count_sol_files(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol" || e == "t.sol") {
                count += 1;
            } else if path.is_dir() {
                count += count_sol_files(&path);
            }
        }
    }
    count
}

fn count_invariant_functions(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    count += content.lines().filter(|l| l.contains("function invariant_")).count();
                }
            } else if path.is_dir() {
                count += count_invariant_functions(&path);
            }
        }
    }
    count
}

fn parse_foundry_failure(line: &str, idx: usize) -> Option<Value> {
    // Extract test name from [FAIL. Reason: ...] invariant_Xxx()
    let test_name = line
        .split_whitespace()
        .find(|w| w.starts_with("invariant_") || w.starts_with("test_"))?
        .trim_end_matches('(')
        .trim_end_matches(')');

    // Extract property number if present
    let prop_id = if let Some(pos) = test_name.find('P') {
        let rest = &test_name[pos + 1..];
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            Some(format!("P-{num}"))
        } else {
            None
        }
    } else {
        None
    };

    // Extract reason
    let reason = line
        .split("Reason:")
        .nth(1)
        .map(|s| s.split(']').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| "Invariant broken".into());

    Some(json!({
        "id": format!("FUZZ-{idx:03}"),
        "source": "fuzzer",
        "severity": "High",
        "confidence": "High",
        "title": format!("Fuzzer broke invariant: {test_name}"),
        "contract": "multiple",
        "function": test_name,
        "lines": [],
        "property_violated": prop_id,
        "attack_scenario": format!(
            "Foundry invariant fuzzer found counterexample breaking {test_name}. Reason: {reason}. Run with -vvvv for full trace."
        ),
        "poc_file": null,
        "poc_status": "compiles_and_demonstrates",
        "dedup_hash": ""
    }))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(path, content)?;
    Ok(())
}
