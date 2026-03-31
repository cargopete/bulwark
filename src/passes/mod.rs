pub mod agents;
pub mod formal;
pub mod fuzzing;
pub mod poc;
pub mod recon;
pub mod review;
pub mod stub;

use crate::pipeline::pass::PipelineContext;
use std::path::{Path, PathBuf};

/// Scan the target project's test infrastructure and return a summary string
/// that helps Claude generate compilable tests. Includes remappings, foundry.toml
/// config, and existing test base contract signatures.
pub fn scan_test_infrastructure(ctx: &PipelineContext) -> String {
    let mut info = String::new();

    for pkg in &ctx.config.target.scope {
        let pkg_path = ctx.audit_dir.join(pkg);
        let test_dir = pkg_path.join("test");

        // Find test base contracts
        if test_dir.exists() {
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
        }

        // Remappings
        let remappings = pkg_path.join("remappings.txt");
        if let Ok(content) = std::fs::read_to_string(&remappings) {
            info.push_str(&format!("\nRemappings ({pkg}):\n{content}\n"));
        }

        // foundry.toml
        let foundry_toml = pkg_path.join("foundry.toml");
        if let Ok(content) = std::fs::read_to_string(&foundry_toml) {
            let relevant: String = content
                .lines()
                .filter(|l| {
                    ["src", "test", "out", "libs", "remappings", "solc", "evm_version"]
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
