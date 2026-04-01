use crate::error::Result;
use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::{self, ClaudeSession};
use crate::tools::{forge, slither};
use chrono::Utc;
use console::style;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Pass 1: Reconnaissance — entirely deterministic, no AI.
///
/// Produces structural JSON artefacts consumed by all subsequent passes.
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let recon_dir = ctx.workspace.recon_dir();
    let forge_bin = ctx.config.resolve_tool("forge")?;

    let mut compile_ok = true;
    let mut compiler_version = String::from("unknown");

    // ── 1.1 Compile contracts ─────────────────────────────────────
    eprintln!("  Compiling contracts...");

    for pkg in &ctx.config.target.scope {
        let pkg_path = ctx.audit_dir.join(pkg);
        if !pkg_path.exists() {
            continue;
        }
        let pkg_name = pkg_path.file_name().unwrap_or_default().to_string_lossy();
        eprintln!("    Building {pkg_name}...");

        let result = forge::build(&forge_bin, &pkg_path).await?;
        if result.success {
            eprintln!("    {} {pkg_name} compiled", style("✓").green());
            if compiler_version == "unknown" {
                compiler_version = forge::get_compiler_version(&forge_bin, &pkg_path).await?;
            }
        } else {
            eprintln!("    {} {pkg_name} build failed", style("✗").red());
            let stderr_path = recon_dir.join(format!("build-{pkg_name}-stderr.txt"));
            std::fs::write(&stderr_path, &result.stderr)?;
            compile_ok = false;
        }
    }

    // ── 1.2 Solidity version check ────────────────────────────────
    eprintln!("  Checking Solidity versions...");
    let pragma_versions = collect_pragma_versions(ctx)?;
    let pragma_count = pragma_versions.as_array().map_or(0, |a| a.len());
    write_json(&recon_dir.join("pragma-versions.json"), &pragma_versions)?;
    eprintln!("    {} Found {pragma_count} pragma declarations", style("✓").green());

    // ── 1.3 Static analysis (Slither) ─────────────────────────────
    eprintln!("  Running Slither static analysis...");

    let slither_combined = run_slither_analysis(ctx, &recon_dir).await?;
    let slither_summary = aggregate_slither_summary(&slither_combined);
    write_json(&recon_dir.join("slither-results.json"), &slither_combined)?;
    eprintln!("    {} Slither: {slither_summary}", style("✓").green());

    // ── 1.3b AI-assisted vulnerability scan (scv-scan) ────────────
    if ctx.config.passes.recon.scv_scan {
        eprintln!("  Running AI vulnerability scan (scv-scan)...");
        match run_scv_scan(ctx, &recon_dir).await {
            Ok(summary) => eprintln!("    {} scv-scan: {summary}", style("✓").green()),
            Err(e) => eprintln!("    {} scv-scan skipped: {e}", style("⚠").yellow()),
        }
    }

    // ── 1.4 Entry point mapping ───────────────────────────────────
    eprintln!("  Mapping entry points...");
    let entry_points = map_entry_points(ctx, &forge_bin).await?;
    let func_count: u64 = entry_points
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|v| v.get("functions").and_then(|f| f.as_array()).map(|a| a.len() as u64))
                .sum()
        })
        .unwrap_or(0);
    let contract_count = entry_points.as_object().map_or(0, |m| m.len());
    write_json(&recon_dir.join("entry-points.json"), &entry_points)?;
    eprintln!(
        "    {} Mapped {func_count} state-changing functions across {contract_count} contracts",
        style("✓").green()
    );

    // ── 1.5 Storage layouts ───────────────────────────────────────
    eprintln!("  Extracting storage layouts...");
    let storage = extract_storage_layouts(ctx, &forge_bin).await?;
    let storage_count = storage.as_object().map_or(0, |m| m.len());
    write_json(&recon_dir.join("storage-layouts.json"), &storage)?;
    eprintln!("    {} {storage_count} contracts inspected", style("✓").green());

    // ── 1.6 Dependency graph ──────────────────────────────────────
    eprintln!("  Building dependency graph...");
    let dep_graph = build_dependency_graph(ctx)?;
    let inheritance_count = dep_graph
        .get("inheritance")
        .and_then(|v| v.as_object())
        .map_or(0, |m| m.len());
    write_json(&recon_dir.join("dependency-graph.json"), &dep_graph)?;
    eprintln!(
        "    {} {inheritance_count} contracts with inheritance data",
        style("✓").green()
    );

    // ── 1.7 Access control ────────────────────────────────────────
    eprintln!("  Enumerating access control roles...");
    let access = enumerate_access_control(ctx)?;
    let mod_count = access
        .get("modifiers")
        .and_then(|v| v.as_object())
        .map_or(0, |m| m.len());
    let role_count = access
        .get("roles")
        .and_then(|v| v.as_object())
        .map_or(0, |m| m.len());
    write_json(&recon_dir.join("access-control.json"), &access)?;
    eprintln!(
        "    {} {mod_count} modifiers, {role_count} roles",
        style("✓").green()
    );

    // ── 1.8 Math operations inventory ─────────────────────────────
    eprintln!("  Inventorying arithmetic operations...");
    let math = inventory_math_operations(ctx)?;
    let math_count: usize = math
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|v| v.get("operations").and_then(|o| o.as_array()).map(|a| a.len()))
                .sum()
        })
        .unwrap_or(0);
    write_json(&recon_dir.join("math-operations.json"), &math)?;
    eprintln!(
        "    {} {math_count} operations inventoried",
        style("✓").green()
    );

    // ── 1.9 Proxy mappings ────────────────────────────────────────
    eprintln!("  Identifying proxy relationships...");
    let proxies = identify_proxies(ctx)?;
    let proxy_count = proxies.as_array().map_or(0, |a| a.len());
    write_json(&recon_dir.join("proxy-mappings.json"), &proxies)?;
    eprintln!(
        "    {} {proxy_count} proxy-related contracts",
        style("✓").green()
    );

    // ── 1.10 Assemble recon summary ───────────────────────────────
    let summary = json!({
        "generated": Utc::now().to_rfc3339(),
        "pass": "recon",
        "contracts_compiled": compile_ok,
        "compiler_version": compiler_version,
        "slither_summary": serde_json::to_value(&slither_summary)?,
        "entry_point_count": func_count,
        "storage_contracts": storage_count,
        "proxy_contracts": proxy_count,
        "math_operations_contracts": math.as_object().map_or(0, |m| m.len()),
        "access_control_modifiers": mod_count,
        "artefacts": [
            "recon/entry-points.json",
            "recon/storage-layouts.json",
            "recon/slither-results.json",
            "recon/scv-scan-results.json",
            "recon/dependency-graph.json",
            "recon/math-operations.json",
            "recon/access-control.json",
            "recon/proxy-mappings.json",
            "recon/pragma-versions.json",
        ],
    });
    write_json(&ctx.workspace.recon_summary(), &summary)?;

    Ok(format!(
        "compiled={compile_ok}, slither={slither_summary}, entry_points={func_count}, proxies={proxy_count}"
    ))
}

// ── Helper functions ──────────────────────────────────────────────

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Find all .sol files in scope (excludes tests, mocks, libs, node_modules).
fn find_sol_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_sol_files(dir, &mut files);
    files.sort();
    files
}

fn walk_sol_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            // Skip test/mock/lib/dependency directories
            if matches!(
                name.as_ref(),
                "test" | "tests" | "mock" | "mocks" | "node_modules" | "lib" | "dependencies"
            ) {
                continue;
            }
            walk_sol_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "sol") {
            out.push(path);
        }
    }
}

fn collect_pragma_versions(ctx: &PipelineContext) -> Result<Value> {
    let mut versions = Vec::new();

    for pkg in &ctx.config.target.scope {
        let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
        if !contracts_dir.exists() {
            continue;
        }
        for sol_file in find_sol_files(&contracts_dir) {
            let content = std::fs::read_to_string(&sol_file)?;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("pragma solidity") {
                    let version = trimmed
                        .strip_prefix("pragma solidity")
                        .unwrap_or("")
                        .trim()
                        .trim_end_matches(';')
                        .trim();
                    let rel_path = sol_file
                        .strip_prefix(&ctx.audit_dir)
                        .unwrap_or(&sol_file)
                        .to_string_lossy();
                    versions.push(json!({
                        "file": rel_path,
                        "version": version,
                    }));
                    break;
                }
            }
        }
    }

    Ok(Value::Array(versions))
}

async fn run_slither_analysis(ctx: &PipelineContext, recon_dir: &Path) -> Result<Value> {
    let mut combined = json!({"packages": {}});

    if !ctx.config.has_tool("slither") {
        eprintln!(
            "    {} Slither not found, skipping static analysis",
            style("⚠").yellow()
        );
        return Ok(combined);
    }

    let slither_bin = ctx.config.resolve_tool("slither")?;

    for pkg in &ctx.config.target.scope {
        let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
        if !contracts_dir.exists() {
            continue;
        }

        let pkg_name = Path::new(pkg)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        eprintln!("    Analysing {pkg_name}...");
        let output_path = recon_dir.join(format!("slither-{pkg_name}-raw.json"));

        let result = slither::analyze(&slither_bin, &contracts_dir, &output_path).await?;
        if let Some(json) = result.json {
            combined["packages"][&pkg_name] = json;
            eprintln!("    {} Slither completed for {pkg_name}", style("✓").green());
        } else {
            eprintln!(
                "    {} Slither failed for {pkg_name}",
                style("⚠").yellow()
            );
            // Write stderr for debugging
            let stderr_path = recon_dir.join(format!("slither-{pkg_name}-stderr.txt"));
            std::fs::write(&stderr_path, &result.stderr)?;
        }
    }

    Ok(combined)
}

fn aggregate_slither_summary(combined: &Value) -> slither::SlitherSummary {
    let packages = combined.get("packages").and_then(|p| p.as_object());
    let mut summary = slither::SlitherSummary::default();

    if let Some(pkgs) = packages {
        for pkg_result in pkgs.values() {
            let partial = slither::summarize(pkg_result);
            summary.high += partial.high;
            summary.medium += partial.medium;
            summary.low += partial.low;
            summary.informational += partial.informational;
            summary.optimization += partial.optimization;
        }
    }

    summary
}

async fn map_entry_points(ctx: &PipelineContext, forge_bin: &Path) -> Result<Value> {
    let mut entry_points = json!({});

    for pkg in &ctx.config.target.scope {
        let pkg_path = ctx.audit_dir.join(pkg);
        if !pkg_path.exists() {
            continue;
        }
        let pkg_name = Path::new(pkg)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        for contract in &ctx.config.target.core_contracts {
            // Skip contracts already inspected from a previous package
            if entry_points.get(contract).is_some() {
                continue;
            }

            let abi = forge::inspect_abi(forge_bin, &pkg_path, contract).await?;
            let Some(abi) = abi else { continue };
            let Some(abi_arr) = abi.as_array() else {
                continue;
            };

            // Extract state-changing functions from ABI
            let functions: Vec<Value> = abi_arr
                .iter()
                .filter(|item| {
                    item.get("type").and_then(|t| t.as_str()) == Some("function")
                        && !matches!(
                            item.get("stateMutability").and_then(|s| s.as_str()),
                            Some("view") | Some("pure")
                        )
                })
                .map(|item| {
                    json!({
                        "name": item.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                        "visibility": "external",
                        "mutability": item.get("stateMutability").and_then(|s| s.as_str()).unwrap_or(""),
                        "inputs": item.get("inputs").cloned().unwrap_or(json!([])),
                        "outputs": item.get("outputs").cloned().unwrap_or(json!([])),
                        "modifiers": [],
                    })
                })
                .collect();

            if functions.is_empty() {
                continue;
            }

            // Find source file
            let source_file = find_contract_source(&ctx.audit_dir.join(pkg), contract);
            let rel_source = source_file
                .as_ref()
                .and_then(|p| p.strip_prefix(&ctx.audit_dir).ok())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            entry_points[contract] = json!({
                "file": rel_source,
                "package": pkg_name,
                "functions": functions,
            });

            eprintln!(
                "    {contract}: {} state-changing functions",
                functions.len()
            );
        }
    }

    // TODO: Enrich with modifier data from source (matching bash 1.4 enrichment)

    Ok(entry_points)
}

fn find_contract_source(pkg_dir: &Path, contract_name: &str) -> Option<PathBuf> {
    let target = format!("{contract_name}.sol");
    let contracts_dir = pkg_dir.join("contracts");
    if !contracts_dir.exists() {
        return None;
    }
    find_sol_files(&contracts_dir)
        .into_iter()
        .find(|p| p.file_name().unwrap_or_default().to_string_lossy() == target)
}

async fn extract_storage_layouts(ctx: &PipelineContext, forge_bin: &Path) -> Result<Value> {
    let mut storage = json!({});

    for pkg in &ctx.config.target.scope {
        let pkg_path = ctx.audit_dir.join(pkg);
        if !pkg_path.exists() {
            continue;
        }

        for contract in &ctx.config.target.core_contracts {
            // Skip contracts already inspected from a previous package
            if storage.get(contract).is_some() {
                continue;
            }

            let layout = forge::inspect_storage_layout(forge_bin, &pkg_path, contract).await?;
            let Some(layout) = layout else { continue };

            let gap_count = layout
                .get("storage")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|item| {
                            item.get("label")
                                .and_then(|l| l.as_str())
                                .is_some_and(|l| l.starts_with("__gap"))
                        })
                        .count()
                })
                .unwrap_or(0);

            let slot_count = layout
                .get("storage")
                .and_then(|s| s.as_array())
                .map_or(0, |a| a.len());

            storage[contract] = json!({
                "layout": layout,
                "gap_count": gap_count,
            });

            eprintln!("    {contract}: {slot_count} slots, {gap_count} gaps");
        }
    }

    Ok(storage)
}

fn build_dependency_graph(ctx: &PipelineContext) -> Result<Value> {
    let mut graph = json!({"inheritance": {}, "external_calls": {}});

    for pkg in &ctx.config.target.scope {
        let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
        if !contracts_dir.exists() {
            continue;
        }

        for sol_file in find_sol_files(&contracts_dir) {
            let content = std::fs::read_to_string(&sol_file)?;

            // Extract contract name and inheritance
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("contract ") {
                    if let Some(is_pos) = rest.find(" is ") {
                        let contract_name = rest[..is_pos].trim();
                        let parents_str = rest[is_pos + 4..]
                            .trim_end_matches('{')
                            .trim_end_matches('{')
                            .trim();
                        let parents: Vec<String> = parents_str
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if !parents.is_empty() {
                            graph["inheritance"][contract_name] = json!(parents);
                        }
                    }
                }
            }
        }
    }

    Ok(graph)
}

fn enumerate_access_control(ctx: &PipelineContext) -> Result<Value> {
    let mut access = json!({"modifiers": {}, "roles": {}});

    for pkg in &ctx.config.target.scope {
        let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
        if !contracts_dir.exists() {
            continue;
        }

        for sol_file in find_sol_files(&contracts_dir) {
            let content = std::fs::read_to_string(&sol_file)?;
            let rel_file = sol_file
                .strip_prefix(&ctx.audit_dir)
                .unwrap_or(&sol_file)
                .to_string_lossy()
                .to_string();

            for (line_num, line) in content.lines().enumerate() {
                let trimmed = line.trim();

                // Find modifier definitions
                if trimmed.starts_with("modifier ") {
                    if let Some(name) = trimmed
                        .strip_prefix("modifier ")
                        .and_then(|s| s.split('(').next())
                        .map(|s| s.trim())
                    {
                        let entry = access["modifiers"]
                            .as_object_mut()
                            .unwrap()
                            .entry(name)
                            .or_insert(json!([]));
                        if let Some(arr) = entry.as_array_mut() {
                            arr.push(json!({"file": &rel_file, "line": line_num + 1}));
                        }
                    }
                }

                // Find role-like constants
                if (trimmed.contains("bytes32") || trimmed.contains("uint256"))
                    && (trimmed.contains("ROLE") || trimmed.contains("role") || trimmed.contains("Role"))
                {
                    // Extract the role name
                    for word in trimmed.split_whitespace() {
                        if word.contains("ROLE") || word.contains("role") || word.contains("Role") {
                            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                            if !clean.is_empty() {
                                let entry = access["roles"]
                                    .as_object_mut()
                                    .unwrap()
                                    .entry(clean)
                                    .or_insert(json!([]));
                                if let Some(arr) = entry.as_array_mut() {
                                    if !arr.iter().any(|v| v.as_str() == Some(&rel_file)) {
                                        arr.push(json!(&rel_file));
                                    }
                                }
                                break;
                            }
                        }
                    }
                }

                // Find onlyX modifier usage
                if !trimmed.starts_with("//") {
                    for word in trimmed.split_whitespace() {
                        if word.starts_with("only") && word.len() > 4 {
                            let mod_name = word.trim_matches(|c: char| !c.is_alphanumeric());
                            if mod_name.starts_with("only") {
                                let entry = access["modifiers"]
                                    .as_object_mut()
                                    .unwrap()
                                    .entry(mod_name)
                                    .or_insert(json!([]));
                                if let Some(arr) = entry.as_array_mut() {
                                    arr.push(json!({"file": &rel_file}));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(access)
}

fn inventory_math_operations(ctx: &PipelineContext) -> Result<Value> {
    let mut math = json!({});

    for pkg in &ctx.config.target.scope {
        let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
        if !contracts_dir.exists() {
            continue;
        }

        for contract_name in &ctx.config.target.math_sensitive {
            let target_file = format!("{contract_name}.sol");
            let sol_file = find_sol_files(&contracts_dir)
                .into_iter()
                .find(|p| p.file_name().unwrap_or_default().to_string_lossy() == target_file);

            let Some(sol_file) = sol_file else { continue };
            let content = std::fs::read_to_string(&sol_file)?;
            let rel_file = sol_file
                .strip_prefix(&ctx.audit_dir)
                .unwrap_or(&sol_file)
                .to_string_lossy()
                .to_string();

            let mut ops = Vec::new();

            for (line_num, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }

                let op_type = if trimmed.contains("mulDiv") || trimmed.contains("FullMath") {
                    Some("mulDiv")
                } else if trimmed.contains(" / ") || trimmed.contains(".div(") {
                    Some("division")
                } else if trimmed.contains(" * ") || trimmed.contains(".mul(") {
                    Some("multiplication")
                } else if trimmed.contains(" % ") || trimmed.contains(".mod(") {
                    Some("modulo")
                } else {
                    None
                };

                if let Some(op) = op_type {
                    ops.push(json!({
                        "line": line_num + 1,
                        "operation": op,
                        "context": &trimmed[..trimmed.len().min(200)],
                    }));
                }
            }

            if !ops.is_empty() {
                eprintln!("    {contract_name}: {} arithmetic operations", ops.len());
                math[contract_name] = json!({
                    "file": rel_file,
                    "operations": ops,
                });
            }
        }
    }

    Ok(math)
}

/// Run the tob-scv-scan skill via Claude to scan for 36 vulnerability classes.
///
/// Gracefully degrades: skips if skill not installed or Claude not authenticated.
async fn run_scv_scan(ctx: &PipelineContext, recon_dir: &Path) -> Result<String> {
    if !claude::is_skill_available("tob-scv-scan") {
        return Err(crate::error::BulwarkError::PrerequisiteNotMet {
            pass: "recon".into(),
            reason: "tob-scv-scan skill not installed".into(),
        });
    }

    if !claude::check_auth().is_authenticated() {
        return Err(crate::error::BulwarkError::PrerequisiteNotMet {
            pass: "recon".into(),
            reason: "Claude not authenticated — skipping AI scan".into(),
        });
    }

    let claude_bin = ctx.config.resolve_tool("claude")?;

    // Build contract file list for scope
    let contract_files: Vec<String> = ctx
        .config
        .target
        .scope
        .iter()
        .flat_map(|pkg| {
            let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
            find_sol_files(&contracts_dir)
                .into_iter()
                .filter_map(|p| {
                    p.strip_prefix(&ctx.audit_dir)
                        .ok()
                        .map(|r| r.to_string_lossy().to_string())
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let file_list = contract_files.join("\n");
    let output_path = recon_dir.join("scv-scan-results.json");
    let log_file = recon_dir.join("scv-scan.log");

    let prompt = format!(
        "You MUST complete these two steps:\n\n\
         STEP 1: Run /tob-scv-scan on the following Solidity files in scope:\n\n\
         {file_list}\n\n\
         STEP 2: After the scan completes, write the results as a JSON array to this EXACT path:\n\
         {}\n\n\
         The file MUST contain a JSON array of finding objects. If the scan produces no findings, \
         write an empty array: []\n\n\
         Do NOT skip writing the file. The pipeline depends on this output existing.",
        output_path.display()
    );

    let session = ClaudeSession {
        claude_bin,
        prompt,
        max_turns: ctx.config.passes.recon.scv_scan_max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: log_file.clone(),
        model: Some(ctx.config.model.clone()),
        allowed_tools: vec![],
        timeout_minutes: Some(30),
    };

    let _ = session.run().await?;

    if output_path.exists() {
        let content = std::fs::read_to_string(&output_path)?;
        let count = serde_json::from_str::<Vec<serde_json::Value>>(&content)
            .map(|arr| arr.len())
            .unwrap_or(0);
        Ok(format!("{count} findings"))
    } else {
        // Fallback: try to extract findings from the Claude log
        let findings = claude::extract_findings_from_log(&log_file)?;
        if !findings.is_empty() {
            let content = serde_json::to_string_pretty(&findings)?;
            std::fs::write(&output_path, content)?;
            Ok(format!("{} findings (extracted from log)", findings.len()))
        } else {
            Ok("completed (no findings captured)".into())
        }
    }
}

fn identify_proxies(ctx: &PipelineContext) -> Result<Value> {
    let mut proxies = Vec::new();
    let proxy_patterns = [
        "TransparentUpgradeableProxy",
        "UUPSUpgradeable",
        "ERC1967",
        "delegatecall",
        "_implementation",
    ];

    for pkg in &ctx.config.target.scope {
        let contracts_dir = ctx.audit_dir.join(pkg).join("contracts");
        if !contracts_dir.exists() {
            continue;
        }

        for sol_file in find_sol_files(&contracts_dir) {
            let content = std::fs::read_to_string(&sol_file)?;
            if !proxy_patterns.iter().any(|p| content.contains(p)) {
                continue;
            }

            let rel_file = sol_file
                .strip_prefix(&ctx.audit_dir)
                .unwrap_or(&sol_file)
                .to_string_lossy()
                .to_string();

            // Extract contract name
            let contract_name = content
                .lines()
                .find_map(|line| {
                    let trimmed = line.trim();
                    trimmed
                        .strip_prefix("contract ")
                        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
                })
                .unwrap_or_default();

            let proxy_type = if content.contains("TransparentUpgradeableProxy") {
                "transparent"
            } else if content.contains("UUPSUpgradeable") {
                "uups"
            } else if content.contains("delegatecall") {
                "custom-delegatecall"
            } else {
                "unknown"
            };

            proxies.push(json!({
                "contract": contract_name,
                "file": rel_file,
                "proxy_type": proxy_type,
            }));
        }
    }

    Ok(Value::Array(proxies))
}
