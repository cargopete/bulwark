//! What a Pass 2 session owes and whether its coverage file shows the work was done.

use crate::config::{AgentsPassConfig, Config};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// What an agent is answerable for reading in full: a hunter its component's files, any other
/// agent the program's assets. Empty means every file counts.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub assets: HashSet<String>,
    pub components: HashMap<String, Vec<String>>,
}

impl Scope {
    pub fn from_config(c: &Config) -> Self {
        Self {
            assets: c
                .program
                .iter()
                .flat_map(|p| &p.assets)
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty())
                .collect(),
            components: c
                .passes
                .agents
                .components
                .iter()
                .map(|k| (format!("hunt-{}", k.name), k.files.clone()))
                .collect(),
        }
    }

    /// Coverage entries are free text that starts with a path.
    pub fn counts(&self, agent: &str, entry: &str) -> bool {
        let path = entry_path(entry);
        if let Some(own) = self.components.get(agent) {
            return own.iter().any(|f| path.starts_with(f.as_str()));
        }
        self.assets.is_empty() || self.assets.contains(stem(path))
    }

    /// Each item must be met by at least one entry read in full.
    fn owed(&self, agent: &str) -> Vec<String> {
        match self.components.get(agent) {
            Some(files) => files.clone(),
            None => {
                let mut assets: Vec<String> = self.assets.iter().cloned().collect();
                assets.sort();
                assets
            }
        }
    }

    fn meets(&self, agent: &str, owed: &str, path: &str) -> bool {
        if self.components.contains_key(agent) {
            path.starts_with(owed)
        } else {
            stem(path) == owed
        }
    }
}

fn entry_path(entry: &str) -> &str {
    entry.split_whitespace().next().unwrap_or("")
}

fn stem(path: &str) -> &str {
    Path::new(path)
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Coverage {
    pub files: usize,
    pub files_read_in_full: usize,
    /// The array form of the coverage file carries no hypotheses.
    pub hypotheses: Option<usize>,
}

fn file_entries(v: &Value) -> &[Value] {
    match v {
        Value::Array(a) => a.as_slice(),
        obj => obj
            .get("files")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice),
    }
}

fn hypotheses(v: &Value) -> Option<usize> {
    match v {
        Value::Array(_) => None,
        obj => Some(
            obj.get("hypotheses")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        ),
    }
}

fn entry_file(f: &Value) -> &str {
    f.get("file").and_then(Value::as_str).unwrap_or("")
}

fn read_in_full(f: &Value) -> bool {
    f.get("read_in_full").and_then(Value::as_bool) == Some(true)
}

pub fn coverage_stats(v: &Value, counts: impl Fn(&str) -> bool) -> Coverage {
    let files: Vec<&Value> = file_entries(v)
        .iter()
        .filter(|f| counts(entry_file(f)))
        .collect();
    Coverage {
        files: files.len(),
        files_read_in_full: files.iter().filter(|f| read_in_full(f)).count(),
        hypotheses: hypotheses(v),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub min_hypotheses: usize,
    pub min_forge_tests: u64,
}

impl Thresholds {
    pub fn from_config(c: &AgentsPassConfig) -> Self {
        Self {
            min_hypotheses: c.min_hypotheses,
            min_forge_tests: c.min_forge_tests,
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self::from_config(&AgentsPassConfig::default())
    }
}

/// What a finished Pass 2 session left behind.
#[derive(Debug, Clone)]
pub struct Evidence {
    pub findings_file: bool,
    /// The parsed coverage file, or why there is none.
    pub coverage: Result<Value, String>,
    pub forge_test_runs: u64,
}

impl Evidence {
    pub fn read(ws: &crate::workspace::Workspace, agent: &str, forge_test_runs: u64) -> Self {
        let path = ws.agent_coverage(agent);
        let coverage = match std::fs::read_to_string(&path) {
            Err(_) => Err("no coverage file".to_string()),
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("coverage file does not parse ({e})")),
        };
        Self {
            findings_file: ws.agent_raw_output(agent).exists(),
            coverage,
            forge_test_runs,
        }
    }
}

/// Everything the session still owes, each item specific enough to act on. Empty means it passed.
pub fn missing(scope: &Scope, agent: &str, ev: &Evidence, t: &Thresholds) -> Vec<String> {
    let hunter = agent.starts_with("hunt-");
    let mut out = Vec::new();
    if !ev.findings_file {
        out.push(format!(
            "no findings file: write {agent}-agent-raw.json, `[]` if you found nothing"
        ));
    }
    match &ev.coverage {
        Err(why) => out.push(format!("{why}: write {agent}-agent-coverage.json")),
        Ok(v) => {
            let entries: Vec<&Value> = file_entries(v)
                .iter()
                .filter(|f| scope.counts(agent, entry_file(f)))
                .collect();
            for f in entries.iter().filter(|f| !read_in_full(f)) {
                out.push(format!(
                    "`{}` is not read in full",
                    entry_path(entry_file(f))
                ));
            }
            let owed = scope.owed(agent);
            for item in &owed {
                let listed = entries
                    .iter()
                    .any(|f| scope.meets(agent, item, entry_path(entry_file(f))));
                if !listed {
                    out.push(format!("`{item}` is not in the coverage file"));
                }
            }
            if owed.is_empty() && entries.is_empty() {
                out.push("the coverage file lists no files".into());
            }
            let hyps = hypotheses(v).unwrap_or(0);
            if hunter && hyps < t.min_hypotheses {
                out.push(format!(
                    "{hyps} hypotheses recorded, at least {} required",
                    t.min_hypotheses
                ));
            }
        }
    }
    if hunter && ev.forge_test_runs < t.min_forge_tests {
        out.push(format!(
            "{} `forge test` runs, at least {} required",
            ev.forge_test_runs, t.min_forge_tests
        ));
    }
    out
}

/// The message a resumed session receives.
pub fn resume_prompt(missing: &[String]) -> String {
    let list: String = missing.iter().map(|m| format!("- {m}\n")).collect();
    format!(
        "Your work is not finished. The evidence check on your output found:\n\n{list}\n\
         Do that work now, then update your findings and coverage files before you stop.\n"
    )
}

/// A Pass 2 session still short of evidence after its resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shallow {
    pub agent: String,
    pub missing: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            assets: ["Shares".to_string(), "Vault".to_string()]
                .into_iter()
                .collect(),
            components: [(
                "hunt-q".to_string(),
                vec!["src/q/Queue.sol".to_string(), "src/q/lib/".to_string()],
            )]
            .into_iter()
            .collect(),
        }
    }

    fn ev(coverage: Value, forge: u64) -> Evidence {
        Evidence {
            findings_file: true,
            coverage: Ok(coverage),
            forge_test_runs: forge,
        }
    }

    fn hyps(n: usize) -> Vec<Value> {
        (0..n).map(|i| json!({"idea": i})).collect()
    }

    #[test]
    fn scope_holds_hunters_to_their_component_and_others_to_assets() {
        let scope = scope();
        assert!(scope.counts("hunt-q", "src/q/Queue.sol"));
        assert!(!scope.counts("hunt-q", "src/fees/FeeHandler.sol (dependency)"));
        assert!(scope.counts("red", "src/shares/Shares.sol"));
        assert!(!scope.counts("red", "src/infra/deployment/SharesDeployer.sol"));
        assert!(Scope::default().counts("red", "anything.sol"));

        let cov = json!([
            {"file": "src/shares/Shares.sol", "read_in_full": true},
            {"file": "src/infra/deployment/SharesDeployer.sol", "read_in_full": false}
        ]);
        let c = coverage_stats(&cov, |f| scope.counts("red", f));
        assert_eq!((c.files, c.files_read_in_full), (1, 1));
    }

    #[test]
    fn coverage_reads_both_shapes() {
        let arr =
            json!([{"file": "a", "read_in_full": true}, {"file": "b", "read_in_full": false}]);
        assert_eq!(
            coverage_stats(&arr, |_| true),
            Coverage {
                files: 2,
                files_read_in_full: 1,
                hypotheses: None
            }
        );
        let obj =
            json!({"files": [{"file": "a", "read_in_full": true}], "hypotheses": [{}, {}, {}]});
        assert_eq!(
            coverage_stats(&obj, |_| true),
            Coverage {
                files: 1,
                files_read_in_full: 1,
                hypotheses: Some(3)
            }
        );
    }

    #[test]
    fn thorough_hunter_passes_and_ignores_its_dependencies() {
        let cov = json!({
            "files": [
                {"file": "src/q/Queue.sol", "read_in_full": true},
                {"file": "src/q/lib/Math.sol", "read_in_full": true},
                {"file": "src/fees/FeeHandler.sol (dependency, skimmed)", "read_in_full": false}
            ],
            "hypotheses": hyps(15),
        });
        let t = Thresholds::default();
        assert!(missing(&scope(), "hunt-q", &ev(cov, 1), &t).is_empty());
    }

    #[test]
    fn thorough_persona_passes_without_hypotheses_or_tests() {
        let cov = json!([
            {"file": "src/Shares.sol", "read_in_full": true, "checked": "x"},
            {"file": "src/Vault.sol (and its proxy)", "read_in_full": true},
            {"file": "src/Helper.sol", "read_in_full": false}
        ]);
        let t = Thresholds::default();
        assert!(missing(&scope(), "red", &ev(cov, 0), &t).is_empty());
    }

    #[test]
    fn each_gap_is_named() {
        let t = Thresholds::default();
        let cov = json!({
            "files": [{"file": "src/q/Queue.sol", "read_in_full": false}],
            "hypotheses": hyps(14),
        });
        assert_eq!(
            missing(&scope(), "hunt-q", &ev(cov, 0), &t),
            vec![
                "`src/q/Queue.sol` is not read in full",
                "`src/q/lib/` is not in the coverage file",
                "14 hypotheses recorded, at least 15 required",
                "0 `forge test` runs, at least 1 required",
            ]
        );

        let skipped = json!([{"file": "src/Shares.sol", "read_in_full": true}]);
        assert_eq!(
            missing(&scope(), "blue", &ev(skipped, 0), &t),
            vec!["`Vault` is not in the coverage file"]
        );

        let none = Evidence {
            findings_file: false,
            coverage: Err("no coverage file".into()),
            forge_test_runs: 3,
        };
        assert_eq!(
            missing(&Scope::default(), "gold", &none, &t),
            vec![
                "no findings file: write gold-agent-raw.json, `[]` if you found nothing",
                "no coverage file: write gold-agent-coverage.json",
            ]
        );
        assert_eq!(
            missing(&Scope::default(), "gold", &ev(json!([]), 0), &t),
            vec!["the coverage file lists no files"]
        );
    }

    #[test]
    fn thresholds_come_from_config() {
        let t = Thresholds {
            min_hypotheses: 2,
            min_forge_tests: 0,
        };
        let cov = json!({"files": [
            {"file": "src/q/Queue.sol", "read_in_full": true},
            {"file": "src/q/lib/A.sol", "read_in_full": true}
        ], "hypotheses": hyps(2)});
        assert!(missing(&scope(), "hunt-q", &ev(cov, 0), &t).is_empty());
    }

    #[test]
    fn resume_prompt_lists_every_gap() {
        let p = resume_prompt(&[
            "`src/q/lib/` is not in the coverage file".into(),
            "0 `forge test` runs, at least 1 required".into(),
        ]);
        assert!(p.contains("- `src/q/lib/` is not in the coverage file\n"));
        assert!(p.contains("- 0 `forge test` runs, at least 1 required\n"));
    }
}
