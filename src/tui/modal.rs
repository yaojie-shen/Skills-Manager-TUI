//! Overlays: help, messages, confirmations, text prompts, agent picker,
//! and the update conflict resolver.
//! Filtered repository choices retain their unfiltered viewport on each draw.
//! Outside presses cancel through the owner; complex dialogs route children first.

use super::app::{Action, Ctx, Hints, MetaFn, Step, WriteFn};
use super::views::{View, search::SearchView};
use super::widgets::{
    ButtonIntent, ButtonSpec, Input, ListNav, PopupClass, centered, draw_button_row, fit,
    popup_size, popup_too_small,
};
use crate::tui::widgets::PopupSurface;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Normal,
    RepairResults,
    MigrationRecovery,
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
        rect: Rect,
    },
    Message {
        kind: MessageKind,
        title: String,
        lines: Vec<String>,
        scroll: u16,
        return_to: Option<Box<Modal>>,
        rect: Rect,
    },
    InstallComplete {
        keys: Vec<String>,
        message: String,
        btn: usize,
        btn_rects: Vec<Rect>,
        rect: Rect,
    },
    /// Preview of link changes with Apply / Cancel.
    Confirm {
        title: String,
        lines: Vec<String>,
        actions: Vec<deploy::Action>,
        intent: ButtonIntent,
        apply_label: &'static str,
        /// Emitted only once the changes actually went through, so a cancelled
        /// or failed confirmation leaves the history where it was.
        then: Option<Step>,
        scroll: u16,
        btn: usize,
        btn_rects: Vec<Rect>,
        rect: Rect,
    },
    /// Write confirmation with an explicit action label and semantic intent.
    ConfirmWrite {
        title: String,
        lines: Vec<String>,
        write: Option<MetaFn>,
        background: Option<Vec<String>>,
        scope: MutationScope,
        /// Set when this confirmation is a history step.
        then: Option<Step>,
        after: Option<Box<Action>>,
        apply_label: String,
        intent: ButtonIntent,
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
        files: Vec<(String, FileChange)>,
        list: ListNav,
        default_take: Take,
        btn: usize,
        btn_rects: Vec<Rect>,
        rect: Rect,
    },
}

impl Modal {
    /// Whether this dialog contains an in-progress edit of Library state.
    ///
    /// Read-only overlays deliberately return false so a background root
    /// reconcile does not wait for help, status, or navigation dialogs.
    pub fn library_edit_active(&self) -> bool {
        matches!(
            self,
            Self::Input { .. }
                | Self::ConfirmWrite {
                    scope: MutationScope::Library,
                    ..
                }
                | Self::Resolve { .. }
        )
    }

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
        Modal::Help {
            scroll: 0,
            rect: Rect::default(),
        }
    }
    pub fn message(title: impl Into<String>, lines: Vec<String>) -> Self {
        Modal::Message {
            kind: MessageKind::Normal,
            title: title.into(),
            lines,
            scroll: 0,
            return_to: None,
            rect: Rect::default(),
        }
    }
    pub fn repair_results(lines: Vec<String>) -> Self {
        Modal::Message {
            kind: MessageKind::RepairResults,
            title: "Repair results".into(),
            lines,
            scroll: 0,
            return_to: None,
            rect: Rect::default(),
        }
    }
    pub fn migration_recovery(report: &skills::migration::MigrationReport) -> Self {
        Modal::Message {
            kind: MessageKind::MigrationRecovery,
            title: "Metadata migrated".into(),
            lines: report.recovery_lines(),
            scroll: 0,
            return_to: None,
            rect: Rect::default(),
        }
    }
    pub fn install_complete(keys: Vec<String>, message: String) -> Self {
        Modal::InstallComplete {
            keys,
            message,
            btn: 0,
            btn_rects: Vec::new(),
            rect: Rect::default(),
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
        let removes = actions.iter().any(|a| {
            matches!(
                a,
                deploy::Action::Unlink { .. }
                    | deploy::Action::Clean { .. }
                    | deploy::Action::Relink { .. }
            )
        });
        let apply_label = if actions
            .iter()
            .any(|a| matches!(a, deploy::Action::Relink { .. }))
        {
            "Relink"
        } else if removes {
            "Remove"
        } else {
            "Apply"
        };
        Modal::Confirm {
            title,
            lines,
            actions,
            intent: if removes {
                ButtonIntent::Danger
            } else {
                ButtonIntent::Normal
            },
            apply_label,
            then,
            scroll: 0,
            btn: usize::from(removes),
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
            after: None,
            apply_label: "Apply".into(),
            intent: ButtonIntent::Normal,
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

    pub(crate) fn apply_label(mut self, label: &str) -> Self {
        if let Self::ConfirmWrite { apply_label, .. } = &mut self {
            *apply_label = label.to_string();
        }
        self
    }

    /// Destructive actions always start on Cancel, regardless of builder order.
    pub(crate) fn destructive(mut self, label: &str) -> Self {
        if let Self::ConfirmWrite {
            apply_label,
            intent,
            btn,
            ..
        } = &mut self
        {
            *apply_label = label.into();
            *intent = ButtonIntent::Danger;
            *btn = 1;
        }
        self
    }
    fn after(mut self, action: Action) -> Self {
        if let Self::ConfirmWrite { after, .. } = &mut self {
            *after = Some(Box::new(action));
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
            hint: "name".into(),
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
            hint: "one sentence · empty clears".into(),
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
            hint: "new name".into(),
            rect: Rect::default(),
        }
    }
    /// Ask where to fetch a skill from. Accepts what `skills install` accepts.
    pub fn install() -> Self {
        Modal::Input {
            title: " install a skill ".into(),
            input: Input::default(),
            kind: InputKind::Install,
            hint: "owner/repo[/path] · Git or archive URL · local path\nUse owner/repo#branch or a GitHub /tree/ URL"
                .into(),
            rect: Rect::default(),
        }
    }

    pub fn new_tag() -> Self {
        Self::Input {
            title: " create tag ".into(),
            input: Input::default(),
            kind: InputKind::TagName,
            hint: "name".into(),
            rect: Rect::default(),
        }
    }

    pub fn tag_description(name: &str, current: Option<&str>) -> Self {
        Self::Input {
            title: format!(" description of {name} "),
            input: Input::with_value(current.unwrap_or_default()),
            kind: InputKind::TagDescription { name: name.into() },
            hint: "one sentence · empty clears".into(),
            rect: Rect::default(),
        }
    }

    pub fn rename_tag(old: &str) -> Self {
        Modal::Input {
            title: format!(" rename tag {old} "),
            input: Input::with_value(old),
            kind: InputKind::RenameTag { old: old.into() },
            hint: "new name · updates all references".into(),
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
            hint: "display name".into(),
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
            hint: "new name · changes are reviewed before applying".into(),
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
        .apply_label("Rename")
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
        .apply_label("Adopt")
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
        .destructive("Delete")
    }
    pub fn delete_preset(name: &str) -> Self {
        let n = name.to_string();
        Self::confirm_meta(
            format!(" delete preset {name} "),
            vec![format!(
                "Delete preset \"{name}\"? Deployed links are left as they are."
            )],
            Box::new(move |ws| history::preset_delete(ws, &n)),
        )
        .destructive("Delete")
    }
    pub fn remove_source(alias: &str, name: &str, summary: &edit::RepositoryRemoveSummary) -> Self {
        let alias = alias.to_string();
        let mut lines = vec![
            format!("Remove local source \"{name}\"?"),
            String::new(),
            "This will remove locally:".into(),
            format!("• {} installed/recorded skill(s)", summary.keys.len()),
            "• source registration, skill metadata, Tags and Preset references".into(),
        ];
        if summary.modified > 0 {
            lines.push(String::new());
            lines.push(format!(
                "Warning: {} skill(s) have local modifications.",
                summary.modified
            ));
        }
        if summary.missing > 0 {
            lines.push(format!(
                "{} recorded skill(s) are already missing locally.",
                summary.missing
            ));
        }
        if summary.attention > 0 {
            lines.push(format!(
                "{} additional skill(s) need attention and will also be removed.",
                summary.attention
            ));
        }
        lines.push(String::new());
        lines.push("Existing Global and Local deployment links will be left unchanged.".into());
        lines.push(
            "Links to removed skills will become broken; clean them explicitly in Agents or Health."
                .into(),
        );
        lines.push(String::new());
        lines.push("GitHub and the remote repository will not be changed.".into());
        let keys = summary.keys.clone();
        Self::confirm_write(
            format!(" remove local source {name} "),
            lines,
            Box::new(move |ws| {
                let report = edit::remove_repository(ws, &alias)?;
                Ok(format!(
                    "removed source {alias} and {} skill(s)",
                    report.keys.len()
                ))
            }),
        )
        .destructive("Remove source and skills")
        .in_background(keys)
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
        .destructive("Remove")
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
        .destructive("Delete")
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
        fn removes_skills(back: &history::WriteBack) -> bool {
            match back {
                history::WriteBack::RemoveInstalled { .. } => true,
                history::WriteBack::TargetDeployment {
                    expected, desired, ..
                } => expected.difference(desired).next().is_some(),
                history::WriteBack::Group(writes) => writes.iter().any(removes_skills),
                _ => false,
            }
        }
        let destructive = removes_skills(&back);
        let verb = if dir == Step::Undo { "undo" } else { "redo" };
        let modal = Self::confirm_write(
            format!(" {verb} "),
            vec![format!("{}?", capitalize(&describe))],
            Box::new(move |ws| back.apply(ws)),
        )
        .apply_label(if dir == Step::Undo { "Undo" } else { "Redo" })
        .with_step(dir);
        if destructive {
            modal.destructive("Remove")
        } else {
            modal
        }
    }

    /// Carry the history move onto a write confirmation. The Apply button
    /// takes the focus here: a destructive confirmation starts on Cancel so a
    /// second Enter cannot delete anything, but an undo was asked for with
    /// Ctrl-Z a moment ago, and Enter should do what was asked rather than
    /// quietly cancel it. Explicit destructive intent still keeps Cancel focused.
    fn with_step(mut self, dir: Step) -> Self {
        if let Modal::ConfirmWrite {
            then, btn, intent, ..
        } = &mut self
        {
            *then = Some(dir);
            if *intent != ButtonIntent::Danger {
                *btn = 0;
            }
        }
        self
    }

    pub fn resolve(prepared: Prepared) -> Self {
        let files: Vec<(String, FileChange)> = prepared
            .files
            .iter()
            .filter(|(_, c)| **c != FileChange::Unchanged)
            .map(|(f, c)| (f.clone(), *c))
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

    pub fn accepts_file_browser(&mut self, output: &super::views::file_browser::Output) -> bool {
        match self {
            Self::PresetSkills(view) => view.accepts_file_browser(output),
            Self::Repository(picker) => picker.accepts_file_browser(output),
            Self::Message {
                return_to: Some(modal),
                ..
            } => modal.accepts_file_browser(output),
            _ => false,
        }
    }

    pub fn apply_file_browser(
        &mut self,
        output: super::views::file_browser::Output,
    ) -> Vec<super::views::file_browser::Request> {
        match self {
            Self::PresetSkills(view) => view.apply_file_browser(output),
            Self::Repository(picker) => picker.apply_file_browser(output),
            Self::Message {
                return_to: Some(modal),
                ..
            } => modal.apply_file_browser(output),
            _ => vec![],
        }
    }

    pub fn take_file_browser_requests(&mut self) -> Vec<super::views::file_browser::Request> {
        match self {
            Self::PresetSkills(view) => view.take_file_browser_requests(),
            Self::Repository(picker) => picker.take_file_browser_requests(),
            Self::Message {
                return_to: Some(modal),
                ..
            } => modal.take_file_browser_requests(),
            _ => vec![],
        }
    }

    pub fn help_keys(&self) -> Vec<(&'static str, &'static str)> {
        if let Self::PresetSkills(view) = self {
            return view.hints().to_vec();
        }
        let mut keys = self.hints().to_vec();
        if matches!(
            self,
            Self::Input { .. }
                | Self::Picker {
                    input_focus: true,
                    ..
                }
        ) || keys.iter().any(|(key, _)| {
            *key == "type" || *key == "Tab" && keys.iter().any(|(_, desc)| *desc == "complete")
        }) {
            keys.extend_from_slice(super::footer::INPUT_KEYS);
        }
        if matches!(self, Self::Help { .. } | Self::Message { .. }) {
            keys.extend([("j/k", "scroll"), ("Home", "first line"), ("q", "close")]);
        }
        keys
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
            Modal::Message {
                kind: MessageKind::RepairResults | MessageKind::MigrationRecovery,
                ..
            } => &[
                ("↑↓", "scroll"),
                ("PgUp/PgDn", "page"),
                ("Enter/Esc", "close"),
            ],
            Modal::Help { .. } | Modal::Message { .. } => {
                &[("↑↓", "scroll"), ("PgUp/PgDn", "page"), ("Esc", "close")]
            }
            Modal::InstallComplete { btn: 0, .. } => &[("Enter/Esc", "done"), ("←→", "buttons")],
            Modal::InstallComplete { .. } => {
                &[("Enter", "deploy now"), ("Esc", "done"), ("←→", "buttons")]
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
            } => &[("Enter", "discover"), ("Esc", "cancel")],
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
            Modal::Resolve { btn: 1, .. } => &[
                ("Enter", "cancel"),
                ("←→", "buttons"),
                ("y", "apply choice"),
                ("Esc", "cancel"),
            ],
            Modal::Resolve { .. } => &[
                ("Enter", "apply choice"),
                ("←→", "buttons"),
                ("Space", "switch whole-skill choice"),
                ("l/u", "keep whole skill/use upstream"),
                ("Esc", "cancel"),
            ],
        }
    }

    /// Descriptions and keys use the same modal/child focus as input dispatch.
    pub fn footer_help(&self) -> super::footer::Help<'_> {
        use super::footer::Help;
        match self {
            Self::DeploymentChoices(p) => p.footer_help(),
            Self::PresetSkills(p) => p.footer_help(),
            Self::HealthRepair(p) => p.footer_help(),
            Self::Sync(p) => p.footer_help(),
            Self::Batch(p) => p.footer_help(),
            Self::Repository(p) => p.footer_help(),
            Self::DeployTargets(p) => p.footer_help(),
            Self::Input { hint, .. } => Help::new(self.hints()).detail(hint.as_str()),
            Self::Message {
                return_to: Some(_), ..
            } => Help::new(self.hints())
                .detail("Return to the selection to fix the error and apply again"),
            Self::Confirm { btn: 0, .. } | Self::ConfirmWrite { btn: 0, .. } => {
                Help::new(self.hints()).detail("Apply the changes shown in this confirmation")
            }
            Self::Confirm { .. } | Self::ConfirmWrite { .. } => {
                Help::new(self.hints()).detail("Close without applying the proposed changes")
            }
            Self::InstallComplete { btn: 0, .. } => {
                Help::new(self.hints()).detail("Finish without changing deployments")
            }
            Self::InstallComplete { .. } => {
                Help::new(self.hints()).detail("Choose agents for the newly installed skills")
            }
            Self::Resolve { btn: 1, .. } => {
                Help::new(self.hints()).detail("Cancel this update and keep current files")
            }
            Self::Resolve {
                default_take: Take::Local,
                ..
            } => Help::new(self.hints())
                .detail("Keep the entire local skill; skip this upstream update"),
            Self::Resolve { .. } => {
                Help::new(self.hints()).detail("Replace the entire skill with upstream content")
            }
            _ => Help::new(self.hints()),
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
                kind: MessageKind::RepairResults | MessageKind::MigrationRecovery,
                return_to: None,
                ..
            } if k.code == KeyCode::Enter
                && !k.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                vec![Action::CloseModal]
            }
            Modal::InstallComplete { keys, btn, .. } => match k.code {
                KeyCode::Enter if *btn == 1 => vec![Action::OpenModal(Box::new(
                    Modal::batch_deploy(std::mem::take(keys), ctx),
                ))],
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => vec![Action::CloseModal],
                KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                    *btn = 1 - *btn;
                    vec![]
                }
                _ => vec![],
            },
            Modal::Help { scroll, .. } | Modal::Message { scroll, .. } => match k.code {
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
                after,
                btn,
                background,
                scope,
                title,
                ..
            } => match k.code {
                KeyCode::Char('y') => write
                    .take()
                    .map(|w| {
                        write_actions(
                            w,
                            then.take(),
                            after.take(),
                            background.take(),
                            title.clone(),
                            *scope,
                        )
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
                                    after.take(),
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
                        vec![]
                    }
                    KeyCode::Char('l') if !ctrl => {
                        *default_take = Take::Local;
                        vec![]
                    }
                    KeyCode::Char('u') if !ctrl => {
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
        // Complex owners route their children before checking parent bounds.
        let outer = match self {
            Modal::Help { rect, .. }
            | Modal::Message { rect, .. }
            | Modal::InstallComplete { rect, .. }
            | Modal::Confirm { rect, .. }
            | Modal::ConfirmWrite { rect, .. }
            | Modal::Input { rect, .. }
            | Modal::Picker { rect, .. }
            | Modal::Resolve { rect, .. } => Some(*rect),
            _ => None,
        };
        if let Some(outer) = outer {
            match super::widgets::popup_mouse(m, outer) {
                super::widgets::PopupMouse::OutsidePress => {
                    if let Modal::Resolve { prepared, .. } = self {
                        if let Some(p) = prepared.take() {
                            p.cleanup();
                        }
                        return vec![Action::CloseModal, Action::Toast("update cancelled".into())];
                    }
                    return vec![Action::CloseModal];
                }
                super::widgets::PopupMouse::Inside => {}
                _ => return vec![],
            }
        }
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
            Modal::InstallComplete {
                keys,
                btn_rects,
                rect,
                ..
            } => {
                if click {
                    if btn_rects.first().is_some_and(|r| r.contains(at)) || !rect.contains(at) {
                        return vec![Action::CloseModal];
                    }
                    if btn_rects.get(1).is_some_and(|r| r.contains(at)) {
                        return vec![Action::OpenModal(Box::new(Modal::batch_deploy(
                            std::mem::take(keys),
                            ctx,
                        )))];
                    }
                }
                vec![]
            }
            Modal::Help { scroll, .. } | Modal::Message { scroll, .. } => {
                if let Some(d) = wheel {
                    *scroll = (*scroll as i32 + d).max(0) as u16;
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
                after,
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
                                    after.take(),
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
                    if list.rows.contains(at)
                        && let Some((row, double)) = list.click(m.row, shown.len())
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
                    if list.rows.contains(at) {
                        let _ = list.click(m.row, n);
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
                f.render_widget(PopupSurface, r);
                let block = th.block(view.picker_title(), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                view.draw(f, inner, ctx);
                view.set_picker_rect(r);
            }
            Modal::HealthRepair(p) => p.draw(f, area, ctx),
            Modal::Sync(p) => p.draw(f, area, ctx),
            Modal::Batch(p) => p.draw(f, area, ctx),
            Modal::Repository(p) => p.draw(f, area, ctx),
            Modal::DeployTargets(p) => p.draw(f, area, ctx),
            Modal::Help { scroll, rect } => {
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
                *rect = r;
                let visible = r.height.saturating_sub(2);
                *scroll = (*scroll).min(total.saturating_sub(visible));
                f.render_widget(PopupSurface, r);
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
                kind,
                title,
                lines,
                scroll,
                rect,
                ..
            } => {
                let repair_result = *kind == MessageKind::RepairResults;
                let expanded = matches!(
                    kind,
                    MessageKind::RepairResults | MessageKind::MigrationRecovery
                );
                let ls: Vec<Line> = lines
                    .iter()
                    .map(|line| {
                        if repair_result {
                            repair_result_line(line, th)
                        } else {
                            Line::from(line.as_str())
                        }
                    })
                    .collect();
                let class = if expanded {
                    PopupClass::Preview
                } else {
                    PopupClass::Choice
                };
                let content_width = popup_size(area, class, 3, (1, 1)).inner.width;
                let paragraph = Paragraph::new(ls).wrap(Wrap { trim: false });
                let content_height = paragraph
                    .line_count(content_width.max(1))
                    .min(u16::MAX as usize) as u16;
                let size = popup_size(area, class, content_height.saturating_add(2), (1, 1));
                let r = size.outer;
                *rect = r;
                let visible = r.height.saturating_sub(2);
                *scroll = (*scroll).min(content_height.saturating_sub(visible));
                f.render_widget(PopupSurface, r);
                f.render_widget(
                    paragraph
                        .scroll((*scroll, 0))
                        .block(th.block(format!(" {} ", title.trim()), true)),
                    r,
                );
                if !size.usable {
                    popup_too_small(f, size, th);
                }
            }
            Modal::InstallComplete {
                message,
                btn,
                btn_rects,
                rect,
                ..
            } => {
                let body = Paragraph::new(message.as_str()).wrap(Wrap { trim: false });
                btn_rects.clear();
                let width = popup_size(area, PopupClass::Input, 5, (12, 3)).inner.width;
                let height = body
                    .line_count(width)
                    .saturating_add(4)
                    .min(u16::MAX as usize) as u16;
                let size = popup_size(area, PopupClass::Input, height, (12, 3));
                let r = size.outer;
                *rect = r;
                f.render_widget(PopupSurface, r);
                let block = th.block(" install complete ", true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                if !size.usable {
                    *btn_rects = vec![Rect::default(); 2];
                    popup_too_small(f, size, th);
                    return;
                }
                f.render_widget(
                    body,
                    Rect {
                        height: inner.height.saturating_sub(2),
                        ..inner
                    },
                );
                *btn_rects = draw_buttons(f, inner, &[("Done", 0), ("Deploy now", 0)], *btn, th);
            }
            Modal::Confirm {
                title,
                lines,
                intent,
                apply_label,
                scroll,
                btn,
                btn_rects,
                rect,
                ..
            } => {
                btn_rects.clear();
                let size = popup_size(
                    area,
                    PopupClass::Preview,
                    (lines.len().min(u16::MAX as usize) as u16).saturating_add(5),
                    (12, 3),
                );
                let r = size.outer;
                *rect = r;
                f.render_widget(PopupSurface, r);
                let block = th.block(format!(" {} ", title.trim()), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                if !size.usable {
                    *btn_rects = vec![Rect::default(); 2];
                    popup_too_small(f, size, th);
                    return;
                }
                let body = Rect {
                    height: inner.height.saturating_sub(2),
                    ..inner
                };
                let ls: Vec<Line> = lines.iter().map(|l| action_line(l, th)).collect();
                f.render_widget(Paragraph::new(ls).scroll((*scroll, 0)), body);
                let n_changes = lines.iter().filter(|l| !l.starts_with("skip")).count();
                let label = format!("{apply_label} ({n_changes})");
                *btn_rects = draw_button_row(
                    f,
                    inner,
                    &[
                        ButtonSpec {
                            label: &label,
                            enabled: true,
                            intent: *intent,
                        },
                        ButtonSpec::new("Cancel"),
                    ],
                    Some(*btn),
                    th,
                );
            }
            Modal::ConfirmWrite {
                title,
                lines,
                apply_label,
                intent,
                btn,
                btn_rects,
                rect,
                ..
            } => {
                btn_rects.clear();
                let width = popup_size(area, PopupClass::Input, 5, (12, 3)).inner.width;
                let ls: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
                let body = Paragraph::new(ls).wrap(Wrap { trim: false });
                let height = body
                    .line_count(width)
                    .saturating_add(4)
                    .min(u16::MAX as usize) as u16;
                let size = popup_size(area, PopupClass::Input, height, (12, 3));
                let r = size.outer;
                *rect = r;
                f.render_widget(PopupSurface, r);
                let block = th.block(format!(" {} ", title.trim()), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                if !size.usable {
                    *btn_rects = vec![Rect::default(); 2];
                    popup_too_small(f, size, th);
                    return;
                }
                f.render_widget(
                    body,
                    Rect {
                        height: inner.height.saturating_sub(2),
                        ..inner
                    },
                );
                *btn_rects = draw_button_row(
                    f,
                    inner,
                    &[
                        ButtonSpec {
                            label: apply_label,
                            enabled: true,
                            intent: *intent,
                        },
                        ButtonSpec::new("Cancel"),
                    ],
                    Some(*btn),
                    th,
                );
            }
            Modal::Input {
                title, input, rect, ..
            } => {
                input.clear_area();
                let size = popup_size(area, PopupClass::Input, 3, (3, 1));
                let r = size.outer;
                *rect = r;
                f.render_widget(PopupSurface, r);
                let block = th.block(title.as_str(), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                if !size.usable {
                    popup_too_small(f, size, th);
                    return;
                }
                let field = Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    height: 1,
                    ..inner
                };
                input.render(f, field, true, "", th);
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
                input.clear_area();
                *input_rect = Rect::default();
                list.rows = Rect::default();
                let size = popup_size(
                    area,
                    PopupClass::Choice,
                    (items.len().min(u16::MAX as usize) as u16).saturating_add(5),
                    (4, 3),
                );
                let r = size.outer;
                *rect = r;
                f.render_widget(PopupSurface, r);
                let block = th.block(title.as_str(), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                if !size.usable {
                    popup_too_small(f, size, th);
                    return;
                }
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
                        let available = (list_area.width as usize).saturating_sub(6);
                        let label_width = available.min(26);
                        let label = fit(&it.label, label_width);
                        let sub_width = available.saturating_sub(super::widgets::width(&label));
                        ListItem::new(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(label, Style::default()),
                            Span::raw("  "),
                            Span::styled(fit(&it.sub, sub_width), th.dim()),
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
                btn_rects.clear();
                list.rows = Rect::default();
                let modal_width = popup_size(area, PopupClass::Preview, 6, (12, 4))
                    .outer
                    .width;
                let content_width = modal_width.saturating_sub(2).max(1);
                let new_skills = new_upstream_skills_text(&p.new_skills);
                let new_skill_height = new_skills
                    .as_ref()
                    .map(|text| {
                        Paragraph::new(text.as_str())
                            .wrap(Wrap { trim: false })
                            .line_count(content_width) as u16
                    })
                    .unwrap_or(0);
                let head_height = 3 + new_skill_height;
                let size = popup_size(
                    area,
                    PopupClass::Preview,
                    (files.len().min(u16::MAX as usize) as u16).saturating_add(head_height + 6),
                    (12, 4),
                );
                let r = size.outer;
                *rect = r;
                f.render_widget(PopupSurface, r);
                let block = th.block(format!(" update {} ", p.skill), true);
                let inner = block.inner(r);
                f.render_widget(block, r);
                if !size.usable {
                    popup_too_small(f, size, th);
                    return;
                }
                let head_height = head_height.min(inner.height.saturating_sub(3));

                let choice = whole_skill_choice(*default_take);
                let mut head = vec![
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
                                "   local changes detected"
                            } else {
                                "   no local changes"
                            },
                            if p.needs_resolution() {
                                th.warn()
                            } else {
                                th.ok()
                            },
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("Whole-skill choice: ", th.dim()),
                        Span::styled(choice, th.accent()),
                    ]),
                    Line::from(Span::styled(
                        if p.baseline_dir.is_some() {
                            "File comparison: installed revision → local and upstream"
                        } else {
                            "File comparison: local ↔ upstream (installed revision unavailable)"
                        },
                        th.dim(),
                    )),
                ];
                if let Some(text) = new_skills {
                    head.push(Line::from(text));
                }
                f.render_widget(
                    Paragraph::new(head).wrap(Wrap { trim: false }),
                    Rect {
                        height: head_height.min(inner.height),
                        ..inner
                    },
                );
                let list_area = Rect {
                    y: inner.y + head_height,
                    height: inner.height.saturating_sub(head_height + 2),
                    ..inner
                };
                list.rows = list_area;
                let items: Vec<ListItem> = files
                    .iter()
                    .map(|(name, change)| {
                        let (label, style) = match change {
                            FileChange::UpstreamChanged => ("upstream changed", th.accent()),
                            FileChange::LocalChanged => ("local changed   ", th.warn()),
                            FileChange::BothChanged => ("both changed    ", th.err()),
                            FileChange::Differs => ("differs         ", th.warn()),
                            FileChange::Unchanged => ("unchanged       ", th.dim()),
                        };
                        ListItem::new(Line::from(vec![
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
                    f.render_widget(
                        Paragraph::new(Span::styled("No file changes to display.", th.dim())),
                        list_area,
                    );
                }
                *btn_rects =
                    draw_buttons(f, inner, &[("Apply choice", 0), ("Cancel", 0)], *btn, th);
            }
        }
    }
}

fn whole_skill_choice(take: Take) -> &'static str {
    match take {
        Take::Local => "Keep local whole skill (skip this update)",
        Take::Upstream => "Use upstream whole skill (replace local content)",
    }
}

fn new_upstream_skills_text(skills: &[String]) -> Option<String> {
    (!skills.is_empty())
        .then(|| format!("New upstream skills (not installed): {}", skills.join(", ")))
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
    after: Option<Box<Action>>,
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
    if let Some(action) = after {
        out.push(*action);
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

fn merge_confirm(kind: &str, old: &str, new: &str, write: MetaFn) -> Modal {
    Modal::confirm_meta(
        format!(" merge {old} into {new} "),
        vec![
            format!("{kind} {new} already exists."),
            format!(
                "Merge combines skills, keeps {new}'s colour and description, and removes {old}."
            ),
        ],
        write,
    )
    .destructive("Merge")
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
            let old = old.clone();
            let land_on = new.clone();
            if matches!(ctx.ws.presets.conflicts(&old, &new), Ok(true)) {
                let merge_old = old.clone();
                let merge_new = new.clone();
                vec![Action::OpenModal(Box::new(
                    merge_confirm(
                        "preset",
                        &old,
                        &new,
                        Box::new(move |ws| history::preset_merge(ws, &merge_old, &merge_new)),
                    )
                    .after(Action::SelectPreset(land_on)),
                ))]
            } else {
                vec![
                    Action::WriteMeta(Box::new(move |ws| history::preset_rename(ws, &old, &new))),
                    Action::SelectPreset(land_on),
                ]
            }
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
                Ok(parsed) => vec![Action::Spawn(crate::tui::event::Task::DiscoverRepository {
                    label: reference,
                    reference: parsed,
                })],
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
                        Ok(format!("created {name} — open Actions to edit skills"))
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
            if matches!(ctx.ws.tags.conflicts(&old, &new), Ok(true)) {
                let merge_old = old.clone();
                let merge_new = new.clone();
                vec![Action::OpenModal(Box::new(merge_confirm(
                    "Tag",
                    &old,
                    &new,
                    Box::new(move |ws| {
                        history::tag_edit(ws, |ws| {
                            edit::tag_merge(ws, &merge_old, &merge_new).map(|n| {
                                format!("merged {merge_old} into {merge_new} on {n} skill(s)")
                            })
                        })
                    }),
                )))]
            } else {
                vec![Action::WriteMeta(Box::new(move |ws| {
                    history::tag_edit(ws, |ws| {
                        edit::tag_rename(ws, &old, &new)
                            .map(|n| format!("renamed tag on {n} skill(s)"))
                    })
                }))]
            }
        }
    }
}

/// Render a right-aligned button row on the last inner line; returns the button rects.
fn draw_buttons(
    f: &mut Frame,
    inner: Rect,
    buttons: &[(&str, usize)],
    active: usize,
    th: &super::theme::Theme,
) -> Vec<Rect> {
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
    let specs: Vec<_> = labels.iter().map(|l| ButtonSpec::new(l)).collect();
    draw_button_row(f, inner, &specs, Some(active), th)
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
  F1                current-focus help from any control; click the footer message for the same help
                    Esc / Enter closes; arrows, Page Up/Down, Home/End and wheel scroll
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
  F2 / View         categorized options: layout, group by, skill sort, group order
                    Left/Right or Tab switches category; Up/Down applies settings immediately
                    Enter, Esc, or F2 closes the menu and keeps applied settings
  v / V             cycle skill layout while browsing results
  Enter / click     on a group header: collapse / expand; Left closes, Right opens
  Ctrl-Left/Right   collapse / expand all matching groups while results are focused
                    or use the title-bar buttons
  Auto view         browse by repository; text searches are flat and relevance-ranked
  a                 Actions: tags, presets, deploy, notes, source, updates, remove
  m                 enter multi-select (status marker also starts selection)
  Space  Ctrl-A     toggle skill / select current results in multi-select
                    all matching skills are included, even inside collapsed groups
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
    fn source_picker_filter_geometry_stays_stable_and_resize_scrolls_selection() {
        let tmp = skills::ops::DownloadDir::new("source-filter-geometry").unwrap();
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
        let mut modal = Modal::picker(
            "Sources".into(),
            (0..30)
                .map(|i| PickItem {
                    id: format!("source-{i:02}"),
                    label: format!("source-{i:02}"),
                    sub: "repository".into(),
                })
                .collect(),
        );
        let mut previous = None;
        for (width, height) in [(100, 40), (60, 18), (100, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut geometry = None;
            for (query, count) in [("", 30), ("source-29", 1), ("no-such-source", 0), ("", 30)] {
                let Modal::Picker {
                    input, input_focus, ..
                } = &mut modal
                else {
                    unreachable!()
                };
                input.clear();
                *input_focus = true;
                modal.paste(query, &ctx);
                let Modal::Picker {
                    input,
                    items,
                    shown,
                    list,
                    ..
                } = &mut modal
                else {
                    unreachable!()
                };
                refilter(input.value(), items, shown);
                list.last(shown.len());
                assert_eq!(shown.len(), count);
                terminal
                    .draw(|f| modal.draw(f, Rect::new(0, 0, width, height - 2), &ctx))
                    .unwrap();
                let Modal::Picker {
                    rect,
                    input_rect,
                    list,
                    ..
                } = &modal
                else {
                    unreachable!()
                };
                let current = (*rect, *input_rect, list.rows);
                if let Some(expected) = geometry {
                    assert_eq!(current, expected);
                } else {
                    geometry = Some(current);
                }
                if let Some(selected) = list.selected() {
                    let offset = list.state.offset();
                    assert!(selected >= offset && selected < offset + list.rows.height as usize);
                    let y = list.rows.y + (selected - offset) as u16;
                    assert_eq!(terminal.backend().buffer()[(list.rows.x, y)].symbol(), "▸");
                }
            }
            if let Some(before) = previous {
                assert_ne!(geometry.unwrap().0, before);
            }
            previous = Some(geometry.unwrap().0);
        }
    }

    #[test]
    fn responsive_popups_keep_content_and_clear_geometry_on_shrink() {
        let tmp = skills::ops::DownloadDir::new("responsive-popups").unwrap();
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
        let mut menus = vec![
            Modal::install(),
            Modal::Picker {
                title: "Choose".into(),
                input: Input::default(),
                input_focus: true,
                input_rect: Rect::default(),
                items: vec![PickItem {
                    id: "one".into(),
                    label: "Selected option".into(),
                    sub: "detail".into(),
                }],
                shown: vec![0],
                list: ListNav::default(),
                rect: Rect::default(),
            },
            Modal::install_complete(vec![], "Installed.".into()),
            Modal::confirm_write(
                "Confirm".into(),
                vec!["Preview".into()],
                Box::new(|_| Ok("done".into())),
            ),
            Modal::confirm("Deploy".into(), vec![]),
            Modal::message("Message", vec!["Visible message".into()]),
            Modal::batch_tags(vec![], &ctx),
            Modal::batch_deploy(vec![], &ctx),
            Modal::batch_presets(vec![], &ctx),
            Modal::DeployTargets(Box::new(super::super::deploy_picker::DeployPicker::new(
                vec![],
                &ctx,
            ))),
            Modal::Sync(Box::new(
                super::super::sync_picker::SyncPicker::new(&ctx).unwrap(),
            )),
        ];
        for menu in &mut menus {
            for (width, height) in [
                (160, 40),
                (112, 32),
                (60, 20),
                (40, 12),
                (4, 3),
                (1, 1),
                (0, 0),
            ] {
                let mut terminal =
                    Terminal::new(TestBackend::new(width.max(1), height.max(1))).unwrap();
                let area = Rect::new(0, 0, width, height.saturating_sub(2));
                terminal.draw(|frame| menu.draw(frame, area, &ctx)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                if width == 40 {
                    assert!(text.chars().any(char::is_alphanumeric), "blank dialog");
                    assert!(!text.contains("Resize"));
                }
                match menu {
                    Modal::Input { rect, .. }
                    | Modal::Picker { rect, .. }
                    | Modal::Confirm { rect, .. }
                    | Modal::ConfirmWrite { rect, .. }
                    | Modal::InstallComplete { rect, .. } => {
                        assert_eq!(rect.intersection(area), *rect)
                    }
                    _ => {}
                }
                if let Modal::Picker {
                    input_rect, list, ..
                } = menu
                {
                    for hit in [*input_rect, list.rows] {
                        assert!(hit.is_empty() || hit.intersection(area) == hit);
                    }
                    if width < 4 {
                        assert_eq!(list.rows, Rect::default());
                    }
                }
                if let Modal::ConfirmWrite { btn_rects, .. } = menu {
                    assert!(
                        btn_rects
                            .iter()
                            .all(|hit| hit.is_empty() || hit.intersection(area) == *hit)
                    );
                }
            }
            let closed = (0..3).any(|_| {
                matches!(
                    menu.handle_key(KeyCode::Esc.into(), &ctx).first(),
                    Some(Action::CloseModal)
                )
            });
            assert!(closed, "Escape must reach the existing close/back path");
        }
    }

    #[test]
    fn confirmation_button_clicks_match_drawn_cells_and_keep_modal_keys() {
        let temp = skills::ops::DownloadDir::new("confirmation-buttons").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(temp.path())
        .unwrap();
        let ws = Workspace::open(temp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let make = || {
            Modal::confirm_write(
                "No title inference".into(),
                vec!["Preview retained".into()],
                Box::new(|_| Ok("saved".into())),
            )
            .destructive("删除")
        };
        for w in [8, 12, 24, 80] {
            let mut terminal = Terminal::new(TestBackend::new(w, 15)).unwrap();
            let mut modal = make();
            terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
            let Modal::ConfirmWrite {
                btn_rects, rect, ..
            } = &modal
            else {
                unreachable!()
            };
            let rects = btn_rects.clone();
            assert!(
                rects
                    .iter()
                    .all(|r| r.is_empty() || r.intersection(*rect) == *r)
            );
            for (i, r) in rects.iter().enumerate() {
                for x in r.x..r.right() {
                    let mut clicked = make();
                    terminal.draw(|f| clicked.draw(f, f.area(), &ctx)).unwrap();
                    let actions = clicked.handle_mouse(
                        MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            column: x,
                            row: r.y,
                            modifiers: KeyModifiers::NONE,
                        },
                        &ctx,
                    );
                    if i == 0 {
                        assert!(matches!(
                            actions.as_slice(),
                            [Action::CloseModal, Action::WriteMeta(_)]
                        ));
                    } else {
                        assert!(matches!(actions.as_slice(), [Action::CloseModal]));
                    }
                }
            }
            for x in rects[0].right()..rects[1].x {
                assert!(
                    modal
                        .handle_mouse(
                            MouseEvent {
                                kind: MouseEventKind::Down(MouseButton::Left),
                                column: x,
                                row: rects[0].y,
                                modifiers: KeyModifiers::NONE,
                            },
                            &ctx
                        )
                        .is_empty(),
                    "spacing must not activate a button"
                );
            }
        }
        for code in [KeyCode::Enter, KeyCode::Char('n'), KeyCode::Esc] {
            assert!(matches!(
                make()
                    .handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
                    .as_slice(),
                [Action::CloseModal]
            ));
        }
        assert!(matches!(
            make()
                .handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &ctx)
                .as_slice(),
            [Action::CloseModal, Action::WriteMeta(_)]
        ));
        let mut modal = make();
        for code in [
            KeyCode::Right,
            KeyCode::Left,
            KeyCode::Tab,
            KeyCode::BackTab,
        ] {
            assert!(
                modal
                    .handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
                    .is_empty()
            );
        }
        assert!(matches!(modal, Modal::ConfirmWrite { btn: 1, .. }));
        modal.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &ctx);
        assert!(matches!(
            modal
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
                .as_slice(),
            [Action::CloseModal, Action::WriteMeta(_)]
        ));
    }

    #[test]
    fn scoped_deployment_history_removal_defaults_to_cancel() {
        let deployment =
            |expected: &[&str], desired: &[&str]| history::WriteBack::TargetDeployment {
                agent: skills::config::AgentConfig {
                    key: "synthetic".into(),
                    name: "Synthetic".into(),
                    skills_dir: "unused".into(),
                },
                project: None,
                expected: expected.iter().map(|key| (*key).to_string()).collect(),
                desired: desired.iter().map(|key| (*key).to_string()).collect(),
            };
        let cases = [
            (
                "removing",
                deployment(&["kept", "removed"], &["kept"]),
                true,
            ),
            ("add-only", deployment(&["kept"], &["kept", "added"]), false),
            ("unchanged", deployment(&["kept"], &["kept"]), false),
            ("empty", deployment(&[], &[]), false),
            (
                "replacement with equal counts",
                deployment(&["repos/source/old"], &["repos/source/new"]),
                true,
            ),
            (
                "nested removing group",
                history::WriteBack::Group(vec![
                    deployment(&["kept"], &["kept", "added"]),
                    history::WriteBack::Group(vec![deployment(&["removed"], &[])]),
                ]),
                true,
            ),
            (
                "nested benign group",
                history::WriteBack::Group(vec![
                    deployment(&["kept"], &["kept"]),
                    history::WriteBack::Group(vec![deployment(&[], &["added"])]),
                ]),
                false,
            ),
            (
                "nested installed removal",
                history::WriteBack::Group(vec![history::WriteBack::Group(vec![
                    history::WriteBack::RemoveInstalled {
                        skill: "removed".into(),
                    },
                ])]),
                true,
            ),
        ];
        for dir in [Step::Undo, Step::Redo] {
            for (name, back, destructive) in &cases {
                let modal = Modal::undo_write("restore deployment state".into(), back.clone(), dir);
                let Modal::ConfirmWrite {
                    btn,
                    intent,
                    apply_label,
                    then,
                    ..
                } = modal
                else {
                    panic!("expected a write confirmation");
                };
                assert_eq!(btn, usize::from(*destructive), "{name}: {dir:?}");
                assert_eq!(
                    intent,
                    if *destructive {
                        ButtonIntent::Danger
                    } else {
                        ButtonIntent::Normal
                    },
                    "{name}: {dir:?}"
                );
                assert_eq!(
                    apply_label,
                    if *destructive {
                        "Remove"
                    } else if dir == Step::Undo {
                        "Undo"
                    } else {
                        "Redo"
                    },
                    "{name}: {dir:?}"
                );
                assert_eq!(then, Some(dir), "{name}: {dir:?}");
            }
        }
    }

    #[test]
    fn destructive_labels_and_defaults_are_explicit_and_builder_order_safe() {
        for (modal, label) in [
            (Modal::delete_tag("tag"), "Delete"),
            (Modal::delete_preset("preset"), "Delete"),
            (Modal::remove("skill"), "Remove"),
            (
                Modal::discard_invalid("skill", "missing SKILL.md"),
                "Delete",
            ),
            (
                merge_confirm(
                    "Tag",
                    "old",
                    "new",
                    Box::new(|_| Ok(("merged".into(), None))),
                ),
                "Merge",
            ),
            (
                Modal::confirm_meta(
                    "benign title".into(),
                    vec![],
                    Box::new(|_| Ok(("saved".into(), None))),
                )
                .destructive("Remove")
                .with_step(Step::Undo),
                "Remove",
            ),
            (
                Modal::undo_write(
                    "undo install".into(),
                    history::WriteBack::RemoveInstalled {
                        skill: "skill".into(),
                    },
                    Step::Undo,
                ),
                "Remove",
            ),
        ] {
            assert!(
                matches!(modal, Modal::ConfirmWrite { btn: 1, intent: ButtonIntent::Danger, ref apply_label, .. } if apply_label == label)
            );
        }
        assert!(
            matches!(Modal::confirm_write("delete in title only".into(), vec![], Box::new(|_| Ok("saved".into()))), Modal::ConfirmWrite { intent: ButtonIntent::Normal, ref apply_label, .. } if apply_label == "Apply")
        );
        let link = deploy::Action::Link {
            agent: "fake".into(),
            skill: "fake".into(),
            path: PathBuf::from("unused"),
            target: PathBuf::from("unused"),
        };
        assert!(matches!(
            Modal::confirm("delete in title only".into(), vec![link]),
            Modal::Confirm {
                btn: 0,
                intent: ButtonIntent::Normal,
                ..
            }
        ));
        let relink = deploy::Action::Relink {
            agent: "fake".into(),
            skill: "fake".into(),
            path: PathBuf::from("unused"),
            target: PathBuf::from("unused"),
        };
        assert!(matches!(
            Modal::confirm("benign title".into(), vec![relink]),
            Modal::Confirm {
                btn: 1,
                intent: ButtonIntent::Danger,
                apply_label: "Relink",
                ..
            }
        ));
    }

    #[test]
    fn tag_rename_submit_reports_invalid_name() {
        let temp = skills::ops::DownloadDir::new("tag-rename-modal").unwrap();
        Config::default().save(temp.path()).unwrap();
        let ws = Workspace::open(temp.path()).unwrap();
        ws.tags
            .save(&skills::tag::Tag {
                name: "valid".into(),
                skills: vec![],
                color: None,
                description: None,
            })
            .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut actions = submit(
            &InputKind::RenameTag {
                old: "valid".into(),
            },
            " (untagged) ".into(),
            &ctx,
        );
        let Action::WriteMeta(write) = actions.remove(0) else {
            panic!("expected metadata write")
        };
        assert!(actions.is_empty());
        assert_eq!(
            format!("{:#}", write(&ws).unwrap_err()),
            "invalid tag name \"(untagged)\": reserved for untagged skills"
        );
    }

    #[test]
    fn update_resolver_describes_only_whole_skill_choices() {
        assert_eq!(
            whole_skill_choice(Take::Local),
            "Keep local whole skill (skip this update)"
        );
        assert_eq!(
            whole_skill_choice(Take::Upstream),
            "Use upstream whole skill (replace local content)"
        );
        let modal = Modal::Resolve {
            prepared: None,
            files: vec![],
            list: ListNav::default(),
            default_take: Take::Local,
            btn: 0,
            btn_rects: vec![],
            rect: Rect::default(),
        };
        let hints = modal.hints();
        assert!(hints.iter().any(|(_, hint)| hint.contains("whole skill")));
        assert!(!hints.iter().any(|(_, hint)| hint.contains("toggle side")));
    }

    #[test]
    fn new_upstream_skills_wrap_at_eighty_columns() {
        let skills = (1..=8)
            .map(|n| format!("new-upstream-skill-with-readable-name-{n}"))
            .collect::<Vec<_>>();
        let text = new_upstream_skills_text(&skills).unwrap();
        for skill in &skills {
            assert!(text.contains(skill));
        }
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        assert!(paragraph.line_count(76) > 1);
    }

    #[test]
    fn startup_help_describes_read_only_explicit_repair() {
        assert!(HELP.contains("Startup scanning is read-only"));
        assert!(HELP.contains("analyze, review, and apply a repair"));
        assert!(!HELP.contains("repaired automatically"));
        assert!(!HELP.contains("are removed after"));
    }

    #[test]
    fn source_removal_warns_about_local_scope_and_defaults_to_cancel() {
        let tmp = skills::ops::DownloadDir::new("remove-source-modal").unwrap();
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
        let summary = edit::RepositoryRemoveSummary {
            alias: "demo".into(),
            keys: vec!["repos/demo/one".into(), "repos/demo/two".into()],
            modified: 1,
            missing: 1,
            attention: 0,
        };
        let mut modal = Modal::remove_source("demo", "Demo source", &summary);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| modal.draw(frame, frame.area(), &ctx))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Remove source and skills"));
        assert!(text.contains("Global and Local deployment links will be left unchanged"));
        assert!(text.contains("become broken"));
        assert!(text.contains("Agents or Health"));
        assert!(!text.contains("agent deployment link(s)"));
        assert!(text.contains("remote repository will not be changed"));
        assert!(text.contains("local modifications"));
        assert!(matches!(
            modal
                .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx)
                .as_slice(),
            [Action::CloseModal]
        ));
    }

    #[test]
    fn install_complete_defaults_to_done_and_requires_explicit_deploy() {
        let tmp = skills::ops::DownloadDir::new("install-complete").unwrap();
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
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);

        let mut modal = Modal::install_complete(vec!["sample".into()], "Installed sample.".into());
        assert!(matches!(
            modal.handle_key(enter, &ctx).as_slice(),
            [Action::CloseModal]
        ));

        let mut modal = Modal::install_complete(vec!["sample".into()], "Installed sample.".into());
        assert!(modal.handle_key(right, &ctx).is_empty());
        assert!(matches!(
            modal.handle_key(enter, &ctx).as_slice(),
            [Action::OpenModal(next)] if matches!(next.as_ref(), Modal::DeployTargets(_))
        ));

        let mut modal = Modal::install_complete(vec!["sample".into()], "Installed sample.".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Done"));
        assert!(text.contains("Deploy now"));
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
        let mut modal = Modal::repair_results(vec!["1 repaired".into()]);
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
            assert!(!text.contains("Enter/Esc close"));
            assert!(!text.contains("page · Esc close"));
        }
        let mut narrow = Modal::repair_results(vec![
            "1 repaired · 0 unchanged · 0 failed".into(),
            format!(
                "REPAIRED  shared/{} → Library/repos/example/{}",
                "long-skill-name-".repeat(4),
                "long-skill-name-".repeat(4)
            ),
        ]);
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
        assert!(!text.contains("Enter/Esc close"));
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
                kind: MessageKind::Normal,
                title: "Repair results".into(),
                lines: vec![],
                scroll: 0,
                return_to: Some(Box::new(Modal::help())),
                rect: Rect::default(),
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
    fn migration_recovery_path_is_reachable_at_multiple_widths_and_closes() {
        let tmp = skills::ops::DownloadDir::new("migration-recovery-modal").unwrap();
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
        let backup = PathBuf::from("/tmp/claude-1000")
            .join("deep component with spaces")
            .join("迁移备份")
            .join("another-extraordinarily-long-component");
        let report = skills::migration::MigrationReport {
            backup_dir: backup.clone(),
            migrated_names: vec!["daily".into()],
            migrated_tags: vec!["work".into()],
            config_migrated: true,
            migrated_repositories: vec![".root.toml".into()],
            from_layout: 0,
            to_layout: 1,
        };
        for width in [60, 80, 120] {
            let mut modal = Modal::migration_recovery(&report);
            let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
            let mut rendered = String::new();
            let mut title_rendered = false;
            for _ in 0..30 {
                terminal.draw(|f| modal.draw(f, f.area(), &ctx)).unwrap();
                title_rendered |= terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("Metadata migrated");
                let buffer = terminal.backend().buffer();
                let inside =
                    popup_size(Rect::new(0, 0, width, 16), PopupClass::Preview, 16, (1, 1)).inner;
                for row in inside.top()..inside.bottom() {
                    for column in inside.left()..inside.right() {
                        rendered.push_str(buffer[(column, row)].symbol());
                    }
                }
                modal.handle_key(KeyEvent::from(KeyCode::Down), &ctx);
            }
            assert!(title_rendered);
            let compact_rendered: String =
                rendered.chars().filter(|ch| !ch.is_whitespace()).collect();
            for segment in backup
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .filter(|part| !part.is_empty())
            {
                let compact_segment: String =
                    segment.chars().filter(|ch| !ch.is_whitespace()).collect();
                assert!(
                    compact_rendered.contains(&compact_segment),
                    "width {width} omitted {segment:?}"
                );
            }
        }
        for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('q')] {
            let mut modal = Modal::migration_recovery(&report);
            assert!(matches!(
                modal.handle_key(KeyEvent::from(code), &ctx).as_slice(),
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
    fn input_guidance_is_returned_to_the_footer_without_local_duplication() {
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
            assert!(!text.contains("discover · Esc"), "{width}: {text}");
            assert!(!text.contains("cancel"), "{width}: {text}");
            assert!(!text.contains("owner/repo[/path]"));
            assert!(modal.footer_help().detail.contains("owner/repo[/path]"));
            assert!(modal.footer_help().detail.contains("#branch"));
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
    fn preset_rename_uses_store_name_rule() {
        let tmp = skills::ops::DownloadDir::new("preset-rename-name-rule").unwrap();
        Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();
        ws.presets
            .save(&Preset {
                name: "daily".into(),
                ..Default::default()
            })
            .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };

        for (old, new) in [("daily", "team/daily"), ("team/daily", "archive")] {
            let mut actions = submit(
                &InputKind::RenamePreset { old: old.into() },
                new.into(),
                &ctx,
            );
            assert!(
                matches!(actions.as_slice(), [Action::WriteMeta(_), Action::SelectPreset(name)] if name == new)
            );
            let Action::WriteMeta(rename) = actions.remove(0) else {
                unreachable!()
            };
            rename(&ws).unwrap();
            assert!(ws.presets.load(old).unwrap().is_none());
            assert_eq!(ws.presets.load(new).unwrap().unwrap().name, new);
        }
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
        assert!(!view.input_focused());
        modal.handle_key(key(KeyCode::Char('/')), &ctx);
        for c in "network tools".chars() {
            assert!(modal.handle_key(key(KeyCode::Char(c)), &ctx).is_empty());
        }
        modal.handle_key(key(KeyCode::Enter), &ctx);
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
