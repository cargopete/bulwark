//! Compare what is deployed with what the run audits. A pinned tag can lag or lead the
//! implementation a proxy actually routes to, and an hour of agent work on code that is not
//! live is wasted (SSV, #40).

use crate::error::{BulwarkError, Result};
use crate::pipeline::pass::PipelineContext;
use console::style;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// EIP-1967 implementation slot.
const IMPL_SLOT: &str = "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnMismatch {
    #[default]
    Stop,
    Warn,
    Off,
}

/// Line endings and trailing whitespace are not a difference in what is deployed.
fn normalise(src: &str) -> String {
    src.replace("\r\n", "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// The file in the audited tree that a verified source path refers to: the longest shared
/// path suffix wins, so `src/Vault.sol` beats an unrelated `mocks/Vault.sol`.
fn local_match<'a>(verified_path: &str, local: &'a [PathBuf], root: &Path) -> Option<&'a PathBuf> {
    let want: Vec<&str> = verified_path.rsplit('/').collect();
    local
        .iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned();
            let have: Vec<String> = rel.rsplit('/').map(String::from).collect();
            let shared = want
                .iter()
                .zip(&have)
                .take_while(|(a, b)| **a == b.as_str())
                .count();
            (shared > 0).then_some((shared, p))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

#[derive(Debug, PartialEq)]
pub struct Comparison {
    pub same: Vec<String>,
    pub differs: Vec<String>,
    pub not_in_tree: Vec<String>,
}

pub fn compare(verified: &BTreeMap<String, String>, local: &[PathBuf], root: &Path) -> Comparison {
    let mut c = Comparison {
        same: vec![],
        differs: vec![],
        not_in_tree: vec![],
    };
    for (path, content) in verified {
        match local_match(path, local, root) {
            None => c.not_in_tree.push(path.clone()),
            Some(file) => {
                let ours = std::fs::read_to_string(file).unwrap_or_default();
                if normalise(&ours) == normalise(content) {
                    c.same.push(path.clone());
                } else {
                    c.differs.push(path.clone());
                }
            }
        }
    }
    c
}

/// Sources from an Etherscan `SourceCode` field: a single file, a `{path: {content}}` map,
/// or a standard-JSON input wrapped in an extra pair of braces.
pub fn etherscan_sources(source_code: &str, contract_name: &str) -> BTreeMap<String, String> {
    let s = source_code.trim();
    let inner = s
        .strip_prefix("{{")
        .and_then(|x| x.strip_suffix("}}"))
        .map(|x| format!("{{{x}}}"));
    let parsed: Option<Value> = serde_json::from_str(inner.as_deref().unwrap_or(s)).ok();
    let map = parsed.as_ref().map(|v| v.get("sources").unwrap_or(v));
    match map.and_then(|m| m.as_object()) {
        Some(obj) => obj
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.get("content")?.as_str()?.to_string())))
            .collect(),
        None if !s.is_empty() => [(format!("{contract_name}.sol"), s.to_string())].into(),
        None => BTreeMap::new(),
    }
}

async fn run(cmd: &str, args: &[&str], dir: &Path) -> Option<String> {
    let out = crate::tools::run_command(cmd, args, dir).await.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn implementation(addr: &str, rpc: &str, block: u64, dir: &Path) -> String {
    let b = block.to_string();
    let slot = run(
        "cast",
        &["storage", addr, IMPL_SLOT, "--rpc-url", rpc, "--block", &b],
        dir,
    )
    .await;
    match slot.as_deref().map(|s| s.trim_start_matches("0x")) {
        Some(hex) if hex.len() == 64 && hex.chars().any(|c| c != '0') => {
            format!("0x{}", &hex[24..])
        }
        _ => addr.to_string(),
    }
}

async fn verified_sources(
    chain: u64,
    addr: &str,
    name: &str,
    dir: &Path,
) -> Option<BTreeMap<String, String>> {
    let url = format!("https://sourcify.dev/server/v2/contract/{chain}/{addr}?fields=sources");
    if let Some(body) = run("curl", &["-sSLf", "-A", "bulwark", &url], dir).await {
        let v: Value = serde_json::from_str(&body).ok()?;
        let sources: BTreeMap<String, String> = v
            .get("sources")?
            .as_object()?
            .iter()
            .filter_map(|(k, s)| Some((k.clone(), s.get("content")?.as_str()?.to_string())))
            .collect();
        if !sources.is_empty() {
            return Some(sources);
        }
    }
    let key = std::env::var("ETHERSCAN_API_KEY").ok()?;
    let url = format!(
        "https://api.etherscan.io/v2/api?chainid={chain}&module=contract&action=getsourcecode&address={addr}&apikey={key}"
    );
    let body = run("curl", &["-sSLf", &url], dir).await?;
    let v: Value = serde_json::from_str(&body).ok()?;
    let src = v.get("result")?.get(0)?.get("SourceCode")?.as_str()?;
    let sources = etherscan_sources(src, name);
    (!sources.is_empty()).then_some(sources)
}

fn sol_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if !matches!(
                name.as_ref(),
                "node_modules" | "lib" | "out" | "cache" | ".git"
            ) {
                sol_files(&p, out);
            }
        } else if name.ends_with(".sol") {
            out.push(p);
        }
    }
}

/// Runs before Pass 1. Writes `recon/deployed-source.json` and the verified sources beside it.
pub async fn check(ctx: &PipelineContext) -> Result<()> {
    let mode = ctx.config.passes.recon.deployed_source;
    let Some(program) = ctx.config.program.as_ref() else {
        return Ok(());
    };
    if mode == OnMismatch::Off {
        return Ok(());
    }
    let poc = &ctx.config.passes.poc;
    let rpc = poc
        .fork_rpc_env
        .as_deref()
        .and_then(|e| std::env::var(e).ok());
    let (Some(rpc), Some(block)) = (rpc, poc.fork_block) else {
        eprintln!(
            "  {} Deployed-source check skipped: no fork RPC and block",
            style("→").cyan()
        );
        return Ok(());
    };
    let dir = &ctx.audit_dir;
    let fork_chain: u64 = run("cast", &["chain-id", "--rpc-url", &rpc], dir)
        .await
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let mut local = Vec::new();
    sol_files(dir, &mut local);
    let out_dir = ctx.workspace.recon_dir().join("deployed-source");
    let mut report = Vec::new();
    let mut stops = Vec::new();

    eprintln!("  Comparing deployed source with the pinned commit...");
    for asset in &program.assets {
        let (Some(addr), true) = (asset.address.as_deref(), asset.kind == "smart_contract") else {
            continue;
        };
        let chain = asset.chain_id.unwrap_or(1);
        if chain != fork_chain || asset.name.is_empty() {
            report.push(json!({"asset": asset.name, "address": addr, "chain_id": chain, "status": "unchecked"}));
            continue;
        }
        let imp = implementation(addr, &rpc, block, dir).await;
        let Some(sources) = verified_sources(chain, &imp, &asset.name, dir).await else {
            report.push(json!({"asset": asset.name, "address": addr, "implementation": imp, "status": "unverified"}));
            continue;
        };
        let asset_dir = out_dir.join(format!("{}-{}", asset.name, &imp[..10.min(imp.len())]));
        for (path, content) in &sources {
            let p = asset_dir.join(path.trim_start_matches('/'));
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, content);
        }
        let c = compare(&sources, &local, dir);
        let own = format!("{}.sol", asset.name);
        let own_differs = c
            .differs
            .iter()
            .any(|p| p.rsplit('/').next() == Some(own.as_str()));
        let status = if own_differs {
            "differs"
        } else if c.differs.is_empty() {
            "matches"
        } else {
            "dependencies_differ"
        };
        if own_differs {
            stops.push(format!(
                "{} ({}): {}",
                asset.name,
                imp,
                c.differs.join(", ")
            ));
        }
        eprintln!(
            "    {} {} at {}: {status} ({} same, {} differ, {} not in tree)",
            if status == "matches" {
                style("✓").green()
            } else {
                style("⚠").yellow()
            },
            asset.name,
            imp,
            c.same.len(),
            c.differs.len(),
            c.not_in_tree.len()
        );
        report.push(json!({
            "asset": asset.name, "address": addr, "implementation": imp, "status": status,
            "differs": c.differs, "not_in_tree": c.not_in_tree, "same": c.same.len(),
            "verified_source": asset_dir.strip_prefix(dir).unwrap_or(&asset_dir),
        }));
    }
    std::fs::create_dir_all(ctx.workspace.recon_dir())?;
    std::fs::write(
        ctx.workspace.recon_dir().join("deployed-source.json"),
        serde_json::to_string_pretty(&report)?,
    )?;

    if !stops.is_empty() && mode == OnMismatch::Stop {
        return Err(BulwarkError::Config(format!(
            "the deployed source differs from the pinned commit for: {}. Pin the deployed commit, \
             or set passes.recon.deployed_source = \"warn\" to audit anyway. Verified sources are in {}",
            stops.join("; "),
            out_dir.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_is_not_a_difference() {
        assert_eq!(normalise("a  \r\nb\n\n"), normalise("a\nb"));
        assert_ne!(normalise("a\nb"), normalise("a\nc"));
    }

    #[test]
    fn the_longest_shared_suffix_picks_the_local_file() {
        let root = Path::new("/r");
        let local = vec![
            PathBuf::from("/r/mocks/Vault.sol"),
            PathBuf::from("/r/src/core/Vault.sol"),
        ];
        assert_eq!(
            local_match("project/src/core/Vault.sol", &local, root),
            Some(&PathBuf::from("/r/src/core/Vault.sol"))
        );
        assert_eq!(local_match("lib/Other.sol", &local, root), None);
    }

    #[test]
    fn compare_sorts_files_into_same_differs_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("src/A.sol");
        let b = dir.path().join("src/B.sol");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, "contract A {}\n").unwrap();
        std::fs::write(&b, "contract B { uint x; }").unwrap();
        let verified: BTreeMap<String, String> = [
            ("src/A.sol".into(), "contract A {}".into()),
            ("src/B.sol".into(), "contract B { uint y; }".into()),
            ("lib/oz/C.sol".into(), "c".into()),
        ]
        .into();
        let c = compare(&verified, &[a, b], dir.path());
        assert_eq!(c.same, vec!["src/A.sol"]);
        assert_eq!(c.differs, vec!["src/B.sol"]);
        assert_eq!(c.not_in_tree, vec!["lib/oz/C.sol"]);
    }

    #[test]
    fn etherscan_source_shapes() {
        let std_json = r#"{{"language":"Solidity","sources":{"src/A.sol":{"content":"a"}}}}"#;
        assert_eq!(etherscan_sources(std_json, "A")["src/A.sol"], "a");
        let map = r#"{"src/B.sol":{"content":"b"}}"#;
        assert_eq!(etherscan_sources(map, "B")["src/B.sol"], "b");
        assert_eq!(
            etherscan_sources("contract C {}", "C")["C.sol"],
            "contract C {}"
        );
    }
}
