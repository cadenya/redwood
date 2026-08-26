//! Spec input resolution: a `--spec` argument is either a filesystem path
//! or an http(s) URL fetched at generation time, so consumers can point
//! straight at a published contract (https://openapi.cadenya.com/api-spec.yml)
//! without a download step.

use anyhow::{Context, Result};

/// Specs comfortably fit in single-digit megabytes; the cap only exists so
/// a misconfigured URL (an HTML portal, a tarball) fails fast and clearly.
const MAX_SPEC_BYTES: u64 = 64 * 1024 * 1024;

/// Read the spec source from a path or an http(s) URL.
pub fn read_spec(location: &str) -> Result<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        let mut response = ureq::get(location)
            .call()
            .with_context(|| format!("fetching spec {location}"))?;
        response
            .body_mut()
            .with_config()
            .limit(MAX_SPEC_BYTES)
            .read_to_string()
            .with_context(|| format!("reading spec body from {location}"))
    } else {
        std::fs::read_to_string(location).with_context(|| format!("reading spec {location}"))
    }
}
