use crate::error::Result;
use crate::pipeline::pass::PipelineContext;

/// Placeholder for passes not yet implemented in Rust.
/// Mirrors the bash stubs — creates directories and writes empty outputs.
pub async fn run(_ctx: &PipelineContext, pass_name: &str) -> Result<String> {
    eprintln!("  Pass '{pass_name}' is not yet implemented in the Rust CLI.");
    eprintln!("  Use the bash pipeline for this pass: pipeline/pass*-{pass_name}.sh");

    Ok(format!("{pass_name}: not yet implemented"))
}
