//! Safety-critical deletion machinery: path guards, Trash, manifests, undo.
//!
//! Every public function here is covered by a test (#35). The only code
//! excluded from the coverage run is the OS glue no test may reach: the call
//! that moves a real item to the real Trash, and the real volume lookup
//! behind [`bin::SystemBin::new`]. Tests never touch either.
//!
//! - [`guard`] — may this path be touched at all? (#29)
//! - [`preflight`] — is it still exactly what the scan saw? (#29)
//! - [`trash`] — move a confirmed receipt to the Trash, recorded in a
//!   [`manifest`] written first (#30)
//! - [`undo`] — put a deletion back (#30)
//! - [`bin`] — the macOS Trash and the Windows Recycle Bin (#30)

#![forbid(unsafe_code)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod bin;
pub mod guard;
pub mod manifest;
pub mod preflight;
pub mod trash;
pub mod undo;

pub use guard::{Guard, Refusal};
pub use manifest::{Manifest, ManifestError, ManifestItem, Status};
pub use preflight::{compare, preflight, Skip};
pub use trash::{execute, execute_with, Bin, Item, Outcome, Wanted};
pub use undo::{undo, UndoItem, UndoReport, Undone};

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
