//! The cleanup rules engine (#31, PLAN §10): one engine, JSON catalogs per
//! platform, no hard-coded paths.
//!
//! - [`Rule`] is the JSON schema. A rule names its paths with globs over
//!   variables (`~`, `%LOCALAPPDATA%`, …), says what kind of space it finds,
//!   and how risky removing it is. Its `risk` and `confidence` default from
//!   its `kind`.
//! - [`RuleSet`] is a catalog compiled for one platform and one user: the
//!   built-in JSON (`crates/analysis/rules/*.json`, compiled in) plus any
//!   rules in a folder, re-read whenever its [`fingerprint`] changes — so a
//!   catalog can be edited without recompiling.
//! - [`evaluate`] runs a set over a scan's arena in one pass and returns the
//!   [`Evaluation`]: hits, each file's verdict for the Cleanliness view, and
//!   the [`Reclaim`] score.
//!
//! Two rules make the engine safe by construction:
//!
//! - A hit is only offered for deletion through [`Candidate`], which cannot
//!   be built for a `never` or report-only rule.
//! - Every deletable hit passes the safety crate's path guard. One that would
//!   touch a system location, a protected folder, or a bundle's insides is
//!   dropped, so no deletable rule can match a system path however its glob
//!   is written.

mod catalog;
mod eval;
pub mod glob;

use serde::{Deserialize, Serialize};

pub use catalog::{fingerprint, CompiledRule, Context, RuleSet, BUILTIN};
pub use eval::{evaluate, Candidate, Evaluation, Hit, Reclaim};

use crate::Risk;

/// What a rule finds (PLAN §10, "Rule kinds").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Rebuilt automatically on next use.
    Regenerable,
    /// Untouched for a long time.
    Stale,
    /// Byte-identical to another file.
    Duplicate,
    /// Belongs to something that no longer exists.
    Leftover,
    /// Real data: backups, mail.
    Valuable,
    /// Report only; never deletable.
    System,
}

impl Kind {
    /// The risk a rule of this kind has unless it says otherwise.
    pub const fn default_risk(self) -> Risk {
        match self {
            Self::Regenerable => Risk::Safe,
            Self::Stale | Self::Duplicate | Self::Leftover => Risk::Review,
            Self::Valuable => Risk::Keep,
            Self::System => Risk::Never,
        }
    }

    /// The confidence a rule of this kind has unless it says otherwise.
    pub const fn default_confidence(self) -> f64 {
        match self {
            Self::Regenerable => 0.95,
            Self::Stale => 0.8,
            Self::Duplicate => 0.9,
            Self::Leftover => 0.6,
            Self::Valuable => 0.5,
            Self::System => 0.0,
        }
    }

    /// Whether hits start selected on the Cleanup screen.
    pub const fn selected_by_default(self) -> bool {
        matches!(self, Self::Regenerable | Self::Stale)
    }
}

/// How much of a hit's bytes count toward the Reclaim score (PLAN §10).
/// `keep` (valuable data) and `never` count for nothing.
pub const fn risk_weight(risk: Risk) -> f64 {
    match risk {
        Risk::Safe => 1.0,
        Risk::Review => 0.5,
        Risk::Keep | Risk::Never => 0.0,
    }
}

/// The name a risk has in JSON and in `rule_hit.risk`.
pub const fn risk_name(risk: Risk) -> &'static str {
    match risk {
        Risk::Safe => "safe",
        Risk::Review => "review",
        Risk::Keep => "keep",
        Risk::Never => "never",
    }
}

/// A risk as written in a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskName {
    Safe,
    Review,
    Keep,
    Never,
}

impl From<RiskName> for Risk {
    fn from(name: RiskName) -> Self {
        match name {
            RiskName::Safe => Risk::Safe,
            RiskName::Review => Risk::Review,
            RiskName::Keep => Risk::Keep,
            RiskName::Never => Risk::Never,
        }
    }
}

/// What may be done with a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Offered for the Trash.
    Trash,
    /// Shown, with its explanation, but never deletable (`Docker.raw`).
    Report,
}

/// Which platforms a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePlatform {
    Any,
    Macos,
    Windows,
}

/// What a rule may match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    /// Folders and files.
    #[default]
    Any,
    Dir,
    File,
}

/// One glob or several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Globs {
    One(String),
    Many(Vec<String>),
}

impl Globs {
    /// Every glob.
    pub fn all(&self) -> &[String] {
        match self {
            Self::One(glob) => std::slice::from_ref(glob),
            Self::Many(globs) => globs,
        }
    }
}

/// One rule, as written in a catalog (PLAN §10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Stable, dotted: `dev.node_modules`, `macos.xcode.derived_data`.
    pub id: String,
    pub platform: RulePlatform,
    pub title: String,
    /// Where it looks. Variables: `~` (home) and `%NAME%` (environment).
    pub glob: Globs,
    /// Paths under the glob that are not hits.
    #[serde(default)]
    pub exclude: Vec<String>,
    pub kind: Kind,
    /// Defaults from `kind`.
    #[serde(default)]
    pub risk: Option<RiskName>,
    /// 0–1. Defaults from `kind`.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Only when nothing inside has been modified for this many days.
    #[serde(default)]
    pub stale_days: Option<u32>,
    /// One or two calm sentences: what it is and what removing it means.
    pub explain: String,
    /// Defaults to `trash`, or `report` for a `never` rule.
    #[serde(default)]
    pub action: Option<Action>,
    #[serde(default, rename = "match")]
    pub matches: Match,
    /// Only when one of these names is beside the match (`Cargo.toml` beside
    /// `target`), so a folder that merely shares the name is not a hit.
    #[serde(default)]
    pub when_sibling: Vec<String>,
    /// Only when one of these names is inside the match (`pyvenv.cfg`).
    #[serde(default)]
    pub when_child: Vec<String>,
}

impl Rule {
    /// The rule's risk, explicit or from its kind.
    pub fn risk(&self) -> Risk {
        self.risk.map_or(self.kind.default_risk(), Risk::from)
    }

    /// The rule's confidence, explicit or from its kind.
    pub fn confidence(&self) -> f64 {
        self.confidence
            .unwrap_or_else(|| self.kind.default_confidence())
    }

    /// The rule's action, explicit or from its risk.
    pub fn action(&self) -> Action {
        self.action.unwrap_or(if self.risk() == Risk::Never {
            Action::Report
        } else {
            Action::Trash
        })
    }

    /// Whether its hits may be offered for deletion at all.
    pub fn deletable(&self) -> bool {
        self.action() == Action::Trash && self.risk() != Risk::Never
    }

    /// Check the fields that globs cannot: the kind/risk/action contract.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("a rule needs an id".to_owned());
        }
        if !(0.0..=1.0).contains(&self.confidence()) {
            return Err(format!("{}: confidence must be between 0 and 1", self.id));
        }
        if self.kind == Kind::System && self.risk() != Risk::Never {
            return Err(format!("{}: a system rule must have risk never", self.id));
        }
        if self.risk() == Risk::Never && self.action() == Action::Trash {
            return Err(format!(
                "{}: a never rule cannot have action trash",
                self.id
            ));
        }
        if self.glob.all().is_empty() {
            return Err(format!("{}: a rule needs at least one glob", self.id));
        }
        Ok(())
    }
}
