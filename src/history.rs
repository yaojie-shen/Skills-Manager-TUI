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
//! Metadata writes cannot be re-derived that way. A link that was removed can
//! be put back because the skill and the agent directory between them still say
//! what it was; a note that was overwritten exists nowhere else the moment the
//! file is saved. So a metadata entry carries the values themselves — for tags
//! and preset members the two sets the write added and took off, for a note, a
//! source or a preset description the value on either side — and only the
//! *reversal* is re-derived against the file as it stands. Sets are undone
//! element by element, so a tag added by hand in between survives, and undoing
//! "add a tag that was already there" removes nothing, because adding it
//! changed nothing to begin with. A note, a source or a description has no
//! such structure: it is put back only while the file still holds what this
//! step wrote, and anything else means someone edited it since, so it is left
//! alone.
//!
//! The history lives only as long as the process. Replaying an intent against a
//! tree that git or another session has moved on is not something a stored log
//! could make safe, and this tool keeps no database.

use crate::Workspace;
use crate::config::Config;
use crate::meta;
use crate::ops::deploy::{self, Action};
use crate::ops::edit;
use crate::ops::install::{self, InstallRef};
use crate::preset;
use crate::reconcile::Snapshot;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// One link: a skill in one agent's directory.
pub type Pair = (String, String);

#[derive(Debug, Clone)]
pub enum Intent {
    Group(Vec<Intent>),
    TargetSelection {
        agent: crate::config::AgentConfig,
        project: Option<std::path::PathBuf>,
        before: crate::ops::targets::Selection,
        after: crate::ops::targets::Selection,
    },
    /// Links created and removed together, as one batch: deploy, undeploy,
    /// sync, a preset switched either way, or a whole-directory conversion.
    Links {
        added: Vec<Pair>,
        removed: Vec<Pair>,
    },
    /// Metadata written: tags, a note, preset membership. One batch, because a
    /// tag renamed or deleted touches every skill that carried it and comes
    /// back as one step.
    Meta(Vec<MetaChange>),
    /// A skill fetched into the root. Undoing removes what was fetched.
    Install {
        skill: String,
    },
    /// A skill moved to a new name: directory, metadata, every link and every
    /// preset that named it. Undoing is the same move the other way, planned
    /// afresh when it is asked for, so a skill gone or a name taken in the
    /// meantime stops it rather than being written over.
    Rename {
        from: String,
        to: String,
    },
    /// A preset moved to a new name: its file, and its entry in the
    /// auto-deploy list if it had one. Undone the way a skill rename is, by
    /// planning the move back when it is asked for, so the old name being
    /// taken since stops it.
    PresetRename {
        from: String,
        to: String,
    },
    /// Something that discarded content and so cannot be taken back. Kept so
    /// the log stays honest about everything that happened.
    OneWay {
        what: String,
    },
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
                // A relink deleted a directory. Taking the link out again would
                // not bring the directory back, and the link planners have no
                // way to say "a real directory with this content"; a step that
                // only half reverses is worse than none, so it is not recorded.
                Action::Relink { .. } | Action::Skip { .. } | Action::Mkdir { .. } => {}
            }
        }
        (!added.is_empty() || !removed.is_empty()).then_some(Intent::Links { added, removed })
    }

    /// A short line for the history list.
    pub fn describe(&self) -> String {
        match self {
            Intent::Group(intents) => intents
                .iter()
                .map(Intent::describe)
                .collect::<Vec<_>>()
                .join("; "),
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
            Intent::TargetSelection { agent, .. } => {
                format!("changed installations in {}", agent.skills_dir)
            }
            Intent::Meta(changes) => describe_changes(changes),
            Intent::Install { skill } => format!("installed {skill}"),
            Intent::Rename { from, to } => format!("renamed {from} to {to}"),
            Intent::PresetRename { from, to } => format!("renamed preset {from} to {to}"),
            Intent::OneWay { what } => what.clone(),
        }
    }

    /// Whether taking this back is possible at all. Deleting a skill and
    /// updating one to a new revision both discard the old contents, and this
    /// tool keeps no copies of them.
    pub fn reversible(&self) -> bool {
        match self {
            Intent::Group(intents) => intents.iter().all(Intent::reversible),
            _ => !matches!(self, Intent::OneWay { .. }),
        }
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

/// One metadata write, as the difference it made. Reversing it swaps the two
/// sides; applying it means "leave the file holding this", which is why the
/// same value works for undo and for redo.
#[derive(Debug, Clone, PartialEq)]
pub enum MetaChange {
    /// Tags of one skill: what the write put on it and what it took off.
    Tags {
        skill: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// The note of one skill. `None` on either side is no note at all, which is
    /// a value like any other and has to be recorded as one.
    Note {
        skill: String,
        before: Option<String>,
        after: Option<String>,
    },
    /// The recorded source of one skill. Like a note it is one value with no
    /// structure to merge, so it goes back only while the file still holds
    /// what this step wrote. The whole value travels, revision included: a
    /// reference alone would come back without the revision the install had
    /// recorded, and the next check would call the skill out of date.
    Source {
        skill: String,
        before: Option<meta::Source>,
        after: Option<meta::Source>,
    },
    /// A tag's `[[tags]]` entry in the config (its colour, its description)
    /// moved to a new name by a rename. Recorded only for a plain rename: a
    /// merge drops the old entry in favour of the target's, and moving the
    /// target's entry "back" would steal it.
    TagEntry { from: String, to: String },
    /// Members of one preset.
    Preset {
        name: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// The description of one preset: one sentence with no structure to
    /// merge, so it is handled exactly as a note is, with `None` for no
    /// description at all.
    PresetDescription {
        name: String,
        before: Option<String>,
        after: Option<String>,
    },
}

/// What applying one change would do to the files as they stand now.
enum Fate {
    /// The file already reads the way applying it would leave it.
    Done,
    /// Applying it would discard an edit made since, so it is not applied.
    Blocked(String),
    Ready,
}

impl MetaChange {
    fn reverse(&self) -> MetaChange {
        match self.clone() {
            MetaChange::Tags {
                skill,
                added,
                removed,
            } => MetaChange::Tags {
                skill,
                added: removed,
                removed: added,
            },
            MetaChange::TagEntry { from, to } => MetaChange::TagEntry { from: to, to: from },
            MetaChange::Note {
                skill,
                before,
                after,
            } => MetaChange::Note {
                skill,
                before: after,
                after: before,
            },
            MetaChange::Source {
                skill,
                before,
                after,
            } => MetaChange::Source {
                skill,
                before: after,
                after: before,
            },
            MetaChange::Preset {
                name,
                added,
                removed,
            } => MetaChange::Preset {
                name,
                added: removed,
                removed: added,
            },
            MetaChange::PresetDescription {
                name,
                before,
                after,
            } => MetaChange::PresetDescription {
                name,
                before: after,
                after: before,
            },
        }
    }

    /// One line, phrased as the change to make. It reads as the question a
    /// confirmation asks and as the line the log shows.
    fn describe(&self) -> String {
        match self {
            MetaChange::Tags {
                skill,
                added,
                removed,
            } => match (added.is_empty(), removed.is_empty()) {
                (false, true) => format!("tag {skill} with {}", some(added, 3, "tags")),
                (true, false) => format!("take {} off {skill}", some(removed, 3, "tags")),
                _ => format!("change the tags on {skill}"),
            },
            // The text goes in the line: one note looks much like another, and
            // this is what a confirmation is for.
            MetaChange::TagEntry { from, to } => {
                format!("move the [[tags]] entry {from} to {to}")
            }
            MetaChange::Note { skill, after, .. } => match after {
                Some(t) => format!("set the note on {skill} to \"{}\"", snippet(t)),
                None => format!("clear the note on {skill}"),
            },
            MetaChange::Source { skill, after, .. } => match after {
                Some(s) => format!("set the source of {skill} to {}", s.summary()),
                None => format!("clear the source of {skill}"),
            },
            MetaChange::Preset {
                name,
                added,
                removed,
            } => match (added.is_empty(), removed.is_empty()) {
                (false, true) => format!("put {} in preset {name}", some(added, 3, "skills")),
                (true, false) => {
                    format!("take {} out of preset {name}", some(removed, 3, "skills"))
                }
                _ => format!("change the members of preset {name}"),
            },
            MetaChange::PresetDescription { name, after, .. } => match after {
                Some(t) => format!("set the description of preset {name} to \"{}\"", snippet(t)),
                None => format!("clear the description of preset {name}"),
            },
        }
    }

    fn fate(&self, ws: &Workspace) -> Result<Fate> {
        match self {
            MetaChange::Tags {
                skill,
                added,
                removed,
            } => match current_tags(ws, skill)? {
                None => Ok(Fate::Blocked(format!("{skill} is gone"))),
                Some(now) => Ok(
                    if added.iter().any(|t| !now.contains(t))
                        || removed.iter().any(|t| now.contains(t))
                    {
                        Fate::Ready
                    } else {
                        Fate::Done
                    },
                ),
            },
            MetaChange::TagEntry { from, to } => {
                let names: Vec<String> = Config::load(&ws.root)?
                    .tags
                    .into_iter()
                    .map(|t| t.name)
                    .collect();
                Ok(match (names.contains(from), names.contains(to)) {
                    (true, false) => Fate::Ready,
                    (false, true) => Fate::Done,
                    (false, false) => {
                        Fate::Blocked(format!("no [[tags]] entry for {from} any more"))
                    }
                    (true, true) => {
                        Fate::Blocked(format!("{to} now has a [[tags]] entry of its own"))
                    }
                })
            }
            MetaChange::Note {
                skill,
                before,
                after,
            } => match current_note(ws, skill)? {
                None => Ok(Fate::Blocked(format!("{skill} is gone"))),
                Some(now) if &now == after => Ok(Fate::Done),
                Some(now) if &now == before => Ok(Fate::Ready),
                Some(_) => Ok(Fate::Blocked(format!(
                    "the note on {skill} was changed since"
                ))),
            },
            MetaChange::Source {
                skill,
                before,
                after,
            } => match current_source(ws, skill)? {
                None => Ok(Fate::Blocked(format!("{skill} is gone"))),
                Some(now) if &now == after => Ok(Fate::Done),
                Some(now) if &now == before => Ok(Fate::Ready),
                Some(_) => Ok(Fate::Blocked(format!(
                    "the source of {skill} was changed since"
                ))),
            },
            MetaChange::Preset {
                name,
                added,
                removed,
            } => match ws.presets.load(name)? {
                None => Ok(Fate::Blocked(format!("preset {name} is gone"))),
                Some(p) => Ok(
                    if added.iter().any(|s| !p.skills.contains(s))
                        || removed.iter().any(|s| p.skills.contains(s))
                    {
                        Fate::Ready
                    } else {
                        Fate::Done
                    },
                ),
            },
            MetaChange::PresetDescription {
                name,
                before,
                after,
            } => match ws.presets.load(name)? {
                None => Ok(Fate::Blocked(format!("preset {name} is gone"))),
                Some(p) if &p.description == after => Ok(Fate::Done),
                Some(p) if &p.description == before => Ok(Fate::Ready),
                Some(_) => Ok(Fate::Blocked(format!(
                    "the description of preset {name} was changed since"
                ))),
            },
        }
    }

    /// Leave the file holding this change. The fate is worked out again here
    /// because the confirmation the user answered was drawn a moment ago.
    fn apply(&self, ws: &Workspace) -> Result<String> {
        match self.fate(ws)? {
            Fate::Done => return Ok(format!("already done: {}", self.describe())),
            Fate::Blocked(why) => return Ok(format!("{why}; left as it is")),
            Fate::Ready => {}
        }
        match self {
            MetaChange::Tags {
                skill,
                added,
                removed,
            } => {
                let mut tags = Config::load(&ws.root)?.skill_tags(skill);
                tags.retain(|t| !removed.contains(t));
                tags.extend(added.iter().filter(|t| !removed.contains(t)).cloned());
                let tags = edit::tag_set(ws, skill, &tags)?;
                Ok(format!("{skill}: {}", tags.join(", ")))
            }
            MetaChange::TagEntry { from, to } => {
                Config::rename_tag_entry(&ws.root, from, to)?;
                Ok(format!("[[tags]] entry {from} moved to {to}"))
            }
            MetaChange::Note { skill, after, .. } => {
                edit::note_set(ws, skill, after.as_deref())?;
                Ok(format!(
                    "note {} on {skill}",
                    if after.is_some() {
                        "restored"
                    } else {
                        "cleared"
                    }
                ))
            }
            // Written as the value, not through `install::set_source`: that
            // takes a reference and records no revision, and this has to put
            // back exactly what was there.
            MetaChange::Source { skill, after, .. } => {
                let mut meta = edit::load_or_init(ws, skill)?;
                meta.source = after.clone();
                ws.meta.save(skill, &meta)?;
                Ok(format!(
                    "source {} on {skill}",
                    if after.is_some() {
                        "restored"
                    } else {
                        "cleared"
                    }
                ))
            }
            MetaChange::Preset {
                name,
                added,
                removed,
            } => {
                let mut p = ws
                    .presets
                    .load(name)?
                    .with_context(|| format!("no such preset: {name}"))?;
                p.skills.retain(|s| !removed.contains(s));
                let before = p.skills.len();
                for s in added {
                    if !p.skills.contains(s) {
                        p.skills.push(s.clone());
                    }
                }
                if p.skills.len() != before {
                    p.skills.sort();
                }
                ws.presets.save(&p)?;
                Ok(format!("{name}: {} skill(s)", p.skills.len()))
            }
            MetaChange::PresetDescription { name, after, .. } => {
                let mut p = ws
                    .presets
                    .load(name)?
                    .with_context(|| format!("no such preset: {name}"))?;
                p.description = after.clone();
                ws.presets.save(&p)?;
                Ok(format!(
                    "description {} on preset {name}",
                    if after.is_some() {
                        "restored"
                    } else {
                        "cleared"
                    }
                ))
            }
        }
    }
}

/// The tags a skill carries now, or `None` if neither its metadata nor its
/// directory is there any more.
fn current_tags(ws: &Workspace, skill: &str) -> Result<Option<Vec<String>>> {
    let tags = Config::load(&ws.root)?.skill_tags(skill);
    Ok((ws.skill_path(skill).is_dir() || !tags.is_empty()).then_some(tags))
}

/// The note a skill carries now. The outer `None` is "no such skill", the inner
/// one "no note".
fn current_note(ws: &Workspace, skill: &str) -> Result<Option<Option<String>>> {
    match ws.meta.load(skill).ok().flatten() {
        Some(m) => Ok(Some(m.note)),
        None => Ok(ws.skill_path(skill).is_dir().then_some(None)),
    }
}

/// The source a skill records now, with the same two layers as `current_note`.
fn current_source(ws: &Workspace, skill: &str) -> Result<Option<Option<meta::Source>>> {
    match ws.meta.load(skill).ok().flatten() {
        Some(m) => Ok(Some(m.source)),
        None => Ok(ws.skill_path(skill).is_dir().then_some(None)),
    }
}

fn describe_changes(changes: &[MetaChange]) -> String {
    match changes {
        [] => "nothing".into(),
        [one] => one.describe(),
        many if many.iter().all(|c| matches!(c, MetaChange::Tags { .. })) => {
            format!("change the tags on {} skills", many.len())
        }
        many => format!("{} metadata changes", many.len()),
    }
}

/// The opening of a note, on one line, for a dialog that has room for little.
fn snippet(note: &str) -> String {
    let first = note.lines().map(str::trim).find(|l| !l.is_empty());
    let first = first.unwrap_or("");
    match first.char_indices().nth(40) {
        Some((i, _)) => format!("{}…", &first[..i]),
        None if first.len() < note.trim().len() => format!("{first}…"),
        None => first.to_string(),
    }
}

fn some(items: &[String], max: usize, plural: &str) -> String {
    let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    list(&refs, max, plural)
}

/// Run a write that may change the tags of any number of skills, and record the
/// per-skill differences it left behind. Comparing every metadata file on both
/// sides costs nothing next to the rescan that follows, and it covers a rename
/// or a delete spanning many skills with the same code as a single edit.
pub fn tag_edit(
    ws: &Workspace,
    write: impl FnOnce(&Workspace) -> Result<String>,
) -> Result<(String, Option<Intent>)> {
    let before = all_tags(ws)?;
    let entries_before = tag_entry_names(ws)?;
    let message = write(ws)?;
    let after = all_tags(ws)?;
    let entries_after = tag_entry_names(ws)?;
    let mut changes = Vec::new();
    let gone: Vec<&String> = entries_before
        .iter()
        .filter(|n| !entries_after.contains(n))
        .collect();
    let came: Vec<&String> = entries_after
        .iter()
        .filter(|n| !entries_before.contains(n))
        .collect();
    if let ([from], [to]) = (gone.as_slice(), came.as_slice()) {
        changes.push(MetaChange::TagEntry {
            from: (*from).clone(),
            to: (*to).clone(),
        });
    }
    let empty = Vec::new();
    for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        let (b, a) = (
            before.get(key).unwrap_or(&empty),
            after.get(key).unwrap_or(&empty),
        );
        let added: Vec<String> = a.iter().filter(|t| !b.contains(t)).cloned().collect();
        let removed: Vec<String> = b.iter().filter(|t| !a.contains(t)).cloned().collect();
        if !added.is_empty() || !removed.is_empty() {
            changes.push(MetaChange::Tags {
                skill: key.clone(),
                added,
                removed,
            });
        }
    }
    Ok((
        message,
        (!changes.is_empty()).then_some(Intent::Meta(changes)),
    ))
}

fn tag_entry_names(ws: &Workspace) -> Result<Vec<String>> {
    Ok(Config::load(&ws.root)?
        .tags
        .into_iter()
        .map(|t| t.name)
        .collect())
}

fn all_tags(ws: &Workspace) -> Result<BTreeMap<String, Vec<String>>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for tag in Config::load(&ws.root)?.tags {
        for key in tag.skills {
            out.entry(key).or_default().push(tag.name.clone());
        }
    }
    Ok(out)
}

/// Write a note, keeping the text it replaced.
pub fn note_edit(
    ws: &Workspace,
    skill: &str,
    text: Option<&str>,
) -> Result<(String, Option<Intent>)> {
    let before = ws.meta.load(skill).ok().flatten().and_then(|m| m.note);
    let after = edit::note_set(ws, skill, text)?.note;
    let message = format!(
        "note {} on {skill}",
        if after.is_some() { "saved" } else { "cleared" }
    );
    let intent = (after != before).then(|| {
        Intent::Meta(vec![MetaChange::Note {
            skill: skill.to_string(),
            before,
            after,
        }])
    });
    Ok((message, intent))
}

/// Point a skill at a new source, keeping the one it replaced.
pub fn source_edit(
    ws: &Workspace,
    skill: &str,
    r: &InstallRef,
) -> Result<(String, Option<Intent>)> {
    let before = ws.meta.load(skill).ok().flatten().and_then(|m| m.source);
    let after = install::set_source(ws, skill, r)?.source;
    let message = format!(
        "source of {skill} set to {}",
        after.as_ref().map(|s| s.summary()).unwrap_or_default()
    );
    let intent = (after != before).then(|| {
        Intent::Meta(vec![MetaChange::Source {
            skill: skill.to_string(),
            before,
            after,
        }])
    });
    Ok((message, intent))
}

/// Change the membership of a preset, recording which skills went in and out.
pub fn preset_edit(
    ws: &Workspace,
    name: &str,
    change: impl FnOnce(&mut Vec<String>),
) -> Result<(String, Option<Intent>)> {
    let mut p = ws
        .presets
        .load(name)?
        .with_context(|| format!("no such preset: {name}"))?;
    let before = p.skills.clone();
    change(&mut p.skills);
    ws.presets.save(&p)?;
    let added: Vec<String> = p
        .skills
        .iter()
        .filter(|s| !before.contains(s))
        .cloned()
        .collect();
    let removed: Vec<String> = before
        .iter()
        .filter(|s| !p.skills.contains(s))
        .cloned()
        .collect();
    let message = match (added.as_slice(), removed.as_slice()) {
        ([], []) => return Ok((format!("{name} unchanged"), None)),
        ([s], []) => format!("added {s} to {name}"),
        ([], [s]) => format!("removed {s} from {name}"),
        _ => format!("{name}: {} skill(s)", p.skills.len()),
    };
    Ok((
        message,
        Some(Intent::Meta(vec![MetaChange::Preset {
            name: name.to_string(),
            added,
            removed,
        }])),
    ))
}

/// Write a preset's description, keeping the text it replaced. An empty
/// string is no description: the field is one sentence or nothing, and a
/// blank one would show as a blank line on the card.
pub fn preset_description_edit(
    ws: &Workspace,
    name: &str,
    text: Option<&str>,
) -> Result<(String, Option<Intent>)> {
    let mut p = ws
        .presets
        .load(name)?
        .with_context(|| format!("no such preset: {name}"))?;
    let before = p.description.clone();
    let after = text
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    if after == before {
        return Ok((format!("description of {name} unchanged"), None));
    }
    p.description = after.clone();
    ws.presets.save(&p)?;
    let message = format!(
        "description {} on {name}",
        if after.is_some() { "saved" } else { "cleared" }
    );
    Ok((
        message,
        Some(Intent::Meta(vec![MetaChange::PresetDescription {
            name: name.to_string(),
            before,
            after,
        }])),
    ))
}

/// Move a preset to a new name, taking its auto-deploy entry with it, and
/// record the move. The two writes are not one transaction; the file is
/// moved first because a rename that fails there has changed nothing, and a
/// config that then cannot be rewritten is reported with the preset already
/// under its new name, which the message says.
pub fn preset_rename(ws: &Workspace, from: &str, to: &str) -> Result<(String, Option<Intent>)> {
    ws.presets.rename(from, to)?;
    crate::ops::targets::rename_preset_reference(ws, from, to)?;
    let listed = preset::rename_deploy_reference(&ws.root, from, to)
        .with_context(|| format!("preset renamed to {to}, but its auto-deploy entry was not"))?;
    let message = if listed {
        format!("renamed preset {from} to {to}, config.toml too")
    } else {
        format!("renamed preset {from} to {to}")
    };
    Ok((
        message,
        Some(Intent::PresetRename {
            from: from.to_string(),
            to: to.to_string(),
        }),
    ))
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
    /// Nothing left to do, and why: the tree already looks the way the step
    /// would leave it, or something changed since that must not be overwritten.
    Nothing(String),
}

/// A non-link step to run. Carrying the values rather than a closure keeps the
/// plan inspectable, which the confirmation needs.
#[derive(Debug, Clone)]
pub enum WriteBack {
    Group(Vec<WriteBack>),
    TargetSelection {
        agent: crate::config::AgentConfig,
        project: Option<std::path::PathBuf>,
        expected: crate::ops::targets::Selection,
        desired: crate::ops::targets::Selection,
    },
    RemoveInstalled {
        skill: String,
    },
    /// Metadata to write, already turned the way this step runs it.
    Meta(Vec<MetaChange>),
    /// A skill to move, already turned the way this step runs it.
    Rename {
        from: String,
        to: String,
    },
    /// A preset to move, likewise.
    PresetRename {
        from: String,
        to: String,
    },
}

impl WriteBack {
    pub fn apply(&self, ws: &Workspace) -> Result<String> {
        match self {
            WriteBack::Group(writes) => writes
                .iter()
                .map(|write| write.apply(ws))
                .collect::<Result<Vec<_>>>()
                .map(|messages| messages.join("; ")),
            WriteBack::TargetSelection {
                agent,
                project,
                expected,
                desired,
            } => crate::ops::targets::restore_selection(
                ws,
                agent,
                project.as_deref(),
                expected,
                desired,
            ),
            WriteBack::RemoveInstalled { skill } => {
                let snap = ws.scan()?;
                edit::remove(ws, &snap, skill, false).map(|_| format!("removed {skill} again"))
            }
            // The links to move are read off a fresh scan, as everything the
            // rename touches is, since the plan the user confirmed was drawn
            // a moment ago.
            WriteBack::Rename { from, to } => {
                let snap = ws.scan()?;
                edit::rename(ws, &snap, from, to).map(|_| format!("renamed {from} to {to}"))
            }
            WriteBack::PresetRename { from, to } => {
                preset_rename(ws, from, to).map(|(message, _)| message)
            }
            WriteBack::Meta(changes) => {
                let mut done = Vec::new();
                for c in changes {
                    done.push(c.apply(ws)?);
                }
                Ok(done.join("; "))
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
            if !deploying {
                actions.extend(deploy::plan_undeploy(ws, snap, &skills, &scope)?);
                continue;
            }
            // To the deploy planner a name the snapshot has no record of is a
            // caller's mistake, and it stops. Here it means the skill left the
            // root, metadata and all, after the step was taken: putting a link
            // back is off the table, and that is a reason to skip, not to stop.
            let (known, gone): (Vec<String>, Vec<String>) =
                skills.into_iter().partition(|s| snap.get(s).is_some());
            for skill in gone {
                actions.push(Action::Skip {
                    agent: agent.to_string(),
                    skill,
                    reason: "not in the skills root".into(),
                });
            }
            if !known.is_empty() {
                actions.extend(deploy::plan_deploy(ws, snap, &known, &scope)?);
            }
        }
    }
    Ok(actions)
}

fn group_plan(ws: &Workspace, snap: &Snapshot, intents: &[Intent], undo: bool) -> Result<Plan> {
    let mut writes = Vec::new();
    let ordered: Vec<_> = if undo {
        intents.iter().rev().collect()
    } else {
        intents.iter().collect()
    };
    for intent in ordered {
        match if undo {
            undo_plan(ws, snap, intent)?
        } else {
            redo_plan(ws, snap, intent)?
        } {
            Plan::Write { apply, .. } => writes.push(apply),
            Plan::Nothing(_) => {}
            Plan::Links(_) => bail!("mixed history groups are not supported"),
        }
    }
    Ok(Plan::Write {
        describe: "restore deployment selections".into(),
        apply: WriteBack::Group(writes),
    })
}

/// Work out what undoing `intent` would do, against the tree as it is now.
pub fn undo_plan(ws: &Workspace, snap: &Snapshot, intent: &Intent) -> Result<Plan> {
    match intent {
        Intent::Group(intents) => group_plan(ws, snap, intents, true),
        // Reversed: what was added comes out, what was removed goes back.
        Intent::TargetSelection {
            agent,
            project,
            before,
            after,
        } => Ok(Plan::Write {
            describe: intent.describe(),
            apply: WriteBack::TargetSelection {
                agent: agent.clone(),
                project: project.clone(),
                expected: after.clone(),
                desired: before.clone(),
            },
        }),
        Intent::Links { added, removed } => links(plan_pairs(ws, snap, removed, added)?, intent),
        Intent::Meta(changes) => meta(
            ws,
            &changes.iter().map(MetaChange::reverse).collect::<Vec<_>>(),
        ),
        Intent::Install { skill } => {
            if snap.get(skill).is_none() {
                return Ok(Plan::Nothing(format!(
                    "already done: {}",
                    intent.describe()
                )));
            }
            Ok(Plan::Write {
                describe: format!("remove {skill} and every link to it"),
                apply: WriteBack::RemoveInstalled {
                    skill: skill.clone(),
                },
            })
        }
        Intent::Rename { from, to } => rename(ws, snap, to, from),
        Intent::PresetRename { from, to } => preset_rename_plan(ws, to, from),
        Intent::OneWay { what } => bail!("{what} cannot be taken back"),
    }
}

/// Redoing runs the original intent again.
pub fn redo_plan(ws: &Workspace, snap: &Snapshot, intent: &Intent) -> Result<Plan> {
    match intent {
        Intent::Group(intents) => group_plan(ws, snap, intents, false),
        Intent::TargetSelection {
            agent,
            project,
            before,
            after,
        } => Ok(Plan::Write {
            describe: intent.describe(),
            apply: WriteBack::TargetSelection {
                agent: agent.clone(),
                project: project.clone(),
                expected: before.clone(),
                desired: after.clone(),
            },
        }),
        Intent::Links { added, removed } => links(plan_pairs(ws, snap, added, removed)?, intent),
        Intent::Meta(changes) => meta(ws, changes),
        // Fetching is a fresh network operation, not a reversal of a removal.
        Intent::Install { skill } => bail!("install {skill} again to bring it back"),
        Intent::Rename { from, to } => rename(ws, snap, from, to),
        Intent::PresetRename { from, to } => preset_rename_plan(ws, from, to),
        Intent::OneWay { what } => bail!("{what} cannot be redone"),
    }
}

/// Plan moving `from` to `to`, which is what either direction of a rename
/// comes down to. The skill has to be where the step left it, and the name it
/// is going to has to be free: a skill absent means someone already took this
/// step or removed the skill since, and a name taken since is not something
/// to move over. Either way the tree is left as it is and the reason said.
fn rename(ws: &Workspace, snap: &Snapshot, from: &str, to: &str) -> Result<Plan> {
    let present = |key: &str| snap.get(key).is_some_and(|r| r.status.is_present());
    if !present(from) {
        return Ok(Plan::Nothing(if present(to) {
            format!("already done: renamed {from} to {to}")
        } else {
            format!("{from} is gone")
        }));
    }
    // Metadata alone counts as taken: `edit::rename` would refuse to move a
    // file over it, and a missing skill's tags and note are still someone's.
    if snap.get(to).is_some() || ws.skill_path(to).exists() {
        return Ok(Plan::Nothing(format!(
            "{to} is taken; {from} left as it is"
        )));
    }
    Ok(Plan::Write {
        describe: format!("rename {from} to {to}"),
        apply: WriteBack::Rename {
            from: from.to_string(),
            to: to.to_string(),
        },
    })
}

/// The same reasoning as `rename`, for a preset. A file is what counts as
/// taken, whether or not it parses: a preset this tool cannot read is still
/// not one it may write over.
fn preset_rename_plan(ws: &Workspace, from: &str, to: &str) -> Result<Plan> {
    let present = |name: &str| ws.presets.path(name).exists();
    if !present(from) {
        return Ok(Plan::Nothing(if present(to) {
            format!("already done: renamed preset {from} to {to}")
        } else {
            format!("preset {from} is gone")
        }));
    }
    if present(to) {
        return Ok(Plan::Nothing(format!(
            "preset {to} is taken; {from} left as it is"
        )));
    }
    Ok(Plan::Write {
        describe: format!("rename preset {from} to {to}"),
        apply: WriteBack::PresetRename {
            from: from.to_string(),
            to: to.to_string(),
        },
    })
}

/// An empty plan means the tree already matches; say so rather than opening an
/// empty confirmation.
fn links(actions: Vec<Action>, intent: &Intent) -> Result<Plan> {
    if actions.iter().any(|a| a.is_change()) {
        Ok(Plan::Links(actions))
    } else {
        Ok(Plan::Nothing(format!(
            "already done: {}",
            intent.describe()
        )))
    }
}

/// Keep only the changes that would still do something. One a hand edit has
/// overtaken is dropped with its reason rather than written over.
fn meta(ws: &Workspace, changes: &[MetaChange]) -> Result<Plan> {
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for c in changes {
        match c.fate(ws)? {
            Fate::Ready => ready.push(c.clone()),
            Fate::Blocked(why) => blocked.push(why),
            Fate::Done => {}
        }
    }
    if ready.is_empty() {
        return Ok(Plan::Nothing(if blocked.is_empty() {
            format!("already done: {}", describe_changes(changes))
        } else {
            blocked.join("; ")
        }));
    }
    Ok(Plan::Write {
        describe: describe_changes(&ready),
        apply: WriteBack::Meta(ready),
    })
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
    fn reversing_a_metadata_change_swaps_the_two_sides() {
        let note = MetaChange::Note {
            skill: "printer".into(),
            before: None,
            after: Some("out of paper".into()),
        };
        assert_eq!(
            note.describe(),
            "set the note on printer to \"out of paper\""
        );
        let back = note.reverse();
        assert_eq!(back.describe(), "clear the note on printer");
        assert_eq!(back.reverse(), note, "twice over is where it started");

        let tags = MetaChange::Tags {
            skill: "bicycle".into(),
            added: vec!["commute".into()],
            removed: vec![],
        };
        assert_eq!(tags.describe(), "tag bicycle with commute");
        assert_eq!(tags.reverse().describe(), "take commute off bicycle");

        let source = MetaChange::Source {
            skill: "etcd".into(),
            before: None,
            after: Some(meta::Source::Git {
                url: "https://github.com/acme/etcd".into(),
                subpath: None,
                branch: Some("main".into()),
                revision: None,
            }),
        };
        assert_eq!(
            source.describe(),
            "set the source of etcd to https://github.com/acme/etcd@main"
        );
        assert_eq!(source.reverse().describe(), "clear the source of etcd");
        assert_eq!(source.reverse().reverse(), source);

        let description = MetaChange::PresetDescription {
            name: "commute".into(),
            before: None,
            after: Some("rides to work".into()),
        };
        assert_eq!(
            description.describe(),
            "set the description of preset commute to \"rides to work\""
        );
        assert_eq!(
            description.reverse().describe(),
            "clear the description of preset commute"
        );
        assert_eq!(description.reverse().reverse(), description);
    }

    #[test]
    fn a_write_across_several_skills_is_described_as_one() {
        let changes: Vec<MetaChange> = ["printer", "bicycle", "etcd"]
            .iter()
            .map(|s| MetaChange::Tags {
                skill: (*s).into(),
                added: vec!["paper".into()],
                removed: vec!["stationery".into()],
            })
            .collect();
        assert_eq!(
            Intent::Meta(changes).describe(),
            "change the tags on 3 skills"
        );
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
