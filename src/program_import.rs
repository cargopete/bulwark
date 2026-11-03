//! `bulwark program import <url>`: an Immunefi scope page into `program.toml`.
//!
//! The page is a Next.js app-router render; the program sits in the RSC payload as a
//! `{"bounty": {...}}` object, with long text fields as `$<id>` references to `T` rows.

use crate::error::{BulwarkError, Result};
use crate::findings::Severity;
use crate::program::{Asset, Impact, OutOfScope, ProgramConfig};
use serde_json::Value;
use std::collections::BTreeSet;
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

pub struct Imported {
    pub program: ProgramConfig,
    /// The raw bounty object, kept as the snapshot the next import diffs against.
    pub snapshot: Value,
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

    let assets = bounty
        .get("assets")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter(smart)
        .filter_map(|a| {
            let url = a.get("url")?.as_str()?.to_string();
            let (chain_id, address) = match explorer_address(&url) {
                Some((c, a)) => (c, Some(a)),
                None => (None, None),
            };
            Some(Asset {
                name: String::new(),
                address,
                chain_id,
                url: Some(url),
                kind: "smart_contract".into(),
            })
        })
        .collect();

    let program_specific = text_field(&payload, bounty, "customOutOfScopeInformation")
        .map(|t| bullets(&t))
        .unwrap_or_default();

    let program = ProgramConfig {
        platform: "immunefi".into(),
        slug: bounty
            .get("slug")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .into(),
        severity_system: "v2.3".into(),
        max_payout_usd: bounty.get("maxBounty").and_then(|m| m.as_u64()),
        reward_formula: "fixed".into(),
        reward_pct: None,
        requires_poc: true,
        requires_kyc: bounty.get("kyc").and_then(|k| k.as_bool()).unwrap_or(false),
        assets,
        impacts,
        out_of_scope: OutOfScope { program_specific },
    };
    Ok(Imported {
        program,
        snapshot: bounty.clone(),
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
        "# Imported by `bulwark program import`. Asset names are empty: fill each `name`\n\
         # with the contract name as it appears in source, or assets do not filter findings.\n\n{body}"
    ))
}

pub async fn run(url: &str, project_dir: &Path) -> Result<()> {
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
    Ok(())
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
        assert_eq!(p.assets[1].address, None);
        assert_eq!(
            p.out_of_scope.program_specific,
            vec!["Best practice critiques", "Frontrunning"]
        );
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
