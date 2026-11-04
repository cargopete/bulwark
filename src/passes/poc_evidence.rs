//! Grading a generated PoC from what it measured, not from whether `forge test` went green.
//!
//! A PoC demonstrates only when a test passes, logs a nonzero `bulwark:attacker_gain` or
//! `bulwark:victim_loss`, and trips no cheatcode lint. On a fork of deployed contracts that is
//! `compiles_and_demonstrates`; on a local deployment it is `demonstrated_local`.

use serde::Serialize;
use std::collections::BTreeSet;

pub const GAIN_MARKER: &str = "bulwark:attacker_gain";
pub const LOSS_MARKER: &str = "bulwark:victim_loss";
pub const NOT_REPRODUCIBLE_MARKER: &str = "BULWARK_NOT_REPRODUCIBLE";
pub const REQUIRES_FORK_MARKER: &str = "REQUIRES_FORK";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PocMode {
    Fork { rpc_env: String, block: u64 },
    Local,
}

impl PocMode {
    pub fn label(&self) -> &'static str {
        match self {
            PocMode::Fork { .. } => "fork",
            PocMode::Local => "local",
        }
    }

    /// Extra `forge test` arguments, and the same with the RPC URL left as `$ENV` for logs.
    pub fn forge_args(&self, rpc_url: Option<&str>) -> (Vec<String>, Vec<String>) {
        match self {
            PocMode::Local => (vec![], vec![]),
            PocMode::Fork { rpc_env, block } => (
                vec![
                    "--fork-url".into(),
                    rpc_url.unwrap_or_default().into(),
                    "--fork-block-number".into(),
                    block.to_string(),
                ],
                vec![
                    "--fork-url".into(),
                    format!("${rpc_env}"),
                    "--fork-block-number".into(),
                    block.to_string(),
                ],
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestOutcome {
    pub name: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Decimal strings: uint256 does not fit any Rust integer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attacker_gain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub victim_loss: Option<String>,
}

impl TestOutcome {
    pub fn has_impact(&self) -> bool {
        [&self.attacker_gain, &self.victim_loss]
            .iter()
            .any(|v| v.as_deref().is_some_and(is_nonzero_decimal))
    }
}

fn is_nonzero_decimal(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.bytes().any(|b| b != b'0')
}

/// Parse `forge test --json -vv` stdout. `None` means forge printed no JSON (no tests matched,
/// or it crashed); `-vv` is required or `decoded_logs` comes back empty.
pub fn parse_forge_json(stdout: &str) -> Option<Vec<TestOutcome>> {
    let start = stdout.find('{')?;
    let root: serde_json::Value = serde_json::from_str(stdout[start..].trim_end()).ok()?;
    let mut out = Vec::new();
    for suite in root.as_object()?.values() {
        let Some(results) = suite.get("test_results").and_then(|r| r.as_object()) else {
            continue;
        };
        for (name, r) in results {
            let logs: Vec<&str> = r
                .get("decoded_logs")
                .and_then(|l| l.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            out.push(TestOutcome {
                name: name.clone(),
                passed: r.get("status").and_then(|s| s.as_str()) == Some("Success"),
                reason: r.get("reason").and_then(|s| s.as_str()).map(String::from),
                attacker_gain: marker_value(&logs, GAIN_MARKER),
                victim_loss: marker_value(&logs, LOSS_MARKER),
            });
        }
    }
    Some(out)
}

fn marker_value(logs: &[&str], marker: &str) -> Option<String> {
    logs.iter().rev().find_map(|l| {
        let rest = l.trim().strip_prefix(marker)?;
        let v = rest.trim_start_matches([':', ' ', '\t']).trim();
        Some(v.to_string())
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LintFlag {
    pub line: usize,
    pub cheat: String,
    pub detail: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct LintReport {
    pub flags: Vec<LintFlag>,
    /// `deal` calls funding fresh actors: the attack's declared capital.
    pub capital: Vec<String>,
}

/// Rewrites protocol state outright, so any use fakes a precondition.
const STATE_CHEATS: &[&str] = &[
    "vm.store",
    "vm.etch",
    "vm.mockCall",
    "vm.mockCallRevert",
    "vm.mockFunction",
    "deployCodeTo",
];

/// Cheats that act as an address; the address must be a fresh actor, or it may hold a role.
const ACTOR_CHEATS: &[(&str, usize)] = &[
    ("vm.prank", 0),
    ("vm.startPrank", 0),
    ("changePrank", 0),
    ("hoax", 0),
    ("startHoax", 0),
    ("vm.deal", 0),
];

/// Static lint of a generated test. Fresh actors are `makeAddr`/`vm.addr` results and
/// `address(this)`; pranking or funding anything else could be borrowing a privileged role.
pub fn lint(source: &str) -> LintReport {
    let code = strip_comments(source);
    let fresh = fresh_actors(&code);
    let is_fresh = |arg: &str| {
        let a = arg.trim();
        a == "address(this)" || fresh.contains(a)
    };
    let mut report = LintReport::default();

    for (pos, name) in calls(&code) {
        let line = line_of(&code, pos);
        let args = call_args(&code, pos + name.len());
        if STATE_CHEATS.contains(&name) {
            report.flags.push(LintFlag {
                line,
                cheat: name.into(),
                detail: "rewrites contract state or code directly".into(),
            });
            continue;
        }
        if let Some(&(_, idx)) = ACTOR_CHEATS.iter().find(|(n, _)| *n == name) {
            let who = args.get(idx).map(String::as_str).unwrap_or("");
            if !is_fresh(who) {
                report.flags.push(LintFlag {
                    line,
                    cheat: name.into(),
                    detail: format!("acts as `{who}`, which is not a fresh makeAddr actor"),
                });
            } else if name == "vm.deal" || name.ends_with("oax") {
                report.capital.push(format!("{name}({})", args.join(", ")));
            }
            continue;
        }
        if name == "deal" {
            // deal(who, amount) for ETH, deal(token, who, amount[, adjust]) for ERC20s.
            let who = if args.len() == 2 {
                args.first()
            } else {
                args.get(1)
            };
            let who = who.map(String::as_str).unwrap_or("");
            if is_fresh(who) {
                report.capital.push(format!("deal({})", args.join(", ")));
            } else {
                report.flags.push(LintFlag {
                    line,
                    cheat: "deal".into(),
                    detail: format!(
                        "mints balance to `{who}`, which is not a fresh makeAddr actor"
                    ),
                });
            }
        }
    }
    report
}

fn calls(code: &str) -> Vec<(usize, &'static str)> {
    let names: Vec<&'static str> = STATE_CHEATS
        .iter()
        .copied()
        .chain(ACTOR_CHEATS.iter().map(|(n, _)| *n))
        .chain(std::iter::once("deal"))
        .collect();
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    for name in names {
        let mut from = 0;
        while let Some(off) = code[from..].find(name) {
            let pos = from + off;
            from = pos + name.len();
            let before_ok = pos == 0 || {
                let b = bytes[pos - 1];
                !(b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
            };
            let after = code[from..].trim_start();
            if before_ok && after.starts_with('(') {
                found.push((pos, name));
            }
        }
    }
    found.sort();
    found
}

/// Top-level comma-separated arguments of the call whose name ends at `after_name`.
fn call_args(code: &str, after_name: usize) -> Vec<String> {
    let rest = &code[after_name..];
    let Some(open) = rest.find('(') else {
        return vec![];
    };
    let mut depth = 0usize;
    let mut args = Vec::new();
    let mut cur = String::new();
    for c in rest[open + 1..].chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' if depth == 0 => break,
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => args.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        args.push(cur.trim().to_string());
    }
    args
}

fn fresh_actors(code: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for ctor in ["makeAddr(", "vm.addr("] {
        let mut from = 0;
        while let Some(off) = code[from..].find(ctor) {
            let pos = from + off;
            from = pos + ctor.len();
            let lhs = code[..pos].trim_end();
            let Some(lhs) = lhs.strip_suffix('=') else {
                continue;
            };
            if lhs.ends_with(['=', '!', '<', '>']) {
                continue;
            }
            let ident: String = lhs
                .trim_end()
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !ident.is_empty() {
                out.insert(ident);
            }
        }
    }
    out
}

/// Blank out comments and string literals, keeping newlines so line numbers survive.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            '"' | '\'' => {
                out.push(c);
                let mut escaped = false;
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                    }
                    if !escaped && n == c {
                        break;
                    }
                    escaped = !escaped && n == '\\';
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn line_of(code: &str, pos: usize) -> usize {
    code[..pos].matches('\n').count() + 1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grade {
    pub status: &'static str,
    pub reasons: Vec<String>,
    /// Another generation attempt could change the verdict.
    pub retryable: bool,
}

pub fn grade(
    source: &str,
    tests: Option<&[TestOutcome]>,
    lint: &LintReport,
    mode: &PocMode,
) -> Grade {
    let inconclusive = |reasons: Vec<String>, retryable| Grade {
        status: "compiles_but_inconclusive",
        reasons,
        retryable,
    };

    if let Some(line) = source.lines().find(|l| l.contains(NOT_REPRODUCIBLE_MARKER)) {
        return Grade {
            status: "infeasible_conditions",
            reasons: vec![line.trim().trim_start_matches("//").trim().to_string()],
            retryable: false,
        };
    }
    if *mode == PocMode::Local && source.contains(REQUIRES_FORK_MARKER) {
        return Grade {
            status: "requires_mainnet_simulation",
            reasons: vec!["PoC needs live state and no fork is configured".into()],
            retryable: false,
        };
    }

    let tests = match tests {
        Some(t) if !t.is_empty() => t,
        _ => return inconclusive(vec!["no test ran".into()], true),
    };
    let Some(proving) = tests.iter().find(|t| t.passed && t.has_impact()) else {
        let reasons = if tests.iter().any(|t| t.passed) {
            vec![format!(
                "test passed without logging a nonzero `{GAIN_MARKER}` or `{LOSS_MARKER}`"
            )]
        } else {
            tests
                .iter()
                .map(|t| {
                    format!(
                        "{} failed: {}",
                        t.name,
                        t.reason.as_deref().unwrap_or("no reason given")
                    )
                })
                .collect()
        };
        return inconclusive(reasons, true);
    };

    if !lint.flags.is_empty() {
        let reasons = lint
            .flags
            .iter()
            .map(|f| format!("line {}: {} {}", f.line, f.cheat, f.detail))
            .collect();
        return inconclusive(reasons, true);
    }
    match mode {
        PocMode::Fork { .. } => Grade {
            status: "compiles_and_demonstrates",
            reasons: vec![format!("{} measured impact", proving.name)],
            retryable: false,
        },
        PocMode::Local => Grade {
            status: "demonstrated_local",
            reasons: vec![format!(
                "{} measured impact on a local deployment, not a fork of the deployed contracts",
                proving.name
            )],
            retryable: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORGE_JSON: &str = r#"{"test/P.t.sol:P":{"duration":"1ms","test_results":{
        "test_fails()":{"status":"Failure","reason":"nope: 3 != 5","counterexample":null,"logs":[],"decoded_logs":[]},
        "test_gain()":{"status":"Success","reason":null,"counterexample":null,"logs":[],
            "decoded_logs":["bulwark:attacker_gain 100","bulwark:victim_loss 0"]}},"warnings":[]}}"#;

    fn fork() -> PocMode {
        PocMode::Fork {
            rpc_env: "RPC".into(),
            block: 1,
        }
    }

    fn passing(gain: &str) -> Vec<TestOutcome> {
        vec![TestOutcome {
            name: "test_x()".into(),
            passed: true,
            reason: None,
            attacker_gain: Some(gain.into()),
            victim_loss: None,
        }]
    }

    #[test]
    fn parses_status_reason_and_markers() {
        let mut t = parse_forge_json(FORGE_JSON).unwrap();
        t.sort_by(|a, b| a.name.cmp(&b.name));
        assert!(!t[0].passed);
        assert_eq!(t[0].reason.as_deref(), Some("nope: 3 != 5"));
        assert!(t[1].passed);
        assert_eq!(t[1].attacker_gain.as_deref(), Some("100"));
        assert!(t[1].has_impact());
    }

    #[test]
    fn no_json_is_none() {
        assert!(parse_forge_json("Warning: No tests found in project!").is_none());
    }

    #[test]
    fn zero_or_garbage_is_not_impact() {
        assert!(!passing("0")[0].has_impact());
        assert!(!passing("000")[0].has_impact());
        assert!(!passing("-5")[0].has_impact());
        assert!(passing("1000000000000000000000000000000000000000000")[0].has_impact());
    }

    #[test]
    fn pass_without_impact_is_inconclusive() {
        let mut t = passing("0");
        t[0].attacker_gain = None;
        let g = grade("", Some(&t), &LintReport::default(), &fork());
        assert_eq!(g.status, "compiles_but_inconclusive");
        assert!(g.retryable);
    }

    #[test]
    fn clean_fork_pass_with_impact_demonstrates() {
        let g = grade("", Some(&passing("5")), &LintReport::default(), &fork());
        assert_eq!(g.status, "compiles_and_demonstrates");
    }

    #[test]
    fn clean_local_pass_with_impact_is_demonstrated_local() {
        let g = grade(
            "",
            Some(&passing("90909090909")),
            &LintReport::default(),
            &PocMode::Local,
        );
        assert_eq!(g.status, "demonstrated_local");
        assert!(g.reasons[0].contains("local deployment"));
        assert!(!g.retryable);
    }

    #[test]
    fn local_victim_loss_alone_is_demonstrated_local() {
        let mut t = passing("0");
        t[0].victim_loss = Some("7".into());
        let g = grade("", Some(&t), &LintReport::default(), &PocMode::Local);
        assert_eq!(g.status, "demonstrated_local");
    }

    #[test]
    fn local_pass_without_impact_stays_inconclusive() {
        let g = grade(
            "",
            Some(&passing("0")),
            &LintReport::default(),
            &PocMode::Local,
        );
        assert_eq!(g.status, "compiles_but_inconclusive");
        assert!(g.retryable);
    }

    #[test]
    fn local_failure_with_impact_logged_stays_inconclusive() {
        let mut t = passing("5");
        t[0].passed = false;
        let g = grade("", Some(&t), &LintReport::default(), &PocMode::Local);
        assert_eq!(g.status, "compiles_but_inconclusive");
    }

    #[test]
    fn lint_flags_block_local_demonstration() {
        let lint = lint("function t() public { vm.prank(governor); }");
        let g = grade("", Some(&passing("5")), &lint, &PocMode::Local);
        assert_eq!(g.status, "compiles_but_inconclusive");
        assert!(g.reasons.iter().any(|r| r.contains("vm.prank")));
        assert!(g.retryable);
    }

    #[test]
    fn lint_flags_block_demonstration() {
        let lint = lint("function t() public { vm.store(address(1), bytes32(0), bytes32(0)); }");
        let g = grade("", Some(&passing("5")), &lint, &fork());
        assert_eq!(g.status, "compiles_but_inconclusive");
        assert!(g.reasons[0].contains("vm.store"));
        assert!(g.retryable);
    }

    #[test]
    fn failed_tests_report_their_reasons() {
        let t = parse_forge_json(FORGE_JSON).unwrap();
        let only_fail: Vec<_> = t.into_iter().filter(|t| !t.passed).collect();
        let g = grade("", Some(&only_fail), &LintReport::default(), &fork());
        assert!(g.reasons[0].contains("nope: 3 != 5"));
    }

    #[test]
    fn not_reproducible_marker_wins() {
        let src = "// BULWARK_NOT_REPRODUCIBLE: require on line 40 blocks it\ncontract P {}";
        let g = grade(src, Some(&passing("5")), &LintReport::default(), &fork());
        assert_eq!(g.status, "infeasible_conditions");
        assert!(g.reasons[0].contains("line 40"));
    }

    #[test]
    fn requires_fork_only_matters_locally() {
        let src = "// REQUIRES_FORK\n";
        let g = grade(src, None, &LintReport::default(), &PocMode::Local);
        assert_eq!(g.status, "requires_mainnet_simulation");
        let g = grade(src, Some(&passing("5")), &LintReport::default(), &fork());
        assert_eq!(g.status, "compiles_and_demonstrates");
    }

    const POC: &str = r#"
        contract P is Test {
            address attacker = makeAddr("attacker");
            address victim;
            function setUp() public {
                victim = makeAddr("victim");
                deal(address(token), attacker, 1e18);
                deal(address(token), address(staking), 1e30);
                vm.deal(attacker, 1 ether);
            }
            function test_x() public {
                vm.prank(victim);
                token.approve(address(staking), 1);
                vm.startPrank(governor);
                // vm.store(address(staking), 0, 0) in a comment is ignored
                string memory s = "vm.etch(x, y)";
                vm.prank(staking.owner());
                vm.mockCall(address(oracle), abi.encode(1), abi.encode(2));
            }
        }"#;

    #[test]
    fn lint_separates_capital_from_flags() {
        let r = lint(POC);
        let cheats: Vec<(&str, usize)> =
            r.flags.iter().map(|f| (f.cheat.as_str(), f.line)).collect();
        assert_eq!(
            cheats,
            vec![
                ("deal", 8),
                ("vm.startPrank", 14),
                ("vm.prank", 17),
                ("vm.mockCall", 18)
            ]
        );
        assert_eq!(
            r.capital,
            vec![
                "deal(address(token), attacker, 1e18)".to_string(),
                "vm.deal(attacker, 1 ether)".to_string()
            ]
        );
    }

    #[test]
    fn eth_deal_two_args_checks_first() {
        assert_eq!(lint("deal(governor, 1 ether);").flags.len(), 1);
        assert_eq!(lint("deal(address(this), 1 ether);").flags.len(), 0);
    }

    #[test]
    fn comparison_is_not_assignment() {
        let r = lint("if (x == makeAddr(\"a\")) {} vm.prank(x);");
        assert_eq!(r.flags.len(), 1);
    }

    #[test]
    fn fork_args_hide_the_url_in_the_logged_form() {
        let (real, shown) = fork().forge_args(Some("https://secret"));
        assert!(real.contains(&"https://secret".to_string()));
        assert!(shown.contains(&"$RPC".to_string()));
        assert!(!shown.iter().any(|a| a.contains("secret")));
    }
}
