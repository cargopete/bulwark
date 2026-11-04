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
pub fn analyze_args(contracts_dir: &Path, output_path: &Path, triage_db: &Path) -> Vec<String> {
    vec![
        contracts_dir.to_string_lossy().into_owned(),
        "--json".into(),
        output_path.to_string_lossy().into_owned(),
        "--triage-database".into(),
        triage_db.to_string_lossy().into_owned(),
    ]
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
) -> Result<SlitherResult> {
    let _ = std::fs::remove_file(triage_db);
    let args = analyze_args(contracts_dir, output_path, triage_db);
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
