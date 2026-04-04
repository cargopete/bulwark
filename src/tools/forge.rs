use crate::error::Result;
use std::path::Path;

/// Run `forge build --force --ast` in a package directory.
/// The `--ast` flag embeds source AST in build artifacts — required by Halmos 0.2.x
/// for symbolic execution. Without it, Halmos crashes with `IndexError: pop from empty list`.
pub async fn build(forge_bin: &Path, pkg_dir: &Path) -> Result<BuildResult> {
    let output = super::run_command(
        forge_bin.to_str().unwrap_or("forge"),
        &["build", "--force", "--ast"],
        pkg_dir,
    )
    .await?;

    let success = output.status.success();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(BuildResult { success, stderr })
}

pub struct BuildResult {
    pub success: bool,
    pub stderr: String,
}

/// Run `forge inspect <contract> abi` and parse the JSON output.
///
/// Forge prefixes compilation tables and may include empty `[]` arrays
/// before the actual ABI, so we search for `[{` to find the real data.
pub async fn inspect_abi(
    forge_bin: &Path,
    pkg_dir: &Path,
    contract: &str,
) -> Result<Option<serde_json::Value>> {
    let output = super::run_command(
        forge_bin.to_str().unwrap_or("forge"),
        &["inspect", contract, "abi", "--json"],
        pkg_dir,
    )
    .await?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // With --json, output should be clean JSON
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
        return Ok(Some(v));
    }

    // Fallback: forge may still prefix compilation messages before the JSON.
    // Search for `[{` — ABI arrays are always arrays of objects.
    if let Some(start) = stdout.find("[{") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout[start..]) {
            return Ok(Some(v));
        }
        if let Some(arr) = crate::tools::claude::try_parse_json_array(&stdout[start..]) {
            return Ok(Some(serde_json::Value::Array(arr)));
        }
    }

    Ok(None)
}

/// Run `forge inspect <contract> storage-layout` and parse the JSON.
pub async fn inspect_storage_layout(
    forge_bin: &Path,
    pkg_dir: &Path,
    contract: &str,
) -> Result<Option<serde_json::Value>> {
    let output = super::run_command(
        forge_bin.to_str().unwrap_or("forge"),
        &["inspect", contract, "storage-layout", "--json"],
        pkg_dir,
    )
    .await?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str(&stdout) {
        Ok(v) => Ok(Some(v)),
        Err(_) => Ok(None),
    }
}

/// Run `forge config --json` to get compiler version.
pub async fn get_compiler_version(forge_bin: &Path, pkg_dir: &Path) -> Result<String> {
    let output = super::run_command(
        forge_bin.to_str().unwrap_or("forge"),
        &["config", "--json"],
        pkg_dir,
    )
    .await?;

    if !output.status.success() {
        return Ok("unknown".into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let config: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_default();
    Ok(config
        .get("solc")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string())
}

/// Run `forge test` with optional match patterns.
pub async fn test(
    forge_bin: &Path,
    pkg_dir: &Path,
    match_path: Option<&str>,
    verbosity: u8,
) -> Result<TestResult> {
    let mut args = vec!["test"];

    let v_flag = match verbosity {
        0 => None,
        1 => Some("-v"),
        2 => Some("-vv"),
        3 => Some("-vvv"),
        _ => Some("-vvvv"),
    };
    if let Some(v) = v_flag {
        args.push(v);
    }

    let match_arg;
    if let Some(path) = match_path {
        args.push("--match-path");
        match_arg = path.to_string();
        args.push(&match_arg);
    }

    let output = super::run_command(forge_bin.to_str().unwrap_or("forge"), &args, pkg_dir).await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(TestResult {
        success: output.status.success(),
        stdout,
        stderr,
    })
}

pub struct TestResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}
