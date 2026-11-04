//! Per-file scope pins: a bounty scope can pin each file at its own commit, while the run audits
//! one `target.commit`. A finding in code that changed after its file's pin may not be deployed.

use crate::pipeline::pass::PipelineContext;
use crate::program::Asset;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Lines either side of a finding's range that still count as the affected region.
const CONTEXT_LINES: u32 = 3;
const MAX_DIFF_LINES: usize = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub commit: String,
    pub path: String,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Matches,
    Differs(String),
    NoPin(String),
}

/// Tags each finding not yet tagged with `scope_pin`, and `scope_pin_diff` where it differs.
pub fn tag(ctx: &PipelineContext, findings: &mut [Value]) {
    let assets = ctx
        .config
        .program
        .as_ref()
        .map(|p| p.assets.as_slice())
        .unwrap_or_default();
    let pins = pins(assets, &ctx.config.target.repo);
    let audited = ctx.config.target.commit.as_deref().unwrap_or("HEAD");
    tag_with(&ctx.audit_dir, audited, &pins, findings);
}

fn tag_with(repo: &Path, audited: &str, pins: &[Pin], findings: &mut [Value]) {
    let mut available: HashMap<String, bool> = HashMap::new();
    for f in findings.iter_mut() {
        if f.get("scope_pin").is_some() {
            continue;
        }
        let outcome = match resolve(pins, f) {
            Ok(pin) => {
                let have = *available
                    .entry(pin.commit.clone())
                    .or_insert_with(|| ensure_commit(repo, &pin.commit));
                if have {
                    f["scope_pin_commit"] = json!(pin.commit);
                    check(repo, audited, pin, &lines(f))
                } else {
                    Outcome::NoPin(format!("pin {} could not be fetched", short(&pin.commit)))
                }
            }
            Err(why) => Outcome::NoPin(why),
        };
        match outcome {
            Outcome::Matches => f["scope_pin"] = json!("matches_pin"),
            Outcome::Differs(diff) => {
                f["scope_pin"] = json!("differs_from_pin");
                f["scope_pin_diff"] = json!(diff);
            }
            Outcome::NoPin(why) => {
                f["scope_pin"] = json!("no_pin");
                f["scope_pin_note"] = json!(why);
            }
        }
    }
}

/// File pins from asset URLs of the form `github.com/<org>/<repo>/blob/<sha>/<path>`.
/// Pins into a repo other than the target's are dropped.
pub fn pins(assets: &[Asset], target_repo: &str) -> Vec<Pin> {
    let target = github_slug(target_repo);
    assets
        .iter()
        .filter_map(|a| {
            let url = a.url.as_deref()?;
            let (slug, rest) = url.split_once("/blob/")?;
            if let (Some(t), Some(s)) = (&target, github_slug(slug)) {
                if *t != s {
                    return None;
                }
            }
            let (commit, path) = rest.split_once('/')?;
            let path = path.split(['#', '?']).next().unwrap_or(path);
            (!commit.is_empty() && !path.is_empty()).then(|| Pin {
                commit: commit.to_string(),
                path: path.to_string(),
            })
        })
        .collect()
}

fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .split_once("github.com/")
        .or_else(|| url.split_once("github.com:"))?
        .1;
    let mut parts = rest.split('/');
    let org = parts.next()?;
    let repo = parts.next()?.trim_end_matches(".git");
    Some(format!("{org}/{repo}").to_lowercase())
}

/// The pin for the finding's file, matched by path where the finding gives one, else by file stem.
pub fn resolve<'a>(pins: &'a [Pin], finding: &Value) -> Result<&'a Pin, String> {
    let candidates: Vec<&str> = ["contract", "file", "path", "file_path", "location"]
        .iter()
        .filter_map(|k| finding.get(*k).and_then(Value::as_str))
        .map(|s| s.split([':', '#']).next().unwrap_or(s).trim())
        .filter(|s| !s.is_empty())
        .collect();
    if pins.is_empty() {
        return Err("no asset pins a file".into());
    }
    let mut hits: Vec<&Pin> = Vec::new();
    // A path is more specific than a stem, so stems are only tried when no path matched.
    for by_path in [true, false] {
        for c in &candidates {
            if c.contains('/') != by_path {
                continue;
            }
            for p in pins {
                let hit = if by_path {
                    let c = c.trim_start_matches("./");
                    ends_with_path(&p.path, c) || ends_with_path(c, &p.path)
                } else {
                    stem(&p.path).eq_ignore_ascii_case(stem(c))
                };
                if hit && !hits.iter().any(|h| h.path == p.path) {
                    hits.push(p);
                }
            }
        }
        if !hits.is_empty() {
            break;
        }
    }
    match hits.as_slice() {
        [] => Err("no asset pins this file".into()),
        [one] => Ok(one),
        many => Err(format!(
            "ambiguous: {}",
            many.iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn ends_with_path(full: &str, tail: &str) -> bool {
    full == tail || full.ends_with(&format!("/{tail}"))
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split_once('.').map_or(name, |(s, _)| s)
}

fn lines(finding: &Value) -> Vec<u32> {
    finding
        .get("lines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_u64())
                .map(|n| n as u32)
                .collect()
        })
        .unwrap_or_default()
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The clone may be shallow, so a pin older than `target.commit` is often missing locally.
fn ensure_commit(repo: &Path, commit: &str) -> bool {
    let obj = format!("{commit}^{{commit}}");
    if git(repo, &["cat-file", "-e", &obj]).is_some() {
        return true;
    }
    let shallow =
        git(repo, &["rev-parse", "--is-shallow-repository"]).is_some_and(|s| s.trim() == "true");
    let mut args = vec!["fetch", "-q"];
    if shallow {
        args.push("--depth=1");
    }
    args.extend(["origin", commit]);
    git(repo, &args).is_some() && git(repo, &["cat-file", "-e", &obj]).is_some()
}

/// Compares the affected lines of `pin.path` at `audited` against the same file at the pin.
pub fn check(repo: &Path, audited: &str, pin: &Pin, lines: &[u32]) -> Outcome {
    let at = |rev: &str| format!("{rev}:{}", pin.path);
    if git(repo, &["cat-file", "-e", &at(&pin.commit)]).is_none() {
        return Outcome::NoPin(format!("{} absent at pin {}", pin.path, short(&pin.commit)));
    }
    if git(repo, &["cat-file", "-e", &at(audited)]).is_none() {
        return Outcome::Differs(format!("{} absent at the audited commit", pin.path));
    }
    let Some(diff) = git(
        repo,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-U3",
            &pin.commit,
            audited,
            "--",
            &pin.path,
        ],
    ) else {
        return Outcome::NoPin("git diff failed".into());
    };
    let region = match (lines.iter().min(), lines.iter().max()) {
        (Some(lo), Some(hi)) => lo.saturating_sub(CONTEXT_LINES)..=hi + CONTEXT_LINES,
        _ => 0..=u32::MAX,
    };
    let hunks: Vec<&str> = hunks(&diff)
        .into_iter()
        .filter(|h| changed_lines(h).iter().any(|n| region.contains(n)))
        .collect();
    if hunks.is_empty() {
        return Outcome::Matches;
    }
    let mut out = vec![
        format!("--- {} @ {} (pin)", pin.path, short(&pin.commit)),
        format!("+++ {} @ {} (audited)", pin.path, short(audited)),
    ];
    out.extend(hunks.iter().flat_map(|h| h.lines()).map(String::from));
    if out.len() > MAX_DIFF_LINES {
        let more = out.len() - MAX_DIFF_LINES;
        out.truncate(MAX_DIFF_LINES);
        out.push(format!("... {more} more lines"));
    }
    Outcome::Differs(out.join("\n"))
}

fn hunks(diff: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = diff.match_indices("\n@@ ").map(|(i, _)| i + 1).collect();
    if diff.starts_with("@@ ") {
        starts.insert(0, 0);
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &s)| &diff[s..starts.get(i + 1).copied().unwrap_or(diff.len())])
        .collect()
}

/// Lines of the audited file a hunk touches; a deletion counts against the line that follows it.
fn changed_lines(hunk: &str) -> Vec<u32> {
    let mut body = hunk.lines();
    let Some(new) = body
        .next()
        .and_then(|h| h.split(" +").nth(1))
        .and_then(|s| s.split(' ').next())
    else {
        return vec![];
    };
    let (start, len) = new.split_once(',').unwrap_or((new, "1"));
    let (Ok(start), Ok(len)) = (start.parse::<u32>(), len.parse::<u32>()) else {
        return vec![];
    };
    let mut n = if len == 0 { start + 1 } else { start };
    let mut out = Vec::new();
    for l in body {
        match l.as_bytes().first() {
            Some(b'+') => {
                out.push(n);
                n += 1;
            }
            Some(b'-') => out.push(n),
            Some(b' ') => n += 1,
            _ => {}
        }
    }
    out
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(10)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(url: &str) -> Asset {
        Asset {
            name: String::new(),
            address: None,
            chain_id: None,
            url: Some(url.into()),
            kind: "smart_contract".into(),
        }
    }

    fn pin(commit: &str, path: &str) -> Pin {
        Pin {
            commit: commit.into(),
            path: path.into(),
        }
    }

    #[test]
    fn pins_come_from_blob_urls_in_the_target_repo() {
        let assets = [
            asset("https://github.com/enzyme/onyx/blob/abc123/src/shares/ShareS.sol"),
            asset("https://github.com/enzyme/onyx/blob/def456/src/Vault.sol#L10"),
            asset("https://github.com/other/thing/blob/abc123/src/X.sol"),
            asset("https://github.com/enzyme/onyx/tree/abc123/src"),
            asset("https://etherscan.io/address/0x1"),
        ];
        let got = pins(&assets, "https://github.com/Enzyme/onyx.git");
        assert_eq!(
            got,
            vec![
                pin("abc123", "src/shares/ShareS.sol"),
                pin("def456", "src/Vault.sol")
            ]
        );
    }

    #[test]
    fn contract_resolves_by_stem_or_path() {
        let pins = [
            pin("a", "src/shares/ShareS.sol"),
            pin("b", "src/Vault.sol"),
            pin("c", "src/periphery/Vault.sol"),
        ];
        let r = |f: Value| resolve(&pins, &f).map(|p| p.commit.clone());
        assert_eq!(r(json!({"contract": "ShareS.sol"})), Ok("a".into()));
        assert_eq!(r(json!({"contract": "ShareS"})), Ok("a".into()));
        assert_eq!(
            r(json!({"contract": "shares/ShareS.sol:42"})),
            Ok("a".into())
        );
        assert_eq!(
            r(json!({"contract": "Vault", "file": "src/periphery/Vault.sol"})),
            Ok("c".into())
        );
        assert!(
            r(json!({"contract": "Vault.sol"}))
                .unwrap_err()
                .starts_with("ambiguous")
        );
        assert_eq!(
            r(json!({"contract": "Router.sol"})),
            Err("no asset pins this file".into())
        );
    }

    fn repo_with_change() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let body: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/ShareS.sol"), body.join("\n") + "\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "pin"]);
        let pinned = git(&["rev-parse", "HEAD"]);
        let mut changed = body.clone();
        changed[29] = "line 30 changed".into();
        std::fs::write(root.join("src/ShareS.sol"), changed.join("\n") + "\n").unwrap();
        git(&["commit", "-qam", "audited"]);
        let audited = git(&["rev-parse", "HEAD"]);
        (dir, pinned, audited)
    }

    #[test]
    fn finding_far_from_the_change_matches_its_pin() {
        let (dir, pinned, audited) = repo_with_change();
        let pins = [pin(&pinned, "src/ShareS.sol")];
        let mut fs = [json!({"contract": "ShareS.sol", "lines": [5, 9]})];
        tag_with(dir.path(), &audited, &pins, &mut fs);
        assert_eq!(fs[0]["scope_pin"], "matches_pin");
        assert_eq!(fs[0]["scope_pin_commit"], json!(pinned));
        assert!(fs[0].get("scope_pin_diff").is_none());
    }

    #[test]
    fn finding_on_changed_lines_differs_from_its_pin_with_the_diff() {
        let (dir, pinned, audited) = repo_with_change();
        let pins = [pin(&pinned, "src/ShareS.sol")];
        // 27 is within the context window of the change at 30.
        let mut fs = [
            json!({"contract": "ShareS.sol", "lines": [20, 27]}),
            json!({"contract": "ShareS.sol"}),
        ];
        tag_with(dir.path(), &audited, &pins, &mut fs);
        for f in &fs {
            assert_eq!(f["scope_pin"], "differs_from_pin");
            let diff = f["scope_pin_diff"].as_str().unwrap();
            assert!(diff.contains("\n-line 30\n+line 30 changed"), "{diff}");
            assert!(diff.starts_with("--- src/ShareS.sol @ "), "{diff}");
        }
    }

    #[test]
    fn unpinned_or_unfetchable_files_have_no_pin() {
        let (dir, _, audited) = repo_with_change();
        let pins = [pin(&"0".repeat(40), "src/ShareS.sol")];
        let mut fs = [
            json!({"contract": "Other.sol", "lines": [1]}),
            json!({"contract": "ShareS.sol", "lines": [1]}),
        ];
        tag_with(dir.path(), &audited, &pins, &mut fs);
        assert_eq!(fs[0]["scope_pin"], "no_pin");
        assert_eq!(fs[0]["scope_pin_note"], "no asset pins this file");
        assert_eq!(fs[1]["scope_pin"], "no_pin");
        assert!(
            fs[1]["scope_pin_note"]
                .as_str()
                .unwrap()
                .contains("could not be fetched")
        );
        let mut untouched = [json!({"contract": "ShareS.sol", "scope_pin": "matches_pin"})];
        tag_with(dir.path(), &audited, &[], &mut untouched);
        assert!(untouched[0].get("scope_pin_note").is_none());
    }
}
