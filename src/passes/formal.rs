use crate::error::Result;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::ClaudeSession;
use console::style;
use serde_json::{json, Value};
use std::path::Path;

/// Pass 5: Formal Verification via Halmos bounded model checking.
///
/// Targets critical properties: P-10, P-15, P-19, P-1, P-16.
/// Runs in parallel with Pass 4 conceptually (both are post-PoC).
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let formal_dir = ctx.workspace.formal_dir();
    let logs_dir = formal_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let halmos_available = ctx.config.has_tool("halmos");
    let solver_timeout = ctx.config.passes.formal.solver_timeout;
    let loop_bound = ctx.config.passes.formal.loop_bound;
    let target_properties = &ctx.config.passes.formal.target_properties;

    if halmos_available {
        eprintln!("  {} Halmos available", style("✓").green());
    } else {
        eprintln!(
            "  {} Halmos not installed — tests will be generated but not run symbolically",
            style("⚠").yellow()
        );
    }

    // ── Step 1: Generate symbolic tests ─────────────────────────────
    let tests_generated = generate_symbolic_tests(ctx, &formal_dir, &logs_dir).await;
    if tests_generated > 0 {
        eprintln!(
            "  {} Generated {tests_generated} symbolic test files",
            style("✓").green()
        );
    } else {
        eprintln!(
            "  {} No symbolic tests generated",
            style("⚠").yellow()
        );
    }

    // ── Step 1b: Enforce suffixed function naming ────────────────────
    // The AI sometimes generates bare `check_P10()` instead of `check_P10_something()`.
    // Halmos uses prefix matching so bare names match nothing when we pass --function check_P10_.
    // Patch any bare names in-place before copying to the forge project.
    patch_bare_check_names(&formal_dir);

    // ── Step 2: Compile ─────────────────────────────────────────────
    let build_dir = ctx.build_dir();
    let tests_exist = count_sol_files(&formal_dir) > 0;

    // Copy generated tests into the forge project so forge can find them
    let forge_test_dir = build_dir.join("test/formal");
    if tests_exist {
        std::fs::create_dir_all(&forge_test_dir)?;
        copy_sol_files(&formal_dir, &forge_test_dir)?;

        eprintln!("  Compiling symbolic tests...");
        let forge_bin = ctx.config.resolve_tool("forge")?;
        let result = crate::tools::forge::build(&forge_bin, &build_dir).await?;
        let result = if !result.success {
            let patched = crate::passes::fuzzing::patch_missing_remappings(&build_dir, &result.stderr);
            if patched > 0 {
                eprintln!("  Added {patched} missing remapping(s) — recompiling...");
                crate::tools::forge::build(&forge_bin, &build_dir).await?
            } else {
                result
            }
        } else {
            result
        };
        if result.success {
            eprintln!("  {} Symbolic tests compile", style("✓").green());
        } else {
            eprintln!("  {} Some tests failed to compile", style("⚠").yellow());
            let _ = std::fs::write(logs_dir.join("formal-build.log"), &result.stderr);
        }
    }

    // ── Step 3: Run Halmos on each property ─────────────────────────
    let mut verification: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut formal_findings: Vec<Value> = Vec::new();

    let properties = if target_properties.is_empty() {
        vec![
            "P-10".into(),
            "P-15".into(),
            "P-19".into(),
            "P-1".into(),
            "P-16".into(),
        ]
    } else {
        target_properties.clone()
    };

    if halmos_available && tests_exist {
        let halmos_bin = ctx.config.resolve_tool("halmos")?;
        eprintln!(
            "  Running Halmos (solver-timeout={solver_timeout}s, loop={loop_bound})...\n"
        );

        let solver_str = solver_timeout.to_string();
        let loop_str = loop_bound.to_string();
        let total_timeout_str = (solver_timeout + 60).to_string();

        // Read the forge out dir from foundry.toml (may differ from default "out")
        let forge_out_dir = read_foundry_out_dir(&build_dir);

        for prop in &properties {
            let prop_num = prop.strip_prefix("P-").unwrap_or(prop);
            // Bare name for searching file contents; underscore-suffixed for Halmos invocation
            // so "check_P1_" doesn't prefix-match "check_P10_", "check_P15_", etc.
            // Suffix is required: "check_P10_" matches "check_P10_something" but NOT bare "check_P10()"
            // This prevents both vacuous VERIFIED results and cross-property prefix contamination.
            let check_func = format!("check_P{prop_num}_");

            // Check if any test file contains a properly-suffixed function
            let has_test = find_function_in_dir(&formal_dir, &check_func);
            if !has_test {
                eprintln!("    {prop}: no symbolic test found — skipping");
                verification.insert(
                    prop.clone(),
                    json!({"status": "no_test", "duration": 0}),
                );
                continue;
            }

            eprintln!("    {prop}: verifying...");
            let start = std::time::Instant::now();

            let output = crate::tools::run_command(
                "timeout",
                &[
                    &total_timeout_str,
                    halmos_bin.to_str().unwrap_or("halmos"),
                    "--function",
                    &check_func,
                    "--loop",
                    &loop_str,
                    "--solver-timeout-assertion",
                    &solver_str,
                    "--solver-timeout-branching",
                    &solver_str,
                    "--forge-build-out",
                    &forge_out_dir,
                    "-vvv",
                ],
                &build_dir,
            )
            .await?;

            let duration = start.elapsed().as_secs();
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let _ = std::fs::write(formal_dir.join(format!("halmos-{prop}.log")), &combined);

            let combined_lower = combined.to_lowercase();
            let status = if combined.contains("Counterexample") || combined.contains("counterexample") {
                eprintln!(
                    "    {} {prop}: VIOLATED — counterexample found ({duration}s)",
                    style("⚠").yellow()
                );
                "VIOLATED"
            } else if combined_lower.contains("timeout") {
                eprintln!("    {prop}: TIMEOUT after {duration}s");
                "TIMEOUT"
            } else if combined.contains("Verified")
                || combined.contains("passed")
                || combined.contains("0 counterexample")
            {
                // If Halmos finishes in under 5s it likely ran 0 tests (no matching functions).
                // This happens when generated tests use bare names like check_P10() instead of
                // check_P10_something() — our --function check_P10_ filter won't match.
                if duration < 5 {
                    eprintln!(
                        "    {} {prop}: VACUOUS — no matching test functions found (check naming: need check_P{}_<desc> suffix)",
                        style("?").yellow(),
                        prop.strip_prefix("P-").unwrap_or(prop)
                    );
                    "VACUOUS"
                } else {
                    eprintln!(
                        "    {} {prop}: VERIFIED (bounded, loop={loop_bound}) in {duration}s",
                        style("✓").green()
                    );
                    "VERIFIED"
                }
            } else if combined.contains("forge build") && combined_lower.contains("error") {
                eprintln!(
                    "    {} {prop}: BUILD ERROR — symbolic tests failed to compile",
                    style("✗").red()
                );
                "build_error"
            } else if combined_lower.contains("error") || combined_lower.contains("panic") {
                eprintln!(
                    "    {} {prop}: ERROR — see halmos-{prop}.log",
                    style("✗").red()
                );
                "ERROR"
            } else {
                eprintln!(
                    "    {} {prop}: UNKNOWN result in {duration}s",
                    style("?").yellow()
                );
                "UNKNOWN"
            };

            let counterexample = if status == "VIOLATED" {
                combined
                    .lines()
                    .skip_while(|l| !l.contains("Counterexample") && !l.contains("counterexample"))
                    .take(20)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                String::new()
            };

            verification.insert(
                prop.clone(),
                json!({
                    "status": status,
                    "duration": duration,
                    "counterexample": if counterexample.is_empty() { Value::Null } else { json!(counterexample) },
                    "loop_bound": loop_bound,
                }),
            );

            // Generate finding for violations
            if status == "VIOLATED" {
                formal_findings.push(json!({
                    "id": format!("HALMOS-{prop}"),
                    "source": "halmos",
                    "severity": "Critical",
                    "confidence": "High",
                    "title": format!("Halmos found counterexample violating {prop}"),
                    "contract": "multiple",
                    "function": check_func,
                    "lines": [],
                    "property_violated": prop,
                    "attack_scenario": format!(
                        "Halmos bounded model checker found a concrete counterexample proving {prop} can be violated. Counterexample: {counterexample}"
                    ),
                    "poc_file": null,
                    "poc_status": "compiles_and_demonstrates",
                    "dedup_hash": ""
                }));
            }
        }
    } else {
        // Record all properties as not_run
        for prop in &properties {
            let reason = if !halmos_available {
                "halmos_not_installed"
            } else {
                "no_tests"
            };
            verification.insert(
                prop.clone(),
                json!({"status": "not_run", "reason": reason}),
            );
        }
    }

    // ── Write outputs ───────────────────────────────────────────────
    let verified: Vec<&String> = properties
        .iter()
        .filter(|p| verification.get(*p).and_then(|v| v.get("status")).and_then(|s| s.as_str()) == Some("VERIFIED"))
        .collect();
    let violated: Vec<&String> = properties
        .iter()
        .filter(|p| verification.get(*p).and_then(|v| v.get("status")).and_then(|s| s.as_str()) == Some("VIOLATED"))
        .collect();
    let timeouts: Vec<&String> = properties
        .iter()
        .filter(|p| verification.get(*p).and_then(|v| v.get("status")).and_then(|s| s.as_str()) == Some("TIMEOUT"))
        .collect();
    let build_errors: Vec<&String> = properties
        .iter()
        .filter(|p| verification.get(*p).and_then(|v| v.get("status")).and_then(|s| s.as_str()) == Some("build_error"))
        .collect();
    let not_run: Vec<&String> = properties
        .iter()
        .filter(|p| {
            let s = verification.get(*p).and_then(|v| v.get("status")).and_then(|s| s.as_str()).unwrap_or("");
            s == "not_run" || s == "no_test" || s == "build_error"
        })
        .collect();

    let summary = json!({
        "properties": verification,
        "summary": {
            "verified": verified,
            "violated": violated,
            "timeout": timeouts,
            "build_errors": build_errors,
            "not_run": not_run,
        },
        "loop_bound": loop_bound,
        "solver_timeout": solver_timeout,
    });

    write_json(&formal_dir.join("verification-summary.json"), &summary)?;
    write_json(
        &formal_dir.join("formal-findings.json"),
        &Value::Array(formal_findings.clone()),
    )?;

    Ok(format!(
        "verified={}, violated={}, timeout={}, not_run={}",
        verified.len(),
        violated.len(),
        timeouts.len(),
        not_run.len()
    ))
}

async fn generate_symbolic_tests(
    ctx: &PipelineContext,
    formal_dir: &Path,
    logs_dir: &Path,
) -> usize {
    let prompts_dir = ctx.bulwark_root.join(&ctx.config.prompts.dir);
    let prompt_path = prompts_dir.join("halmos-generator.md");

    if !prompt_path.exists() || !ctx.config.has_tool("claude") {
        eprintln!("  Claude or prompt not available — skipping test generation");
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

    let halmos_note = if ctx.config.has_tool("halmos") {
        "Halmos IS installed. Tests will be run."
    } else {
        "Halmos is NOT installed. Generate compilable tests anyway — they serve as documentation."
    };

    let test_infra = super::scan_test_infrastructure(ctx);

    let prompt = format!(
        "{base_prompt}\n\n---\n\n## Context Files\n\n\
         Read these files:\n\
         - PROPERTIES.md (the properties to verify)\n\
         - AUDIT_CONTEXT.md (protocol overview)\n\
         - audit-workspace/recon/entry-points.json (function signatures)\n\n\
         ## Project Build Infrastructure\n\n\
         CRITICAL: Read the remappings and foundry.toml below carefully. Your tests \
         MUST use these exact import paths or they will not compile.\n\
         {test_infra}\n\n\
         Before writing any test, read at least one existing test file from the list above \
         to understand the import patterns, deployment setup, and test base contracts used \
         in this project. Mirror their style exactly.\n\n\
         ## Halmos Availability\n\n{halmos_note}\n\n\
         ## Output Directory\n\nWrite all test files and the assessment JSON to: {}\n",
        formal_dir.display()
    );

    let session = ClaudeSession {
        claude_bin,
        prompt,
        max_turns: ctx.config.passes.formal.max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: logs_dir.join("halmos-generation.log"),
        model: Some(ctx.config.passes.formal.model.clone().unwrap_or_else(|| ctx.config.model.clone())),
        allowed_tools: vec![
            "Read".into(), "Write".into(), "Edit".into(),
            "Glob".into(), "Grep".into(), "Bash".into(),
        ],
        timeout_minutes: Some(60),
    };

    let _ = session.run_with_spinner("Generating symbolic tests...").await;
    count_sol_files(formal_dir)
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
            if path.is_dir() {
                count += count_sol_files(&path);
            } else if path.extension().is_some_and(|e| e == "sol") {
                count += 1;
            }
        }
    }
    count
}

fn find_function_in_dir(dir: &Path, func_name: &str) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "sol") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if content.contains(func_name) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Patch bare check_P{N}() function names to check_P{N}_verify() in all .sol files.
/// The AI often generates bare names like `check_P10()` which Halmos's prefix filter
/// `--function check_P10_` cannot match. Rename them in-place before compilation.
fn patch_bare_check_names(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "sol") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else { continue };

        // Find `function check_P{digits}(` — bare name (no underscore after digits).
        // We do this with a simple string scan rather than pulling in a regex dep.
        let mut patched = String::with_capacity(content.len());
        let mut rest = content.as_str();
        let mut changed = false;

        while let Some(pos) = rest.find("function check_P") {
            patched.push_str(&rest[..pos + 16]); // up to and including "function check_P"
            rest = &rest[pos + 16..];

            // Consume digits
            let digit_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            let digits = &rest[..digit_end];
            rest = &rest[digit_end..];

            patched.push_str(digits);

            // If next char is `(` it's a bare name — append `_verify`
            if rest.starts_with('(') {
                patched.push_str("_verify");
                changed = true;
            }
            // (If it starts with `_` it already has a suffix — leave it)
        }
        patched.push_str(rest);

        if changed {
            let _ = std::fs::write(&path, &patched);
        }
    }
}

/// Read `out = '...'` from foundry.toml, defaulting to "out" if not found.
fn read_foundry_out_dir(build_dir: &Path) -> String {
    let toml_path = build_dir.join("foundry.toml");
    if let Ok(content) = std::fs::read_to_string(toml_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("out") {
                if let Some(val) = trimmed.splitn(2, '=').nth(1) {
                    let dir = val.trim().trim_matches('\'').trim_matches('"').to_string();
                    if !dir.is_empty() {
                        return dir;
                    }
                }
            }
        }
    }
    "out".to_string()
}
