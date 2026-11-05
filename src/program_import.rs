//! `bulwark program import <url>`: an Immunefi scope page into `program.toml`.
//!
//! The page is a Next.js app-router render; the program sits in the RSC payload as a
//! `{"bounty": {...}}` object, with long text fields as `$<id>` references to `T` rows.

use crate::config::TargetConfig;
use crate::error::{BulwarkError, Result};
use crate::findings::Severity;
use crate::program::{Asset, Impact, OutOfScope, ProgramConfig};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The RSC payload strings pushed by `self.__next_f.push([1, "..."])`, joined.
fn rsc_payload(html: &str) -> String {
    const MARK: &str = "self.__next_f.push([1,";
    let mut out = String::new();
    let mut from = 0;
    while let Some(off) = html[from..].find(MARK) {
        let start = from + off + MARK.len();
        from = start;
        let mut stream = serde_json::Deserializer::from_str(&html[start..]).into_iter::<String>();
        if let Some(Ok(s)) = stream.next() {
            out.push_str(&s);
        }
    }
    out
}

/// Text of a `$<id>` reference: a payload row `<id>:T<hex byte length>,<text>`.
fn resolve_ref(payload: &str, value: &str) -> Option<String> {
    let id = value.strip_prefix('$')?;
    let marker = format!("{id}:T");
    let mut from = 0;
    while let Some(off) = payload[from..].find(&marker) {
        let at = from + off;
        from = at + marker.len();
        // The row must start a line, or `1a:T` would match inside `51a:T`.
        if at > 0 && !payload[..at].ends_with('\n') {
            continue;
        }
        let rest = &payload[from..];
        let comma = rest.find(',')?;
        let len = usize::from_str_radix(&rest[..comma], 16).ok()?;
        let body = rest.get(comma + 1..comma + 1 + len)?;
        return Some(body.to_string());
    }
    None
}

fn text_field(payload: &str, bounty: &Value, key: &str) -> Option<String> {
    let v = bounty.get(key)?.as_str()?;
    if v.starts_with('$') {
        resolve_ref(payload, v)
    } else {
        Some(v.to_string())
    }
}

/// Top-level Markdown bullets; a line that starts no bullet continues the one before.
fn bullets(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in md.lines() {
        match line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            Some(item) => out.push(item.trim().to_string()),
            None if !line.trim().is_empty() && !line.starts_with(' ') => {
                if let Some(last) = out.last_mut() {
                    last.push(' ');
                    last.push_str(line.trim());
                }
            }
            None => {}
        }
    }
    out.into_iter()
        .map(|l| l.trim_end_matches('.').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Chain id and address from an explorer link such as `https://etherscan.io/address/0x..`.
fn explorer_address(url: &str) -> Option<(Option<u64>, String)> {
    let addr = url.split("/address/").nth(1)?;
    let addr: String = addr
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == 'x')
        .collect();
    if addr.len() != 42 || !addr.starts_with("0x") {
        return None;
    }
    let host = url.split("//").nth(1)?.split('/').next()?;
    let chain = match host.trim_start_matches("www.") {
        "etherscan.io" => Some(1),
        "arbiscan.io" => Some(42161),
        "optimistic.etherscan.io" => Some(10),
        "basescan.org" => Some(8453),
        "polygonscan.com" => Some(137),
        "bscscan.com" => Some(56),
        "snowtrace.io" => Some(43114),
        "gnosisscan.io" => Some(100),
        _ => None,
    };
    Some((chain, addr.to_lowercase()))
}

/// A file pinned by a GitHub blob URL: `github.com/<owner>/<repo>/blob/<commit>/<path>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Blob {
    pub repo: String,
    pub commit: String,
    pub path: String,
}

fn github_blob(url: &str) -> Option<Blob> {
    let url = url.split(['?', '#']).next()?;
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let mut parts = rest.strip_prefix("github.com/")?.splitn(5, '/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next()? != "blob" {
        return None;
    }
    let (commit, path) = (parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || commit.is_empty() || path.is_empty() {
        return None;
    }
    Some(Blob {
        repo: format!("{owner}/{repo}").to_lowercase(),
        commit: commit.to_string(),
        path: path.trim_end_matches('/').to_string(),
    })
}

const SOURCE_EXTENSIONS: &[&str] = &[
    "sol", "vy", "rs", "move", "cairo", "fc", "func", "tact", "huff", "yul",
];

/// Contract name from an address asset's description, e.g. "IssuanceHours (BSC)".
fn description_name(description: &str) -> Option<String> {
    let first = description
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()?;
    let ident = first.chars().next()?.is_ascii_alphabetic()
        && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ident.then(|| first.to_string())
}

/// Contract name from a source file path: the stem, if the extension is a contract language.
fn contract_name(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    (!stem.is_empty() && SOURCE_EXTENSIONS.contains(&ext)).then(|| stem.to_string())
}

/// Distinct commits pinned by blob URLs with their file counts, most files first.
pub fn pinned_commits(assets: &[Asset]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for b in assets.iter().filter_map(|a| github_blob(a.url.as_deref()?)) {
        *counts.entry(b.commit).or_default() += 1;
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Smart-contract reward ceilings per severity, and the Critical percentage of funds at risk.
fn rewards(bounty: &Value) -> (std::collections::BTreeMap<String, u64>, Option<f64>) {
    let mut table = std::collections::BTreeMap::new();
    let mut pct = None;
    let rows = bounty
        .get("programRewards")
        .or_else(|| bounty.get("rewards"))
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    for r in rows
        .iter()
        .filter(|r| r.get("assetType").and_then(|a| a.as_str()) == Some("smart_contract"))
    {
        let Some(sev) = r.get("severity").and_then(|s| s.as_str()) else {
            continue;
        };
        let mut cap = sev.chars();
        let sev = cap
            .next()
            .map(|c| c.to_uppercase().chain(cap).collect::<String>())
            .unwrap_or_default();
        if let Some(max) = r
            .get("maxReward")
            .or_else(|| r.get("fixedReward"))
            .and_then(|m| m.as_u64())
        {
            table.insert(sev.clone(), max);
        }
        if sev == "Critical" {
            pct = r
                .get("rewardCalculationPercentage")
                .and_then(|p| p.as_f64());
        }
    }
    (table, pct)
}

/// `owner/repo` of a GitHub clone URL, https or ssh.
fn github_repo(url: &str) -> Option<String> {
    let rest = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split("github.com")
        .nth(1)?;
    let rest = rest.trim_start_matches([':', '/']);
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !repo.is_empty() && parts.next().is_none())
        .then(|| format!("{owner}/{repo}").to_lowercase())
}

pub struct Imported {
    pub program: ProgramConfig,
    /// The raw bounty object, kept as the snapshot the next import diffs against.
    pub snapshot: Value,
    /// Asset URLs that are neither a source file nor an explorer address.
    pub skipped: Vec<String>,
}

pub fn parse_scope_page(html: &str) -> Result<Imported> {
    let payload = rsc_payload(html);
    let start = payload
        .find("{\"bounty\":{")
        .ok_or_else(|| BulwarkError::Config("no bounty object in the page".into()))?;
    let mut stream = serde_json::Deserializer::from_str(&payload[start..]).into_iter::<Value>();
    let root = stream
        .next()
        .and_then(|r| r.ok())
        .ok_or_else(|| BulwarkError::Config("bounty object did not parse".into()))?;
    let bounty = root
        .get("bounty")
        .ok_or_else(|| BulwarkError::Config("bounty object did not parse".into()))?;

    let smart = |v: &&Value| v.get("type").and_then(|t| t.as_str()) == Some("smart_contract");
    let impacts = bounty
        .get("impacts")
        .and_then(|i| i.as_array())
        .into_iter()
        .flatten()
        .filter(smart)
        .filter_map(|i| {
            let severity: Severity = serde_json::from_value(i.get("severity")?.clone()).ok()?;
            Some(Impact {
                severity,
                text: i.get("title")?.as_str()?.trim().to_string(),
            })
        })
        .collect::<Vec<_>>();
    let mut impacts = impacts;
    impacts.sort_by_key(|i| std::cmp::Reverse(i.severity));

    let mut assets = Vec::new();
    let mut skipped = Vec::new();
    let urls = bounty
        .get("assets")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter(smart)
        .filter_map(|a| {
            let description = a.get("description").and_then(|d| d.as_str()).unwrap_or("");
            Some((a.get("url")?.as_str()?, description))
        });
    for (url, description) in urls {
        let (name, address, chain_id) = if let Some((chain, addr)) = explorer_address(url) {
            (
                description_name(description).unwrap_or_default(),
                Some(addr),
                chain,
            )
        } else if let Some(name) = github_blob(url).and_then(|b| contract_name(&b.path)) {
            (name, None, None)
        } else {
            skipped.push(url.to_string());
            continue;
        };
        assets.push(Asset {
            name,
            address,
            chain_id,
            url: Some(url.to_string()),
            kind: "smart_contract".into(),
        });
    }

    let program_specific = text_field(&payload, bounty, "customOutOfScopeInformation")
        .map(|t| bullets(&t))
        .unwrap_or_default();

    let (rewards, reward_pct) = rewards(bounty);

    let program = ProgramConfig {
        platform: "immunefi".into(),
        slug: bounty
            .get("slug")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .into(),
        severity_system: "v2.3".into(),
        max_payout_usd: bounty.get("maxBounty").and_then(|m| m.as_u64()),
        reward_formula: if reward_pct.is_some() {
            "pct_of_funds_at_risk"
        } else {
            "fixed"
        }
        .into(),
        reward_pct,
        requires_poc: true,
        requires_kyc: bounty.get("kyc").and_then(|k| k.as_bool()).unwrap_or(false),
        assets,
        impacts,
        rewards,
        out_of_scope: OutOfScope { program_specific },
    };
    Ok(Imported {
        program,
        snapshot: bounty.clone(),
        skipped,
    })
}

/// Asset URLs and impact titles added or removed since the last snapshot.
pub fn diff(old: &Value, new: &Value) -> Vec<String> {
    let set = |v: &Value, list: &str, field: &str| -> BTreeSet<String> {
        v.get(list)
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(|x| x.get(field)?.as_str().map(String::from))
            .collect()
    };
    let mut out = Vec::new();
    for (list, field) in [("assets", "url"), ("impacts", "title")] {
        let (o, n) = (set(old, list, field), set(new, list, field));
        out.extend(n.difference(&o).map(|x| format!("+ {list}: {x}")));
        out.extend(o.difference(&n).map(|x| format!("- {list}: {x}")));
    }
    out
}

pub fn to_toml(p: &ProgramConfig) -> Result<String> {
    #[derive(serde::Serialize)]
    struct Wrap<'a> {
        program: &'a ProgramConfig,
    }
    let body = toml::to_string(&Wrap { program: p })
        .map_err(|e| BulwarkError::Config(format!("could not write program.toml: {e}")))?;
    Ok(format!(
        "# Imported by `bulwark program import`. Names come from source file names; fill any\n\
         # empty `name` with the contract name as in source, or that asset filters nothing.\n\n{body}"
    ))
}

pub async fn run(url: &str, project_dir: &Path, target: &TargetConfig) -> Result<()> {
    let out = crate::tools::run_command_strict(
        "curl",
        &["-sSLf", "-A", "bulwark-program-import", url],
        project_dir,
    )
    .await?;
    let html = String::from_utf8_lossy(&out.stdout);
    let imported = parse_scope_page(&html)?;
    let p = &imported.program;

    let snap_path = project_dir.join("program-snapshot.json");
    if let Ok(old) = std::fs::read_to_string(&snap_path) {
        let old: Value = serde_json::from_str(&old)?;
        let changes = diff(&old, &imported.snapshot);
        if changes.is_empty() {
            eprintln!("  No asset or impact changes since the last import");
        } else {
            eprintln!("  Changes since the last import:");
            for c in &changes {
                eprintln!("    {c}");
            }
        }
    }
    std::fs::write(
        &snap_path,
        serde_json::to_string_pretty(&imported.snapshot)?,
    )?;
    std::fs::write(project_dir.join("program.toml"), to_toml(p)?)?;
    eprintln!(
        "  Wrote program.toml for {}: {} impacts, {} assets, {} program exclusions",
        p.slug,
        p.impacts.len(),
        p.assets.len(),
        p.out_of_scope.program_specific.len()
    );
    for url in &imported.skipped {
        eprintln!("  warning: skipped {url}: not a source file or explorer address");
    }
    report_pins(p, target, project_dir).await;
    Ok(())
}

async fn report_pins(p: &ProgramConfig, target: &TargetConfig, dir: &Path) {
    let pins = pinned_commits(&p.assets);
    if pins.is_empty() {
        return;
    }
    eprintln!("  Assets pinned at {} commits:", pins.len());
    for (commit, n) in &pins {
        let s = if *n == 1 { "" } else { "s" };
        eprintln!("    {}  {n} file{s}", short(commit));
    }
    let blobs: Vec<Blob> = p
        .assets
        .iter()
        .filter_map(|a| github_blob(a.url.as_deref()?))
        .collect();
    let fallback = &pins[0].0;
    let Some(repo) = github_repo(&target.repo).filter(|r| blobs.iter().any(|b| &b.repo == r))
    else {
        eprintln!(
            "  Branch check skipped: no blob URL is in target.repo ({})",
            target.repo
        );
        eprintln!(
            "  Suggested target.commit: {} (pins the most files)",
            short(fallback)
        );
        return;
    };
    let blobs: Vec<Blob> = blobs.into_iter().filter(|b| b.repo == repo).collect();
    match check_branch(&repo, &target.branch, &blobs, dir).await {
        Ok(check) => {
            eprintln!(
                "  {} of {} pinned files differ at {} ({}):",
                check.differ.len(),
                blobs.len(),
                target.branch,
                short(&check.head)
            );
            for b in &check.differ {
                eprintln!("    {} (pinned {})", b.path, short(&b.commit));
            }
            match check.newest_ancestor {
                Some(c) => eprintln!(
                    "  Suggested target.commit: {} (newest pin contained in {})",
                    short(&c),
                    target.branch
                ),
                None => eprintln!(
                    "  Suggested target.commit: {} (pins the most files; no pin is in {})",
                    short(fallback),
                    target.branch
                ),
            }
        }
        Err(e) => {
            let e = e.to_string();
            eprintln!(
                "  Branch check skipped: {}",
                e.lines().next().unwrap_or_default()
            );
            eprintln!(
                "  Suggested target.commit: {} (pins the most files)",
                short(fallback)
            );
        }
    }
}

fn short(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

struct BranchCheck {
    head: String,
    differ: Vec<Blob>,
    newest_ancestor: Option<String>,
}

async fn gh_api(path: &str, dir: &Path) -> Result<Value> {
    let out = crate::tools::run_command_strict("gh", &["api", path], dir).await?;
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// Blob sha of every file in the tree at `rev`.
async fn tree(repo: &str, rev: &str, dir: &Path) -> Result<BTreeMap<String, String>> {
    let t = gh_api(&format!("repos/{repo}/git/trees/{rev}?recursive=1"), dir).await?;
    // A truncated tree would report the files it left out as changed.
    if t.get("truncated").and_then(|v| v.as_bool()) != Some(false) {
        return Err(BulwarkError::Config(format!(
            "tree of {repo} at {rev} is truncated"
        )));
    }
    Ok(t.get("tree")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some((
                e.get("path")?.as_str()?.into(),
                e.get("sha")?.as_str()?.into(),
            ))
        })
        .collect())
}

async fn check_branch(repo: &str, branch: &str, blobs: &[Blob], dir: &Path) -> Result<BranchCheck> {
    let head: String = gh_api(&format!("repos/{repo}/commits/{branch}"), dir)
        .await?
        .get("sha")
        .and_then(|s| s.as_str())
        .ok_or_else(|| BulwarkError::Config(format!("no head commit for {repo} {branch}")))?
        .into();
    let at_head = tree(repo, &head, dir).await?;
    let commits: BTreeSet<&str> = blobs.iter().map(|b| b.commit.as_str()).collect();
    let mut differ = Vec::new();
    let mut newest: Option<(u64, String)> = None;
    for commit in commits {
        let pinned = tree(repo, commit, dir).await?;
        for b in blobs.iter().filter(|b| b.commit == commit) {
            let sha = pinned.get(&b.path);
            if sha.is_none() || sha != at_head.get(&b.path) {
                differ.push(b.clone());
            }
        }
        let cmp = gh_api(&format!("repos/{repo}/compare/{commit}...{head}"), dir).await?;
        let status = cmp.get("status").and_then(|s| s.as_str());
        let ahead = cmp
            .get("ahead_by")
            .and_then(|a| a.as_u64())
            .unwrap_or(u64::MAX);
        if matches!(status, Some("ahead" | "identical"))
            && newest.as_ref().is_none_or(|(n, _)| ahead < *n)
        {
            newest = Some((ahead, commit.to_string()));
        }
    }
    differ.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(BranchCheck {
        head,
        differ,
        newest_ancestor: newest.map(|(_, c)| c),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(payload: &str) -> String {
        format!(
            "<html><script>self.__next_f.push([1,{}])</script></html>",
            serde_json::to_string(payload).unwrap()
        )
    }

    const BOUNTY: &str = r#"c:["$","$L64",null,{"bounty":{"slug":"demo","maxBounty":2000000,"kyc":true,
        "assets":[{"url":"https://etherscan.io/address/0xAE7ab96520DE3A18E5e111B5EaAb095312D7fE84","type":"smart_contract"},
                  {"url":"https://github.com/demo/core/blob/abc123/src/Vault.sol#L10","type":"smart_contract"},
                  {"url":"https://github.com/demo/core","type":"smart_contract"},
                  {"url":"https://demo.fi","type":"websites_and_applications"}],
        "impacts":[{"type":"smart_contract","severity":"high","title":"Theft of unclaimed yield"},
                   {"type":"websites_and_applications","severity":"critical","title":"RCE"},
                   {"type":"smart_contract","severity":"critical","title":"Direct theft of any user funds"}],
        "customOutOfScopeInformation":"$65"}}]
65:T2a,- Best practice critiques.
- Frontrunning
"#;

    #[test]
    fn parses_bounty_from_rsc_payload() {
        let p = parse_scope_page(&page(BOUNTY)).unwrap().program;
        assert_eq!(p.slug, "demo");
        assert_eq!(p.max_payout_usd, Some(2_000_000));
        assert!(p.requires_kyc);
        assert_eq!(p.impacts.len(), 2);
        assert_eq!(p.impacts[0].severity, Severity::Critical);
        assert_eq!(p.impact("I-2").unwrap().text, "Theft of unclaimed yield");
        assert_eq!(p.assets.len(), 2);
        assert_eq!(p.assets[0].chain_id, Some(1));
        assert_eq!(
            p.assets[0].address.as_deref(),
            Some("0xae7ab96520de3a18e5e111b5eaab095312d7fe84")
        );
        assert_eq!(p.assets[0].name, "");
        assert_eq!(p.assets[1].address, None);
        assert_eq!(p.assets[1].name, "Vault");
        assert_eq!(
            p.out_of_scope.program_specific,
            vec!["Best practice critiques", "Frontrunning"]
        );
    }

    #[test]
    fn names_come_from_source_blobs_only() {
        let b =
            github_blob("https://github.com/Org/Repo/blob/d8a522d6/src/shares/Shares.sol").unwrap();
        assert_eq!(b.repo, "org/repo");
        assert_eq!(b.commit, "d8a522d6");
        assert_eq!(b.path, "src/shares/Shares.sol");
        assert_eq!(contract_name(&b.path).as_deref(), Some("Shares"));
        assert_eq!(contract_name("contracts/Pool.vy").as_deref(), Some("Pool"));
        assert_eq!(contract_name("sources/coin.move").as_deref(), Some("coin"));
        assert_eq!(contract_name("README.md"), None);
        assert!(github_blob("https://github.com/org/repo/tree/main/src").is_none());
        assert!(github_blob("https://immunefi.com/").is_none());
    }

    #[test]
    fn address_assets_are_named_from_their_description() {
        assert_eq!(
            description_name("IssuanceHours (BSC)").as_deref(),
            Some("IssuanceHours")
        );
        assert_eq!(
            description_name("PortfolioVault").as_deref(),
            Some("PortfolioVault")
        );
        assert_eq!(description_name("ERC-20 token"), None);
        assert_eq!(description_name(""), None);
    }

    #[test]
    fn target_repo_url_forms() {
        for url in [
            "https://github.com/enzymefinance/protocol-onyx.git",
            "https://github.com/enzymefinance/protocol-onyx/",
            "git@github.com:enzymefinance/protocol-onyx.git",
        ] {
            assert_eq!(
                github_repo(url).as_deref(),
                Some("enzymefinance/protocol-onyx")
            );
        }
        assert_eq!(github_repo("https://gitlab.com/a/b"), None);
    }

    /// Asset URLs from the Enzyme Onyx scope page as imported on 2026-11.
    #[test]
    fn onyx_assets() {
        let bounty: Value =
            serde_json::from_str(include_str!("../testdata/onyx-bounty.json")).unwrap();
        let payload = format!("{}", serde_json::json!({ "bounty": bounty }));
        let imported = parse_scope_page(&page(&payload)).unwrap();
        let assets = &imported.program.assets;
        assert_eq!(assets.len(), 45);
        assert!(assets.iter().all(|a| !a.name.is_empty()));
        assert!(assets.iter().any(|a| a.name == "Shares"));
        assert_eq!(imported.skipped, vec!["https://immunefi.com/"]);

        let pins = pinned_commits(assets);
        assert_eq!(pins.len(), 9);
        assert_eq!(pins.iter().map(|(_, n)| n).sum::<usize>(), 45);
        assert!(pins[0].0.starts_with("7b48d243"));
    }

    #[test]
    fn wrapped_bullet_lines_are_joined() {
        let md = "- One.\n- Two is described in t\nerms of impact.\n  - nested, dropped\n- Three";
        assert_eq!(
            bullets(md),
            vec!["One", "Two is described in t erms of impact", "Three"]
        );
    }

    #[test]
    fn reference_must_start_a_row() {
        let payload = "x51a:T3,bad\n1a:T4,good";
        assert_eq!(resolve_ref(payload, "$1a").as_deref(), Some("good"));
    }

    #[test]
    fn written_toml_loads_back() {
        let p = parse_scope_page(&page(BOUNTY)).unwrap().program;
        let text = to_toml(&p).unwrap();
        #[derive(serde::Deserialize)]
        struct Wrap {
            program: ProgramConfig,
        }
        let back: Wrap = toml::from_str(&text).unwrap();
        assert_eq!(back.program.impacts.len(), 2);
        assert_eq!(back.program.assets[0].chain_id, Some(1));
    }

    #[test]
    fn diff_reports_added_and_removed() {
        let old = serde_json::json!({"assets":[{"url":"a"},{"url":"b"}],"impacts":[{"title":"x"}]});
        let new = serde_json::json!({"assets":[{"url":"b"},{"url":"c"}],"impacts":[{"title":"x"}]});
        assert_eq!(diff(&old, &new), vec!["+ assets: c", "- assets: a"]);
    }

    /// The real Lido page, saved by hand: `BULWARK_SCOPE_PAGE=lido.html cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_page_parses() {
        let path = std::env::var("BULWARK_SCOPE_PAGE").unwrap();
        let html = std::fs::read_to_string(path).unwrap();
        let p = parse_scope_page(&html).unwrap().program;
        eprintln!(
            "{}: {} impacts, {} assets, {} exclusions",
            p.slug,
            p.impacts.len(),
            p.assets.len(),
            p.out_of_scope.program_specific.len()
        );
        assert!(!p.impacts.is_empty());
        assert!(!p.assets.is_empty());
    }
}

#[cfg(test)]
mod reward_tests {
    use super::*;

    #[test]
    fn rewards_read_ceilings_and_the_critical_percentage() {
        let b: Value = serde_json::from_str(r#"{"programRewards": [
            {"assetType": "smart_contract", "maxReward": 1000000, "rewardCalculationPercentage": 10, "severity": "critical"},
            {"assetType": "smart_contract", "maxReward": 50000, "severity": "high"},
            {"assetType": "smart_contract", "fixedReward": 10000, "severity": "medium"},
            {"assetType": "websites_and_applications", "fixedReward": 5, "severity": "low"}]}"#).unwrap();
        let (t, pct) = rewards(&b);
        assert_eq!(t["Critical"], 1_000_000);
        assert_eq!(t["High"], 50_000);
        assert_eq!(t["Medium"], 10_000);
        assert!(!t.contains_key("Low"));
        assert_eq!(pct, Some(10.0));
    }
}
