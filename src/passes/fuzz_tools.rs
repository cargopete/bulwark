//! Config writers and output parsers for the three fuzzers Pass 4 runs on one suite.
//! Output shapes are taken from Forge 1.x, Medusa v1.5.1 and Echidna v2.3.3.

use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

/// The contract Medusa and Echidna deploy; its constructor runs the suite's setup.
pub const TESTER: &str = "CryticTester";
pub const PREFIX: &str = "invariant_";

#[derive(Debug, Clone)]
pub struct Fork {
    pub rpc: String,
    pub block: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    pub tool: &'static str,
    /// Property or method name, without arguments.
    pub invariant: String,
    pub reason: String,
    pub sequence: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolRun {
    pub tool: &'static str,
    pub passed: usize,
    pub failed: usize,
    pub failures: Vec<Failure>,
    /// Why the tool did not run, or ran nothing.
    pub note: Option<String>,
}

impl ToolRun {
    pub fn executed(&self) -> usize {
        self.passed + self.failed
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn bare_name(sig: &str) -> String {
    let s = sig.trim();
    let s = s.rsplit('.').next().unwrap_or(s);
    s.split('(').next().unwrap_or(s).trim().to_string()
}

// ── Forge ───────────────────────────────────────────────────────────

/// `forge test -vvv` output. A failure is a `[FAIL...]` line, an optional `[Sequence]` block of
/// `sender=... calldata=...` lines, and the test name, either on the FAIL line (older Forge)
/// or on the line that closes the block (`invariant_x() (runs: ...)`).
pub fn parse_forge(out: &str) -> ToolRun {
    let out = strip_ansi(out);
    let mut run = ToolRun {
        tool: "forge",
        ..Default::default()
    };
    let mut pending: Option<(String, Vec<String>)> = None;
    for line in out.lines() {
        let t = line.trim();
        if t.starts_with("[PASS]") {
            run.passed += 1;
            continue;
        }
        if let Some(rest) = t.strip_prefix("[FAIL") {
            run.failed += 1;
            let (reason, tail) = match rest.find(']') {
                Some(i) => (
                    rest[..i].trim_start_matches([':', '.']).trim(),
                    rest[i + 1..].trim(),
                ),
                None => (rest.trim(), ""),
            };
            let reason = reason.trim_start_matches("Reason:").trim().to_string();
            if tail.contains('(') {
                run.failures.push(Failure {
                    tool: "forge",
                    invariant: bare_name(tail.split(" (runs").next().unwrap_or(tail)),
                    reason,
                    sequence: vec![],
                });
            } else {
                pending = Some((reason, vec![]));
            }
            continue;
        }
        if let Some((_, seq)) = pending.as_mut() {
            if t.starts_with("sender=") || t.contains(" calldata=") {
                seq.push(t.to_string());
            } else if t.contains("(runs:") {
                let (reason, sequence) = pending.take().unwrap();
                run.failures.push(Failure {
                    tool: "forge",
                    invariant: bare_name(t),
                    reason,
                    sequence,
                });
            }
        }
    }
    run
}

// ── Medusa ──────────────────────────────────────────────────────────

pub fn medusa_config(corpus: &Path, timeout_s: u64, workers: usize, fork: Option<&Fork>) -> Value {
    json!({
        "fuzzing": {
            "workers": workers,
            "workerResetLimit": 50,
            "timeout": timeout_s,
            "testLimit": 0,
            "shrinkLimit": 5000,
            "callSequenceLength": 100,
            "corpusDirectory": corpus,
            "coverageEnabled": true,
            "coverageFormats": [],
            "targetContracts": [TESTER],
            "targetContractsBalances": ["0xffffffffffffffffffffffff"],
            "deployerAddress": "0x30000",
            "senderAddresses": ["0x10000", "0x20000", "0x30000"],
            "blockNumberDelayMax": 60480,
            "blockTimestampDelayMax": 604800,
            "transactionGasLimit": 1_000_000_000u64,
            "testing": {
                "stopOnFailedTest": false,
                "stopOnFailedContractMatching": false,
                "stopOnNoTests": true,
                "testAllContracts": false,
                "testViewMethods": true,
                "assertionTesting": {
                    "enabled": true,
                    "panicCodeConfig": {"failOnAssertion": true}
                },
                "propertyTesting": {"enabled": true, "testPrefixes": [PREFIX]},
                "optimizationTesting": {"enabled": false, "testPrefixes": ["optimize_"]}
            },
            "chainConfig": {
                "codeSizeCheckDisabled": true,
                "cheatCodes": {"cheatCodesEnabled": true, "enableFFI": false},
                "skipAccountChecks": true,
                "forkConfig": {
                    "forkModeEnabled": fork.is_some(),
                    "rpcUrl": fork.map(|f| f.rpc.as_str()).unwrap_or(""),
                    "rpcBlock": fork.map(|f| f.block).unwrap_or(1),
                    "poolSize": 20
                }
            }
        },
        "compilation": {
            "platform": "crytic-compile",
            "platformConfig": {
                "target": ".",
                "solcVersion": "",
                "exportDirectory": "",
                "args": ["--foundry-compile-all"]
            }
        },
        "logging": {"level": "info", "logDirectory": "", "noColor": true}
    })
}

/// A failed test prints `[FAILED] Property Test: C.f()` (or `Assertion Test:`), then
/// `[Call Sequence]` and one numbered line per call. The run ends with
/// `Test summary: N test(s) passed, M test(s) failed`.
pub fn parse_medusa(out: &str) -> ToolRun {
    let out = strip_ansi(out);
    let mut run = ToolRun {
        tool: "medusa",
        ..Default::default()
    };
    let mut seen_pass = std::collections::BTreeSet::new();
    let mut current: Option<Failure> = None;
    let mut in_seq = false;
    let flush = |cur: &mut Option<Failure>, run: &mut ToolRun| {
        if let Some(f) = cur.take() {
            if !run.failures.iter().any(|g| g.invariant == f.invariant) {
                run.failures.push(f);
            }
        }
    };
    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("[FAILED]") {
            flush(&mut current, &mut run);
            let (kind, sig) = rest.trim().split_once(':').unwrap_or(("", rest));
            current = Some(Failure {
                tool: "medusa",
                invariant: bare_name(sig),
                reason: format!("{} failed", kind.trim()),
                sequence: vec![],
            });
            in_seq = false;
        } else if let Some(rest) = t.strip_prefix("[PASSED]") {
            flush(&mut current, &mut run);
            seen_pass.insert(bare_name(rest.split_once(':').map(|x| x.1).unwrap_or(rest)));
            in_seq = false;
        } else if t.starts_with("[Call Sequence]") {
            in_seq = current.is_some();
        } else if t.starts_with('[') {
            in_seq = false;
        } else if in_seq && !t.is_empty() {
            if let Some(f) = current.as_mut() {
                f.sequence.push(t.to_string());
            }
        }
    }
    flush(&mut current, &mut run);
    run.failed = run.failures.len();
    run.passed = seen_pass
        .iter()
        .filter(|p| !run.failures.iter().any(|f| &f.invariant == *p))
        .count();
    if let Some(summary) = out.lines().rev().find(|l| l.contains("Test summary:")) {
        let nums: Vec<usize> = summary
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse().ok())
            .collect();
        if let [passed, failed, ..] = nums[..] {
            run.passed = passed;
            run.failed = failed.max(run.failures.len());
        }
    }
    run
}

// ── Echidna ─────────────────────────────────────────────────────────

pub fn echidna_config(
    corpus: &Path,
    test_limit: u64,
    timeout_s: u64,
    workers: usize,
    fork: Option<&Fork>,
) -> String {
    let mut y = format!(
        "testMode: property\n\
         prefix: \"{PREFIX}\"\n\
         testLimit: {test_limit}\n\
         timeout: {timeout_s}\n\
         seqLen: 100\n\
         shrinkLimit: 5000\n\
         workers: {workers}\n\
         corpusDir: \"{}\"\n\
         format: text\n\
         codeSize: 0xffffffff\n\
         balanceContract: 0xffffffffffffffffffffffff\n\
         deployer: \"0x30000\"\n\
         sender: [\"0x10000\", \"0x20000\", \"0x30000\"]\n\
         cryticArgs: [\"--foundry-compile-all\"]\n",
        corpus.display()
    );
    if let Some(f) = fork {
        y.push_str(&format!("rpcUrl: \"{}\"\nrpcBlock: {}\n", f.rpc, f.block));
    }
    y
}

/// The final report lists `name: passing` (or `passed! 🎉`, `verified ✅`) and
/// `name: failed!💥` followed by `Reason:` and an indented `Call sequence:` block.
pub fn parse_echidna(out: &str) -> ToolRun {
    let out = strip_ansi(out);
    let mut run = ToolRun {
        tool: "echidna",
        ..Default::default()
    };
    let mut current: Option<Failure> = None;
    let mut in_seq = false;
    for line in out.lines() {
        let t = line.trim();
        let status_line = (!line.starts_with(' '))
            .then(|| line.split_once(": "))
            .flatten()
            .filter(|(name, _)| (!name.is_empty() && !name.contains(' ')) || name.contains('('));
        if let Some((name, status)) = status_line {
            let status = status.trim();
            if status.starts_with("failed") || status.starts_with("could not evaluate") {
                if let Some(f) = current.take() {
                    run.failures.push(f);
                }
                current = Some(Failure {
                    tool: "echidna",
                    invariant: bare_name(name),
                    reason: if status.starts_with("could not") {
                        status.to_string()
                    } else {
                        String::new()
                    },
                    sequence: vec![],
                });
                in_seq = false;
                continue;
            }
            if status.starts_with("passing")
                || status.starts_with("passed")
                || status.starts_with("verified")
            {
                if let Some(f) = current.take() {
                    run.failures.push(f);
                }
                run.passed += 1;
                in_seq = false;
                continue;
            }
        }
        let Some(f) = current.as_mut() else { continue };
        if let Some(r) = t.strip_prefix("Reason:") {
            f.reason = r.trim().to_string();
        } else if t.starts_with("Call sequence") {
            in_seq = true;
        } else if in_seq && !t.is_empty() {
            f.sequence.push(t.to_string());
        } else if in_seq && t.is_empty() {
            in_seq = false;
        }
    }
    if let Some(f) = current.take() {
        run.failures.push(f);
    }
    run.failed = run.failures.len();
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORGE: &str = "\
Ran 3 tests for test/bulwark/BulwarkFoundry.t.sol:BulwarkInvariants
[PASS] invariant_P1_supply() (runs: 256, calls: 128000, reverts: 12)
[FAIL: P-2 shares exceed assets: 101 > 100]
\t[Sequence] (original: 4, shrunk: 2)
\t\tsender=0x0000000000000000000000000000000000010000 addr=[test/bulwark/CryticTester.sol:CryticTester]0x5615dEB798BB3E4dFa0139dFa1b3D433Cc23b72f calldata=target_deposit(uint256,uint256) args=[3, 100]
\t\tsender=0x0000000000000000000000000000000000020000 addr=[test/bulwark/CryticTester.sol:CryticTester]0x5615dEB798BB3E4dFa0139dFa1b3D433Cc23b72f calldata=target_donate(uint256) args=[1]
 invariant_P2_backing() (runs: 1, calls: 2, reverts: 0)
[FAIL. Reason: revert: overflow] invariant_P3_fees() (runs: 9, calls: 900, reverts: 1)
Suite result: FAILED. 1 passed; 2 failed; 0 skipped";

    #[test]
    fn forge_failures_carry_name_reason_and_sequence() {
        let r = parse_forge(FORGE);
        assert_eq!((r.passed, r.failed), (1, 2));
        assert_eq!(r.failures[0].invariant, "invariant_P2_backing");
        assert_eq!(r.failures[0].reason, "P-2 shares exceed assets: 101 > 100");
        assert_eq!(r.failures[0].sequence.len(), 2);
        assert!(r.failures[0].sequence[1].contains("target_donate"));
        assert_eq!(r.failures[1].invariant, "invariant_P3_fees");
        assert_eq!(r.failures[1].reason, "revert: overflow");
    }

    const MEDUSA: &str = "\
\u{1b}[1m[FAILED]\u{1b}[0m Property Test: CryticTester.invariant_P2_backing()
Test for method \"CryticTester.invariant_P2_backing()\" failed after the following call sequence:
[Call Sequence]
1) CryticTester.target_deposit(uint256,uint256)(3, 100) (block=2, time=5, gas=12500000, gasprice=1, value=0, sender=0x10000)
2) CryticTester.target_donate(uint256)(1) (block=3, time=9, gas=12500000, gasprice=1, value=0, sender=0x20000)
[Property Test Execution Trace]
[Execution Trace]
 => [call] CryticTester.invariant_P2_backing()() (addr=0xA647ff3c36cFab592509E13860ab8c4F28781a66, value=0, sender=0x10000)
[PASSED] Property Test: CryticTester.invariant_P1_supply()
[PASSED] Assertion Test: CryticTester.target_deposit(uint256,uint256)
Test summary: 2 test(s) passed, 1 test(s) failed";

    #[test]
    fn medusa_failures_and_summary() {
        let r = parse_medusa(MEDUSA);
        assert_eq!((r.passed, r.failed), (2, 1));
        let f = &r.failures[0];
        assert_eq!(f.invariant, "invariant_P2_backing");
        assert_eq!(f.sequence.len(), 2);
        assert!(f.sequence[0].starts_with("1) CryticTester.target_deposit"));
    }

    const ECHIDNA: &str = "\
invariant_P1_supply: passing
invariant_P2_backing: failed!💥
  Reason: Return false
  Call sequence:
    CryticTester.target_deposit(3,100) from: 0x0000000000000000000000000000000000010000
    CryticTester.target_donate(1) from: 0x0000000000000000000000000000000000020000 Time delay: 5 seconds Block delay: 1

invariant_P3_fees: could not evaluate ☣
  ExecutionError
AssertionFailed(..): passed! 🎉
Unique instructions: 4821
Corpus size: 12";

    #[test]
    fn echidna_failures_and_passes() {
        let r = parse_echidna(ECHIDNA);
        assert_eq!(r.passed, 2);
        assert_eq!(r.failed, 2);
        assert_eq!(r.failures[0].invariant, "invariant_P2_backing");
        assert_eq!(r.failures[0].reason, "Return false");
        assert_eq!(r.failures[0].sequence.len(), 2);
        assert_eq!(r.failures[1].invariant, "invariant_P3_fees");
        assert!(r.failures[1].reason.starts_with("could not evaluate"));
    }

    #[test]
    fn configs_carry_fork_target_and_prefix() {
        let fork = Fork {
            rpc: "http://127.0.0.1:8545".into(),
            block: 123,
        };
        let m = medusa_config(Path::new("/w/corpus/medusa"), 600, 4, Some(&fork));
        assert_eq!(m["fuzzing"]["targetContracts"][0], TESTER);
        assert_eq!(
            m["fuzzing"]["testing"]["propertyTesting"]["testPrefixes"][0],
            PREFIX
        );
        assert_eq!(
            m["fuzzing"]["chainConfig"]["forkConfig"]["forkModeEnabled"],
            true
        );
        assert_eq!(m["fuzzing"]["chainConfig"]["forkConfig"]["rpcBlock"], 123);
        assert_eq!(m["fuzzing"]["corpusDirectory"], "/w/corpus/medusa");
        let local = medusa_config(Path::new("/c"), 600, 4, None);
        assert_eq!(
            local["fuzzing"]["chainConfig"]["forkConfig"]["forkModeEnabled"],
            false
        );

        let e = echidna_config(Path::new("/w/corpus/echidna"), 50_000, 600, 4, Some(&fork));
        assert!(e.contains("prefix: \"invariant_\"") && e.contains("testMode: property"));
        assert!(e.contains("rpcUrl: \"http://127.0.0.1:8545\"\nrpcBlock: 123"));
        assert!(e.contains("corpusDir: \"/w/corpus/echidna\""));
        assert!(!echidna_config(Path::new("/c"), 1, 1, 1, None).contains("rpcUrl"));
    }
}
