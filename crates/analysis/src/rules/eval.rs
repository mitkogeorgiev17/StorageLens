//! Evaluating a rule set over a scan.
//!
//! One depth-first walk from the root. Each node carries the glob states its
//! parent left it, stepped by its own name, so no path string is built until
//! something matches; a subtree no glob can still match is not visited. A
//! rule that hits a folder stops looking inside it (the hit already covers
//! everything below), but other rules carry on.
//!
//! A node several rules hit has one **primary** hit, the most cautious, and
//! every file takes its verdict from its nearest primary hit — the most
//! specific rule that covers it. The Reclaim score counts each byte once, at
//! that primary hit:
//!
//! ```text
//! score = Σ (bytes × confidence × risk_weight)     safe 1.0 · review 0.5 · keep, never 0
//! ```
//!
//! Report-only rules count for nothing: their bytes cannot be freed here.

use storagelens_catalog::{Arena, NodeIndex, RuleHitRow, NIL, UNKNOWN_TIME};
use storagelens_core::EntryKind;
use storagelens_safety::{Guard, Platform, Refusal};

use super::catalog::RuleSet;
use super::glob::{components, Glob};
use super::{risk_name, risk_weight, Kind, Match};
use crate::Risk;

const DAY: i64 = 86_400;

/// A rule's match in one scan.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub rule_id: String,
    pub kind: Kind,
    pub risk: Risk,
    pub confidence: f64,
    /// Whether it may be offered for deletion (see [`Candidate`]).
    pub deletable: bool,
    /// Whether the Cleanup screen starts with it selected.
    pub selected: bool,
    pub node: NodeIndex,
    /// Absolute path.
    pub path: String,
    /// Allocated bytes of the whole subtree, as counted.
    pub bytes: u64,
    /// The node's verdict, when several rules hit it.
    pub primary: bool,
}

/// A hit that may be deleted. There is no way to make one for a `never` or
/// report-only rule.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    hit: &'a Hit,
}

impl<'a> Candidate<'a> {
    pub fn hit(&self) -> &'a Hit {
        self.hit
    }
}

/// The Reclaim score (PLAN §10), split as the Overview shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Reclaim {
    /// Σ bytes × confidence over safe hits.
    pub safe: f64,
    /// Σ bytes × confidence × 0.5 over review hits.
    pub review: f64,
}

impl Reclaim {
    /// `safe + review`: "You can safely free ~X".
    pub fn score(&self) -> f64 {
        self.safe + self.review
    }
}

/// Everything a rule set found in one scan.
#[derive(Debug, Clone)]
pub struct Evaluation {
    pub hits: Vec<Hit>,
    /// Deletable matches the path guard refused: `(rule id, path, why)`.
    pub refused: Vec<(String, String, Refusal)>,
    pub reclaim: Reclaim,
    /// Per node: the primary hit covering it, or `NIL`.
    owner: Vec<u32>,
}

impl Evaluation {
    /// The verdict on `index`: its nearest primary hit's risk, if any.
    pub fn risk_of(&self, index: NodeIndex) -> Option<Risk> {
        let owner = *self.owner.get(index as usize)?;
        self.hits.get(owner as usize).map(|hit| hit.risk)
    }

    /// The hits as `rule_hit` rows, entry ids resolved in `arena`.
    pub fn rows(&self, arena: &Arena) -> Vec<RuleHitRow> {
        self.hits
            .iter()
            .map(|hit| RuleHitRow {
                rule_id: hit.rule_id.clone(),
                path: hit.path.clone(),
                entry_id: arena.entry_id(hit.node),
                bytes: hit.bytes,
                risk: risk_name(hit.risk).to_owned(),
                confidence: hit.confidence,
                selected: hit.selected,
            })
            .collect()
    }

    /// The hits that may be offered for deletion.
    pub fn candidates(&self) -> impl Iterator<Item = Candidate<'_>> {
        self.hits
            .iter()
            .filter(|hit| hit.deletable)
            .map(|hit| Candidate { hit })
    }
}

/// One automaton: a glob (or an exclude glob) of one rule.
struct Automaton<'a> {
    rule: usize,
    exclude: bool,
    glob: &'a Glob,
}

/// `(automaton, state)` pairs.
type States = Vec<(u32, u32)>;

fn step_all(
    automata: &[Automaton<'_>],
    states: &States,
    name: &str,
    scratch: &mut Vec<usize>,
) -> States {
    let mut next = States::new();
    for &(automaton, state) in states {
        scratch.clear();
        automata[automaton as usize]
            .glob
            .step(state as usize, name, scratch);
        next.extend(scratch.iter().map(|&s| (automaton, s as u32)));
    }
    next.sort_unstable();
    next.dedup();
    next
}

fn name_is(arena: &Arena, index: NodeIndex, names: &[String]) -> bool {
    arena
        .name(index)
        .is_some_and(|name| names.iter().any(|wanted| wanted.eq_ignore_ascii_case(name)))
}

struct Walk<'a> {
    rules: &'a RuleSet,
    arena: &'a Arena,
    guard: &'a Guard,
    root_path: &'a str,
    separator: char,
    newest: Option<Vec<i64>>,
    now: i64,
    hits: Vec<Hit>,
    refused: Vec<(String, String, Refusal)>,
}

impl Walk<'_> {
    fn path_of(&self, index: NodeIndex) -> String {
        let mut path = self.root_path.to_owned();
        for name in self.arena.path_names(index) {
            if !path.ends_with(['/', '\\']) {
                path.push(self.separator);
            }
            path.push_str(name);
        }
        path
    }

    /// Whether `rule` hits `index`, apart from its globs.
    fn conditions_hold(&self, rule: &super::Rule, index: NodeIndex, kind: EntryKind) -> bool {
        let kind_ok = match rule.matches {
            Match::Any => true,
            Match::Dir => kind == EntryKind::Dir,
            Match::File => kind == EntryKind::File,
        };
        if !kind_ok {
            return false;
        }
        if !rule.when_child.is_empty()
            && !self
                .arena
                .children(index)
                .any(|child| name_is(self.arena, child, &rule.when_child))
        {
            return false;
        }
        if !rule.when_sibling.is_empty() {
            let parent = self.arena.node(index).map_or(NIL, |node| node.parent);
            if !self
                .arena
                .children(parent)
                .any(|sibling| sibling != index && name_is(self.arena, sibling, &rule.when_sibling))
            {
                return false;
            }
        }
        match (rule.stale_days, &self.newest) {
            (Some(days), Some(newest)) => {
                let newest = newest[index as usize];
                newest != UNKNOWN_TIME && newest <= self.now - i64::from(days) * DAY
            }
            // Without times nothing can be shown to be stale.
            (Some(_), None) => false,
            (None, _) => true,
        }
    }

    /// Record the hits at `index`; returns the rules that hit (or were
    /// refused) there, so its subtree stops looking for them.
    fn judge(
        &mut self,
        automata: &[Automaton<'_>],
        states: &States,
        index: NodeIndex,
    ) -> Vec<usize> {
        let Some(node) = self.arena.node(index) else {
            return Vec::new();
        };
        let kind = node.kind();
        if !matches!(kind, EntryKind::Dir | EntryKind::File) {
            return Vec::new();
        }
        let accepted = |exclude: bool| {
            let mut rules: Vec<usize> = states
                .iter()
                .filter(|&&(a, s)| {
                    let automaton = &automata[a as usize];
                    automaton.exclude == exclude && s as usize == automaton.glob.len()
                })
                .map(|&(a, _)| automata[a as usize].rule)
                .collect();
            rules.dedup();
            rules
        };
        let excluded = accepted(true);
        let mut done = Vec::new();
        let first = self.hits.len();
        for rule_index in accepted(false) {
            if excluded.contains(&rule_index) {
                continue;
            }
            let rule = &self.rules.rules[rule_index].rule;
            if !self.conditions_hold(rule, index, kind) {
                continue;
            }
            done.push(rule_index);
            let path = self.path_of(index);
            if rule.deletable() {
                if let Err(refusal) = self.guard.check_resolved(&path) {
                    self.refused.push((rule.id.clone(), path, refusal));
                    continue;
                }
            }
            self.hits.push(Hit {
                rule_id: rule.id.clone(),
                kind: rule.kind,
                risk: rule.risk(),
                confidence: rule.confidence(),
                deletable: rule.deletable(),
                selected: rule.starts_selected(),
                node: index,
                path,
                bytes: node.total_alloc,
                primary: false,
            });
        }
        // The most cautious hit on a node is its verdict.
        if let Some(primary) = (first..self.hits.len())
            .max_by_key(|&h| (caution(self.hits[h].risk), std::cmp::Reverse(h)))
        {
            self.hits[primary].primary = true;
        }
        done
    }
}

const fn caution(risk: Risk) -> u8 {
    match risk {
        Risk::Safe => 0,
        Risk::Review => 1,
        Risk::Keep => 2,
        Risk::Never => 3,
    }
}

/// Newest file modification time in every subtree. Children follow their
/// parents in the arena, so one backwards pass suffices.
fn newest_times(arena: &Arena, modified: &[i64]) -> Vec<i64> {
    let mut newest: Vec<i64> = (0..arena.len())
        .map(|index| modified.get(index).copied().unwrap_or(UNKNOWN_TIME))
        .collect();
    for index in (1..arena.len()).rev() {
        let parent = arena.nodes()[index].parent;
        if parent != NIL {
            newest[parent as usize] = newest[parent as usize].max(newest[index]);
        }
    }
    newest
}

/// Run `rules` over `arena`, a scan of `root_path`. `modified` is per-node
/// file mtimes (`catalog::modified_times`), needed by stale rules: without it
/// they never hit. `guard` is built for the same root; deletable hits it
/// refuses are dropped into [`Evaluation::refused`].
pub fn evaluate(
    rules: &RuleSet,
    arena: &Arena,
    root_path: &str,
    modified: Option<&[i64]>,
    now: i64,
    guard: &Guard,
) -> Evaluation {
    let automata: Vec<Automaton<'_>> = rules
        .rules
        .iter()
        .enumerate()
        .flat_map(|(rule, compiled)| {
            let globs = compiled.globs.iter().map(move |glob| Automaton {
                rule,
                exclude: false,
                glob,
            });
            let excludes = compiled.excludes.iter().map(move |glob| Automaton {
                rule,
                exclude: true,
                glob,
            });
            globs.chain(excludes)
        })
        .collect();
    let any_stale = rules
        .rules
        .iter()
        .any(|compiled| compiled.rule.stale_days.is_some());
    let mut walk = Walk {
        rules,
        arena,
        guard,
        root_path,
        separator: if rules.platform == Platform::Windows {
            '\\'
        } else {
            '/'
        },
        newest: modified
            .filter(|_| any_stale)
            .map(|times| newest_times(arena, times)),
        now,
        hits: Vec::new(),
        refused: Vec::new(),
    };

    let mut scratch = Vec::new();
    let mut states: States = Vec::new();
    for (index, automaton) in automata.iter().enumerate() {
        scratch.clear();
        automaton.glob.close(0, &mut scratch);
        states.extend(scratch.iter().map(|&s| (index as u32, s as u32)));
    }
    for name in components(root_path) {
        states = step_all(&automata, &states, name, &mut scratch);
    }

    if !arena.is_empty() {
        let mut stack = vec![(arena.root(), states)];
        while let Some((index, states)) = stack.pop() {
            let done = walk.judge(&automata, &states, index);
            let live: States = states
                .into_iter()
                .filter(|&(a, _)| !done.contains(&automata[a as usize].rule))
                .collect();
            if live.is_empty() {
                continue;
            }
            for child in arena.children(index) {
                let Some(name) = arena.name(child) else {
                    continue;
                };
                let next = step_all(&automata, &live, name, &mut scratch);
                if !next.is_empty() {
                    stack.push((child, next));
                }
            }
        }
    }

    let mut hits = walk.hits;
    let refused = walk.refused;
    // Hits in arena order, so the owner pass below sees parents first.
    hits.sort_by_key(|hit| (hit.node, hit.rule_id.clone()));

    let mut owner = vec![NIL; arena.len()];
    for (h, hit) in hits.iter().enumerate() {
        if hit.primary {
            owner[hit.node as usize] = h as u32;
        }
    }
    for index in 1..arena.len() {
        if owner[index] == NIL {
            let parent = arena.nodes()[index].parent;
            if parent != NIL {
                owner[index] = owner[parent as usize];
            }
        }
    }

    // Each byte counts once, at its nearest primary hit.
    let mut exclusive: Vec<u64> = hits.iter().map(|hit| hit.bytes).collect();
    for hit in hits.iter().filter(|hit| hit.primary) {
        let parent = arena.nodes()[hit.node as usize].parent;
        if parent != NIL && owner[parent as usize] != NIL {
            let outer = owner[parent as usize] as usize;
            exclusive[outer] = exclusive[outer].saturating_sub(hit.bytes);
        }
    }
    let mut reclaim = Reclaim::default();
    for (hit, bytes) in hits.iter().zip(&exclusive) {
        if !hit.primary || !hit.deletable {
            continue;
        }
        let value = *bytes as f64 * hit.confidence * risk_weight(hit.risk);
        match hit.risk {
            Risk::Safe => reclaim.safe += value,
            _ => reclaim.review += value,
        }
    }

    Evaluation {
        hits,
        refused,
        reclaim,
        owner,
    }
}
