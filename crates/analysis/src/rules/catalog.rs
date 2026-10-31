//! Loading and compiling rule catalogs.
//!
//! A catalog file is a JSON array of [`Rule`]s. The built-in files are
//! compiled in; a rules folder (the app's, or a developer's checkout of
//! `crates/analysis/rules`) is read at runtime, and a rule there replaces a
//! built-in one with the same id. Loading never fails as a whole: a file that
//! does not parse, a rule that is invalid, or a glob whose variable is not
//! set is skipped and listed in [`RuleSet::problems`].

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

use storagelens_safety::Platform;

use super::glob::Glob;
use super::{Rule, RulePlatform};

/// The catalogs compiled into the binary, by file name.
pub const BUILTIN: &[(&str, &str)] = &[
    ("shared.json", include_str!("../../rules/shared.json")),
    ("macos.json", include_str!("../../rules/macos.json")),
];

/// What variables in a glob expand to, and which platform's rules apply.
#[derive(Debug, Clone)]
pub struct Context {
    pub platform: Platform,
    /// `~`. Rules that use it are skipped without one.
    pub home: Option<String>,
    /// `%NAME%` values, by upper-case name (`LOCALAPPDATA`, `TEMP`, …).
    pub vars: HashMap<String, String>,
}

impl Context {
    /// A context with a home folder and no other variables.
    pub fn new(platform: Platform, home: Option<&str>) -> Self {
        Self {
            platform,
            home: home.map(|home| home.trim_end_matches(['/', '\\']).to_owned()),
            vars: HashMap::new(),
        }
    }

    /// Add a `%NAME%` variable.
    pub fn with_var(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(
            name.to_ascii_uppercase(),
            value.trim_end_matches(['/', '\\']).to_owned(),
        );
        self
    }

    fn applies(&self, platform: RulePlatform) -> bool {
        match platform {
            RulePlatform::Any => true,
            RulePlatform::Macos => self.platform == Platform::MacOs,
            RulePlatform::Windows => self.platform == Platform::Windows,
        }
    }

    /// `glob` with `~` and every `%NAME%` replaced.
    pub fn expand(&self, glob: &str) -> Result<String, String> {
        let mut out = String::new();
        let rest = if glob == "~" || glob.starts_with("~/") || glob.starts_with("~\\") {
            let home = self
                .home
                .as_deref()
                .ok_or_else(|| format!("\"{glob}\" needs a home folder"))?;
            out.push_str(home);
            &glob[1..]
        } else {
            glob
        };
        let mut parts = rest.split('%');
        out.push_str(parts.next().unwrap_or_default());
        while let Some(name) = parts.next() {
            let Some(after) = parts.next() else {
                return Err(format!("\"{glob}\" has an unclosed %"));
            };
            let value = self
                .vars
                .get(&name.to_ascii_uppercase())
                .ok_or_else(|| format!("\"{glob}\": %{name}% is not set"))?;
            out.push_str(value);
            out.push_str(after);
        }
        Ok(out)
    }
}

/// A rule with its globs compiled for one context.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub rule: Rule,
    pub globs: Vec<Glob>,
    pub excludes: Vec<Glob>,
}

fn compile_all(context: &Context, globs: &[String]) -> Result<Vec<Glob>, String> {
    let mut out = Vec::new();
    for glob in globs {
        let expanded = context.expand(glob)?;
        out.extend(Glob::compile(&expanded).map_err(|error| error.to_string())?);
    }
    Ok(out)
}

/// The rules that apply to one platform and user.
#[derive(Debug, Clone)]
pub struct RuleSet {
    /// The platform the set was compiled for.
    pub platform: Platform,
    pub rules: Vec<CompiledRule>,
    /// Everything that was skipped, and why.
    pub problems: Vec<String>,
}

impl RuleSet {
    /// Compile catalog `files` (`(name, json)`) in order; a later rule
    /// replaces an earlier one with the same id.
    pub fn compile<'a>(
        context: &Context,
        files: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        let mut set = Self {
            platform: context.platform,
            rules: Vec::new(),
            problems: Vec::new(),
        };
        for (name, text) in files {
            let rules: Vec<Rule> = match serde_json::from_str(text) {
                Ok(rules) => rules,
                Err(error) => {
                    set.problems.push(format!("{name}: {error}"));
                    continue;
                }
            };
            for rule in rules {
                if !context.applies(rule.platform) {
                    continue;
                }
                match set.compile_rule(context, rule) {
                    Ok(compiled) => {
                        let id = compiled.rule.id.clone();
                        set.rules.retain(|existing| existing.rule.id != id);
                        set.rules.push(compiled);
                    }
                    Err(problem) => set.problems.push(format!("{name}: {problem}")),
                }
            }
        }
        set
    }

    fn compile_rule(&self, context: &Context, rule: Rule) -> Result<CompiledRule, String> {
        rule.validate()?;
        let globs = compile_all(context, rule.glob.all())
            .map_err(|error| format!("{}: {error}", rule.id))?;
        let excludes =
            compile_all(context, &rule.exclude).map_err(|error| format!("{}: {error}", rule.id))?;
        Ok(CompiledRule {
            rule,
            globs,
            excludes,
        })
    }

    /// The built-in catalogs.
    pub fn builtin(context: &Context) -> Self {
        Self::compile(context, BUILTIN.iter().copied())
    }

    /// The built-in catalogs, then every `*.json` in `dir` (by name). A missing
    /// folder adds nothing; an unreadable file is a problem.
    pub fn load(context: &Context, dir: &Path) -> Self {
        let mut files: Vec<(String, String)> = BUILTIN
            .iter()
            .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
            .collect();
        let mut problems = Vec::new();
        for path in catalog_files(dir) {
            let name = path.display().to_string();
            match fs::read_to_string(&path) {
                Ok(text) => files.push((name, text)),
                Err(error) => problems.push(format!("{name}: {error}")),
            }
        }
        let mut set = Self::compile(
            context,
            files
                .iter()
                .map(|(name, text)| (name.as_str(), text.as_str())),
        );
        problems.append(&mut set.problems);
        set.problems = problems;
        set
    }

    /// The compiled rule with `id`.
    pub fn get(&self, id: &str) -> Option<&CompiledRule> {
        self.rules.iter().find(|compiled| compiled.rule.id == id)
    }
}

/// `*.json` files in `dir`, by name. Empty if it cannot be listed.
fn catalog_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

/// A value that changes whenever a catalog file in `dir` is added, removed,
/// resized, or modified: reload when it differs from the last one.
pub fn fingerprint(dir: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    for path in catalog_files(dir) {
        path.hash(&mut hasher);
        if let Ok(metadata) = fs::metadata(&path) {
            metadata.len().hash(&mut hasher);
            metadata.modified().ok().hash(&mut hasher);
        }
    }
    hasher.finish()
}
