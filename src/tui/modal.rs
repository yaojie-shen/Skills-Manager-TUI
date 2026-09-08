//! Overlays: help, messages, confirmations, text prompts, agent picker,
//! and the update conflict resolver.

use super::app::{Action, Ctx, Hints, MetaFn, Step, WriteFn};
use super::views::{View, search::SearchView};
use super::widgets::{Input, ListNav, button, fit, width};
use crate::tui::widgets::OverlayClear as Clear;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use skills::history;
use skills::ops::deploy;
use skills::ops::edit;
use skills::ops::install;
use skills::ops::update::{self, FileChange, Prepared, Take};
use skills::reconcile::{AgentDirMode, DeployState, EntryState};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One choosable row.
pub struct PickItem {
    /// What the action consumes; not always what is displayed.
    pub id: String,
    pub label: String,
    pub sub: String,
}

pub enum InputKind {
    Tags { skill: String },
    PresetName,
    PresetDescription { name: String },
    RenamePreset { old: String },
    RenameTag { old: String },
    Install,
    Rename { skill: String },
    SetSource { skill: String },
}

pub enum Modal {
    PresetSkills(Box<SearchView>),
    Batch(Box<super::batch::Batch>),
    NameConflict {
        title: String,
        actions: Vec<deploy::Action>,
        rect: Rect,
    },
    Repository(Box<super::repository_picker::RepositoryPicker>),
    Help {
        scroll: u16,
    },
    Message {
        title: String,
        lines: Vec<String>,
        scroll: u16,
    },
    /// Preview of link changes with Apply / Cancel.
    Confirm {
        title: String,
        lines: Vec<String>,
        actions: Vec<deploy::Action>,
        /// Emitted only once the changes actually went through, so a cancelled
        /// or failed confirmation leaves the history where it was.
        then: Option<Step>,
        scroll: u16,
        btn: usize,
        btn_rects: Vec<Rect>,
        rect: Rect,
    },
    /// Destructive write with Apply / Cancel.
    ConfirmWrite {
        title: String,
        lines: Vec<String>,
        write: Option<MetaFn>,
        /// Set when this confirmation is a history step.
        then: Option<Step>,
        btn: usize,
        btn_rects: Vec<Rect>,
        rect: Rect,
    },
    Input {
        title: String,
        input: Input,
        kind: InputKind,
        hint: String,
        rect: Rect,
    },
    AgentPick {
        skill: String,
        list: ListNav,
        rect: Rect,
    },
    /// Browse installed repositories; skill pickers use SearchView or RepositoryPicker.
    Picker {
        title: String,
        input: Input,
        items: Vec<PickItem>,
        /// Whether typing edits the filter rather than operating on results.
        input_focus: bool,
        input_rect: Rect,
        /// Indices matching the filter, in display order.
        shown: Vec<usize>,
        list: ListNav,
        rect: Rect,
    },
    Resolve {
        prepared: Option<Prepared>,
        files: Vec<(String, FileChange, Take)>,
        list: ListNav,
        default_take: Take,
        btn: usize,
        btn_rects: Vec<Rect>,
        rect: Rect,
    },
}

impl Modal {
    // ---- constructors -----------------------------------------------------

    pub fn batch_tags(keys: Vec<String>, ctx: &Ctx) -> Self {
        Self::Batch(Box::new(super::batch::Batch::tags(keys, ctx)))
    }
    pub fn batch_deploy(keys: Vec<String>, ctx: &Ctx) -> Self {
        Self::Batch(Box::new(super::batch::Batch::deploy(keys, ctx)))
    }
    pub fn batch_deploy_agent(keys: Vec<String>, agent: &str, ctx: &Ctx) -> Self {
        Self::Batch(Box::new(super::batch::Batch::deploy_agent(
            keys, agent, ctx,
        )))
    }
    pub fn batch_presets(keys: Vec<String>, ctx: &Ctx) -> Self {
        Self::Batch(Box::new(super::batch::Batch::presets(keys, ctx)))
    }

    pub fn help() -> Self {
        Modal::Help { scroll: 0 }
    }
    pub fn message(title: impl Into<String>, lines: Vec<String>) -> Self {
        Modal::Message {
            title: title.into(),
            lines,
            scroll: 0,
        }
    }
    pub fn confirm(title: String, actions: Vec<deploy::Action>) -> Self {
        Self::confirm_then(title, actions, None)
    }

    /// A confirmation that also moves the history when it succeeds.
    pub fn confirm_then(title: String, actions: Vec<deploy::Action>, then: Option<Step>) -> Self {
        let lines: Vec<String> = actions.iter().map(|a| a.describe()).collect();
        if !actions.iter().any(|a| a.is_change()) {
            return Modal::message(
                title,
                if lines.is_empty() {
                    vec!["nothing to do".into()]
                } else {
                    lines
                },
            );
        }
        Modal::Confirm {
            title,
            lines,
            actions,
            then,
            scroll: 0,
            btn: 0,
            btn_rects: Vec::new(),
            rect: Rect::default(),
        }
    }
    /// A write with nothing to log: everything it could take back is either
    /// gone for good or already an entry of its own.
    fn confirm_write(title: String, lines: Vec<String>, write: WriteFn) -> Self {
        Self::confirm_meta(
            title,
            lines,
            Box::new(move |ws| write(ws).map(|m| (m, None))),
        )
    }
    fn confirm_meta(title: String, lines: Vec<String>, write: MetaFn) -> Self {
        Modal::ConfirmWrite {
            title,
            lines,
            write: Some(write),
            then: None,
            btn: 1,
            btn_rects: Vec::new(),
            rect: Rect::default(),
        }
    }
    pub fn tags(skill: &str, tags: &[String]) -> Self {
        Modal::Input {
            title: format!(" tags for {skill} "),
            input: Input::with_value(&tags.join(", ")),
            kind: InputKind::Tags {
                skill: skill.into(),
            },
            hint: "comma separated · Enter save · Esc cancel".into(),
            rect: Rect::default(),
        }
    }
    pub fn new_preset() -> Self {
        Modal::Input {
            title: " new preset ".into(),
            input: Input::default(),
            kind: InputKind::PresetName,
            hint: "name · Enter create · Esc cancel".into(),
            rect: Rect::default(),
        }
    }

    /// Edit what a preset is for. A single-line prompt, prefilled, rather
    /// than `$EDITOR` or editing on the card: a preset description is one
    /// sentence, like the description in a SKILL.md, and leaving the screen
    /// for an editor is a heavy round trip for that; editing inside a grid
    /// cell that is laid out again every frame is fragile; and create, tags
    /// and install already ask through this same box, so it is the one the
    /// user knows. Prefilled because a description is usually corrected, not
    /// replaced, and clearing the field is how it is removed.
    pub fn preset_description(name: &str, current: Option<&str>) -> Self {
        Modal::Input {
            title: format!(" description of {name} "),
            input: Input::with_value(current.unwrap_or_default()),
            kind: InputKind::PresetDescription { name: name.into() },
            hint: "one sentence · Enter save · empty clears · Esc cancel".into(),
            rect: Rect::default(),
        }
    }

    /// Ask for a preset's new name, starting from the old one as the skill
    /// rename does. No confirmation follows: unlike a skill, a preset is one
    /// file and at most one line of `config.toml`, and undo has it.
    pub fn rename_preset(name: &str) -> Self {
        Modal::Input {
            title: format!(" rename preset {name} "),
            input: Input::with_value(name),
            kind: InputKind::RenamePreset { old: name.into() },
            hint: "new name · Enter rename · Esc cancel".into(),
            rect: Rect::default(),
        }
    }
    /// Ask where to fetch a skill from. Accepts what `skills install` accepts.
    pub fn install() -> Self {
        Modal::Input {
            title: " install a skill ".into(),
            input: Input::default(),
            kind: InputKind::Install,
            hint: "owner/repo[/path] · a git URL · a local path — Enter installs, Esc cancels"
                .into(),
            rect: Rect::default(),
        }
    }

    pub fn rename_tag(old: &str) -> Self {
        Modal::Input {
            title: format!(" rename tag {old} "),
            input: Input::with_value(old),
            kind: InputKind::RenameTag { old: old.into() },
            hint: "new name · Enter rename everywhere · Esc cancel".into(),
            rect: Rect::default(),
        }
    }

    /// Ask for a skill's new name. The field starts with the old one, since a
    /// rename is usually a small edit to it.
    pub fn rename(skill: &str) -> Self {
        Modal::Input {
            title: format!(" rename {skill} "),
            input: Input::with_value(skill.rsplit('/').next().unwrap_or(skill)),
            kind: InputKind::Rename {
                skill: skill.into(),
            },
            hint: "new name · Enter shows what moves · Esc cancel".into(),
            rect: Rect::default(),
        }
    }

    /// What a rename moves, read off the snapshot so the user sees it before
    /// answering. The write plans again from a fresh scan, as `App::step`
    /// does, because this list is a keystroke old by the time it is confirmed.
    fn confirm_rename(old: &str, new: &str, ctx: &Ctx) -> Self {
        let mut lines = vec![
            format!("Rename \"{old}\" to \"{new}\"?"),
            format!("directory  {old}/ → {new}/"),
        ];
        if ctx.snap.get(old).is_some_and(|r| r.meta.is_some()) {
            lines.push(format!("metadata   {old}.toml → {new}.toml"));
        }
        for a in &ctx.snap.agents {
            if a.mode == AgentDirMode::Real
                && matches!(a.entries.get(old), Some(EntryState::Deployed))
            {
                lines.push(format!("link       {}/{old} → {}/{new}", a.key, a.key));
            }
        }
        for p in ctx.ws.presets.list().unwrap_or_default() {
            if p.skills.iter().any(|s| s == old) {
                lines.push(format!("preset     {}: {old} → {new}", p.name));
            }
        }
        let (from, to) = (old.to_string(), new.to_string());
        Self::confirm_meta(
            format!(" rename {old} "),
            lines,
            Box::new(move |ws| {
                let snap = ws.scan()?;
                edit::rename(ws, &snap, &from, &to)?;
                Ok((
                    format!("renamed {from} to {to}"),
                    Some(history::Intent::Rename { from, to }),
                ))
            }),
        )
    }

    /// Point a skill at a different source. The field starts empty rather than
    /// with the old value: a source is replaced, not edited, and what it is
    /// now sits on the hint line for reference.
    pub fn set_source(skill: &str, current: Option<&skills::meta::Source>) -> Self {
        let now = current
            .map(|s| s.summary())
            .unwrap_or_else(|| "none".into());
        Modal::Input {
            title: format!(" source of {skill} "),
            input: Input::default(),
            kind: InputKind::SetSource {
                skill: skill.into(),
            },
            hint: format!("now {now} · owner/repo[/path], a git URL or a path replaces it"),
            rect: Rect::default(),
        }
    }

    /// Take a directory the agent has of its own into the root. What follows
    /// is what `install::adopt` does on that branch: the directory moves into
    /// the root, the agent is left a link to it there, and metadata is created
    /// with the content as it stands for its baseline.
    ///
    /// Not logged. Taking it back would mean moving the directory out of the
    /// root, deleting the link the agent now reads through and recreating a
    /// real directory in its place — three writes on a live path with nothing
    /// on disk to re-derive them from. Nor is it an `Intent::OneWay`: nothing
    /// consumes those yet, and one on top of the stack would only refuse every
    /// undo and hide the reversible steps beneath it.
    pub fn adopt(agent: &str, name: &str, path: PathBuf) -> Self {
        let a = agent.to_string();
        Self::confirm_write(
            format!(" adopt {name} "),
            vec![
                format!("Adopt \"{name}\" from {agent} into the skills root?"),
                format!("Its directory moves into the root; {agent} keeps a link to it there."),
                "Metadata is created with the content as it stands for its baseline.".into(),
                "Undo does not cover this.".into(),
            ],
            Box::new(move |ws| {
                install::adopt(ws, &path, None).map(|k| format!("adopted {k} from {a}"))
            }),
        )
    }
    pub fn delete_tag(tag: &str) -> Self {
        let t = tag.to_string();
        Self::confirm_meta(
            format!(" delete tag {tag} "),
            vec![format!("Remove the tag \"{tag}\" from every skill?")],
            Box::new(move |ws| {
                history::tag_edit(ws, |ws| {
                    edit::tag_delete(ws, &t).map(|n| format!("removed tag from {n} skill(s)"))
                })
            }),
        )
    }
    pub fn delete_preset(name: &str) -> Self {
        let n = name.to_string();
        Self::confirm_write(
            format!(" delete preset {name} "),
            vec![format!(
                "Delete preset \"{name}\"? Deployed links are left as they are."
            )],
            Box::new(move |ws| ws.presets.remove(&n).map(|_| format!("deleted preset {n}"))),
        )
    }
    pub fn remove(skill: &str) -> Self {
        let k = skill.to_string();
        Self::confirm_write(
            format!(" remove {skill} "),
            vec![
                format!("Remove \"{skill}\" from the skills root?"),
                "This unlinks it from every agent, deletes the directory and its metadata.".into(),
            ],
            Box::new(move |ws| {
                let snap = ws.scan()?;
                edit::remove(ws, &snap, &k, false).map(|_| format!("removed {k}"))
            }),
        )
    }
    /// Delete a directory that is not a usable skill. The common cause is a
    /// `git checkout` or `git clean` that removed the files but left the
    /// directory behind, since git does not track directories.
    pub fn discard_invalid(skill: &str, reason: &str) -> Self {
        let k = skill.to_string();
        let empty = reason.contains("missing SKILL.md");
        let mut lines = vec![format!("Delete the directory \"{skill}\"?")];
        lines.push(format!("It is not a usable skill: {reason}."));
        if empty {
            lines.push(
                "A directory left with no SKILL.md is usually what git leaves behind when \
                 the files are discarded, because git does not remove empty directories."
                    .into(),
            );
        }
        lines.push("Its metadata, if any, goes with it.".into());
        Self::confirm_write(
            format!(" discard {skill} "),
            lines,
            Box::new(move |ws| {
                let snap = ws.scan()?;
                edit::remove(ws, &snap, &k, false).map(|_| format!("discarded {k}"))
            }),
        )
    }

    /// Forget the tags and notes of a skill whose directory is gone.
    pub fn forget_missing(skill: &str) -> Self {
        let k = skill.to_string();
        Self::confirm_write(
            format!(" forget {skill} "),
            vec![
                format!("Forget the metadata of \"{skill}\"?"),
                "Its directory is already gone; this discards the tags and note you wrote \
                 for it. Keep it instead if you intend to reinstall the skill."
                    .into(),
            ],
            Box::new(move |ws| ws.meta.remove(&k).map(|_| format!("forgot {k}"))),
        )
    }

    /// Search cards with staged checkboxes for editing a preset's membership.
    pub fn preset_members(preset: &str, ctx: &Ctx) -> Self {
        Self::PresetSkills(Box::new(SearchView::preset_members(preset, ctx)))
    }

    pub fn repositories(ctx: &Ctx) -> Self {
        match skills::repository::Repository::list(&ctx.ws.root) {
            Ok(repositories) => Self::picker(
                " repositories ".into(),
                repositories
                    .into_iter()
                    .map(|r| {
                        let count = ctx
                            .snap
                            .skills
                            .iter()
                            .filter(|s| {
                                skills::repository::alias_of(&s.key) == Some(r.alias.as_str())
                            })
                            .count();
                        PickItem {
                            id: r.alias.clone(),
                            label: format!(
                                "{} {}",
                                crate::tui::icons::git(ctx.ws.config.ui.icons, &r.url),
                                skills::repository::source_name(&r.url).unwrap_or(r.alias)
                            ),
                            sub: format!(
                                "{} {count} skills · {} {} · {}",
                                crate::tui::icons::package(ctx.ws.config.ui.icons),
                                crate::tui::icons::branch(ctx.ws.config.ui.icons),
                                r.branch,
                                r.url
                            ),
                        }
                    })
                    .collect(),
            ),
            Err(e) => Self::message("repositories", vec![format!("{e:#}")]),
        }
    }

    fn picker(title: String, items: Vec<PickItem>) -> Self {
        let shown: Vec<usize> = (0..items.len()).collect();
        let mut list = ListNav::default();
        list.clamp(shown.len());
        Modal::Picker {
            title,
            input: Input::default(),
            input_focus: true,
            input_rect: Rect::default(),
            items,
            shown,
            list,
            rect: Rect::default(),
        }
    }

    /// Confirm a history step that is a write rather than a set of links.
    pub fn undo_write(describe: String, back: skills::history::WriteBack, dir: Step) -> Self {
        let verb = if dir == Step::Undo { "undo" } else { "redo" };
        Self::confirm_write(
            format!(" {verb} "),
            vec![format!("{}?", capitalize(&describe))],
            Box::new(move |ws| back.apply(ws)),
        )
        .with_step(dir)
    }

    /// Carry the history move onto a write confirmation. The Apply button
    /// takes the focus here: a destructive confirmation starts on Cancel so a
    /// second Enter cannot delete anything, but an undo was asked for with
    /// Ctrl-Z a moment ago, and Enter should do what was asked rather than
    /// quietly cancel it. `Modal::Confirm`, used for link steps, already
    /// starts on Apply.
    fn with_step(mut self, dir: Step) -> Self {
        if let Modal::ConfirmWrite { then, btn, .. } = &mut self {
            *then = Some(dir);
            *btn = 0;
        }
        self
    }

    pub fn agent_pick(skill: &str) -> Self {
        let mut list = ListNav::default();
        list.select(Some(0));
        Modal::AgentPick {
            skill: skill.into(),
            list,
            rect: Rect::default(),
        }
    }
    pub fn resolve(prepared: Prepared) -> Self {
        let files: Vec<(String, FileChange, Take)> = prepared
            .files
            .iter()
            .filter(|(_, c)| **c != FileChange::Unchanged)
            .map(|(f, c)| {
                let take = match c {
                    FileChange::LocalChanged => Take::Local,
                    _ => Take::Upstream,
                };
                (f.clone(), *c, take)
            })
            .collect();
        let mut list = ListNav::default();
        list.clamp(files.len());
        if !prepared.needs_resolution() {
            // Clean update: still show what changes, but nothing to pick.
        }
        Modal::Resolve {
            prepared: Some(prepared),
            files,
            list,
            default_take: Take::Upstream,
            btn: 0,
            btn_rects: Vec::new(),
            rect: Rect::default(),
        }
    }

    pub fn refresh(&mut self, ctx: &Ctx) {
        if let Modal::AgentPick { list, .. } = self {
            list.clamp(ctx.snap.agents.len());
        }
    }

    pub fn hints(&self) -> Hints {
        match self {
            Modal::NameConflict { .. } => &[("c", "coexist"), ("r", "replace"), ("Esc", "cancel")],
            Modal::PresetSkills(view) => view.hints(),
            Modal::Batch(p) => p.hints(),
            Modal::Repository(p) => p.hints(),
            Modal::Help { .. } | Modal::Message { .. } => &[("Esc", "close")],
            Modal::Confirm { .. } | Modal::ConfirmWrite { .. } => {
                &[("Enter/y", "apply"), ("Esc/n", "cancel"), ("←→", "buttons")]
            }
            Modal::Input { .. } => &[("Enter", "save"), ("Esc", "cancel")],
            Modal::Picker {
                input_focus: true, ..
            } => &[
                ("type", "filter"),
                ("↓/Tab", "list"),
                ("Enter", "browse"),
                ("Esc", "close"),
            ],
            Modal::Picker { .. } => &[
                ("Enter", "browse"),
                ("u", "check repo"),
                ("U", "update repo"),
                ("↑↓", "move"),
                ("/Tab", "filter"),
                ("Esc", "close"),
            ],
            Modal::AgentPick { .. } => &[("Space/Enter", "toggle"), ("Esc", "close")],
            Modal::Resolve { .. } => &[
                ("Space", "toggle side"),
                ("l/u", "all local/upstream"),
                ("Enter", "apply"),
                ("Esc", "cancel"),
            ],
        }
    }

    // ---- input ------------------------------------------------------------

    pub fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self {
            Modal::NameConflict { title, actions, .. } => match k.code {
                KeyCode::Esc => vec![Action::CloseModal],
                KeyCode::Char(c @ ('c' | 'r')) => match deploy::resolve_names(
                    ctx.snap,
                    actions,
                    Some(if c == 'c' { "coexist" } else { "replace" }),
                ) {
                    Ok(plan) => vec![Action::OpenModal(Box::new(Modal::confirm(
                        title.clone(),
                        plan,
                    )))],
                    Err(e) => vec![Action::Error(format!("{e:#}"))],
                },
                _ => vec![],
            },
            Modal::PresetSkills(view) => view.handle_key(k, ctx),
            Modal::Batch(p) => p.key(k, ctx),
            Modal::Repository(p) => p.key(k, ctx),
            Modal::Help { scroll } | Modal::Message { scroll, .. } => match k.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1);
                    vec![]
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    vec![]
                }
                _ => vec![Action::CloseModal],
            },
            Modal::Confirm {
                actions,
                then,
                btn,
                scroll,
                ..
            } => match k.code {
                KeyCode::Char('y') => apply_links(std::mem::take(actions), then.take()),
                // Cancelling is not an event: the dialog goes and nothing is
                // said, since a notice for it would only pile up.
                KeyCode::Enter => {
                    if *btn == 0 {
                        apply_links(std::mem::take(actions), then.take())
                    } else {
                        vec![Action::CloseModal]
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    vec![Action::CloseModal]
                }
                KeyCode::Left
                | KeyCode::Right
                | KeyCode::Tab
                | KeyCode::Char('h')
                | KeyCode::Char('l') => {
                    *btn = 1 - *btn;
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1);
                    vec![]
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    vec![]
                }
                _ => vec![],
            },
            Modal::ConfirmWrite {
                write, then, btn, ..
            } => match k.code {
                KeyCode::Char('y') => write
                    .take()
                    .map(|w| write_actions(w, then.take()))
                    .unwrap_or_default(),
                KeyCode::Enter => {
                    if *btn == 0 {
                        write
                            .take()
                            .map(|w| write_actions(w, then.take()))
                            .unwrap_or_default()
                    } else {
                        vec![Action::CloseModal]
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    vec![Action::CloseModal]
                }
                KeyCode::Left
                | KeyCode::Right
                | KeyCode::Tab
                | KeyCode::Char('h')
                | KeyCode::Char('l') => {
                    *btn = 1 - *btn;
                    vec![]
                }
                _ => vec![],
            },
            Modal::Input { input, kind, .. } => match k.code {
                KeyCode::Esc => vec![Action::CloseModal],
                KeyCode::Enter => {
                    let value = input.value().to_string();
                    vec![Action::SubmitInput(submit(kind, value, ctx))]
                }
                _ => {
                    input.handle_key(k);
                    vec![]
                }
            },
            Modal::AgentPick { skill, list, .. } => {
                let n = ctx.snap.agents.len();
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => {
                        vec![Action::CloseModal]
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        list.move_by(1, n);
                        vec![]
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        list.move_by(-1, n);
                        vec![]
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => list
                        .selected()
                        .map(|i| toggle_agent(ctx, skill, i))
                        .unwrap_or_default(),
                    KeyCode::Char(c @ '1'..='9') => {
                        toggle_agent(ctx, skill, (c as u8 - b'1') as usize)
                    }
                    _ => vec![],
                }
            }
            Modal::Picker {
                input,
                input_focus,
                items,
                shown,
                list,
                ..
            } => {
                if k.code == KeyCode::Char('/') && !*input_focus {
                    *input_focus = true;
                    return vec![];
                }
                if (k.code == KeyCode::Enter
                    || (!*input_focus && matches!(k.code, KeyCode::Char('u' | 'U'))))
                    && let Some(i) = list.selected().and_then(|i| shown.get(i)).copied()
                {
                    let alias = &items[i].id;
                    if k.code == KeyCode::Enter {
                        return vec![
                            Action::CloseModal,
                            Action::Search {
                                query: repository_query(alias, ctx),
                                focus_list: true,
                            },
                        ];
                    }
                    let keys: Vec<_> = ctx
                        .snap
                        .skills
                        .iter()
                        .filter(|s| skills::repository::alias_of(&s.key) == Some(alias.as_str()))
                        .map(|s| s.key.clone())
                        .collect();
                    return if k.code == KeyCode::Char('u') {
                        vec![Action::Spawn(crate::tui::event::Task::Check(keys))]
                    } else {
                        keys.into_iter()
                            .map(|key| Action::Spawn(crate::tui::event::Task::Prepare(key)))
                            .collect()
                    };
                }
                match k.code {
                    KeyCode::Esc => return vec![Action::CloseModal],
                    KeyCode::Tab => *input_focus = !*input_focus,
                    KeyCode::Down | KeyCode::Char('n') if k.code == KeyCode::Down || ctrl => {
                        if *input_focus {
                            *input_focus = false;
                        } else {
                            list.move_by(1, shown.len());
                        }
                    }
                    KeyCode::Up | KeyCode::Char('p') if k.code == KeyCode::Up || ctrl => {
                        if *input_focus || list.selected() == Some(0) {
                            *input_focus = true;
                        } else {
                            list.move_by(-1, shown.len());
                        }
                    }
                    _ if *input_focus && input.handle_key(k) => {
                        refilter(input.value(), items, shown);
                        list.first(shown.len());
                    }
                    _ => {}
                }
                vec![]
            }
            Modal::Resolve {
                prepared,
                files,
                list,
                default_take,
                btn,
                ..
            } => {
                let n = files.len();
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        if let Some(p) = prepared.take() {
                            p.cleanup();
                        }
                        vec![Action::CloseModal, Action::Toast("update cancelled".into())]
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        list.move_by(1, n);
                        vec![]
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        list.move_by(-1, n);
                        vec![]
                    }
                    KeyCode::Char(' ') => {
                        if let Some(i) = list.selected()
                            && let Some(f) = files.get_mut(i)
                        {
                            f.2 = flip(f.2);
                        }
                        vec![]
                    }
                    KeyCode::Char('l') if !ctrl => {
                        for f in files.iter_mut() {
                            f.2 = Take::Local;
                        }
                        *default_take = Take::Local;
                        vec![]
                    }
                    KeyCode::Char('u') if !ctrl => {
                        for f in files.iter_mut() {
                            f.2 = Take::Upstream;
                        }
                        *default_take = Take::Upstream;
                        vec![]
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                        *btn = 1 - *btn;
                        vec![]
                    }
                    KeyCode::Enter | KeyCode::Char('y') => {
                        if *btn == 1 && k.code == KeyCode::Enter {
                            if let Some(p) = prepared.take() {
                                p.cleanup();
                            }
                            return vec![
                                Action::CloseModal,
                                Action::Toast("update cancelled".into()),
                            ];
                        }
                        let Some(p) = prepared.take() else {
                            return vec![Action::CloseModal];
                        };
                        let default = *default_take;
                        let per_file: BTreeMap<String, Take> = files
                            .iter()
                            .filter(|(_, _, t)| *t != default)
                            .map(|(f, _, t)| (f.clone(), *t))
                            .collect();
                        let key = p.skill.clone();
                        let rev = skills::meta::short_rev(&p.to_revision).to_string();
                        vec![
                            Action::CloseModal,
                            Action::Write(Box::new(move |ws| {
                                update::apply(ws, &p, default, &per_file)
                                    .map(|_| format!("{key} updated to {rev}"))
                            })),
                        ]
                    }
                    _ => vec![],
                }
            }
        }
    }

    pub fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        let at = (m.column, m.row).into();
        let wheel = match m.kind {
            MouseEventKind::ScrollUp => Some(-3),
            MouseEventKind::ScrollDown => Some(3),
            _ => None,
        };
        let click = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        match self {
            Modal::NameConflict { rect, .. } => {
                if click {
                    if !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                    if m.row == rect.bottom().saturating_sub(2) {
                        let code = if m.column < rect.x + rect.width / 2 {
                            'c'
                        } else {
                            'r'
                        };
                        return self.handle_key(
                            KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE),
                            ctx,
                        );
                    }
                }
                vec![]
            }
            Modal::PresetSkills(view) => view.handle_mouse(m, ctx),
            Modal::Batch(p) => p.mouse(m, ctx),
            Modal::Repository(p) => p.mouse(m, ctx),
            Modal::Help { scroll } | Modal::Message { scroll, .. } => {
                if let Some(d) = wheel {
                    *scroll = (*scroll as i32 + d).max(0) as u16;
                } else if click {
                    return vec![Action::CloseModal];
                }
                vec![]
            }
            Modal::Confirm {
                actions,
                then,
                scroll,
                btn_rects,
                rect,
                ..
            } => {
                if let Some(d) = wheel {
                    *scroll = (*scroll as i32 + d).max(0) as u16;
                    return vec![];
                }
                if click {
                    if btn_rects.first().is_some_and(|r| r.contains(at)) {
                        return apply_links(std::mem::take(actions), then.take());
                    }
                    if btn_rects.get(1).is_some_and(|r| r.contains(at)) || !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                }
                vec![]
            }
            Modal::ConfirmWrite {
                write,
                then,
                btn_rects,
                rect,
                ..
            } => {
                if click {
                    if btn_rects.first().is_some_and(|r| r.contains(at)) {
                        return write
                            .take()
                            .map(|w| write_actions(w, then.take()))
                            .unwrap_or_default();
                    }
                    if btn_rects.get(1).is_some_and(|r| r.contains(at)) || !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                }
                vec![]
            }
            Modal::Input { input, rect, .. } => {
                if click {
                    if !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                    input.click(m.column);
                }
                vec![]
            }
            Modal::AgentPick { skill, list, rect } => {
                let n = ctx.snap.agents.len();
                if let Some(d) = wheel {
                    list.move_by(d, n);
                } else if click {
                    if !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                    if let Some((i, _)) = list.click(m.row, n) {
                        return toggle_agent(ctx, skill, i);
                    }
                }
                vec![]
            }
            Modal::Picker {
                input,
                input_focus,
                input_rect,
                items,
                shown,
                list,
                rect,
                ..
            } => {
                if let Some(d) = wheel {
                    if !list.rows.contains(at) {
                        return vec![];
                    }
                    *input_focus = false;
                    list.move_by(d, shown.len());
                } else if click {
                    if !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                    if input_rect.contains(at) {
                        *input_focus = true;
                        input.click(m.column);
                        return vec![];
                    }
                    if let Some((row, double)) = list.click(m.row, shown.len())
                        && let Some(i) = shown.get(row).copied()
                    {
                        *input_focus = false;
                        if double {
                            return vec![
                                Action::CloseModal,
                                Action::Search {
                                    query: repository_query(&items[i].id, ctx),
                                    focus_list: true,
                                },
                            ];
                        }
                    }
                }
                vec![]
            }
            Modal::Resolve {
                files,
                list,
                btn_rects,
                rect,
                prepared,
                default_take,
                ..
            } => {
                let n = files.len();
                if let Some(d) = wheel {
                    list.move_by(d, n);
                } else if click {
                    if btn_rects.first().is_some_and(|r| r.contains(at)) {
                        return self.handle_key(
                            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
                            ctx,
                        );
                    }
                    if btn_rects.get(1).is_some_and(|r| r.contains(at)) || !rect.contains(at) {
                        if let Some(p) = prepared.take() {
                            p.cleanup();
                        }
                        let _ = default_take;
                        return vec![Action::CloseModal, Action::Toast("update cancelled".into())];
                    }
                    if let Some((i, _)) = list.click(m.row, n)
                        && let Some(f) = files.get_mut(i)
                    {
                        f.2 = flip(f.2);
                    }
                }
                vec![]
            }
        }
    }

    // ---- drawing ----------------------------------------------------------

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = ctx.theme;
        match self {
            Modal::NameConflict {
                title,
                actions,
                rect,
            } => {
                let conflicts = deploy::name_conflicts(ctx.snap, actions);
                let mut lines = vec![
                    Line::from("Different folders contain skills with the same frontmatter name."),
                    Line::from("SKILL.md stays unchanged. Agent selection may be ambiguous."),
                    Line::from(""),
                ];
                lines.extend(conflicts.iter().map(|c| {
                    Line::from(format!(
                        "{}: {} ↔ {} ({})",
                        c.agent,
                        c.skill,
                        c.other_path.display(),
                        c.name
                    ))
                }));
                let r = centered(
                    area,
                    100,
                    (lines.len() as u16 + 5).min(area.height.saturating_sub(2)),
                );
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(format!(" name conflict · {title} "), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                f.render_widget(
                    Paragraph::new(lines).wrap(Wrap { trim: false }),
                    Rect {
                        height: inner.height.saturating_sub(2),
                        ..inner
                    },
                );
                f.render_widget(
                    Paragraph::new("[c] Coexist"),
                    Rect::new(
                        inner.x,
                        inner.bottom().saturating_sub(1),
                        inner.width / 2,
                        1,
                    ),
                );
                f.render_widget(
                    Paragraph::new("[r] Replace managed links"),
                    Rect::new(
                        inner.x + inner.width / 2,
                        inner.bottom().saturating_sub(1),
                        inner.width - inner.width / 2,
                        1,
                    ),
                );
            }
            Modal::PresetSkills(view) => {
                let r = centered(
                    area,
                    area.width.saturating_sub(4),
                    area.height.saturating_sub(2),
                );
                f.render_widget(Clear, r);
                let block = th.block(" Preset skills ", true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                view.draw(f, inner, ctx);
            }
            Modal::Batch(p) => p.draw(f, area, ctx),
            Modal::Repository(p) => p.draw(f, area, ctx),
            Modal::Help { scroll } => {
                let lines: Vec<Line> = HELP.lines().map(|l| help_line(l, th)).collect();
                let r = centered(area, 78, lines.len() as u16 + 2);
                f.render_widget(Clear, r);
                f.render_widget(
                    Paragraph::new(lines)
                        .scroll((*scroll, 0))
                        .block(th.block(" help ", true)),
                    r,
                );
            }
            Modal::Message {
                title,
                lines,
                scroll,
            } => {
                let r = centered(area, 84, lines.len() as u16 + 4);
                f.render_widget(Clear, r);
                let mut ls: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
                ls.push(Line::from(""));
                ls.push(Line::from(Span::styled("press any key", th.dim())));
                f.render_widget(
                    Paragraph::new(ls)
                        .wrap(Wrap { trim: false })
                        .scroll((*scroll, 0))
                        .block(th.block(format!(" {} ", title.trim()), true)),
                    r,
                );
            }
            Modal::Confirm {
                title,
                lines,
                scroll,
                btn,
                btn_rects,
                rect,
                ..
            } => {
                let r = centered(
                    area,
                    96,
                    (lines.len() as u16 + 5).min(area.height.saturating_sub(2)),
                );
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(format!(" {} ", title.trim()), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                let body = Rect {
                    height: inner.height.saturating_sub(2),
                    ..inner
                };
                let ls: Vec<Line> = lines.iter().map(|l| action_line(l, th)).collect();
                f.render_widget(Paragraph::new(ls).scroll((*scroll, 0)), body);
                let n_changes = lines.iter().filter(|l| !l.starts_with("skip")).count();
                *btn_rects =
                    draw_buttons(f, inner, &[("Apply", n_changes), ("Cancel", 0)], *btn, th);
            }
            Modal::ConfirmWrite {
                title,
                lines,
                btn,
                btn_rects,
                rect,
                ..
            } => {
                let r = centered(area, 70, lines.len() as u16 + 5);
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(format!(" {} ", title.trim()), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                let ls: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
                f.render_widget(
                    Paragraph::new(ls).wrap(Wrap { trim: false }),
                    Rect {
                        height: inner.height.saturating_sub(2),
                        ..inner
                    },
                );
                *btn_rects = draw_buttons(f, inner, &[("Yes", 0), ("Cancel", 0)], *btn, th);
            }
            Modal::Input {
                title,
                input,
                hint,
                rect,
                ..
            } => {
                let r = centered(area, 72, 4);
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(title.as_str(), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                let field = Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    height: 1,
                    ..inner
                };
                input.render(f, field, true, "", th);
                f.render_widget(
                    Paragraph::new(Span::styled(fit(hint, inner.width as usize), th.dim())),
                    Rect {
                        x: inner.x + 1,
                        y: inner.y + 1,
                        width: inner.width.saturating_sub(2),
                        height: 1,
                    },
                );
            }
            Modal::AgentPick { skill, list, rect } => {
                let rec = ctx.snap.get(skill);
                let items: Vec<ListItem> = ctx
                    .snap
                    .agents
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let st = rec.and_then(|r| r.deploy.get(&a.key));
                        let (mark, style) = match st {
                            Some(DeployState::Deployed) => ("✓", th.ok()),
                            Some(DeployState::NotDeployed)
                            | Some(DeployState::NoAgentDir)
                            | None => (" ", th.dim()),
                            Some(DeployState::Broken) => ("!", th.err()),
                            _ => ("~", th.warn()),
                        };
                        let note = match st {
                            Some(DeployState::Shadow { .. }) => {
                                "  shadow: real dir in agent, not touched"
                            }
                            Some(DeployState::Foreign) => "  foreign link, not touched",
                            Some(DeployState::NoAgentDir) => "  dir will be created",
                            _ => "",
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("[{mark}] "), style),
                            Span::styled(format!("{} ", i + 1), th.dim()),
                            Span::raw(format!("{:<10}", a.key)),
                            Span::styled(a.name.clone(), th.dim()),
                            Span::styled(note, th.dim()),
                        ]))
                    })
                    .collect();
                let r = centered(area, 64, items.len() as u16 + 2);
                *rect = r;
                f.render_widget(Clear, r);
                list.set_area_from_block(r);
                let w = List::new(items)
                    .block(th.block(format!(" deploy {skill} "), true))
                    .highlight_style(th.selected())
                    .highlight_symbol("▸ ");
                f.render_stateful_widget(w, r, &mut list.state);
            }
            Modal::Picker {
                title,
                input,
                input_focus,
                input_rect,
                items,
                shown,
                list,
                rect,
            } => {
                let r = centered(
                    area,
                    76,
                    (shown.len() as u16 + 5).clamp(8, area.height.saturating_sub(4)),
                );
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(title.as_str(), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                let field = Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    height: 1,
                    ..inner
                };
                *input_rect = field;
                input.render(f, field, *input_focus, "type to filter…", th);
                let list_area = Rect {
                    y: inner.y + 2,
                    height: inner.height.saturating_sub(2),
                    ..inner
                };
                list.rows = list_area;
                let rows: Vec<ListItem> = shown
                    .iter()
                    .map(|i| {
                        let it = &items[*i];
                        ListItem::new(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(fit(&it.label, 26), Style::default()),
                            Span::raw("  "),
                            Span::styled(
                                fit(&it.sub, list_area.width.saturating_sub(32) as usize),
                                th.dim(),
                            ),
                        ]))
                    })
                    .collect();
                let w = List::new(rows)
                    .highlight_style(if *input_focus {
                        th.dim()
                    } else {
                        th.selected()
                    })
                    .highlight_symbol("▸ ");
                f.render_stateful_widget(w, list_area, &mut list.state);
                if shown.is_empty() {
                    f.render_widget(
                        Paragraph::new(Span::styled("nothing matches", th.dim())),
                        list_area,
                    );
                }
            }
            Modal::Resolve {
                prepared,
                files,
                list,
                default_take,
                btn,
                btn_rects,
                rect,
            } => {
                let Some(p) = prepared.as_ref() else { return };
                let r = centered(
                    area,
                    100,
                    (files.len() as u16 + 8).min(area.height.saturating_sub(2)),
                );
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(format!(" update {} ", p.skill), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                let head = vec![
                    Line::from(vec![
                        Span::styled("revision ", th.dim()),
                        Span::raw(
                            p.from_revision
                                .as_deref()
                                .map(skills::meta::short_rev)
                                .unwrap_or("-")
                                .to_string(),
                        ),
                        Span::styled(" → ", th.dim()),
                        Span::raw(skills::meta::short_rev(&p.to_revision).to_string()),
                        Span::styled(
                            if p.needs_resolution() {
                                "   modified locally — pick a side per file"
                            } else {
                                "   clean update"
                            },
                            if p.needs_resolution() {
                                th.warn()
                            } else {
                                th.ok()
                            },
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("default  ", th.dim()),
                        Span::styled(format!("{default_take:?}").to_lowercase(), th.accent()),
                        Span::styled(
                            if p.baseline_dir.is_some() {
                                "   (three-way against the installed revision)"
                            } else {
                                "   (two-way: baseline revision unavailable)"
                            },
                            th.dim(),
                        ),
                    ]),
                    Line::from(""),
                ];
                f.render_widget(
                    Paragraph::new(head),
                    Rect {
                        height: 3.min(inner.height),
                        ..inner
                    },
                );
                let list_area = Rect {
                    y: inner.y + 3,
                    height: inner.height.saturating_sub(5),
                    ..inner
                };
                list.rows = list_area;
                let items: Vec<ListItem> = files
                    .iter()
                    .map(|(name, change, take)| {
                        let (label, style) = match change {
                            FileChange::UpstreamChanged => ("upstream changed", th.accent()),
                            FileChange::LocalChanged => ("local changed   ", th.warn()),
                            FileChange::BothChanged => ("both changed    ", th.err()),
                            FileChange::Differs => ("differs         ", th.warn()),
                            FileChange::Unchanged => ("unchanged       ", th.dim()),
                        };
                        let side = match take {
                            Take::Local => Span::styled("[local   ]", th.warn()),
                            Take::Upstream => Span::styled("[upstream]", th.accent()),
                        };
                        ListItem::new(Line::from(vec![
                            side,
                            Span::raw(" "),
                            Span::styled(label, style),
                            Span::raw("  "),
                            Span::raw(name.clone()),
                        ]))
                    })
                    .collect();
                let w = List::new(items)
                    .highlight_style(th.selected())
                    .highlight_symbol("▸ ");
                f.render_stateful_widget(w, list_area, &mut list.state);
                if files.is_empty() {
                    f.render_widget(Paragraph::new(Span::styled("no file-level differences; applying replaces the directory with upstream", th.dim())), list_area);
                }
                *btn_rects = draw_buttons(f, inner, &[("Apply", 0), ("Cancel", 0)], *btn, th);
            }
        }
    }
}

/// Narrow `shown` to the rows whose label or sublabel contains `query`.
fn refilter(query: &str, items: &[PickItem], shown: &mut Vec<usize>) {
    let q = query.trim().to_lowercase();
    shown.clear();
    shown.extend((0..items.len()).filter(|i| {
        q.is_empty()
            || items[*i].label.to_lowercase().contains(&q)
            || items[*i].sub.to_lowercase().contains(&q)
    }));
}

/// A confirmed write, plus the history move it belongs to when it is a step.
fn write_actions(w: MetaFn, then: Option<Step>) -> Vec<Action> {
    let mut out = vec![Action::CloseModal, Action::WriteMeta(w)];
    if let Some(step) = then {
        out.push(Action::Step(step));
    }
    out
}

/// Sentences in a dialog start with a capital.
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn flip(t: Take) -> Take {
    match t {
        Take::Local => Take::Upstream,
        Take::Upstream => Take::Local,
    }
}

fn apply_links(actions: Vec<deploy::Action>, then: Option<Step>) -> Vec<Action> {
    match deploy::apply(&actions) {
        Ok(_) => {
            let mut out = vec![
                Action::CloseModal,
                Action::Toast(deploy::summarize(&actions)),
                Action::Rescan,
            ];
            // Nothing is written to the log until the change is on disk.
            out.push(match then {
                Some(step) => Action::Step(step),
                None => match skills::history::Intent::from_actions(&actions) {
                    Some(intent) => Action::Record(intent),
                    None => return out,
                },
            });
            out
        }
        Err(e) => vec![
            Action::CloseModal,
            Action::Error(format!("{e:#}")),
            Action::Rescan,
        ],
    }
}

fn toggle_agent(ctx: &Ctx, skill: &str, idx: usize) -> Vec<Action> {
    let Some(agent) = ctx.ws.config.agents.get(idx) else {
        return vec![];
    };
    let Some(rec) = ctx.snap.get(skill) else {
        return vec![Action::CloseModal];
    };
    let deployed = matches!(rec.deploy.get(&agent.key), Some(DeployState::Deployed));
    let plan = if deployed {
        deploy::plan_undeploy(
            ctx.ws,
            ctx.snap,
            std::slice::from_ref(&rec.key),
            std::slice::from_ref(&agent.key),
        )
    } else {
        deploy::plan_deploy(
            ctx.ws,
            ctx.snap,
            std::slice::from_ref(&rec.key),
            std::slice::from_ref(&agent.key),
        )
    };
    match plan {
        Ok(actions) => vec![Action::ApplyLinks {
            title: format!(
                "{} {} on {}",
                if deployed { "undeploy" } else { "deploy" },
                rec.key,
                agent.key
            ),
            actions,
        }],
        Err(e) => vec![Action::Error(format!("{e:#}"))],
    }
}

fn submit(kind: &InputKind, value: String, ctx: &Ctx) -> Vec<Action> {
    match kind {
        InputKind::Rename { skill } => {
            let name = value.trim();
            if name.is_empty() {
                return vec![];
            }
            if !skills::util::valid_skill_key(name) {
                return vec![Action::Error(format!("invalid local name: {name:?}"))];
            }
            let new = if skill.starts_with("repos/") {
                format!("{}/{}", skill.rsplit_once('/').unwrap().0, name)
            } else {
                name.to_string()
            };
            if new.is_empty() || new == *skill {
                return vec![];
            }
            // Refused here rather than after the confirmation, which would
            // otherwise list moves that are never going to happen.
            if !skills::repository::valid_id(&new) {
                return vec![Action::Error(format!("invalid skill name: {new:?}"))];
            }
            vec![Action::OpenModal(Box::new(Modal::confirm_rename(
                skill, &new, ctx,
            )))]
        }
        // A metadata field, written straight away like a tag; undo has it.
        InputKind::SetSource { skill } => {
            let reference = value.trim().to_string();
            if reference.is_empty() {
                return vec![];
            }
            let skill = skill.clone();
            match install::parse_ref(&reference, None, None) {
                Ok(r) => vec![Action::WriteMeta(Box::new(move |ws| {
                    history::source_edit(ws, &skill, &r)
                }))],
                Err(e) => vec![Action::Error(format!("{e:#}"))],
            }
        }
        InputKind::Tags { skill } => {
            let skill = skill.clone();
            let tags: Vec<String> = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            vec![Action::WriteMeta(Box::new(move |ws| {
                history::tag_edit(ws, |ws| {
                    edit::tag_set(ws, &skill, &tags).map(|m| {
                        format!(
                            "{skill}: {}",
                            if m.tags.is_empty() {
                                "no tags".into()
                            } else {
                                m.tags.join(", ")
                            }
                        )
                    })
                })
            }))]
        }
        // Name only. What comes next — members, a description — is done on
        // the card the new preset lands on, and the notice says which keys;
        // a second prompt here would be one more thing to dismiss before
        // seeing the result.
        InputKind::PresetName => {
            let name = value.trim().to_string();
            if name.is_empty() {
                return vec![];
            }
            let land_on = name.clone();
            vec![
                Action::Write(Box::new(move |ws| {
                    if ws.presets.load(&name)?.is_some() {
                        anyhow::bail!("preset {name} already exists");
                    }
                    ws.presets.save(&skills::preset::Preset {
                        name: name.clone(),
                        ..Default::default()
                    })?;
                    Ok(format!(
                        "created {name} — a adds skills, e sets the description"
                    ))
                })),
                Action::SelectPreset(land_on),
            ]
        }
        InputKind::PresetDescription { name } => {
            let name = name.clone();
            vec![Action::WriteMeta(Box::new(move |ws| {
                history::preset_description_edit(ws, &name, Some(&value))
            }))]
        }
        InputKind::RenamePreset { old } => {
            let new = value.trim().to_string();
            if new.is_empty() || new == *old {
                return vec![];
            }
            // Preset files share the naming rule of skill directories, since
            // the name is the file name.
            if !skills::util::valid_skill_key(&new) {
                return vec![Action::Error(format!("invalid preset name: {new:?}"))];
            }
            let old = old.clone();
            let land_on = new.clone();
            vec![
                Action::WriteMeta(Box::new(move |ws| history::preset_rename(ws, &old, &new))),
                Action::SelectPreset(land_on),
            ]
        }
        InputKind::Install => {
            let reference = value.trim().to_string();
            if reference.is_empty() {
                return vec![];
            }
            match install::parse_ref(&reference, None, None) {
                Ok(install::InstallRef::Local(_)) => {
                    vec![Action::Spawn(crate::tui::event::Task::Install {
                        reference,
                        subpath: None,
                    })]
                }
                Ok(_) => vec![Action::Spawn(crate::tui::event::Task::DiscoverRepository(
                    reference,
                ))],
                Err(e) => vec![Action::Error(format!("{e:#}"))],
            }
        }
        InputKind::RenameTag { old } => {
            let old = old.clone();
            let new = value.trim().to_string();
            if new.is_empty() || new == old {
                return vec![];
            }
            vec![Action::WriteMeta(Box::new(move |ws| {
                history::tag_edit(ws, |ws| {
                    edit::tag_rename(ws, &old, &new).map(|n| format!("renamed tag on {n} skill(s)"))
                })
            }))]
        }
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(1);
    let h = height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Render a right-aligned button row on the last inner line; returns the button rects.
fn draw_buttons(
    f: &mut Frame,
    inner: Rect,
    buttons: &[(&str, usize)],
    active: usize,
    th: &super::theme::Theme,
) -> Vec<Rect> {
    let y = inner.bottom().saturating_sub(1);
    let labels: Vec<String> = buttons
        .iter()
        .map(|(l, n)| {
            if *n > 0 {
                format!("{l} ({n})")
            } else {
                l.to_string()
            }
        })
        .collect();
    let total: usize =
        labels.iter().map(|l| width(l) + 4).sum::<usize>() + labels.len().saturating_sub(1) * 2;
    let mut x = inner.right().saturating_sub(total as u16 + 1);
    let mut rects = Vec::new();
    let mut spans = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        let w = width(l) as u16 + 4;
        rects.push(Rect::new(x, y, w, 1));
        spans.push(button(l, i == active, th));
        spans.push(Span::raw("  "));
        x += w + 2;
    }
    let start = inner.right().saturating_sub(total as u16 + 1);
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(start, y, total as u16 + 1, 1),
    );
    rects
}

fn action_line<'a>(l: &'a str, th: &super::theme::Theme) -> Line<'a> {
    let style = if l.starts_with("link") {
        th.ok()
    } else if l.starts_with("unlink") {
        th.warn()
    } else if l.starts_with("skip") {
        th.dim()
    } else {
        th.accent()
    };
    Line::from(Span::styled(l, style))
}

fn help_line<'a>(l: &'a str, th: &super::theme::Theme) -> Line<'a> {
    if l.starts_with("  ") || l.is_empty() {
        match l.find("  ").filter(|_| l.len() > 18) {
            Some(_) => {
                let (k, d) = l.split_at(18.min(l.len()));
                Line::from(vec![Span::styled(k, th.key_hint()), Span::raw(d)])
            }
            None => Line::from(l),
        }
    } else {
        Line::from(Span::styled(l, th.bold().fg(th.accent)))
    }
}

const HELP: &str = "Search
  type              fuzzy search over name, tags, description, note
  tag:x agent:y     filters; also status:modified  source:git  untagged
  Enter / Tab       move focus: input → list → preview   (Esc goes back)
  i                 install a skill from a repo or a local path
  t  n  d           tags / note in $EDITOR / deploy picker
  r  s              rename the skill / set where it came from
  a  x              accept local changes / remove
  m                 enter multi-select (status marker also starts selection)
  Space  Ctrl-A     toggle skill / select current results in multi-select
  t  d  p           selected skills: tags / deploy / add to preset
  Esc               cancel multi-select; hidden selections never participate
  u  U              check upstream / update from upstream (git sources)
Agents
  a                 adopt an entry the agent has but the root does not
Mouse
  click             focus panes, select rows, press buttons, switch tabs
  double-click      open preview (or tag / preset / health item)
  right-click       deploy picker for that skill
  wheel             scroll lists and preview
Global
  Ctrl-Z  Ctrl-Y    undo and redo the last change
  1-5  Tab          switch tabs (Alt+1..5 while typing in the search box)
  /                 back to search      Ctrl-R  rescan      Ctrl-C  quit";

fn repository_query(alias: &str, ctx: &Ctx) -> String {
    let name = ctx
        .snap
        .skills
        .iter()
        .filter(|r| skills::repository::alias_of(&r.key) == Some(alias))
        .find_map(|r| match &r.source {
            Some(skills::meta::Source::Git { url, .. }) => skills::repository::source_name(url),
            _ => None,
        })
        .unwrap_or_else(|| alias.to_string());
    format!("repo:{name}")
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::Config, preset::Preset};

    #[test]
    fn repository_browser_moves_focus_without_skipping_and_mouse_focus_matches_keys() {
        let root = std::env::temp_dir().join(format!(
            "skills-repo-focus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut modal = Modal::picker(
            "repositories".into(),
            ["alpha", "beta", "gamma"]
                .into_iter()
                .map(|name| PickItem {
                    id: name.into(),
                    label: name.into(),
                    sub: "sample repo".into(),
                })
                .collect(),
        );
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let state = |modal: &Modal| {
            let Modal::Picker {
                input_focus, list, ..
            } = modal
            else {
                panic!("picker")
            };
            (*input_focus, list.selected())
        };
        modal.handle_key(key(KeyCode::Down), &ctx);
        assert_eq!(
            state(&modal),
            (false, Some(0)),
            "first down only enters results"
        );
        modal.handle_key(key(KeyCode::Down), &ctx);
        assert_eq!(state(&modal), (false, Some(1)));
        modal.handle_key(key(KeyCode::Up), &ctx);
        modal.handle_key(key(KeyCode::Up), &ctx);
        assert_eq!(
            state(&modal),
            (true, Some(0)),
            "up from first row restores filter"
        );
        modal.handle_key(key(KeyCode::Tab), &ctx);
        assert_eq!(state(&modal), (false, Some(0)));
        let actions = modal.handle_key(key(KeyCode::Enter), &ctx);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::Search { query, .. } if query == "repo:alpha"))
        );
        modal.handle_key(key(KeyCode::Char('/')), &ctx);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
        let Modal::Picker {
            input_rect, list, ..
        } = &modal
        else {
            panic!("picker")
        };
        let input_at = *input_rect;
        let rows_at = list.rows;
        let mouse = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        modal.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0), &ctx);
        assert_eq!(
            state(&modal),
            (true, Some(0)),
            "wheel outside the list does not move results"
        );
        modal.handle_mouse(
            mouse(MouseEventKind::ScrollDown, rows_at.x, rows_at.y),
            &ctx,
        );
        assert!(
            !state(&modal).0,
            "wheel over results transfers keyboard focus"
        );
        modal.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                input_at.x,
                input_at.y,
            ),
            &ctx,
        );
        assert!(state(&modal).0);
        modal.handle_key(key(KeyCode::Char('a')), &ctx);
        assert_eq!(
            state(&modal),
            (true, Some(0)),
            "editing filter selects its first result"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preset_search_stages_selection_without_writing() {
        let root =
            std::env::temp_dir().join(format!("skills-preset-search-{}", std::process::id()));
        std::fs::create_dir_all(root.join("printer")).unwrap();
        std::fs::write(
            root.join("printer/SKILL.md"),
            "---\nname: printer\ndescription: network tools\n---\nNetwork tools\n",
        )
        .unwrap();
        Config {
            agents: vec![],
            ..Config::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.presets
            .save(&Preset {
                name: "reading".into(),
                ..Default::default()
            })
            .unwrap();
        let before = std::fs::read(ws.presets.path("reading")).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            theme: &theme,
        };
        let mut modal = Modal::preset_members("reading", &ctx);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let Modal::PresetSkills(view) = &modal else {
            panic!("expected shared Search view")
        };
        assert!(view.input_focused());
        for c in "network tools".chars() {
            assert!(modal.handle_key(key(KeyCode::Char(c)), &ctx).is_empty());
        }
        modal.handle_key(key(KeyCode::Down), &ctx);
        assert!(modal.handle_key(key(KeyCode::Char(' ')), &ctx).is_empty());
        assert_eq!(std::fs::read(ws.presets.path("reading")).unwrap(), before);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("printer"));
        assert!(screen.contains("[✓]"));
        assert!(screen.contains("Apply"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
