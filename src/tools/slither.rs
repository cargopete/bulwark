use crate::error::Result;
use serde_json::Value;
use std::path::Path;

/// Run Slither on a contracts directory and return JSON results.
///
/// Slither returns non-zero when it finds issues — that's normal,
/// not an error. We check for valid JSON output instead.
pub async fn analyze(
    slither_bin: &Path,
    contracts_dir: &Path,
    output_path: &Path,
) -> Result<SlitherResult> {
    let output_str = output_path.to_str().unwrap_or("slither-output.json");

    let output = super::run_command(
        slither_bin.to_str().unwrap_or("slither"),
        &[
            contracts_dir.to_str().unwrap_or("."),
            "--json",
            output_str,
        ],
        contracts_dir.parent().unwrap_or(contracts_dir),
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
        write!(
            f,
            "H:{} M:{} L:{}",
            self.high, self.medium, self.low
        )
    }
}
