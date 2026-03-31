pub mod claude;
pub mod forge;
pub mod slither;

use crate::error::{BulwarkError, Result};
use std::path::Path;
use std::process::Output;
use tokio::process::Command;

/// Run an external command and return the output.
/// Logs stderr on failure but does not treat non-zero exit as automatic error
/// (some tools like slither return non-zero when findings exist).
pub async fn run_command(program: &str, args: &[&str], cwd: &Path) -> Result<Output> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .await?;

    Ok(output)
}

/// Run an external command and require success (exit code 0).
pub async fn run_command_strict(program: &str, args: &[&str], cwd: &Path) -> Result<Output> {
    let output = run_command(program, args, cwd).await?;

    if !output.status.success() {
        return Err(BulwarkError::CommandFailed {
            command: format!("{program} {}", args.join(" ")),
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2000)
                .collect(),
        });
    }

    Ok(output)
}
