use crate::error::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Slither reads `slither.config.json` and `slither.db.json` from here.
pub fn working_dir(contracts_dir: &Path) -> &Path {
    contracts_dir.parent().unwrap_or(contracts_dir)
}

/// The target's own triage database would hide every result its team triaged,
/// so Slither is pointed at `triage_db`, kept outside the target.
pub fn analyze_args(
    contracts_dir: &Path,
    output_path: &Path,
    triage_db: &Path,
    config: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        contracts_dir.to_string_lossy().into_owned(),
        "--json".into(),
        output_path.to_string_lossy().into_owned(),
        "--triage-database".into(),
        triage_db.to_string_lossy().into_owned(),
    ];
    if let Some(c) = config {
        args.push("--config-file".into());
        args.push(c.to_string_lossy().into_owned());
    }
    args
}

/// The target's config without `detectors_to_exclude`, and the detectors it excluded.
pub fn without_exclusions(config: &Value) -> (Value, Vec<String>) {
    let mut config = config.clone();
    let dropped = match config
        .as_object_mut()
        .and_then(|o| o.remove("detectors_to_exclude"))
    {
        Some(Value::String(s)) => s
            .split(',')
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|d| d.as_str().map(String::from))
            .collect(),
        _ => vec![],
    };
    (config, dropped)
}

/// Write the rewritten target config to `dest`. `None` when the target has no readable
/// config, so Slither runs without one; otherwise the detectors that were excluded.
pub fn stage_config(contracts_dir: &Path, dest: &Path) -> Result<Option<Vec<String>>> {
    let _ = std::fs::remove_file(dest);
    let Some(config) =
        std::fs::read_to_string(working_dir(contracts_dir).join("slither.config.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return Ok(None);
    };
    let (config, dropped) = without_exclusions(&config);
    std::fs::write(dest, serde_json::to_string_pretty(&config)?)?;
    Ok(Some(dropped))
}

/// Run Slither on a contracts directory and return JSON results.
///
/// Slither returns non-zero when it finds issues — that's normal,
/// not an error. We check for valid JSON output instead.
pub async fn analyze(
    slither_bin: &Path,
    contracts_dir: &Path,
    output_path: &Path,
    triage_db: &Path,
    config: Option<&Path>,
) -> Result<SlitherResult> {
    let _ = std::fs::remove_file(triage_db);
    let args = analyze_args(contracts_dir, output_path, triage_db, config);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = super::run_command(
        slither_bin.to_str().unwrap_or("slither"),
        &args,
        working_dir(contracts_dir),
    )
    .await?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Check if output file was written with valid JSON
    if output_path.exists() {
        let content = std::fs::read_to_string(output_path)?;
        match serde_json::from_str::<Value>(&content) {
            Ok(json) => Ok(SlitherResult {
                success: true,
                json: Some(json),
                stderr,
            }),
            Err(_) => Ok(SlitherResult {
                success: false,
                json: None,
                stderr,
            }),
        }
    } else {
        Ok(SlitherResult {
            success: false,
            json: None,
            stderr,
        })
    }
}

pub struct SlitherResult {
    pub success: bool,
    pub json: Option<Value>,
    pub stderr: String,
}

/// Extract severity counts from Slither JSON output.
pub fn summarize(results: &Value) -> SlitherSummary {
    let detectors = results
        .pointer("/results/detectors")
        .and_then(|d| d.as_array());

    let mut summary = SlitherSummary::default();

    if let Some(dets) = detectors {
        for det in dets {
            match det.get("impact").and_then(|i| i.as_str()) {
                Some("High") => summary.high += 1,
                Some("Medium") => summary.medium += 1,
                Some("Low") => summary.low += 1,
                Some("Informational") => summary.informational += 1,
                Some("Optimization") => summary.optimization += 1,
                _ => {}
            }
        }
    }

    summary
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SlitherSummary {
    pub high: u32,
    pub medium: u32,
    pub low: u32,
    pub informational: u32,
    pub optimization: u32,
}

impl std::fmt::Display for SlitherSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "H:{} M:{} L:{}", self.high, self.medium, self.low)
    }
}

/// A result the target's team triaged into its `slither.db.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriagedResult {
    pub id: String,
    pub check: String,
    pub impact: String,
    pub confidence: String,
    pub contract: String,
    #[serde(default)]
    pub function: String,
    pub file: String,
    /// First and last line of the first element.
    #[serde(default)]
    pub lines: Vec<u64>,
    pub description: String,
}

/// Parse a triage database: a JSON array of Slither results.
pub fn parse_triage_db(db: &Value) -> Vec<TriagedResult> {
    let Some(results) = db.as_array() else {
        return vec![];
    };
    results
        .iter()
        .map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let first = r
                .get("elements")
                .and_then(Value::as_array)
                .and_then(|e| e.first());
            let (contract, function) = first.map(owners).unwrap_or_default();
            let mapping = first.and_then(|e| e.get("source_mapping"));
            let file = mapping
                .and_then(|m| m.get("filename_relative"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let lines: Vec<u64> = mapping
                .and_then(|m| m.get("lines"))
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default();
            let lines = match (lines.first(), lines.last()) {
                (Some(a), Some(b)) => vec![*a, *b],
                _ => vec![],
            };
            TriagedResult {
                id: s("id"),
                check: s("check"),
                impact: s("impact"),
                confidence: s("confidence"),
                contract,
                function,
                file,
                lines,
                description: s("description"),
            }
        })
        .collect()
}

/// Contract and function that own an element, found by walking its parents.
fn owners(element: &Value) -> (String, String) {
    let (mut contract, mut function) = (String::new(), String::new());
    let mut cur = Some(element);
    while let Some(e) = cur {
        let name = e.get("name").and_then(Value::as_str).unwrap_or("");
        match e.get("type").and_then(Value::as_str) {
            Some("function") if function.is_empty() => function = name.to_string(),
            Some("contract") => {
                contract = name.to_string();
                break;
            }
            _ => {}
        }
        cur = e.pointer("/type_specific_fields/parent");
    }
    (contract, function)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triage_db_points_outside_the_target() {
        let args = analyze_args(
            Path::new("/t/onyx/src"),
            Path::new("/ws/recon/slither-onyx-raw.json"),
            Path::new("/ws/recon/slither.db.json"),
            None,
        );
        assert_eq!(
            args,
            vec![
                "/t/onyx/src",
                "--json",
                "/ws/recon/slither-onyx-raw.json",
                "--triage-database",
                "/ws/recon/slither.db.json",
            ]
        );
        assert_eq!(working_dir(Path::new("/t/onyx/src")), Path::new("/t/onyx"));

        let args = analyze_args(
            Path::new("/t/onyx/src"),
            Path::new("/ws/out.json"),
            Path::new("/ws/db.json"),
            Some(Path::new("/ws/recon/slither-onyx.config.json")),
        );
        assert_eq!(
            &args[5..],
            ["--config-file", "/ws/recon/slither-onyx.config.json"]
        );
    }

    #[test]
    fn config_rewrite_drops_only_detector_exclusions() {
        let onyx = serde_json::json!({
            "detectors_to_exclude": ["calls-loop", "naming-convention"],
            "exclude_dependencies": true,
            "filter_paths": "lib/|test/",
            "solc_remaps": "@oz/=lib/oz/",
        });
        let (config, dropped) = without_exclusions(&onyx);
        assert_eq!(dropped, vec!["calls-loop", "naming-convention"]);
        assert_eq!(
            config,
            serde_json::json!({
                "exclude_dependencies": true,
                "filter_paths": "lib/|test/",
                "solc_remaps": "@oz/=lib/oz/",
            })
        );

        let (_, dropped) = without_exclusions(
            &serde_json::json!({"detectors_to_exclude": "pragma, solc-version"}),
        );
        assert_eq!(dropped, vec!["pragma", "solc-version"]);
        let (config, dropped) = without_exclusions(&serde_json::json!({"exclude_low": true}));
        assert!(dropped.is_empty());
        assert_eq!(config, serde_json::json!({"exclude_low": true}));
    }

    #[test]
    fn config_is_staged_only_when_the_target_has_one() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let dest = dir.path().join("staged.json");

        assert_eq!(stage_config(&src, &dest).unwrap(), None);
        assert!(!dest.exists());

        std::fs::write(
            dir.path().join("slither.config.json"),
            r#"{"detectors_to_exclude": ["pragma"], "exclude_dependencies": true}"#,
        )
        .unwrap();
        assert_eq!(
            stage_config(&src, &dest).unwrap(),
            Some(vec!["pragma".to_string()])
        );
        let staged: Value = serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(staged, serde_json::json!({"exclude_dependencies": true}));
    }

    #[test]
    fn parses_triage_db_fixture() {
        let db: Value = serde_json::from_str(include_str!("testdata/slither.db.json")).unwrap();
        let r = parse_triage_db(&db);
        assert_eq!(r.len(), 4);

        assert_eq!(r[0].check, "timestamp");
        assert_eq!(r[0].impact, "Low");
        assert_eq!(r[0].contract, "SyncRedeemHandler");
        assert_eq!(r[0].function, "__validateSharePriceTimestamp");
        assert_eq!(
            r[0].file,
            "src/components/issuance/redeem-handlers/SyncRedeemHandler.sol"
        );
        assert_eq!(r[0].lines, vec![173, 181]);
        assert!(r[0].id.starts_with("7e7867be"));

        // A local variable: owned by its function, then the contract.
        assert_eq!(r[1].check, "uninitialized-local");
        assert_eq!(r[1].contract, "SyncRedeemHandler");
        assert_eq!(r[1].function, "__maxAssetsWithin");

        assert_eq!(r[2].check, "unprotected-upgrade");
        assert_eq!(r[2].contract, "Global");
        assert_eq!(r[2].function, "");

        // A pragma belongs to no contract.
        assert_eq!(r[3].check, "solc-version");
        assert_eq!(r[3].contract, "");
    }

    #[test]
    fn triage_db_that_is_not_an_array_yields_nothing() {
        assert!(parse_triage_db(&serde_json::json!({"results": []})).is_empty());
    }
}
