use crate::error::Result;
use crate::passes::fuzz_tools::{self, Failure, Fork, PREFIX, TESTER, ToolRun};
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::ClaudeSession;
use console::style;
use serde_json::{Value, json};
use std::path::Path;

/// Pass 4: Fuzzing Campaign.
///
/// 1. Claude generates one suite from PROPERTIES.md that all three fuzzers run
/// 2. Forge runs it, on fork state when the config pins a fork
/// 3. Medusa, then Echidna, run it by deploying `CryticTester`
/// 4. Each broken invariant is classified; only real violations become findings
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let fuzz_dir = ctx.workspace.fuzzing_dir();
    let invariant_dir = fuzz_dir.join("invariant-tests");
    let results_dir = fuzz_dir.join("fuzzing-campaign-results");
    let logs_dir = fuzz_dir.join("logs");
    std::fs::create_dir_all(&invariant_dir)?;
    std::fs::create_dir_all(&results_dir)?;
    std::fs::create_dir_all(&logs_dir)?;

    let forge_bin = ctx.config.resolve_tool("forge")?;

    // ── Step 1: Generate invariant tests ─────────────────────────────
    // Clear stale files from previous runs before generating fresh ones.
    // This prevents partial/timed-out files from a previous run from contaminating the build.
    if invariant_dir.exists() {
        for entry in std::fs::read_dir(&invariant_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "sol") {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let tests_generated = generate_invariant_tests(ctx, &invariant_dir, &logs_dir, None).await;
    if tests_generated == 0 {
        return Err(nothing_ran("no invariant tests were generated; see logs"));
    }
    eprintln!(
        "  {} Generated {tests_generated} invariant test files",
        style("✓").green()
    );

    // ── Step 2: Compile invariant tests ─────────────────────────────
    let build_dir = ctx.build_dir();
    let test_root = build_dir.join(crate::tools::forge::test_dir(&build_dir));
    // A separate directory, so the project's own invariant files (e.g. their Handler.sol)
    // are not overwritten.
    let forge_test_dir = test_root.join("bulwark");

    // Failed PoC generation can leave comment-only stubs whose bad imports break the build.
    clean_stub_poc_files(&test_root.join("pocs"));

    eprintln!("  Compiling invariant tests...");
    let mut build_result =
        compile_generated(&forge_bin, &build_dir, &invariant_dir, &forge_test_dir).await?;
    for repair in 1..=MAX_COMPILE_REPAIRS {
        if build_result.success {
            break;
        }
        let _ = std::fs::write(
            logs_dir.join(format!("invariant-build-{repair}.log")),
            &build_result.stderr,
        );
        eprintln!(
            "  {} Invariant tests failed to compile — repair {repair}/{MAX_COMPILE_REPAIRS}",
            style("⚠").yellow()
        );
        let feedback = format!(
            "## THE FILES IN THE OUTPUT DIRECTORY FAILED TO COMPILE\n\n\
             They are copied to `{}` inside the project and built there with `forge build`. \
             Fix them in place. Check every import against the remappings and foundry.toml above.\n\n\
             ```\n{}\n```\n",
            forge_test_dir
                .strip_prefix(&build_dir)
                .unwrap_or(&forge_test_dir)
                .display(),
            tail(&build_result.stderr, 60)
        );
        generate_invariant_tests(ctx, &invariant_dir, &logs_dir, Some(&feedback)).await;
        build_result =
            compile_generated(&forge_bin, &build_dir, &invariant_dir, &forge_test_dir).await?;
    }
    if !build_result.success {
        let _ = std::fs::write(logs_dir.join("invariant-build.log"), &build_result.stderr);
        // Leaving them in place would break every later forge build in the project.
        let _ = std::fs::remove_dir_all(&forge_test_dir);
        return Err(nothing_ran(
            "generated invariant tests do not compile; see invariant-build.log",
        ));
    }
    eprintln!("  {} Invariant tests compile", style("✓").green());
    eprintln!(
        "  Found {} invariant_ function(s) under {}",
        count_invariant_functions(&test_root),
        test_root
            .strip_prefix(&build_dir)
            .unwrap_or(&test_root)
            .display()
    );
    let suite_rel = forge_test_dir
        .strip_prefix(&build_dir)
        .unwrap_or(&forge_test_dir)
        .display()
        .to_string();
    let fork = fork_target(ctx);
    if let Some(f) = &fork {
        eprintln!("  Fuzzing on fork state at block {}", f.block);
    }
    let cfg = &ctx.config.passes.fuzzing;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    // ── Step 3: Forge ───────────────────────────────────────────────
    eprintln!(
        "  Forge invariant tests (runs={}, depth={})...",
        cfg.invariant_runs, cfg.invariant_depth
    );
    let forge = forge_bin.to_str().unwrap_or("forge");
    let runs_env = format!("FOUNDRY_INVARIANT_RUNS={}", cfg.invariant_runs);
    let depth_env = format!("FOUNDRY_INVARIANT_DEPTH={}", cfg.invariant_depth);
    // Only the Foundry entry: Forge would also run CryticTester's bool properties and ignore `false`.
    let match_path = format!("{suite_rel}/*.t.sol");
    let block_str = fork
        .as_ref()
        .map(|f| f.block.to_string())
        .unwrap_or_default();
    let mut args: Vec<&str> = vec![
        &runs_env,
        &depth_env,
        forge,
        "test",
        "--match-path",
        &match_path,
        "--match-test",
        PREFIX,
        "-vvv",
    ];
    if let Some(f) = &fork {
        args.extend([
            "--fork-url",
            f.rpc.as_str(),
            "--fork-block-number",
            block_str.as_str(),
        ]);
    }
    let output = crate::tools::run_command("env", &args, &build_dir).await?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::write(results_dir.join("foundry-invariant.log"), &combined);
    let forge_run = fuzz_tools::parse_forge(&combined);
    eprintln!(
        "    Forge: {} passed, {} failed",
        forge_run.passed, forge_run.failed
    );
    if forge_run.executed() == 0 {
        return Err(nothing_ran(
            "forge ran no invariant test; see foundry-invariant.log",
        ));
    }

    // ── Step 4: Medusa, then Echidna ────────────────────────────────
    // One after the other: each compiles through crytic-compile, which starts with `forge clean`.
    let corpus = fuzz_dir.join("corpus");
    let (medusa_run, echidna_run) = if !find_contract(&forge_test_dir, TESTER) {
        let note = format!("suite has no {TESTER} contract");
        eprintln!(
            "  {} Medusa and Echidna skipped: {note}",
            style("⚠").yellow()
        );
        (skipped("medusa", &note), skipped("echidna", &note))
    } else {
        let m = run_medusa(
            ctx,
            &build_dir,
            &corpus,
            &results_dir,
            workers,
            fork.as_ref(),
        )
        .await;
        let e = run_echidna(
            ctx,
            &build_dir,
            &corpus,
            &results_dir,
            workers,
            fork.as_ref(),
        )
        .await;
        (m, e)
    };
    for r in [&medusa_run, &echidna_run] {
        match &r.note {
            Some(n) => eprintln!("    {} {}: {n}", style("⚠").yellow(), r.tool),
            None => eprintln!("    {}: {} passed, {} failed", r.tool, r.passed, r.failed),
        }
    }
    let runs = [forge_run, medusa_run, echidna_run];
    write_json(
        &fuzz_dir.join("tool-runs.json"),
        &serde_json::to_value(&runs)?,
    )?;

    // ── Step 5: Classify every broken invariant ─────────────────────
    let broken = group_failures(&runs);
    let mut classifications = Vec::new();
    let mut findings: Vec<Value> = Vec::new();
    if !broken.is_empty() {
        eprintln!(
            "  {} {} broken invariant(s); classifying...",
            style("⚠").yellow(),
            broken.len()
        );
    }
    for (name, failures) in &broken {
        let class = classify(ctx, name, failures, &forge_test_dir, &logs_dir).await;
        eprintln!("    {name}: {} ({})", class.class, class.tools.join(", "));
        if class.class == "real_violation" {
            findings.push(finding(findings.len() + 1, name, failures, &class));
        }
        classifications.push(class);
    }
    write_json(
        &fuzz_dir.join("invariant-classification.json"),
        &serde_json::to_value(&classifications)?,
    )?;
    write_json(
        &fuzz_dir.join("fuzzing-findings.json"),
        &Value::Array(findings.clone()),
    )?;

    let [f, m, e] = &runs;
    Ok(format!(
        "forge {}/{}, medusa {}/{}, echidna {}/{} passed/failed; {} broken, {} real violation(s)",
        f.passed,
        f.failed,
        m.passed,
        m.failed,
        e.passed,
        e.failed,
        broken.len(),
        findings.len()
    ))
}

fn fork_target(ctx: &PipelineContext) -> Option<Fork> {
    let poc = &ctx.config.passes.poc;
    let rpc = std::env::var(poc.fork_rpc_env.as_deref()?).ok()?;
    Some(Fork {
        rpc,
        block: poc.fork_block?,
    })
}

fn skipped(tool: &'static str, note: &str) -> ToolRun {
    ToolRun {
        tool,
        note: Some(note.to_string()),
        ..Default::default()
    }
}

fn find_contract(dir: &Path, name: &str) -> bool {
    let needle = format!("contract {name}");
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|s| s.contains(&needle)))
}

/// Run a fuzzer under a hard wall-clock cap a little past its own timeout.
async fn run_capped(bin: &Path, args: &[&str], dir: &Path, timeout_s: u64, log: &Path) -> String {
    let cap = (timeout_s + 300).to_string();
    let mut full = vec![cap.as_str(), bin.to_str().unwrap_or_default()];
    full.extend_from_slice(args);
    let combined = match crate::tools::run_command("timeout", &full, dir).await {
        Ok(o) => format!(
            "{}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => format!("could not start: {e}"),
    };
    let _ = std::fs::write(log, &combined);
    combined
}

/// A fuzzer that executed nothing is a fault to see, not a clean result.
fn no_tests(mut r: ToolRun, log: &Path) -> ToolRun {
    if r.executed() == 0 && r.note.is_none() {
        r.note = Some(format!("ran no test; see {}", log.display()));
    }
    r
}

async fn run_medusa(
    ctx: &PipelineContext,
    build_dir: &Path,
    corpus: &Path,
    results: &Path,
    workers: usize,
    fork: Option<&Fork>,
) -> ToolRun {
    let Ok(bin) = ctx.config.resolve_tool("medusa") else {
        return skipped("medusa", "not installed");
    };
    let timeout_s = ctx.config.passes.fuzzing.medusa_timeout;
    let config = results.join("medusa.json");
    let cfg =
        fuzz_tools::medusa_config(build_dir, &corpus.join("medusa"), timeout_s, workers, fork);
    if std::fs::write(
        &config,
        serde_json::to_string_pretty(&cfg).unwrap_or_default(),
    )
    .is_err()
    {
        return skipped("medusa", "could not write medusa.json");
    }
    eprintln!("  Medusa (timeout={timeout_s}s, workers={workers})...");
    let log = results.join("medusa.log");
    let cfg_path = config.to_string_lossy().into_owned();
    let out = run_capped(
        &bin,
        &["fuzz", "--config", &cfg_path],
        build_dir,
        timeout_s,
        &log,
    )
    .await;
    no_tests(fuzz_tools::parse_medusa(&out), &log)
}

async fn run_echidna(
    ctx: &PipelineContext,
    build_dir: &Path,
    corpus: &Path,
    results: &Path,
    workers: usize,
    fork: Option<&Fork>,
) -> ToolRun {
    let Ok(bin) = ctx.config.resolve_tool("echidna") else {
        return skipped("echidna", "not installed");
    };
    let cfg = &ctx.config.passes.fuzzing;
    let timeout_s = cfg.medusa_timeout;
    let config = results.join("echidna.yaml");
    let yaml = fuzz_tools::echidna_config(
        &corpus.join("echidna"),
        cfg.echidna_limit,
        timeout_s,
        workers,
        fork,
    );
    if std::fs::write(&config, yaml).is_err() {
        return skipped("echidna", "could not write echidna.yaml");
    }
    eprintln!(
        "  Echidna (testLimit={}, timeout={timeout_s}s, workers={workers})...",
        cfg.echidna_limit
    );
    let log = results.join("echidna.log");
    let cfg_path = config.to_string_lossy().into_owned();
    let out = run_capped(
        &bin,
        &[".", "--contract", TESTER, "--config", &cfg_path],
        build_dir,
        timeout_s,
        &log,
    )
    .await;
    no_tests(fuzz_tools::parse_echidna(&out), &log)
}

/// Failures keyed by invariant name across tools; all three run the same properties.
fn group_failures(runs: &[ToolRun]) -> Vec<(String, Vec<Failure>)> {
    let mut by_name: std::collections::BTreeMap<String, Vec<Failure>> = Default::default();
    for r in runs {
        for f in &r.failures {
            by_name
                .entry(f.invariant.clone())
                .or_default()
                .push(f.clone());
        }
    }
    by_name.into_iter().collect()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Classification {
    invariant: String,
    #[serde(default)]
    tools: Vec<String>,
    /// wrong_invariant, handler_precondition, real_violation, or unclassified
    class: String,
    reason: String,
}

const CLASSES: [&str; 3] = ["wrong_invariant", "handler_precondition", "real_violation"];

fn read_classification(
    path: &Path,
    invariant: &str,
) -> std::result::Result<Classification, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let c: Classification =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if c.invariant != invariant {
        return Err(format!("verdict is for {}, not {invariant}", c.invariant));
    }
    if !CLASSES.contains(&c.class.as_str()) {
        return Err(format!("unknown class {}", c.class));
    }
    Ok(c)
}

async fn classify(
    ctx: &PipelineContext,
    invariant: &str,
    failures: &[Failure],
    suite: &Path,
    logs_dir: &Path,
) -> Classification {
    let tools: Vec<String> = failures.iter().map(|f| f.tool.to_string()).collect();
    let unclassified = |reason: String| Classification {
        invariant: invariant.to_string(),
        tools: tools.clone(),
        class: "unclassified".into(),
        reason,
    };
    let Ok(claude_bin) = ctx.config.resolve_tool("claude") else {
        return unclassified("claude not available".into());
    };
    let verdict_dir = ctx.workspace.fuzzing_dir().join("classification");
    let _ = std::fs::create_dir_all(&verdict_dir);
    let verdict = verdict_dir.join(format!("{invariant}.json"));
    let _ = std::fs::remove_file(&verdict);
    let evidence = serde_json::to_string_pretty(failures).unwrap_or_default();
    let prompt = format!(
        "A fuzzer broke the invariant `{invariant}`. Decide why, from the code, and write one \
         verdict file.\n\n\
         The suite is in `{suite}`. The property is defined there (search for `{invariant}`), and \
         the calls in each sequence are `CryticTester` target functions. Read the property, every \
         handler the sequence goes through, and the protocol code they reach.\n\n\
         ## Failures (tool, reason, call sequence)\n```json\n{evidence}\n```\n\n\
         ## Classes\n\
         - `wrong_invariant`: the property is false by design; it states something the protocol \
           never promised. Cite the code or docs that show it.\n\
         - `handler_precondition`: the sequence is unreachable for real users, because a handler \
           skipped a check the real entry point enforces, pranked a role an attacker cannot hold, \
           or set state directly.\n\
         - `real_violation`: an unprivileged sequence through real entry points breaks a property \
           the protocol does promise.\n\
         Prefer the first two when in doubt; a real violation must survive a hostile reviewer.\n\n\
         Write exactly this JSON to `{verdict}`:\n\
         {{\"invariant\": \"{invariant}\", \"tools\": {tools:?}, \"class\": \"...\", \"reason\": \"two sentences\"}}\n\n\
         Only that file is read; your final message is not.",
        suite = suite.display(),
        verdict = verdict.display(),
    );
    let session = ClaudeSession {
        claude_bin,
        prompt,
        max_turns: 40,
        working_dir: ctx.audit_dir.clone(),
        log_file: logs_dir.join(format!("classify-{invariant}.log")),
        model: Some(ctx.config.model_for("fuzzing")),
        allowed_tools: ["Read", "Grep", "Glob", "Write"].map(String::from).to_vec(),
        timeout_minutes: Some(15),
        label: format!("fuzz-classify:{invariant}"),
        budget_usd: ctx.allowance("fuzzing").split(1),
        ledger: Some(ctx.ledger.clone()),
        resume: None,
    };
    let _ = session
        .run_with_spinner(&format!("Classifying {invariant}..."))
        .await;
    read_classification(&verdict, invariant)
        .unwrap_or_else(|e| unclassified(format!("no usable verdict: {e}")))
}

fn finding(idx: usize, invariant: &str, failures: &[Failure], class: &Classification) -> Value {
    let seed = failures.iter().max_by_key(|f| f.sequence.len()).cloned();
    let prop_id = invariant
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.split('_').next())
        .filter(|p| p.starts_with('P') && p.len() > 1)
        .map(|p| format!("P-{}", &p[1..]));
    json!({
        "id": format!("FUZZ-{idx:03}"),
        "source": "fuzzer",
        "severity": "High",
        "confidence": "Medium",
        "title": format!("Fuzzer broke {invariant}"),
        "contract": "multiple",
        "function": invariant,
        "lines": [],
        "property_violated": prop_id,
        "attack_scenario": format!(
            "{} broke {invariant} ({}). Classified as a real violation: {}",
            class.tools.join(", "),
            seed.as_ref().map(|f| f.reason.as_str()).unwrap_or(""),
            class.reason
        ),
        "call_sequence": seed.map(|f| f.sequence).unwrap_or_default(),
        "poc_file": null,
        "poc_status": "compiles_but_inconclusive",
        "dedup_hash": ""
    })
}

const MAX_COMPILE_REPAIRS: usize = 2;

fn nothing_ran(reason: &str) -> crate::error::BulwarkError {
    crate::error::BulwarkError::NothingRan {
        pass: "fuzzing".into(),
        reason: reason.into(),
    }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Replace the generated copies inside the project and build, patching remappings once.
async fn compile_generated(
    forge_bin: &Path,
    build_dir: &Path,
    invariant_dir: &Path,
    forge_test_dir: &Path,
) -> Result<crate::tools::forge::BuildResult> {
    let _ = std::fs::remove_dir_all(forge_test_dir);
    std::fs::create_dir_all(forge_test_dir)?;
    copy_sol_files(invariant_dir, forge_test_dir)?;
    sanitize_sol_unicode(forge_test_dir);

    let result = crate::tools::forge::build(forge_bin, build_dir).await?;
    if result.success {
        return Ok(result);
    }
    let patched = patch_missing_remappings(build_dir, &result.stderr);
    if patched == 0 {
        return Ok(result);
    }
    eprintln!("  Added {patched} missing remapping(s) — recompiling...");
    crate::tools::forge::build(forge_bin, build_dir).await
}

async fn generate_invariant_tests(
    ctx: &PipelineContext,
    invariant_dir: &Path,
    logs_dir: &Path,
    repair: Option<&str>,
) -> usize {
    let prompt_path = ctx.resolve_prompt_path("invariant-generator.md");

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
    let mode = match fork_target(ctx) {
        Some(f) => format!(
            "All three fuzzers run on a fork of the live chain at block {}. In the setup, attach to \
             the deployed contracts at their real addresses (AUDIT_CONTEXT.md lists them); do not \
             deploy fresh copies. Fund actors by pranking a real holder and transferring.",
            f.block
        ),
        None => "No fork is configured: the setup deploys the protocol locally.".to_string(),
    };

    let prompt = format!(
        "{base_prompt}\n\n---\n\n\
         ## IMPORTANT: No bash available. Just write files. The pipeline compiles them.\n\n\
         ## Chain state\n\n{mode}\n\n\
         ## Project Build Infrastructure\n\n\
         {test_infra}\n\n\
         ## Output Directory\n\n\
         Write ALL test files to this absolute path: {}\n\n\
         Do not write anywhere else. Do not run forge build.\n{}",
        invariant_dir.display(),
        repair.map(|r| format!("\n{r}")).unwrap_or_default()
    );

    let session = ClaudeSession {
        claude_bin,
        prompt: format!("{prompt}{}", ctx.persona_section("fuzzing")),
        max_turns: ctx.config.passes.fuzzing.max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: logs_dir.join(if repair.is_some() {
            "invariant-repair.log"
        } else {
            "invariant-generation.log"
        }),
        model: Some(ctx.config.model_for("fuzzing")),
        // No Bash — prevents Claude from running forge build in a compile-fix loop.
        // The pipeline compiles the generated files itself after this session ends.
        allowed_tools: vec![
            "Read".into(),
            "Write".into(),
            "Edit".into(),
            "Glob".into(),
            "Grep".into(),
        ],
        timeout_minutes: Some(30),
        label: "fuzzing".into(),
        budget_usd: ctx.allowance("fuzzing").split(1),
        ledger: Some(ctx.ledger.clone()),
        resume: None,
    };

    let _ = session
        .run_with_spinner("Generating invariant tests...")
        .await;
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
                    count += content
                        .lines()
                        .filter(|l| l.contains("function invariant_"))
                        .count();
                }
            } else if path.is_dir() {
                count += count_invariant_functions(&path);
            }
        }
    }
    count
}

/// Parse forge build errors for "Source X not found", try to resolve X to a
/// local directory, and append the missing remapping to remappings.txt.
/// Returns the number of remappings patched.
pub fn patch_missing_remappings(build_dir: &Path, stderr: &str) -> usize {
    let remappings_path = build_dir.join("remappings.txt");
    let existing = std::fs::read_to_string(&remappings_path).unwrap_or_default();
    let mut added = 0;

    for line in stderr.lines() {
        // Match lines like: Error (6275): Source "foo/bar/Baz.sol" not found
        let trimmed = line.trim();
        let source_pos = trimmed.find("Source \"");
        if let Some(pos) = source_pos {
            let after = &trimmed[pos + 8..]; // skip `Source "`
            if let Some(path_str) = after.split('"').next() {
                // Extract the remapping prefix (everything up to and including the first '/')
                if let Some(slash) = path_str.find('/') {
                    let prefix = &path_str[..slash];
                    let suffix = &path_str[slash + 1..]; // e.g. "unit/staking/HorizonStaking.t.sol"

                    // Skip if already remapped
                    if existing
                        .lines()
                        .any(|l| l.starts_with(&format!("{prefix}/")))
                    {
                        continue;
                    }

                    // Try to find the file under common local directories
                    let candidates = ["test", "contracts", "src", "lib"];
                    for candidate in candidates {
                        let full = build_dir.join(candidate).join(suffix);
                        if full.exists() {
                            let mapping = format!("{prefix}/={candidate}/\n");
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .append(true)
                                .open(&remappings_path)
                            {
                                use std::io::Write;
                                let _ = f.write_all(mapping.as_bytes());
                                added += 1;
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    added
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Replace curly/smart quotes and typographic dashes in AI-generated .sol files.
/// Solidity only accepts ASCII string literals; curly quotes cause parse errors.
fn sanitize_sol_unicode(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "sol") {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let sanitized = content
                .replace(['\u{201C}', '\u{201D}'], "\"") // curly double quotes
                .replace(['\u{2018}', '\u{2019}'], "'") // curly single quotes
                .replace('\u{2014}', "--") // em dash —
                .replace('\u{2013}', "-") // en dash –
                .replace('\u{2026}', "..."); // horizontal ellipsis …
            if sanitized != content {
                let _ = std::fs::write(&path, sanitized);
            }
        }
    }
}

/// Remove any .sol files in `pocs_dir` that contain no real Solidity code (only comments/blanks).
/// Failed PoC generation can leave stubs like "// File disabled ..." that still get compiled
/// and can contain stale imports that break the entire forge build in subsequent passes.
fn clean_stub_poc_files(pocs_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(pocs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "sol") {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            // A stub file has no lines with actual Solidity content (no `contract`, `pragma`, `import`, etc.)
            let has_real_content = content.lines().any(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with("//") && !t.starts_with("/*") && !t.starts_with('*')
            });
            if !has_real_content {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(tool: &'static str, name: &str, seq: usize) -> Failure {
        Failure {
            tool,
            invariant: name.into(),
            reason: "Return false".into(),
            sequence: (0..seq).map(|i| format!("call {i}")).collect(),
        }
    }

    #[test]
    fn failures_group_by_invariant_across_tools() {
        let runs = [
            ToolRun {
                tool: "forge",
                failures: vec![failure("forge", "invariant_P2_backing", 1)],
                ..Default::default()
            },
            ToolRun {
                tool: "medusa",
                failures: vec![
                    failure("medusa", "invariant_P2_backing", 3),
                    failure("medusa", "invariant_P5_fees", 2),
                ],
                ..Default::default()
            },
        ];
        let g = group_failures(&runs);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, "invariant_P2_backing");
        assert_eq!(g[0].1.len(), 2);
    }

    #[test]
    fn verdict_must_name_the_invariant_and_a_known_class() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.json");
        let write = |s: &str| std::fs::write(&p, s).unwrap();
        write(
            r#"{"invariant":"invariant_x","tools":["medusa"],"class":"real_violation","reason":"r"}"#,
        );
        assert_eq!(
            read_classification(&p, "invariant_x").unwrap().class,
            "real_violation"
        );
        assert!(read_classification(&p, "invariant_y").is_err());
        write(r#"{"invariant":"invariant_x","class":"probably_fine","reason":"r"}"#);
        assert!(read_classification(&p, "invariant_x").is_err());
        assert!(read_classification(&dir.path().join("missing.json"), "invariant_x").is_err());
    }

    #[test]
    fn a_real_violation_carries_the_longest_sequence_as_its_seed() {
        let class = Classification {
            invariant: "invariant_P2_backing".into(),
            tools: vec!["forge".into(), "medusa".into()],
            class: "real_violation".into(),
            reason: "donation".into(),
        };
        let fails = [
            failure("forge", "invariant_P2_backing", 1),
            failure("medusa", "invariant_P2_backing", 3),
        ];
        let f = finding(1, "invariant_P2_backing", &fails, &class);
        assert_eq!(f["id"], "FUZZ-001");
        assert_eq!(f["property_violated"], "P-2");
        assert_eq!(f["call_sequence"].as_array().unwrap().len(), 3);
        assert_eq!(f["poc_status"], "compiles_but_inconclusive");
    }
}
