//! Safety-critical deletion machinery: path guards, Trash, manifests, undo.
//!
//! Every public function here is expected to be covered by a test. See issue #35.
//!
//! - [`guard`] — may this path be touched at all? (#29)
//! - [`preflight`] — is it still exactly what the scan saw? (#29)
//!
//! The Trash service, manifests, and undo follow in #30.

#![forbid(unsafe_code)]

pub mod guard;
pub mod preflight;

pub use guard::{Guard, Refusal};
pub use preflight::{compare, preflight, Skip};

/// Whose rules apply. Both platforms' rules are pure, so both are tested on
/// either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
}

impl Platform {
    /// The platform this build runs on.
    pub const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::MacOs
    };
}
