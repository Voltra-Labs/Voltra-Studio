//! Core vocabulary shared by every Voltra Studio crate.
//!
//! This crate is the contract the rest of the workspace is written against. It
//! deliberately depends on nothing from the operating system, so it builds and
//! its tests run on any target — including a headless CI container with no GPU
//! and no capture devices. Platform code lives behind the traits declared here,
//! never the other way round.
//!
//! At this stage the crate carries only build metadata and the shared error
//! type; frames, colour, timing and the scene graph land in later steps.

// Panicking helpers are denied in library code (see CLAUDE.md §3) but free in
// tests, where a failed assumption should fail the test loudly.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod error;

pub use error::{Error, Result};

/// The version of the Voltra Studio workspace this build came from.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The minimum supported Rust version, kept in sync with `rust-version`.
pub const MSRV: &str = "1.85";

#[cfg(test)]
mod tests {
    use super::{MSRV, VERSION};

    /// The crate version must stay parseable as `major.minor.patch`: release
    /// tooling and the plugin registry both rely on that shape.
    #[test]
    fn version_is_semver_shaped() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "unexpected version `{VERSION}`");
        for part in parts {
            assert!(
                part.parse::<u32>().is_ok(),
                "non-numeric version component in `{VERSION}`"
            );
        }
    }

    /// A stale MSRV constant would silently lie to downstream users, so pin it
    /// against the value Cargo actually enforces.
    #[test]
    fn msrv_matches_cargo_metadata() {
        assert_eq!(MSRV, env!("CARGO_PKG_RUST_VERSION"));
    }
}
