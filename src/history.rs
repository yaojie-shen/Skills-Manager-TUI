//! What the session has done, and how to take it back.
//!
//! An entry records the *intent* of an operation, never the changes it made.
//! Undo re-derives the opposite intent against the filesystem as it stands now,
//! so anything altered in the meantime is handled by the same rules that guard
//! ordinary operations: a link the user already removed is simply not there to
//! remove again, and a directory the agent replaced by hand is left alone.
//!
//! Link operations are recorded as the exact skill-and-agent pairs they
//! touched rather than as the preset or sync that produced them. Undoing then
//! reverses precisely what happened, and does not change meaning if the preset
//! behind it is edited or deleted afterwards.
//!
//! The history lives only as long as the process. Replaying an intent against a
//! tree that git or another session has moved on is not something a stored log
//! could make safe, and this tool keeps no database.

use crate::Workspace;
use crate::ops::deploy::{self, Action};
use crate::reconcile::Snapshot;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::time::Instant;

/// One link: a skill in one agent's directory.
pub type Pair = (String, String);

#[derive(Debug, Clone)]
pub enum Intent {
    /// Links created and removed together, as one batch: deploy, undeploy,
    /// sync, a preset switched either way, or a whole-directory conversion.
    Links {
        added: Vec<Pair>,
        removed: Vec<Pair>,
    },
    /// A skill fetched into the root. Undoing removes what was fetched.
    Install { skill: String },
    /// Something that discarded content and so cannot be taken back. Kept so
    /// the log stays honest about everything that happened.
    OneWay { what: String },
}

impl Intent {
    /// Build from what a batch of link actions actually did. Returns `None`
    /// when the batch changed nothing worth remembering.
    pub fn from_actions(actions: &[Action]) -> Option<Intent> {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        for a in actions {
            match a {
                Action::Link { skill, agent, .. } => added.push((skill.clone(), agent.clone())),
                Action::Unlink { skill, agent, .. } => removed.push((skill.clone(), agent.clone())),
                Action::Skip { .. } | Action::Mkdir { .. } => {}
            }
        }
        (!added.is_empty() || !removed.is_empty()).then_some(Intent::Links { added, removed })
    }

    /// A short line for the history list.
    pub fn describe(&self) -> String {
        match self {
            Intent::Links { added, removed } => {
                let mut parts = Vec::new();
                if !added.is_empty() {
                    parts.push(format!("added {}", pairs(added)));
                }
                if !removed.is_empty() {
                    parts.push(format!("removed {}", pairs(removed)));
                }
                parts.join("; ")
            }
            Intent::Install { skill } => format!("installed {skill}"),
            Intent::OneWay { what } => what.clone(),
        }
    }

    /// Whether taking this back is possible at all. Deleting a skill and
    /// updating one to a new revision both discard the old contents, and this
    /// tool keeps no copies of them.
    pub fn reversible(&self) -> bool {
        !matches!(self, Intent::OneWay { .. })
    }
}

/// `alpha and beta in claude`, counting once the list stops being worth reading.
fn pairs(items: &[Pair]) -> String {
    let mut skills: Vec<&str> = Vec::new();
    let mut agents: Vec<&str> = Vec::new();
    for (s, a) in items {
        if !skills.contains(&s.as_str()) {
            skills.push(s);
        }
        if !agents.contains(&a.as_str()) {
            agents.push(a);
        }
    }
    format!(
        "{} in {}",
        list(&skills, 3, "skills"),
        list(&agents, 2, "agents")
    )
}

fn list(items: &[&str], max: usize, plural: &str) -> String {
    match items.len() {
        0 => "nothing".into(),
        n if n > max => format!("{n} {plural}"),
        1 => items[0].into(),
        2 => format!("{} and {}", items[0], items[1]),
        _ => {
            let (last, rest) = items.split_last().unwrap();
            format!("{} and {last}", rest.join(", "))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub intent: Intent,
    pub at: Instant,
}

/// What undoing or redoing an entry would do, worked out against the tree.
pub enum Plan {
    /// Link changes to confirm and apply.
    Links(Vec<Action>),
    /// A write, described for the confirmation.
    Write { describe: String, apply: WriteBack },
    /// The tree already looks the way the step would leave it.
    Nothing,
}

/// A non-link step to run. Carrying the values rather than a closure keeps the
/// plan inspectable, which the confirmation needs.
#[derive(Debug, Clone)]
pub enum WriteBack {
    RemoveInstalled { skill: String },
}

impl WriteBack {
    pub fn apply(&self, ws: &Workspace) -> Result<String> {
        match self {
            WriteBack::RemoveInstalled { skill } => {
                let snap = ws.scan()?;
                crate::ops::edit::remove(ws, &snap, skill, false)
                    .map(|_| format!("removed {skill} again"))
            }
        }
    }
}

/// The session's log. Undone steps move to a redo stack, so a step is never
/// both done and undone at once. That is what makes repeated undo safe.
#[derive(Debug, Default)]
pub struct History {
    done: Vec<Entry>,
    undone: Vec<Entry>,
}

impl History {
    /// Record something that happened. Anything previously undone is dropped:
    /// a new action makes that branch of the past unreachable, as in an editor.
    pub fn record(&mut self, intent: Intent) {
        self.undone.clear();
        self.done.push(Entry {
            intent,
            at: Instant::now(),
        });
    }

    /// Newest first, for display.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.done.iter().rev()
    }

    pub fn is_empty(&self) -> bool {
        self.done.is_empty()
    }

    pub fn last(&self) -> Option<&Entry> {
        self.done.last()
    }

    pub fn next_redo(&self) -> Option<&Entry> {
        self.undone.last()
    }

    /// Move the newest step onto the redo stack. Call once its plan has been
    /// applied, so a cancelled confirmation leaves the history untouched.
    pub fn commit_undo(&mut self) {
        if let Some(e) = self.done.pop() {
            self.undone.push(e);
        }
    }

    pub fn commit_redo(&mut self) {
        if let Some(e) = self.undone.pop() {
            self.done.push(e);
        }
    }

    /// Drop the newest step without reversing it, for one that turned out to
    /// need nothing done.
    pub fn discard_last(&mut self) {
        self.done.pop();
    }
}

/// Plan the links needed to reach a set of pairs: `add` are wanted, `remove`
/// are not. Grouping by agent lets the ordinary planners apply their guards, so
/// an entry the agent now owns is skipped here exactly as it would be anywhere.
fn plan_pairs(
    ws: &Workspace,
    snap: &Snapshot,
    add: &[Pair],
    remove: &[Pair],
) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    for (pairs, deploying) in [(add, true), (remove, false)] {
        let mut by_agent: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (skill, agent) in pairs {
            by_agent.entry(agent).or_default().push(skill.clone());
        }
        for (agent, skills) in by_agent {
            // An agent dropped from the config since cannot be planned against.
            if ws.config.agent(agent).is_none() {
                continue;
            }
            let scope = [agent.to_string()];
            actions.extend(if deploying {
                deploy::plan_deploy(ws, snap, &skills, &scope)?
            } else {
                deploy::plan_undeploy(ws, snap, &skills, &scope)?
            });
        }
    }
    Ok(actions)
}

/// Work out what undoing `intent` would do, against the tree as it is now.
pub fn undo_plan(ws: &Workspace, snap: &Snapshot, intent: &Intent) -> Result<Plan> {
    match intent {
        // Reversed: what was added comes out, what was removed goes back.
        Intent::Links { added, removed } => links(plan_pairs(ws, snap, removed, added)?),
        Intent::Install { skill } => {
            if snap.get(skill).is_none() {
                return Ok(Plan::Nothing);
            }
            Ok(Plan::Write {
                describe: format!("remove {skill} and every link to it"),
                apply: WriteBack::RemoveInstalled {
                    skill: skill.clone(),
                },
            })
        }
        Intent::OneWay { what } => bail!("{what} cannot be taken back"),
    }
}

/// Redoing runs the original intent again.
pub fn redo_plan(ws: &Workspace, snap: &Snapshot, intent: &Intent) -> Result<Plan> {
    match intent {
        Intent::Links { added, removed } => links(plan_pairs(ws, snap, added, removed)?),
        // Fetching is a fresh network operation, not a reversal of a removal.
        Intent::Install { skill } => bail!("install {skill} again to bring it back"),
        Intent::OneWay { what } => bail!("{what} cannot be redone"),
    }
}

/// An empty plan means the tree already matches; say so rather than opening an
/// empty confirmation.
fn links(actions: Vec<Action>) -> Result<Plan> {
    if actions.iter().any(|a| a.is_change()) {
        Ok(Plan::Links(actions))
    } else {
        Ok(Plan::Nothing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn link(skill: &str, agent: &str) -> Action {
        Action::Link {
            agent: agent.into(),
            skill: skill.into(),
            path: PathBuf::new(),
            target: PathBuf::new(),
        }
    }
    fn unlink(skill: &str, agent: &str) -> Action {
        Action::Unlink {
            agent: agent.into(),
            skill: skill.into(),
            path: PathBuf::new(),
        }
    }
    fn install(name: &str) -> Intent {
        Intent::Install { skill: name.into() }
    }

    #[test]
    fn an_intent_records_the_pairs_a_batch_touched() {
        let intent = Intent::from_actions(&[
            link("alpha", "claude"),
            link("alpha", "codex"),
            unlink("beta", "claude"),
            Action::Skip {
                agent: "claude".into(),
                skill: "gamma".into(),
                reason: "shadow".into(),
            },
        ])
        .unwrap();
        let Intent::Links { added, removed } = &intent else {
            panic!("expected links")
        };
        assert_eq!(
            added.len(),
            2,
            "skipped entries are not part of what happened"
        );
        assert_eq!(removed, &[("beta".to_string(), "claude".to_string())]);
        assert_eq!(
            intent.describe(),
            "added alpha in claude and codex; removed beta in claude"
        );
    }

    #[test]
    fn a_batch_that_changed_nothing_is_not_recorded() {
        assert!(Intent::from_actions(&[]).is_none());
        assert!(
            Intent::from_actions(&[Action::Skip {
                agent: "a".into(),
                skill: "s".into(),
                reason: "already deployed".into(),
            }])
            .is_none()
        );
    }

    #[test]
    fn a_new_action_makes_the_undone_branch_unreachable() {
        let mut h = History::default();
        h.record(install("a"));
        h.record(install("b"));
        h.commit_undo();
        assert!(h.next_redo().is_some());
        assert_eq!(h.done.len(), 1);

        // Doing something new drops what was waiting to be redone, so the log
        // never offers a redo that no longer follows from the current state.
        h.record(install("c"));
        assert!(h.next_redo().is_none());
        assert_eq!(h.done.len(), 2);
    }

    #[test]
    fn undo_and_redo_move_the_same_entry_between_stacks() {
        let mut h = History::default();
        h.record(install("a"));
        h.commit_undo();
        assert!(h.is_empty());
        assert!(h.next_redo().is_some());
        h.commit_redo();
        assert!(!h.is_empty());
        assert!(h.next_redo().is_none());
    }

    #[test]
    fn undoing_past_the_start_is_not_an_error() {
        let mut h = History::default();
        h.commit_undo();
        h.commit_redo();
        assert!(h.is_empty());
        assert!(h.last().is_none());
    }

    #[test]
    fn descriptions_count_once_a_list_stops_being_readable() {
        let many: Vec<Pair> = ["a", "b", "c", "d"]
            .iter()
            .map(|s| (s.to_string(), "claude".to_string()))
            .collect();
        assert_eq!(
            Intent::Links {
                added: many,
                removed: vec![],
            }
            .describe(),
            "added 4 skills in claude"
        );
        assert!(
            !Intent::OneWay {
                what: "deleted x".into()
            }
            .reversible()
        );
    }
}
