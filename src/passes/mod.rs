pub mod agents;
pub mod formal;
pub mod fuzzing;
pub mod poc;
pub mod poc_evidence;
pub mod recon;
pub mod review;
pub mod scope;
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
        let test_rel = crate::tools::forge::test_dir(&pkg_path);
        let test_dir = pkg_path.join(&test_rel);

        // Find test base contracts
        if test_dir.exists() {
            let mut bases = Vec::new();
            if let Ok(entries) = glob_sol_files(&test_dir) {
                for file in entries {
                    if let Ok(content) = std::fs::read_to_string(&file) {
                        if content.contains("contract")
                            && content.contains("Test")
                            && content.contains(" is ")
                        {
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
                info.push_str(&format!(
                    "\nExisting test bases in {pkg}/{}/:\n",
                    test_rel.display()
                ));
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

        // foundry.toml, whole: a key filter drops the body of a multi-line remappings array.
        if let Ok(content) = std::fs::read_to_string(pkg_path.join("foundry.toml")) {
            let content: String = content.chars().take(6000).collect();
            info.push_str(&format!(
                "\nfoundry.toml ({pkg}):\n{content}\nTests are collected only from `{pkg}/{}/`.\n",
                test_rel.display()
            ));
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
