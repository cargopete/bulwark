use std::path::Path;

/// Returns true when running inside the Bulwark Docker container.
pub fn is_in_docker() -> bool {
    Path::new("/.dockerenv").exists() || std::env::var("BULWARK_CONTAINER").is_ok()
}
