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
use skills::ops::update::{self, FileChange, Prepared, Take};
use skills::ops::{MutationScope, deploy, edit, install};
use skills::reconcile::{AgentDirMode, EntryState};
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
    PresetName,
    TagName,
    TagDescription { name: String },
    PresetDescription { name: String },
    RenamePreset { old: String },
    RenameTag { old: String },
    RenameRepository { alias: String },
    Install,
    Rename { skill: String },
    SetSource { skill: String },
}

pub enum Modal {
    DeploymentChoices(Box<super::name_choices::NameChoices>),
    HealthRepair(Box<super::views::health::RepairDialog>),
    Sync(Box<super::sync_picker::SyncPicker>),
    DeployTargets(Box<super::deploy_picker::DeployPicker>),
    PresetSkills(Box<SearchView>),
    Batch(Box<super::batch::Batch>),
    Repository(Box<super::repository_picker::RepositoryPicker>),
    Help {
        scroll: u16,
    },
    Message {
        title: String,
        lines: Vec<String>,
        scroll: u16,
        return_to: Option<Box<Modal>>,
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
        background: Option<Vec<String>>,
        scope: MutationScope,
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
        Self::DeployTargets(Box::new(super::deploy_picker::DeployPicker::new(keys, ctx)))
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
            return_to: None,
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
    pub(super) fn confirm_write(title: String, lines: Vec<String>, write: WriteFn) -> Self {
        Self::confirm_meta(
            title,
            lines,
            Box::new(move |ws| write(ws).map(|m| (m, None))),
        )
    }
    pub(crate) fn confirm_meta(title: String, lines: Vec<String>, write: MetaFn) -> Self {
        Modal::ConfirmWrite {
            title,
            lines,
            write: Some(write),
            background: None,
            scope: MutationScope::Library,
            then: None,
            btn: 1,
            btn_rects: Vec::new(),
            rect: Rect::default(),
        }
    }
    /// Run an expensive confirmed operation without blocking terminal input.
    pub(crate) fn in_background(mut self, keys: Vec<String>) -> Self {
        if let Self::ConfirmWrite { background, .. } = &mut self {
            *background = Some(keys);
        }
        self
    }

    pub(crate) fn deployment_only(mut self) -> Self {
        if let Self::ConfirmWrite { scope, .. } = &mut self {
            *scope = MutationScope::Deployment;
        }
        self
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

    /// Edit a preset description in a prefilled single-line prompt. Submitting
    /// an empty value clears the description.
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
            hint: "owner/repo[/path] · Git or archive URL · local path\nEnter install · Esc cancel"
                .into(),
            rect: Rect::default(),
        }
    }

    pub fn new_tag() -> Self {
        Self::Input {
            title: " create tag ".into(),
            input: Input::default(),
            kind: InputKind::TagName,
            hint: "name · Enter create · Esc cancel".into(),
            rect: Rect::default(),
        }
    }

    pub fn tag_description(name: &str, current: Option<&str>) -> Self {
        Self::Input {
            title: format!(" description of {name} "),
            input: Input::with_value(current.unwrap_or_default()),
            kind: InputKind::TagDescription { name: name.into() },
            hint: "one sentence · Enter save · empty clears · Esc cancel".into(),
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

    pub fn rename_source(alias: &str, current_name: &str) -> Self {
        Self::Input {
            title: " source name ".into(),
            input: Input::with_value(current_name),
            kind: InputKind::RenameRepository {
                alias: alias.into(),
            },
            hint: "display name · Enter save · Esc cancel".into(),
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
            if p.members().iter().any(|s| s == old) {
                lines.push(format!("preset     {}: {old} → {new}", p.name));
            }
        }
        let (from, to) = (old.to_string(), new.to_string());
        Self::confirm_meta(
            format!(" rename {old} "),
            lines,
            Box::new(move |ws| {
                let snap = ws.scan_for_links()?;
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
            hint: format!("now {now} · owner/repo[/path], Git or archive URL"),
            rect: Rect::default(),
        }
    }

    /// Adopt an agent-owned directory into the library and leave a link at its
    /// original location. `install::adopt` creates the library metadata/baseline.
    /// Session undo does not cover adoption: restoring the original directory
    /// and metadata requires a reverse plan that this action does not record.
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
                let snap = ws.scan_for_links()?;
                edit::remove(ws, &snap, &k, false).map(|_| format!("removed {k}"))
            }),
        )
        .in_background(vec![skill.to_string()])
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
                let snap = ws.scan_for_links()?;
                edit::remove(ws, &snap, &k, false).map(|_| format!("discarded {k}"))
            }),
        )
        .in_background(vec![skill.to_string()])
    }

    /// Edit the preset's fixed membership using the shared skill selector.
    pub fn preset_members(preset: &str, ctx: &Ctx) -> Self {
        Self::PresetSkills(Box::new(SearchView::preset_members(preset, ctx)))
    }

    pub fn repositories(ctx: &Ctx) -> Self {
        Self::picker(
            " sources ".into(),
            ctx.snap
                .repositories
                .values()
                .map(|r| {
                    let count = ctx
                        .snap
                        .skills
                        .iter()
                        .filter(|s| skills::repository::alias_of(&s.key) == Some(r.alias.as_str()))
                        .count();
                    let source = r.source("", None);
                    PickItem {
                        id: r.alias.clone(),
                        label: format!(
                            "{} {}",
                            crate::tui::icons::source_icon(ctx.settings.ui.icons, &source),
                            r.display_name()
                        ),
                        sub: format!(
                            "{} {count} skills · {}",
                            crate::tui::icons::package(ctx.settings.ui.icons),
                            crate::tui::icons::source(ctx.settings.ui.icons, &source)
                        ),
                    }
                })
                .collect(),
        )
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

    pub fn resolve(prepared: Prepared) -> Self {
        let files: Vec<(String, FileChange, Take)> = prepared
            .files
            .iter()
            .filter(|(_, c)| **c != FileChange::Unchanged)
            .map(|(f, c)| {
                let take = Take::Local;
                (f.clone(), *c, take)
            })
            .collect();
        let mut list = ListNav::default();
        list.clamp(files.len());
        let default_take = if prepared.needs_resolution() {
            Take::Local
        } else {
            Take::Upstream
        };
        Modal::Resolve {
            prepared: Some(prepared),
            files,
            list,
            default_take,
            btn: 0,
            btn_rects: Vec::new(),
            rect: Rect::default(),
        }
    }

    pub fn refresh(&mut self, ctx: &Ctx) {
        match self {
            Self::Repository(picker) => picker.refresh(ctx),
            Self::PresetSkills(picker) => picker.refresh(ctx),
            _ => {}
        }
    }

    pub fn hints(&self) -> Hints {
        match self {
            Modal::DeploymentChoices(picker) => picker.hints(),
            Modal::PresetSkills(view) => view.hints(),
            Modal::HealthRepair(p) => p.hints(),
            Modal::Sync(p) => p.hints(),
            Modal::Batch(p) => p.hints(),
            Modal::Repository(p) => p.hints(),
            Modal::DeployTargets(p) => p.hints(),
            Modal::Message {
                return_to: Some(_), ..
            } => &[
                ("↑↓", "scroll"),
                ("PgUp/PgDn", "page"),
                ("Esc", "back to selection"),
            ],
            Modal::Message { title, .. } if title.trim() == "Repair results" => &[
                ("↑↓", "scroll"),
                ("PgUp/PgDn", "page"),
                ("Enter/Esc", "close"),
            ],
            Modal::Help { .. } | Modal::Message { .. } => {
                &[("↑↓", "scroll"), ("PgUp/PgDn", "page"), ("Esc", "close")]
            }
            Modal::Confirm { btn: 0, .. } | Modal::ConfirmWrite { btn: 0, .. } => {
                &[("Enter/y", "apply"), ("Esc/n", "cancel"), ("←→", "buttons")]
            }
            Modal::Confirm { .. } | Modal::ConfirmWrite { .. } => {
                &[("Enter/Esc/n", "cancel"), ("y", "apply"), ("←→", "buttons")]
            }
            Modal::Input {
                kind: InputKind::Install,
                ..
            } => &[("Enter", "install"), ("Esc", "cancel")],
            Modal::Input { .. } => &[("Enter", "save"), ("Esc", "cancel")],
            Modal::Picker {
                input_focus: true, ..
            } => &[
                ("type", "filter"),
                ("↓", "list"),
                ("Enter", "browse"),
                ("Esc", "close"),
            ],
            Modal::Picker { .. } => &[
                ("Enter", "browse"),
                ("u", "check repo"),
                ("U", "update repo"),
                ("↑↓", "move"),
                ("/", "filter"),
                ("Esc", "close"),
            ],
            Modal::Resolve { .. } => &[
                ("Space", "toggle side"),
                ("l/u", "keep local/use upstream"),
                ("Enter", "apply"),
                ("Esc", "cancel"),
            ],
        }
    }

    // ---- input ------------------------------------------------------------

    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        match self {
            Modal::PresetSkills(view) => view.paste(text, ctx),
            Modal::HealthRepair(dialog) => dialog.paste(text, ctx),
            Modal::Sync(p) => p.paste(text),
            Modal::Batch(picker) => picker.paste(text),
            Modal::Repository(picker) => picker.paste(text, ctx),
            Modal::DeployTargets(p) => p.paste(text),
            Modal::Input { input, .. } => match input.paste(text) {
                Ok(_) => vec![],
                Err(error) => vec![Action::Error(error.into())],
            },
            Modal::Picker {
                input,
                input_focus: true,
                items,
                shown,
                list,
                ..
            } => match input.paste(text) {
                Ok(true) => {
                    refilter(input.value(), items, shown);
                    list.first(shown.len());
                    vec![]
                }
                Ok(false) => vec![],
                Err(error) => vec![Action::Error(error.into())],
            },
            _ => vec![],
        }
    }

    pub fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(self, Modal::Confirm { .. } | Modal::ConfirmWrite { .. })
            && k.modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return vec![];
        }
        match self {
            Modal::DeploymentChoices(picker) => picker.key(k),
            Modal::HealthRepair(p) => p.key(k, ctx),
            Modal::Sync(p) => p.key(k, ctx),
            Modal::PresetSkills(view) => view.handle_key(k, ctx),
            Modal::Batch(p) => p.key(k, ctx),
            Modal::Repository(p) => p.key(k, ctx),
            Modal::DeployTargets(p) => p.key(k, ctx),
            Modal::Message {
                title,
                return_to: None,
                ..
            } if title.trim() == "Repair results"
                && k.code == KeyCode::Enter
                && !k.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                vec![Action::CloseModal]
            }
            Modal::Help { scroll } | Modal::Message { scroll, .. } => match k.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1);
                    vec![]
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                    vec![]
                }
                KeyCode::PageDown => {
                    *scroll = scroll.saturating_add(10);
                    vec![]
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(10);
                    vec![]
                }
                KeyCode::Home => {
                    *scroll = 0;
                    vec![]
                }
                KeyCode::Esc | KeyCode::Char('q') => vec![Action::CloseModal],
                _ => vec![],
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
                KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
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
                write,
                then,
                btn,
                background,
                scope,
                title,
                ..
            } => match k.code {
                KeyCode::Char('y') => write
                    .take()
                    .map(|w| {
                        write_actions(w, then.take(), background.take(), title.clone(), *scope)
                    })
                    .unwrap_or_default(),
                KeyCode::Enter => {
                    if *btn == 0 {
                        write
                            .take()
                            .map(|w| {
                                write_actions(
                                    w,
                                    then.take(),
                                    background.take(),
                                    title.clone(),
                                    *scope,
                                )
                            })
                            .unwrap_or_default()
                    } else {
                        vec![Action::CloseModal]
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    vec![Action::CloseModal]
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
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
                        *default_take = flip(*default_take);
                        for f in files.iter_mut() {
                            f.2 = *default_take;
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
                    KeyCode::Left | KeyCode::Right => {
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
                        let per_file = BTreeMap::new();
                        let key = p.skill.clone();
                        let rev = skills::meta::short_rev(&p.to_revision).to_string();
                        vec![
                            Action::CloseModal,
                            Action::Write(Box::new(move |ws| {
                                update::apply(ws, &p, default, &per_file).map(|_| {
                                    if default == Take::Local {
                                        format!("{key}: kept local; update skipped")
                                    } else {
                                        format!("{key} updated to {rev}")
                                    }
                                })
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
            Modal::DeploymentChoices(picker) => picker.mouse(m),
            Modal::HealthRepair(p) => p.mouse(m),
            Modal::Sync(p) => p.mouse(m, ctx),
            Modal::PresetSkills(view) => view.handle_mouse(m, ctx),
            Modal::Batch(p) => p.mouse(m, ctx),
            Modal::Repository(p) => p.mouse(m, ctx),
            Modal::DeployTargets(p) => p.mouse(m, ctx),
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
                background,
                scope,
                title,
                btn_rects,
                rect,
                ..
            } => {
                if click {
                    if btn_rects.first().is_some_and(|r| r.contains(at)) {
                        return write
                            .take()
                            .map(|w| {
                                write_actions(
                                    w,
                                    then.take(),
                                    background.take(),
                                    title.clone(),
                                    *scope,
                                )
                            })
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
                        && files.get(i).is_some()
                    {
                        *default_take = flip(*default_take);
                        for f in files.iter_mut() {
                            f.2 = *default_take;
                        }
                    }
                }
                vec![]
            }
        }
    }

    // ---- drawing ----------------------------------------------------------

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        match self {
            Modal::DeploymentChoices(picker) => picker.draw(f, area, ctx),
            Modal::PresetSkills(view) => {
                let r = centered(
                    area,
                    area.width.saturating_sub(4),
                    area.height.saturating_sub(2),
                );
                f.render_widget(Clear, r);
                let block = th.block(view.picker_title(), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                view.draw(f, inner, ctx);
            }
            Modal::HealthRepair(p) => p.draw(f, area, ctx),
            Modal::Sync(p) => p.draw(f, area, ctx),
            Modal::Batch(p) => p.draw(f, area, ctx),
            Modal::Repository(p) => p.draw(f, area, ctx),
            Modal::DeployTargets(p) => p.draw(f, area, ctx),
            Modal::Help { scroll } => {
                let lines: Vec<Line> = HELP
                    .lines()
                    .filter(|l| {
                        ctx.settings.tags_enabled
                            || (!l.to_lowercase().contains("tag") && !l.starts_with("  t "))
                    })
                    .map(|l| help_line(l, th))
                    .collect();
                let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
                let content_width = 78.min(area.width.saturating_sub(2)).saturating_sub(2);
                let total = paragraph.line_count(content_width).min(u16::MAX as usize) as u16;
                let r = centered(area, 78, total.saturating_add(2));
                let visible = r.height.saturating_sub(2);
                *scroll = (*scroll).min(total.saturating_sub(visible));
                f.render_widget(Clear, r);
                f.render_widget(
                    paragraph.scroll((*scroll, 0)).block(th.block(
                        format!(
                            " help · {}–{}/{} ",
                            (*scroll + 1).min(total),
                            (*scroll + visible).min(total),
                            total
                        ),
                        true,
                    )),
                    r,
                );
            }
            Modal::Message {
                title,
                lines,
                scroll,
                return_to,
            } => {
                let repair_result = title.trim() == "Repair results";
                let mut ls: Vec<Line> = lines
                    .iter()
                    .map(|line| {
                        if repair_result {
                            repair_result_line(line, th)
                        } else {
                            Line::from(line.as_str())
                        }
                    })
                    .collect();
                ls.push(Line::from(""));
                ls.push(Line::from(Span::styled(
                    if return_to.is_some() {
                        "Esc returns to your selection; fix the error and apply again."
                    } else if title.trim() == "Repair results" {
                        "↑↓ scroll · PgUp/PgDn page · Enter/Esc close"
                    } else {
                        "↑↓ scroll · PgUp/PgDn page · Esc close"
                    },
                    th.dim(),
                )));
                let wanted_width = if repair_result {
                    area.width.saturating_sub(8).min(112)
                } else {
                    84
                };
                let content_width = wanted_width
                    .min(area.width.saturating_sub(2))
                    .saturating_sub(2);
                let paragraph = Paragraph::new(ls).wrap(Wrap { trim: false });
                let content_height = paragraph
                    .line_count(content_width.max(1))
                    .min(u16::MAX as usize) as u16;
                let wanted_height = if repair_result {
                    content_height.saturating_add(2).max(16)
                } else {
                    content_height.saturating_add(2)
                };
                let height = wanted_height.min(if repair_result {
                    area.height.saturating_sub(4)
                } else {
                    area.height.saturating_sub(2)
                });
                let r = centered(area, wanted_width, height);
                let visible = r.height.saturating_sub(2);
                *scroll = (*scroll).min(content_height.saturating_sub(visible));
                f.render_widget(Clear, r);
                f.render_widget(
                    paragraph
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
                let width = centered(area, 70, 5).width.saturating_sub(2);
                let ls: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
                let body = Paragraph::new(ls).wrap(Wrap { trim: false });
                let height = body
                    .line_count(width)
                    .saturating_add(4)
                    .min(u16::MAX as usize) as u16;
                let r = centered(area, 70, height);
                *rect = r;
                f.render_widget(Clear, r);
                let block = th.block(format!(" {} ", title.trim()), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                f.render_widget(
                    body,
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
                let width = centered(area, 72, 4).width.saturating_sub(4);
                let hint_text = Paragraph::new(hint.as_str())
                    .style(th.dim())
                    .wrap(Wrap { trim: false });
                let hint_height = hint_text.line_count(width).min(6) as u16;
                let r = centered(area, 72, 3 + hint_height);
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
                    hint_text,
                    Rect {
                        x: inner.x + 1,
                        y: inner.y + 1,
                        width: inner.width.saturating_sub(2),
                        height: inner.height.saturating_sub(1),
                    },
                );
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
                                "   modified locally — choose the whole skill"
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
                        Span::styled("whole skill  ", th.dim()),
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
                    Line::from(if p.new_skills.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "New upstream skills (not installed): {}",
                            p.new_skills.join(", ")
                        )
                    }),
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
                    f.render_widget(Paragraph::new(Span::styled("No content diff to display. Local skips; upstream replaces the whole skill.", th.dim())), list_area);
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
fn write_actions(
    w: MetaFn,
    then: Option<Step>,
    background: Option<Vec<String>>,
    title: String,
    scope: MutationScope,
) -> Vec<Action> {
    let action = match (background, then) {
        (Some(keys), None) => Action::BackgroundWrite {
            title,
            write: w,
            keys,
        },
        _ => Action::WriteMeta(w),
    };
    let action = if scope == MutationScope::Deployment {
        Action::deployment(action)
    } else {
        action
    };
    let mut out = vec![Action::CloseModal, action];
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

fn submit(kind: &InputKind, value: String, ctx: &Ctx) -> Vec<Action> {
    match kind {
        InputKind::RenameRepository { alias } => {
            if let Err(error) = skills::repository::validate_name(&value) {
                return vec![Action::Error(error.to_string())];
            }
            let alias = alias.clone();
            vec![Action::Write(Box::new(move |ws| {
                let repo = skills::repository::Repository::rename(ws, &alias, &value)?;
                Ok(format!("Source renamed to {}", repo.display_name()))
            }))]
        }
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
        // Create the empty definition, then select it for member/description editing.
        InputKind::PresetName => {
            let name = value.trim().to_string();
            if name.is_empty() {
                return vec![];
            }
            let land_on = name.clone();
            vec![
                Action::WriteMeta(Box::new(move |ws| history::preset_create(ws, &name))),
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
        InputKind::TagName => {
            let name = value.trim().to_string();
            if name.is_empty() {
                return vec![];
            }
            let selected = name.clone();
            vec![
                Action::WriteMeta(Box::new(move |ws| {
                    anyhow::ensure!(
                        name != "(untagged)" && !name.contains(','),
                        "invalid tag name"
                    );
                    history::tag_edit(ws, |ws| {
                        let mut exists = false;
                        skills::config::Config::edit_tags(&ws.root, |tags| {
                            exists = tags.iter().any(|t| t.name == name);
                            if !exists {
                                tags.push(skills::config::TagConfig {
                                    name: name.clone(),
                                    skills: vec![],
                                    color: None,
                                    description: None,
                                });
                            }
                        })?;
                        anyhow::ensure!(!exists, "tag {name} already exists");
                        Ok(format!("created {name} — press a to add skills"))
                    })
                })),
                Action::SelectTag(selected),
            ]
        }
        InputKind::TagDescription { name } => {
            let name = name.clone();
            let description = value.trim().to_string();
            vec![Action::WriteMeta(Box::new(move |ws| {
                history::tag_edit(ws, |ws| {
                    skills::config::Config::edit_tags(&ws.root, |tags| {
                        if let Some(tag) = tags.iter_mut().find(|t| t.name == name) {
                            tag.description = (!description.is_empty()).then_some(description);
                        }
                    })?;
                    Ok(format!("updated description of {name}"))
                })
            }))]
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

fn repair_result_line<'a>(line: &'a str, th: &super::theme::Theme) -> Line<'a> {
    if line.contains(" repaired · ") && line.contains(" unchanged · ") && line.ends_with(" failed")
    {
        let mut spans = Vec::new();
        for (index, part) in line.split(" · ").enumerate() {
            if index > 0 {
                spans.push(Span::styled(" · ", th.dim()));
            }
            let count = part
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let style = if count == 0 {
                th.description()
            } else if part.ends_with(" repaired") {
                th.ok()
            } else if part.ends_with(" failed") {
                th.err()
            } else {
                th.description()
            };
            spans.push(Span::styled(
                part,
                style.add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        return Line::from(spans);
    }
    if line.ends_with(" faults remain after a fresh scan") {
        let style = if line.starts_with("0 ") {
            th.ok()
        } else {
            th.err()
        };
        return Line::from(Span::styled(line, style));
    }
    if line == "Metadata backup" {
        return Line::from(Span::styled(line, th.description()));
    }
    if let Some(path) = line.strip_prefix("  ") {
        return Line::from(vec![Span::raw("  "), Span::styled(path, th.source())]);
    }

    let Some((status, rest)) = line.split_once(' ') else {
        return Line::from(line);
    };
    let status_style = match status {
        "REPAIRED" => th.ok(),
        "FAILED" => th.err(),
        "UNCHANGED" => th.description(),
        _ => return Line::from(line),
    }
    .add_modifier(ratatui::style::Modifier::BOLD);
    let rest = rest.trim_start();
    let padding = 10usize.saturating_sub(status.len());
    let mut spans = vec![
        Span::styled(status, status_style),
        Span::raw(" ".repeat(padding)),
    ];
    if let Some((source, target)) = rest.split_once(" → ") {
        spans.push(Span::raw(source));
        spans.push(Span::styled(" → ", th.accent()));
        if let Some((target, detail)) = target.split_once(" · ") {
            spans.push(Span::styled(target, th.source()));
            spans.push(Span::styled(" · ", th.dim()));
            spans.push(Span::styled(
                detail,
                if status == "FAILED" {
                    th.err()
                } else {
                    th.description()
                },
            ));
        } else {
            spans.push(Span::styled(target, th.source()));
        }
    } else if let Some((source, detail)) = rest.split_once(" · ") {
        spans.push(Span::raw(source));
        spans.push(Span::styled(" · ", th.dim()));
        spans.push(Span::styled(
            detail,
            if status == "FAILED" {
                th.err()
            } else {
                th.description()
            },
        ));
    } else {
        spans.push(Span::raw(rest));
    }
    Line::from(spans)
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

const HELP: &str = "Startup
  Startup scanning is read-only. Broken deployment links remain unchanged until you explicitly
  analyze, review, and apply a repair in Health.
Global
  :                 search commands outside text inputs: install, repair, backup, settings
  a                 open Actions for the selected item
  ?                 help (outside text inputs)
  Ctrl-R            rescan      Ctrl-C  quit
  Ctrl-Z  Ctrl-Y    undo and redo the last change
  1-6               select a tab outside text inputs; focus stays on the tab strip
  Tab / Shift-Tab   select next / previous tab (close editing dialogs first)
  Enter / Down      enter the selected tab from the tab strip
  Esc / q           return one level inside the current tab; at the tab strip, quit
                    close overlay, end search input, cancel multi-select, clear filter, parent
                    q remains ordinary text while editing
  /                 search the focused panel
  Direct shortcuts  remain available for frequent actions; menus are the discoverable path

Library
  type              fuzzy search over name, tags, description, note
  tag:x preset:y    filters; also agent:codex  status:modified  source:repository  untagged
  Enter             accept a suggestion / open results / preview
  arrows            navigate panels and lists (Esc goes back)
  a                 Actions: tags, presets, deploy, notes, source, updates, remove
  m                 enter multi-select (status marker also starts selection)
  Space  Ctrl-A     toggle skill / select current results in multi-select
  t  d  p           selected skills: tags / deploy / add to preset
  Esc               cancel multi-select; Preset actions include selections outside the filter
  u  U              check upstream / update from upstream (Git or archive sources)
Tags / Presets
  a                 Actions for the selected group or skill
  c                 create a group
  /                 filter names on the left or skills on the right
  arrows            navigate lists and move between panels
  m                 multi-select skills in the current panel
  Ctrl-A            select current skill results; earlier selections are retained
  a                 selected skills: tags / deploy / preset operations
  Tag composition   read-only coverage of the Preset's fixed members
  Enter / click     toggle or create a tag immediately; Esc closes the picker
  Tab               complete an existing tag in the picker
  Backspace         empty tag input: select last token; press again to remove
  Esc / q           clear results filter, skills → group list → tab strip
Staged selection dialogs
  Enter / Space     toggle the current list item; activate the focused button
  Tab / Shift-Tab   list → Apply → Cancel; reverse with Shift
  Down at last item focus Apply; Up from buttons returns to the list
  Left / Right      switch Apply / Cancel when a button has focus
  o                 preview a skill in the member selector
  Esc / q           cancel without applying; search input and preview return first
Agents
  /                 filter skills
  arrows            agents → scopes → deployment groups → search → filter badges → skills
  Esc / q           clear skill filter, then groups → scope → agent → tab strip
  Enter / click     toggle group deployment: partial/empty installs all, full uninstalls all; 0/0 ignored
  +N presets/tags    expand hidden deployment groups; Esc closes without changes
  Skill badges      Left/Right selects; Enter/Space toggles a filter, preserving text and other fields
  Bold + underline  currently applied group filter in the skills panel
  a                 Actions for the selected skill
  v                 change skill layout
  [ / ]             previous / next agent
Mouse
  click             focus panes, select rows, press buttons, switch tabs
  double-click      open preview (or tag / preset / health item)
  right-click       open Actions for that item
  wheel             scroll lists and preview
";

fn repository_query(alias: &str, ctx: &Ctx) -> String {
    let name = ctx
        .snap
        .repositories
        .get(alias)
        .map(|repository| repository.display_name())
        .unwrap_or_else(|| alias.to_string());
    skills::search::source_query_token(&name)
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::Config, preset::Preset};

    #[test]
    fn startup_help_describes_read_only_explicit_repair() {
        assert!(HELP.contains("Startup scanning is read-only"));
        assert!(HELP.contains("analyze, review, and apply a repair"));
        assert!(!HELP.contains("repaired automatically"));
        assert!(!HELP.contains("are removed after"));
    }

    #[test]
    fn repair_results_close_with_enter_or_escape_without_changing_other_messages() {
        let tmp = skills::ops::DownloadDir::new("repair-results-keys").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut modal = Modal::message("Repair results", vec!["1 repaired".into()]);
        assert!(modal.hints().contains(&("Enter/Esc", "close")));
        for width in [60, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 42)).unwrap();
            terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Enter/Esc close"));
            assert!(!text.contains("page · Esc close"));
        }
        let mut narrow = Modal::message(
            "Repair results",
            vec![
                "1 repaired · 0 unchanged · 0 failed".into(),
                format!(
                    "REPAIRED  shared/{} → Library/repos/example/{}",
                    "long-skill-name-".repeat(4),
                    "long-skill-name-".repeat(4)
                ),
            ],
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|f| narrow.draw(f, f.area(), &ctx)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Repair results"));
        assert!(text.contains("Enter/Esc close"));
        for code in [
            KeyCode::Down,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::Home,
        ] {
            assert!(modal.handle_key(KeyEvent::from(code), &ctx).is_empty());
        }
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        ] {
            assert!(
                modal
                    .handle_key(KeyEvent::new(KeyCode::Enter, modifiers), &ctx)
                    .is_empty()
            );
        }
        assert!(matches!(
            modal
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &ctx)
                .as_slice(),
            [Action::CloseModal]
        ));
        for code in [KeyCode::Enter, KeyCode::Esc] {
            assert!(matches!(
                modal.handle_key(KeyEvent::from(code), &ctx).as_slice(),
                [Action::CloseModal]
            ));
        }
        for mut other in [
            Modal::help(),
            Modal::message("Other results", vec![]),
            Modal::Message {
                title: "Repair results".into(),
                lines: vec![],
                scroll: 0,
                return_to: Some(Box::new(Modal::help())),
            },
        ] {
            assert!(!other.hints().contains(&("Enter/Esc", "close")));
            assert!(
                other
                    .handle_key(KeyEvent::from(KeyCode::Enter), &ctx)
                    .is_empty()
            );
            assert!(matches!(
                other
                    .handle_key(KeyEvent::from(KeyCode::Esc), &ctx)
                    .as_slice(),
                [Action::CloseModal]
            ));
        }
    }

    #[test]
    fn repair_result_lines_style_status_arrows_and_targets() {
        let theme = Theme::default();
        let repaired = repair_result_line(
            "REPAIRED  shared/lark-doc → Library/repos/larksuite--cli/lark-doc",
            &theme,
        );
        assert_eq!(repaired.spans[0].content, "REPAIRED");
        assert_eq!(repaired.spans[0].style.fg, Some(theme.ok));
        assert_eq!(repaired.spans[3].content, " → ");
        assert_eq!(repaired.spans[3].style.fg, Some(theme.accent));
        assert_eq!(
            repaired.spans[4].content,
            "Library/repos/larksuite--cli/lark-doc"
        );
        assert_eq!(repaired.spans[4].style.fg, Some(theme.source));

        let failed = repair_result_line(
            "FAILED    shared/foo → Library/bar · destination changed",
            &theme,
        );
        assert_eq!(failed.spans[0].style.fg, Some(theme.err));
        assert_eq!(failed.spans.last().unwrap().style.fg, Some(theme.err));

        let summary = repair_result_line("76 repaired · 2 unchanged · 1 failed", &theme);
        assert_eq!(summary.spans[0].style.fg, Some(theme.ok));
        assert_eq!(
            summary.spans[2].style,
            theme
                .description()
                .add_modifier(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(summary.spans[4].style.fg, Some(theme.err));
        let clean = repair_result_line("76 repaired · 0 unchanged · 0 failed", &theme);
        assert_eq!(
            clean.spans[2].style,
            theme
                .description()
                .add_modifier(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(
            clean.spans[4].style,
            theme
                .description()
                .add_modifier(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn install_hint_wraps_without_losing_cancel_instruction() {
        let root = std::env::temp_dir().join(format!("skills-install-hint-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let theme = Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        for width in [40, 60, 80] {
            let mut modal = Modal::install();
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(
                text.contains("Enter install · Esc cancel"),
                "{width}: {text}"
            );
            assert!(text.contains("owner/repo[/path]"));
            assert!(!text.contains("Esc c…"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

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
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
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
        modal.handle_key(key(KeyCode::Down), &ctx);
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
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
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
