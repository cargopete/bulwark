use crate::error::Result;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::ClaudeSession;
use console::style;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Pass 5: Formal Verification via Halmos bounded model checking.
///
/// Two layers per property. `real` tests deploy the in-scope contracts inside the target
/// project and call them with symbolic inputs; only their counterexamples become findings.
/// `model` tests are import-free arithmetic copies kept as a sanity layer. A VERIFIED result
/// from either is bounded evidence and never clears or downgrades a finding.
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let formal_dir = ctx.workspace.formal_dir();
    let logs_dir = formal_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let halmos_available = ctx.config.has_tool("halmos");
    let solver_timeout = ctx.config.passes.formal.solver_timeout;
    let loop_bound = ctx.config.passes.formal.loop_bound;

    if halmos_available {
        eprintln!("  {} Halmos available", style("✓").green());
    } else {
        eprintln!(
            "  {} Halmos not installed — tests will be generated but not run symbolically",
            style("⚠").yellow()
        );
    }

    let build_dir = ctx.build_dir();
    let real_dir = build_dir
        .join(crate::tools::forge::test_dir(&build_dir))
        .join("bulwark-formal");
    let properties = property_ids(ctx);
    eprintln!(
        "  {} properties from {}: {}",
        properties.len(),
        if ctx.config.passes.formal.target_properties.is_empty() {
            "PROPERTIES.md"
        } else {
            "passes.formal.target_properties"
        },
        properties.join(", ")
    );

    // ── Step 1: Generate symbolic tests ─────────────────────────────
    // A previous run's tests would be graded as this run's work.
    let _ = std::fs::remove_dir_all(&real_dir);
    let _ = std::fs::remove_dir_all(formal_dir.join("real-unbuilt"));
    let tests_generated =
        generate_symbolic_tests(ctx, &formal_dir, &real_dir, &properties, &logs_dir, None).await;
    if tests_generated > 0 {
        eprintln!(
            "  {} Generated {tests_generated} symbolic test files",
            style("✓").green()
        );
    } else {
        eprintln!("  {} No symbolic tests generated", style("⚠").yellow());
    }

    // Halmos matches --function by prefix, so a bare `check_PT2()` matches nothing.
    patch_bare_check_names(&formal_dir);
    patch_bare_check_names(&real_dir);

    // Only the canonical Symbolic*.t.sol model files stay; leftovers from the session's
    // experiments crash Halmos.
    for entry in std::fs::read_dir(&formal_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.extension().is_some_and(|e| e == "sol") && !name.starts_with("Symbolic") {
            let _ = std::fs::remove_file(&p);
        }
    }

    let forge_bin = ctx.config.resolve_tool("forge")?;
    let forge = forge_bin.to_str().unwrap_or("forge");

    // ── Step 2: Compile ─────────────────────────────────────────────
    let model_tests = discover_checks(&formal_dir);
    if !model_tests.is_empty() {
        ensure_formal_foundry_toml(&formal_dir);
        // Halmos reads the AST, storage layout and metadata from each artifact.
        eprintln!("  Compiling model tests...");
        let out = crate::tools::run_command(
            forge,
            &[
                "build",
                "--force",
                "--ast",
                "--extra-output",
                "storageLayout",
                "metadata",
            ],
            &formal_dir,
        )
        .await?;
        if !out.status.success() {
            eprintln!(
                "  {} Some model tests failed to compile",
                style("⚠").yellow()
            );
            let _ = std::fs::write(logs_dir.join("formal-build.log"), &out.stderr);
        }
    }

    let halmos_bin = if halmos_available {
        Some(ctx.config.resolve_tool("halmos")?)
    } else {
        None
    };
    let settings = HalmosSettings {
        solver_timeout,
        loop_bound,
    };
    eprintln!("  Running Halmos (solver-timeout={solver_timeout}s, loop={loop_bound})...\n");

    // ── Step 3a: real layer, with one repair round ──────────────────
    let mut real_build_failed = false;
    let mut real_runs: BTreeMap<String, Vec<(String, HalmosRun)>> = BTreeMap::new();
    if let Some(halmos) = halmos_bin.as_deref() {
        let mut real_tests = discover_checks(&real_dir);
        let mut build_log = compile_real(forge, &build_dir, &real_tests).await?;
        if build_log.is_none() {
            real_runs = run_real_layer(
                halmos,
                &build_dir,
                &formal_dir,
                &properties,
                &real_tests,
                &settings,
            )
            .await?;
        }

        let broken: Vec<&HalmosRun> = real_runs
            .values()
            .flatten()
            .map(|(_, r)| r)
            .filter(|r| did_not_run(r.status))
            .collect();
        if build_log.is_some() || !broken.is_empty() {
            let evidence = match &build_log {
                Some(log) => format!("`forge build` failed:\n```\n{}\n```\n", tail(log, 60)),
                None => broken
                    .iter()
                    .map(|r| {
                        let log = std::fs::read_to_string(&r.log).unwrap_or_default();
                        format!(
                            "{} ({}):\n```\n{}\n```\n",
                            r.log.display(),
                            r.status,
                            halmos_errors(&log)
                        )
                    })
                    .collect(),
            };
            eprintln!(
                "  {} Real-contract tests did not run; one repair round",
                style("⚠").yellow()
            );
            let feedback = format!(
                "\n\n## REPAIR: the real-contract tests did not run\n\n\
                 Fix the files in the real-contract directory in place, then check each one with \
                 `halmos --contract <C> --function <prefix>` before you finish. Do not touch the \
                 model directory.\n\n{evidence}"
            );
            generate_symbolic_tests(
                ctx,
                &formal_dir,
                &real_dir,
                &properties,
                &logs_dir,
                Some(&feedback),
            )
            .await;
            patch_bare_check_names(&real_dir);
            real_tests = discover_checks(&real_dir);
            build_log = compile_real(forge, &build_dir, &real_tests).await?;
            if build_log.is_none() {
                real_runs = run_real_layer(
                    halmos,
                    &build_dir,
                    &formal_dir,
                    &properties,
                    &real_tests,
                    &settings,
                )
                .await?;
            }
        }
        if let Some(log) = build_log {
            // Left in place they would break every later forge build in the project.
            eprintln!(
                "  {} Real-contract tests do not compile; moved to audit-workspace/formal/real-unbuilt/",
                style("⚠").yellow()
            );
            let _ = std::fs::write(logs_dir.join("real-build.log"), &log);
            let parked = formal_dir.join("real-unbuilt");
            let _ = std::fs::remove_dir_all(&parked);
            let _ = std::fs::rename(&real_dir, &parked);
            real_build_failed = !real_tests.is_empty();
        }
    }

    // ── Step 3b: model layer, and the per-property record ───────────
    let mut verification: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut formal_findings: Vec<Value> = Vec::new();
    let model_out = read_foundry_out_dir(&formal_dir);
    for prop in &properties {
        let prefix = check_prefix(prop);
        let has_model = !contracts_with(&model_tests, &prefix).is_empty();
        let mut entry = serde_json::Map::new();

        let Some(halmos) = halmos_bin.as_deref() else {
            entry.insert("status".into(), json!("not_run"));
            entry.insert("reason".into(), json!("halmos_not_installed"));
            verification.insert(prop.clone(), Value::Object(entry));
            continue;
        };

        let real: Vec<Value> = real_runs
            .get(prop)
            .into_iter()
            .flatten()
            .map(|(contract, r)| {
                report_line(prop, Layer::Real, Some(contract), r);
                if r.status == "VIOLATED" {
                    formal_findings.push(real_finding(prop, contract, &prefix, r));
                }
                r.to_json(Some(contract))
            })
            .collect();
        if !real.is_empty() {
            entry.insert(Layer::Real.key().into(), Value::Array(real));
        } else if real_build_failed {
            entry.insert(Layer::Real.key().into(), json!([{"status": "build_error"}]));
        }

        if has_model {
            let r = run_halmos(
                halmos,
                &formal_dir,
                None,
                &prefix,
                Some(&model_out),
                &settings,
                &formal_dir.join(format!("halmos-model-{prop}.log")),
            )
            .await?;
            report_line(prop, Layer::Model, None, &r);
            entry.insert(Layer::Model.key().into(), r.to_json(None));
        }

        if entry.is_empty() {
            eprintln!("    {prop}: no symbolic test found — skipping");
            entry.insert("status".into(), json!("no_test"));
        } else {
            let (layer, status) = headline(&entry);
            entry.insert("status".into(), json!(status));
            entry.insert("layer".into(), json!(layer));
        }
        entry.insert("check_prefix".into(), json!(prefix));
        entry.insert("loop_bound".into(), json!(loop_bound));
        verification.insert(prop.clone(), Value::Object(entry));
    }

    // ── Write outputs ───────────────────────────────────────────────
    let summary = summarise(&verification);
    let n = |k: &str| summary[k].as_array().map_or(0, Vec::len);
    let counts = format!(
        "real: {} verified, {} violated; model: {} verified, {} counterexamples; {} timeout, {} not run",
        n("verified_real"),
        n("violated_real"),
        n("verified_model"),
        n("counterexamples_model"),
        n("timeout"),
        n("not_run"),
    );
    write_json(
        &formal_dir.join("verification-summary.json"),
        &json!({
            "properties": verification,
            "summary": summary,
            "loop_bound": loop_bound,
            "solver_timeout": solver_timeout,
            "note": "VERIFIED is bounded by loop_bound and the solver timeout. It never clears, \
                     downgrades or closes a finding. Only a real-layer counterexample is a finding.",
        }),
    )?;
    write_json(
        &formal_dir.join("formal-findings.json"),
        &Value::Array(formal_findings),
    )?;
    Ok(counts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The in-scope contracts, deployed inside the target project.
    Real,
    /// Import-free arithmetic copies: a sanity layer, never a finding.
    Model,
}

impl Layer {
    pub fn key(self) -> &'static str {
        match self {
            Layer::Real => "real",
            Layer::Model => "model",
        }
    }
}

struct HalmosSettings {
    solver_timeout: u64,
    loop_bound: u32,
}

struct HalmosRun {
    status: &'static str,
    duration: u64,
    candidate_paths: usize,
    counterexample: Option<String>,
    log: std::path::PathBuf,
}

impl HalmosRun {
    fn to_json(&self, contract: Option<&str>) -> Value {
        json!({
            "contract": contract,
            "status": self.status,
            "duration": self.duration,
            "candidate_paths": self.candidate_paths,
            "counterexample": self.counterexample,
            "log": self.log.file_name().map(|n| n.to_string_lossy().into_owned()),
        })
    }
}

async fn run_halmos(
    halmos: &Path,
    dir: &Path,
    contract: Option<&str>,
    prefix: &str,
    forge_out: Option<&str>,
    s: &HalmosSettings,
    log: &Path,
) -> Result<HalmosRun> {
    // A property's checks share one Halmos run, and each may spend the full solver timeout.
    let total = (s.solver_timeout * 3 + 60).to_string();
    let solver = solver_timeout_arg(s.solver_timeout);
    let loop_bound = s.loop_bound.to_string();
    let mut args: Vec<&str> = vec![&total, halmos.to_str().unwrap_or("halmos")];
    if let Some(c) = contract {
        args.extend(["--contract", c]);
    }
    // Branching keeps Halmos's 1ms default: a slow branch query is pruned, not waited on.
    args.extend([
        "--function",
        prefix,
        "--loop",
        &loop_bound,
        "--solver-timeout-assertion",
        &solver,
    ]);
    if let Some(out) = forge_out {
        args.extend(["--forge-build-out", out]);
    }
    args.push("-vvv");

    let start = std::time::Instant::now();
    let output = crate::tools::run_command("timeout", &args, dir).await?;
    let duration = start.elapsed().as_secs();
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::write(log, &combined);
    let status = classify(&combined, output.status.code(), duration);
    let counterexample = (status == "VIOLATED").then(|| {
        combined
            .lines()
            .skip_while(|l| !l.to_lowercase().contains("counterexample"))
            .take(20)
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(HalmosRun {
        status,
        duration,
        candidate_paths: candidate_paths(&combined),
        counterexample,
        log: log.to_path_buf(),
    })
}

/// Halmos's verdict from its log and exit code.
fn classify(combined: &str, code: Option<i32>, duration: u64) -> &'static str {
    let lower = combined.to_lowercase();
    // "Counterexample: unknown" is a solver timeout, not a violation.
    let real_counterexample =
        lower.contains("counterexample") && !lower.contains("counterexample: unknown");
    let ran = combined.contains("[PASS]") || combined.contains("[FAIL]");
    if real_counterexample {
        "VIOLATED"
    } else if combined.contains("setUp() failed") {
        "setup_failed"
    } else if killed_by_timeout(code)
        || combined.contains("[TIMEOUT]")
        || lower.contains("counterexample: unknown")
    {
        "TIMEOUT"
    } else if combined.contains("[PASS]") && !combined.contains("[FAIL]") {
        "VERIFIED"
    } else if !ran && lower.contains("error") {
        "build_error"
    } else if !ran && duration < 5 {
        "VACUOUS"
    } else if lower.contains("error") || lower.contains("panic") || combined.contains("[FAIL]") {
        "ERROR"
    } else {
        "UNKNOWN"
    }
}

fn report_line(prop: &str, layer: Layer, contract: Option<&str>, r: &HalmosRun) {
    let at = contract.map(|c| format!(" on {c}")).unwrap_or_default();
    let tag = format!("{prop} [{}{at}]", layer.key());
    let d = r.duration;
    match r.status {
        "VIOLATED" => eprintln!(
            "    {} {tag}: VIOLATED — counterexample ({d}s)",
            style("⚠").yellow()
        ),
        "VERIFIED" => eprintln!(
            "    {} {tag}: VERIFIED (bounded) in {d}s",
            style("✓").green()
        ),
        "TIMEOUT" if r.candidate_paths > 0 => eprintln!(
            "    {} {tag}: TIMEOUT after {d}s with {} unconfirmed candidate path(s), worth a manual look",
            style("?").yellow(),
            r.candidate_paths
        ),
        "TIMEOUT" => eprintln!("    {tag}: TIMEOUT after {d}s"),
        other => eprintln!(
            "    {} {tag}: {other}, see {}",
            style("✗").red(),
            r.log.display()
        ),
    }
}

fn real_finding(prop: &str, contract: &str, prefix: &str, r: &HalmosRun) -> Value {
    json!({
        "id": format!("HALMOS-{prop}-{contract}"),
        "source": "halmos",
        "severity": "High",
        "confidence": "High",
        "title": format!("Halmos counterexample to {prop} on {contract}"),
        "contract": contract,
        "function": prefix,
        "lines": [],
        "property_violated": prop,
        "attack_scenario": format!(
            "Halmos found concrete inputs that break {prop} on the real {contract}, deployed in the test. Counterexample: {}",
            r.counterexample.as_deref().unwrap_or("")
        ),
        "poc_file": null,
        "poc_status": "compiles_but_inconclusive",
        "poc_reasons": ["a symbolic counterexample still needs a PoC that shows the loss"],
        "dedup_hash": ""
    })
}

/// Worst status first, so one violated contract is not hidden behind a verified one.
fn rank(status: &str) -> u8 {
    match status {
        "VIOLATED" => 0,
        "TIMEOUT" => 1,
        "ERROR" | "build_error" | "setup_failed" => 2,
        "UNKNOWN" | "VACUOUS" => 3,
        "VERIFIED" => 4,
        _ => 5,
    }
}

/// The property's headline: the real layer when it ran, otherwise the model layer.
fn headline(entry: &serde_json::Map<String, Value>) -> (&'static str, String) {
    let real = entry.get("real").and_then(|v| v.as_array()).and_then(|rs| {
        rs.iter()
            .filter_map(|r| r["status"].as_str())
            .min_by_key(|s| rank(s))
    });
    if let Some(s) = real {
        return ("real", s.to_string());
    }
    let model = entry
        .get("model")
        .and_then(|m| m["status"].as_str())
        .unwrap_or("not_run");
    ("model", model.to_string())
}

fn summarise(verification: &serde_json::Map<String, Value>) -> Value {
    let keys = |f: &dyn Fn(&Value) -> bool| -> Vec<String> {
        verification
            .iter()
            .filter(|(_, v)| f(v))
            .map(|(k, _)| k.clone())
            .collect()
    };
    let at = |layer: &'static str, status: &'static str| {
        keys(&move |v: &Value| v["layer"] == layer && v["status"] == status)
    };
    json!({
        "verified_real": at("real", "VERIFIED"),
        "violated_real": at("real", "VIOLATED"),
        "verified_model": at("model", "VERIFIED"),
        "counterexamples_model": at("model", "VIOLATED"),
        "timeout": keys(&|v: &Value| v["status"] == "TIMEOUT"),
        "not_run": keys(&|v: &Value| matches!(
            v["status"].as_str(),
            Some("not_run" | "no_test" | "build_error" | "setup_failed")
        )),
    })
}

/// Property IDs to verify: the configured override, else every ID PROPERTIES.md defines.
fn property_ids(ctx: &PipelineContext) -> Vec<String> {
    let configured = &ctx.config.passes.formal.target_properties;
    if !configured.is_empty() {
        return configured.clone();
    }
    std::fs::read_to_string(ctx.audit_dir.join("PROPERTIES.md"))
        .map(|md| parse_property_ids(&md))
        .unwrap_or_default()
}

/// IDs a PROPERTIES.md defines: a `P-` token opening a heading, list item, bold label or table
/// row. Mentions inside prose ("see P-3") are not definitions.
pub fn parse_property_ids(md: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for line in md.lines() {
        let rest = line.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '#' | '-' | '*' | '|' | '`' | '[' | '_')
        });
        let Some(after) = rest.strip_prefix("P-") else {
            continue;
        };
        let tail: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
            .collect();
        let tail = tail.trim_end_matches('.');
        if tail.is_empty() || !tail.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let id = format!("P-{tail}");
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Halmos function prefix for a property: `P-T2` → `check_PT2_`, `P-1.2` → `check_P1_2_`.
/// The trailing underscore keeps `check_P1_` from matching `check_P10_`.
pub fn check_prefix(id: &str) -> String {
    format!("check_{}_", id.replace('-', "").replace('.', "_"))
}

/// The property a check function belongs to, by longest matching prefix among known IDs.
pub fn property_of<'a>(function: &str, ids: &'a [String]) -> Option<&'a String> {
    ids.iter()
        .filter(|id| function.starts_with(&check_prefix(id)))
        .max_by_key(|id| id.len())
}

/// `check_` functions in a directory's .sol files, with the contract that declares each.
fn discover_checks(dir: &Path) -> Vec<(String, String)> {
    let mut decls = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "sol") {
            continue;
        }
        if let Ok(src) = std::fs::read_to_string(&p) {
            decls.extend(contract_decls(&src));
        }
    }
    resolve_checks(&decls)
}

/// A contract declaration: name, whether it is abstract, its bases, its own check functions.
#[derive(Debug, Clone, PartialEq)]
struct Decl {
    name: String,
    is_abstract: bool,
    bases: Vec<String>,
    checks: Vec<String>,
}

fn contract_decls(src: &str) -> Vec<Decl> {
    let ident = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    let mut out: Vec<Decl> = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        let (is_abstract, rest) = match t.strip_prefix("abstract contract ") {
            Some(r) => (true, Some(r)),
            None => (false, t.strip_prefix("contract ")),
        };
        if let Some(rest) = rest {
            let bases = rest
                .split_once(" is ")
                .map(|(_, b)| {
                    b.split(['{', ','])
                        .map(|x| ident(x.trim()))
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            out.push(Decl {
                name: ident(rest.trim_start()),
                is_abstract,
                bases,
                checks: Vec::new(),
            });
        } else if let Some(rest) = t.strip_prefix("function check_") {
            if let Some(d) = out.last_mut() {
                d.checks.push(format!("check_{}", ident(rest)));
            }
        }
    }
    out
}

/// Halmos runs a concrete contract, so checks written in an abstract base are attributed to
/// every concrete contract that inherits it, directly or through another base.
fn resolve_checks(decls: &[Decl]) -> Vec<(String, String)> {
    let by_name: BTreeMap<&str, &Decl> = decls.iter().map(|d| (d.name.as_str(), d)).collect();
    let mut out = Vec::new();
    for d in decls.iter().filter(|d| !d.is_abstract) {
        let mut stack = vec![d];
        let mut seen: Vec<&str> = Vec::new();
        while let Some(c) = stack.pop() {
            if seen.contains(&c.name.as_str()) {
                continue;
            }
            seen.push(&c.name);
            for f in &c.checks {
                if !out.contains(&(d.name.clone(), f.clone())) {
                    out.push((d.name.clone(), f.clone()));
                }
            }
            stack.extend(
                c.bases
                    .iter()
                    .filter_map(|b| by_name.get(b.as_str()).copied()),
            );
        }
    }
    out
}

#[cfg(test)]
fn checks_in_source(src: &str) -> Vec<(String, String)> {
    resolve_checks(&contract_decls(src))
}

fn contracts_with(checks: &[(String, String)], prefix: &str) -> Vec<String> {
    let mut cs: Vec<String> = Vec::new();
    for (c, f) in checks {
        if f.starts_with(prefix) && !cs.contains(c) {
            cs.push(c.clone());
        }
    }
    cs
}

async fn generate_symbolic_tests(
    ctx: &PipelineContext,
    formal_dir: &Path,
    real_dir: &Path,
    properties: &[String],
    logs_dir: &Path,
    repair: Option<&str>,
) -> usize {
    let _ = std::fs::create_dir_all(real_dir);
    let prompt_path = ctx.resolve_prompt_path("halmos-generator.md");

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

    let names: String = properties
        .iter()
        .map(|p| format!("| {p} | `{}<description>` |\n", check_prefix(p)))
        .collect();
    let build_dir = ctx.build_dir();
    let rel = |p: &Path| {
        p.strip_prefix(&ctx.audit_dir)
            .unwrap_or(p)
            .display()
            .to_string()
    };

    let prompt = format!(
        "{base_prompt}\n\n---\n\n## Context Files\n\n\
         Read these files:\n\
         - PROPERTIES.md (the properties to verify)\n\
         - AUDIT_CONTEXT.md (protocol overview)\n\
         - audit-workspace/recon/entry-points.json (function signatures)\n\
         - audit-workspace/recon/storage-layouts.json (slots, for vm.store when setUp cannot reach a state)\n\n\
         ## Property IDs and function names\n\n\
         Use exactly these prefixes. The summary is keyed by the ID on the left.\n\n\
         | Property | Function name |\n|---|---|\n{names}\n\
         ## Project Build Infrastructure\n\n\
         CRITICAL: Read the remappings and foundry.toml below carefully. Real-contract tests \
         MUST use these exact import paths or they will not compile.\n\
         {test_infra}\n\n\
         Before writing any test, read at least one existing test file from the list above \
         to understand the import patterns, deployment setup, and test base contracts used \
         in this project. Mirror their style exactly.\n\n\
         ## Halmos Availability\n\n{halmos_note}\n\n\
         ## Output Directories\n\n\
         - Real-contract directory: `{}` (inside the project at `{}`, built with its foundry.toml)\n\
         - Build directory, where you run `forge build` and `halmos`: `{}`\n\
         - Model directory, for model tests and halmos-assessment.json: `{}`\n",
        real_dir.display(),
        rel(real_dir),
        rel(&build_dir),
        formal_dir.display()
    );

    let session = ClaudeSession {
        claude_bin,
        prompt: format!(
            "{prompt}{}{}",
            repair.unwrap_or_default(),
            ctx.persona_section("formal")
        ),
        max_turns: ctx.config.passes.formal.max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: logs_dir.join(if repair.is_some() {
            "halmos-repair.log"
        } else {
            "halmos-generation.log"
        }),
        model: Some(ctx.config.model_for("formal")),
        allowed_tools: vec![
            "Read".into(),
            "Write".into(),
            "Edit".into(),
            "Glob".into(),
            "Grep".into(),
            "Bash".into(),
        ],
        timeout_minutes: Some(60),
        label: "formal".into(),
        budget_usd: ctx.allowance("formal").split(1),
        ledger: Some(ctx.ledger.clone()),
        resume: None,
    };

    let _ = session
        .run_with_spinner("Generating symbolic tests...")
        .await;
    count_sol_files(formal_dir) + count_sol_files(real_dir)
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

/// Rename bare `check_PT2()` to `check_PT2_verify()` in every .sol file: Halmos's prefix filter
/// `--function check_PT2_` cannot match a name with no suffix.
fn patch_bare_check_names(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "sol") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let patched = suffix_bare_checks(&content);
        if patched != content {
            let _ = std::fs::write(&path, &patched);
        }
    }
}

fn suffix_bare_checks(src: &str) -> String {
    const MARK: &str = "function check_P";
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(pos) = rest.find(MARK) {
        out.push_str(&rest[..pos + MARK.len()]);
        rest = &rest[pos + MARK.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        out.push_str(&rest[..end]);
        rest = &rest[end..];
        if rest.starts_with('(') {
            out.push_str("_verify");
        }
    }
    out.push_str(rest);
    out
}

/// Statuses that mean the check never explored the property.
fn did_not_run(status: &str) -> bool {
    matches!(status, "setup_failed" | "build_error" | "ERROR" | "VACUOUS")
}

/// `forge build` in the project; `Some(log)` when it fails.
async fn compile_real(
    forge: &str,
    build_dir: &Path,
    tests: &[(String, String)],
) -> Result<Option<String>> {
    if tests.is_empty() {
        return Ok(None);
    }
    eprintln!("  Compiling real-contract tests in the project...");
    let out = crate::tools::run_command(forge, &["build"], build_dir).await?;
    if out.status.success() {
        eprintln!("  {} Real-contract tests compile", style("✓").green());
        Ok(None)
    } else {
        Ok(Some(format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )))
    }
}

async fn run_real_layer(
    halmos: &Path,
    build_dir: &Path,
    formal_dir: &Path,
    properties: &[String],
    tests: &[(String, String)],
    s: &HalmosSettings,
) -> Result<BTreeMap<String, Vec<(String, HalmosRun)>>> {
    let mut runs = BTreeMap::new();
    for prop in properties {
        let prefix = check_prefix(prop);
        let mut results = Vec::new();
        for contract in contracts_with(tests, &prefix) {
            let log = formal_dir.join(format!("halmos-real-{prop}-{contract}.log"));
            let r = run_halmos(halmos, build_dir, Some(&contract), &prefix, None, s, &log).await?;
            results.push((contract, r));
        }
        if !results.is_empty() {
            runs.insert(prop.clone(), results);
        }
    }
    Ok(runs)
}

/// The lines of a Halmos log that say why a check did not run, without the lint noise.
fn halmos_errors(log: &str) -> String {
    let lines: Vec<&str> = log
        .lines()
        .filter(|l| {
            let t = l.to_lowercase();
            t.contains("error")
                || t.contains("unsupported")
                || t.contains("failed")
                || t.contains("warning  in")
                || l.contains("Running ")
        })
        .filter(|l| !l.starts_with("warning["))
        .take(25)
        .collect();
    lines.join("\n")
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Halmos 0.3 reads a bare number as milliseconds, so `300` gave the solver 0.3s.
fn solver_timeout_arg(secs: u64) -> String {
    format!("{secs}s")
}

/// `timeout` exits 124 when it kills the command; 137 if it had to SIGKILL.
fn killed_by_timeout(code: Option<i32>) -> bool {
    matches!(code, Some(124) | Some(137))
}

/// Paths Halmos flagged for the solver but had not confirmed or refuted when it stopped.
fn candidate_paths(log: &str) -> usize {
    log.lines()
        .filter(|l| l.contains("Found potential path"))
        .count()
}

/// Ensure formal_dir has a valid foundry.toml for standalone Halmos execution.
/// The AI sometimes generates one with full remappings, sometimes skips it entirely.
/// We create a minimal self-sufficient config if absent, or patch evm_version if present.
/// Pure arithmetic tests (no imports) need no remappings.
fn ensure_formal_foundry_toml(formal_dir: &Path) {
    let toml_path = formal_dir.join("foundry.toml");
    if toml_path.exists() {
        // Exists — just enforce evm_version = "shanghai"
        patch_foundry_evm_version(formal_dir, "shanghai");
        return;
    }
    // Create a minimal config. No libs/remappings needed for import-free pure tests.
    let minimal = r#"[profile.default]
src = "."
test = "."
out = "out"
cache_path = ".cache"
evm_version = "shanghai"
optimizer = true
optimizer_runs = 200
"#;
    let _ = std::fs::write(&toml_path, minimal);
}

/// Patch (or add) `evm_version = "<version>"` in the `[profile.default]` section of foundry.toml.
/// Halmos 0.2.x cannot execute Cancun opcodes (MCOPY, TLOAD, TSTORE) — force "shanghai" to avoid
/// `IndexError: pop from empty list` on any contract compiled with solc 0.8.24+.
fn patch_foundry_evm_version(build_dir: &Path, version: &str) {
    let toml_path = build_dir.join("foundry.toml");
    let Ok(content) = std::fs::read_to_string(&toml_path) else {
        return;
    };

    let new_line = format!("evm_version = \"{version}\"");

    // If already set, replace it
    if content.contains("evm_version") {
        let patched = content
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("evm_version") {
                    new_line.as_str()
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&toml_path, patched);
        return;
    }

    // Insert after [profile.default]
    let patched = content.replacen(
        "[profile.default]",
        &format!("[profile.default]\n{new_line}"),
        1,
    );
    let _ = std::fs::write(&toml_path, patched);
}

/// Read `out = '...'` from foundry.toml, defaulting to "out" if not found.
fn read_foundry_out_dir(build_dir: &Path) -> String {
    let toml_path = build_dir.join("foundry.toml");
    if let Ok(content) = std::fs::read_to_string(toml_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("out") {
                if let Some(val) = trimmed.split_once('=').map(|(_, v)| v) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_killed_run_is_a_timeout_and_counts_its_candidates() {
        let log = "Found potential path with path_id=997 Panic(0x01)\n\
                   Found potential path with path_id=900 Panic(0x01)\n  Checking with external solver process\n";
        assert!(killed_by_timeout(Some(124)) && killed_by_timeout(Some(137)));
        assert!(!killed_by_timeout(Some(1)) && !killed_by_timeout(None));
        assert_eq!(candidate_paths(log), 2);
    }

    #[test]
    fn property_ids_come_from_definitions_in_any_shape() {
        let md = "# Properties\n\n\
                  ### P-1: Conservation\nSee P-9 for the other side.\n\
                  - **P-T2** non-rebasing supply\n\
                  * P-V3. vault solvency\n\
                  ## P-1: duplicate heading\n";
        assert_eq!(parse_property_ids(md), vec!["P-1", "P-T2", "P-V3"]);

        let table = "| ID | Property |\n|---|---|\n| P-S1 | shares |\n| `P-Q4` | queue |\n| P-1.2 | sub |\n";
        assert_eq!(parse_property_ids(table), vec!["P-S1", "P-Q4", "P-1.2"]);
        assert!(parse_property_ids("| P-X | no digit |\n- Pause flag\n").is_empty());
    }

    #[test]
    fn check_names_map_back_to_their_property() {
        assert_eq!(check_prefix("P-1"), "check_P1_");
        assert_eq!(check_prefix("P-T2"), "check_PT2_");
        assert_eq!(check_prefix("P-1.2"), "check_P1_2_");
        let ids: Vec<String> = ["P-1", "P-10", "P-T2", "P-1.2"].map(String::from).into();
        assert_eq!(property_of("check_PT2_supply_sum", &ids).unwrap(), "P-T2");
        assert_eq!(property_of("check_P10_bounds", &ids).unwrap(), "P-10");
        assert_eq!(property_of("check_P1_2_sub", &ids).unwrap(), "P-1.2");
        assert_eq!(property_of("check_P1_conservation", &ids).unwrap(), "P-1");
        assert!(property_of("check_P7_x", &ids).is_none());
    }

    #[test]
    fn bare_names_get_a_suffix_and_suffixed_ones_do_not() {
        let src = "function check_PT2(uint a) external {}\nfunction check_P10_x() external {}";
        let out = suffix_bare_checks(src);
        assert!(out.contains("function check_PT2_verify(uint a)"));
        assert!(out.contains("function check_P10_x()"));
    }

    #[test]
    fn checks_are_found_with_their_contract() {
        let src = "import {Test} from \"forge-std/Test.sol\";\n\
                   contract SymbolicVault is Test {\n  function setUp() public {}\n\
                   function check_PV3_solvent(uint256 a) public {}\n}\n\
                   contract SymbolicToken {\n  function check_PT2_sum() external {}\n}\n";
        let found = checks_in_source(src);
        assert_eq!(
            found,
            vec![
                ("SymbolicVault".to_string(), "check_PV3_solvent".to_string()),
                ("SymbolicToken".to_string(), "check_PT2_sum".to_string()),
            ]
        );
        assert_eq!(contracts_with(&found, "check_PV3_"), vec!["SymbolicVault"]);
    }

    #[test]
    fn checks_in_an_abstract_base_run_on_the_contracts_that_inherit_it() {
        let src = "abstract contract SymbolicPTMExchangeBase is Test {\n\
                   function check_P10_roundtrip(uint a) public {}\n}\n\
                   abstract contract Middle is SymbolicPTMExchangeBase {}\n\
                   contract SymbolicPTMExchange6 is Middle {\n function setUp() public {}\n}\n\
                   contract SymbolicPTMExchange18 is SymbolicPTMExchangeBase, Other {\n\
                   function check_P10_extra() public {}\n}\n";
        let found = checks_in_source(src);
        assert!(
            found
                .iter()
                .all(|(c, _)| c != "SymbolicPTMExchangeBase" && c != "Middle")
        );
        assert_eq!(
            contracts_with(&found, "check_P10_"),
            vec!["SymbolicPTMExchange6", "SymbolicPTMExchange18"]
        );
        assert!(found.contains(&("SymbolicPTMExchange18".into(), "check_P10_extra".into())));
    }

    #[test]
    fn the_real_layer_heads_the_summary_and_its_worst_result_wins() {
        let mut real = serde_json::Map::new();
        real.insert(
            "real".into(),
            json!([{"contract": "A", "status": "VERIFIED"}, {"contract": "B", "status": "VIOLATED"}]),
        );
        real.insert("model".into(), json!({"status": "VERIFIED"}));
        assert_eq!(headline(&real), ("real", "VIOLATED".to_string()));

        let mut model_only = serde_json::Map::new();
        model_only.insert("model".into(), json!({"status": "VERIFIED"}));
        assert_eq!(headline(&model_only), ("model", "VERIFIED".to_string()));

        let mut v = serde_json::Map::new();
        v.insert(
            "P-T2".into(),
            json!({"layer": "real", "status": "VIOLATED"}),
        );
        v.insert(
            "P-V3".into(),
            json!({"layer": "model", "status": "VERIFIED"}),
        );
        v.insert(
            "P-1".into(),
            json!({"layer": "model", "status": "VIOLATED"}),
        );
        v.insert("P-9".into(), json!({"status": "no_test"}));
        let s = summarise(&v);
        assert_eq!(s["violated_real"], json!(["P-T2"]));
        assert_eq!(s["verified_model"], json!(["P-V3"]));
        assert_eq!(s["counterexamples_model"], json!(["P-1"]));
        assert_eq!(s["verified_real"], json!([]));
        assert_eq!(s["not_run"], json!(["P-9"]));
    }

    #[test]
    fn classify_reads_halmos_verdicts() {
        assert_eq!(
            classify("[PASS] check_PT2_x() (paths: 4)", Some(0), 9),
            "VERIFIED"
        );
        assert_eq!(
            classify(
                "[FAIL] check_PT2_x()\nCounterexample:\n  p_a = 0x01",
                Some(1),
                9
            ),
            "VIOLATED"
        );
        assert_eq!(
            classify("Counterexample: unknown\n[TIMEOUT]", Some(0), 300),
            "TIMEOUT"
        );
        assert_eq!(
            classify(
                "Found potential path with path_id=1 Panic(0x01)",
                Some(124),
                360
            ),
            "TIMEOUT"
        );
        assert_eq!(
            classify("Error: Compiler run failed", Some(1), 2),
            "build_error"
        );
        assert_eq!(classify("no tests matched", Some(0), 1), "VACUOUS");
        let setup = "Running 3 tests for test/bulwark-formal/X.t.sol:X\n\
                     ERROR    setUp() failed: HalmosException: No successful path found in setUp()\n\
                     Symbolic test result: 0 passed; 3 failed; time: 0.02s";
        assert_eq!(classify(setup, Some(1), 3), "setup_failed");
        assert!(did_not_run("setup_failed") && !did_not_run("TIMEOUT"));
    }

    #[test]
    fn solver_timeout_is_passed_in_seconds() {
        assert_eq!(solver_timeout_arg(300), "300s");
    }

    #[test]
    fn repair_evidence_keeps_the_cause_and_drops_lint() {
        let log = "No files changed, compilation skipped\n\
                   Running 3 tests for test/bulwark-formal/F.t.sol:F\n\
                   ↩ CALL 0x (error: HalmosException('Unsupported cheat code: deployCode(string,bytes)'))\n\
                   warning[reentrancy-no-eth]: external call can be reentered\n\
                   ERROR    setUp() failed: HalmosException: No successful path found in setUp()\n";
        let e = halmos_errors(log);
        assert!(
            e.contains("deployCode") && e.contains("setUp() failed") && e.contains("Running 3")
        );
        assert!(!e.contains("reentrancy"));
    }
}
