use crate::error::{DoyranError, Result};
use std::path::Path;

/// Validate each element of a JSON array against a schema.
pub fn validate_findings_array(findings: &serde_json::Value, schema_path: &Path) -> Result<()> {
    let arr = findings.as_array().ok_or_else(|| DoyranError::SchemaValidation {
        file: schema_path.to_path_buf(),
        errors: vec!["expected JSON array".into()],
    })?;

    let schema_content = std::fs::read_to_string(schema_path)?;
    let schema: serde_json::Value = serde_json::from_str(&schema_content)?;

    let validator = jsonschema::validator_for(&schema).map_err(|e| {
        DoyranError::SchemaValidation {
            file: schema_path.to_path_buf(),
            errors: vec![format!("failed to compile schema: {e}")],
        }
    })?;

    let mut all_errors = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        for error in validator.iter_errors(item) {
            all_errors.push(format!("[{i}] {} at {}", error, error.instance_path));
        }
    }

    if all_errors.is_empty() {
        Ok(())
    } else {
        Err(DoyranError::SchemaValidation {
            file: schema_path.to_path_buf(),
            errors: all_errors,
        })
    }
}
