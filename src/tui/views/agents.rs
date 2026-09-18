//! Agent inventory and preset deployment for the selected Global/Local scope.
//!
//! Coverage is calculated from current entries in that scope. Ordinary deployment
//! changes managed links; agent-owned directories require an explicit adopt,
//! relink or conflict-resolution action with its own validation.

use super::matrix::Matrix;
use super::preview::Overlay;
use super::{View, wheel};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::components::search_panel::{PanelLayout, PanelStyle, SearchEvent, SearchPanel};
use crate::tui::components::skill::{SkillPresentation, SkillRenderState};
use crate::tui::components::{
    group,
    layout::{cols_for, skill_frame},
};
use crate::tui::modal::Modal;
use crate::tui::settings::LayoutScope;
use crate::tui::widgets::{CardGrid, fit, width};
use anyhow::Context;
#[cfg(test)]
use crossterm::event::KeyModifiers;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget};
use skills::config::UiLayout;
use skills::ops::deploy::{self, PresetStatus, preset_status};
use skills::preset::{Preset, TagCoverage, tag_coverages};

use skills::reconcile::{AgentDirMode, EntryState};

mod context;
mod groups;
use crate::tui::components::context_menu::{Command, Item, Request, Target};

/// Keyboard focus follows the page bands. `[` and `]` switch agents
/// without requiring focus to return to the agent selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Agents,
    Scopes,
    Groups,
    Filters,
    Entries,
}

/// One inventory entry, including its filesystem state and managed-link status.
struct Row<'a> {
    /// Full Library key, or the directory name for an agent-owned entry.
    key: String,
    name: String,
    state: Option<&'a EntryState>,
    linked: bool,
    record: Option<&'a skills::reconcile::SkillRecord>,
}

/// Actions available for an entry's observed filesystem state. Cached for the
/// footer, which has no `Ctx`; each write must still validate its target again.
#[derive(Debug, Clone, Copy, Default)]
struct Caps {
    /// A link with nothing behind it; removing it loses nothing.
    clean: bool,
    linked: bool,
    /// The agent's own copy, byte for byte what the root has.
    relink: bool,
    adopt: bool,
}

impl Caps {
    fn of(state: Option<&EntryState>) -> Caps {
        Caps {
            adopt: matches!(state, Some(EntryState::AgentOnly)),
            linked: matches!(state, Some(EntryState::Deployed)),
            clean: matches!(state, Some(EntryState::Broken { .. })),
            relink: matches!(state, Some(EntryState::Shadow { same_content: true })),
        }
    }
}

type ScopeData = std::sync::Arc<(skills::Workspace, skills::reconcile::Snapshot)>;
type ScopeKey = Vec<(String, std::path::PathBuf)>;

#[derive(Default)]
pub struct AgentsView {
    /// Key of the agent on show. Empty only while none is configured.
    scope: String,
    destinations: Vec<skills::ops::targets::Scope>,
    launch_directory: Option<std::path::PathBuf>,
    destination: usize,
    destination_offset: usize,
    agent_directories: std::collections::BTreeMap<String, Vec<std::path::PathBuf>>,
    scope_counts: std::collections::BTreeMap<std::path::PathBuf, ScopeInventory>,
    scope_count_rx: Option<std::sync::mpsc::Receiver<(std::path::PathBuf, ScopeInventory)>>,
    destination_rects: Vec<(Rect, usize)>,
    agent_offset: usize,
    scoped: Option<ScopeData>,
    // Valid only for this library snapshot; refresh after writes or explicit rescan.
    scope_cache: std::collections::HashMap<ScopeKey, ScopeData>,
    scope_error: Option<String>,
    focus: FocusState,
    group_rects: [Rect; 3],
    presets: Vec<(Preset, PresetStatus)>,
    preset_offset: usize,
    filter_cursor: usize,
    quick: groups::QuickGroups,
    entries: CardGrid,
    /// One per entry row, in the grid's order.
    caps: Vec<Caps>,
    /// Actual density after adapting the shared preference to available height.
    entry_layout: UiLayout,
    scope_rects: Vec<(Rect, String)>,
    preset_rects: Vec<(usize, Rect)>,
    tags: Vec<TagCoverage>,
    tag_rects: Vec<(usize, Rect)>,
    search_panel: SearchPanel,
    content_filter_rect: Rect,
    content_searcher: std::cell::RefCell<skills::search::Searcher>,
    filter_editing: bool,
    /// The one column the entry scrollbar occupies, empty while it all fits.
    entries_track: Rect,
    left: Rect,
    /// An entry opened for reading, over the page rather than instead of it.
    preview: Overlay,
    /// The whole preset × agent picture, over the page.
    matrix: Matrix,
}

#[derive(Default)]
struct ScopeInventory {
    label: String,
    // Entry directory name -> frontmatter name and description. Never borrow
    // a central skill's metadata for a different agent-owned copy.
    descriptions: std::collections::BTreeMap<String, (String, String)>,
}
impl ScopeInventory {
    fn state(label: &str) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }
}

/// Count readable skills, including agent-owned folders and deployed links.
/// Runs off the UI thread; directory errors must not look like an empty folder.
fn count_scope_skills(path: &std::path::Path) -> ScopeInventory {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path).is_err() =>
        {
            return ScopeInventory::state("Not created");
        }
        Err(_) => return ScopeInventory::state("Unreadable"),
    };
    let mut inventory = ScopeInventory::default();
    for entry in entries {
        let Ok(entry) = entry else {
            return ScopeInventory::state("Unreadable");
        };
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if let Ok(doc) = skills::skill::SkillDoc::load(&entry.path()) {
            inventory
                .descriptions
                .insert(doc.key, (doc.name, doc.description));
        }
    }
    let count = inventory.descriptions.len();
    inventory.label = format!("{count} skills");
    inventory
}

/// Compact card labels; the full destination remains in the Target row.
fn scope_card_labels(scope: &skills::ops::targets::Scope) -> (String, String, String) {
    let tier = if scope.project.is_some() {
        "Local"
    } else {
        "Global"
    };
    let mut kinds = Vec::new();
    if let Some(directory) = &scope.directory {
        kinds.push(scope_directory_kind(directory));
    }
    for (source, _) in &scope.links {
        let kind = scope_directory_kind(source);
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let kind = kinds.join(" 󰌷 ");
    let path = scope
        .directory
        .as_ref()
        .and_then(|p| {
            scope
                .project
                .as_ref()
                .and_then(|root| p.strip_prefix(root).ok())
        })
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| scope.path_label());
    (tier.into(), kind, path)
}

fn scope_directory_kind(directory: &std::path::Path) -> String {
    let folder = directory
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("custom");
    match folder {
        ".agents" => "Shared".to_string(),
        ".claude" => "Claude".to_string(),
        ".codex" => "Codex".to_string(),
        _ => skills::agents::BUILTINS
            .iter()
            .find(|a| {
                [a.global_dir, a.local_dir].iter().any(|p| {
                    std::path::Path::new(p)
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| name == folder)
                })
            })
            .map(|a| a.name.to_string())
            .unwrap_or_else(|| folder.trim_start_matches('.').to_string()),
    }
}

/// `Focus` needs a default for `#[derive(Default)]` on the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FocusState(Focus);

impl Default for FocusState {
    fn default() -> Self {
        FocusState(Focus::Agents)
    }
}

impl AgentsView {
    fn update_scope_counts(&mut self) {
        if let Some(rx) = &self.scope_count_rx {
            loop {
                match rx.try_recv() {
                    Ok((path, count)) => {
                        self.scope_counts.insert(path, count);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.scope_count_rx = None;
                        break;
                    }
                }
            }
        }
        if self.scope_count_rx.is_some() {
            return;
        }
        let paths: Vec<_> = self
            .destinations
            .iter()
            .filter_map(|s| s.directory.as_ref())
            .chain(self.agent_directories.values().flatten())
            .filter(|p| !self.scope_counts.contains_key(*p))
            .cloned()
            .collect();
        if paths.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.scope_count_rx = Some(rx);
        std::thread::spawn(move || {
            for path in paths {
                let count = count_scope_skills(&path);
                if tx.send((path, count)).is_err() {
                    break;
                }
            }
        });
    }

    fn scope_count(&self, scope: &skills::ops::targets::Scope) -> &str {
        scope
            .directory
            .as_ref()
            .and_then(|p| self.scope_counts.get(p))
            .map(|inventory| inventory.label.as_str())
            .unwrap_or("Counting…")
    }

    fn agent_skill_count(&self, key: &str) -> Option<String> {
        let directories = self.agent_directories.get(key)?;
        let mut total = 0;
        for directory in directories {
            let Some(inventory) = self.scope_counts.get(directory) else {
                return Some("Counting…".into());
            };
            if inventory.label == "Unreadable" {
                return Some("Unreadable".into());
            }
            total += inventory.descriptions.len();
        }
        Some(format!("{total} skills total"))
    }

    pub fn discover(&mut self, start: &std::path::Path) -> anyhow::Result<()> {
        self.destinations = skills::ops::targets::discover_scopes(start)?;
        self.launch_directory = self.destinations.get(1).and_then(|s| s.project.clone());
        self.destination = 1;
        Ok(())
    }

    pub fn group_popup_open(&self) -> bool {
        self.quick.popup.is_some()
    }

    pub fn editing(&self) -> bool {
        self.filter_editing
    }

    pub fn paste(&mut self, text: &str, ctx: &Ctx) -> Vec<Action> {
        if !self.filter_editing || self.preview.is_open() || self.matrix.hints().is_some() {
            return vec![];
        }
        match self.search_panel.paste(text) {
            Err(error) => return vec![Action::Error(error)],
            Ok(false) => return vec![],
            Ok(true) => self.entries.first(0),
        }
        let data = self.scoped.clone();
        let scoped = data.as_ref().map(|d| Ctx {
            ws: &d.0,
            snap: &d.1,
            settings: ctx.settings,
        });
        self.update_search_completion(scoped.as_ref().unwrap_or(ctx));
        vec![]
    }

    fn project(&self) -> Option<std::path::PathBuf> {
        self.destinations
            .get(self.destination)
            .and_then(|s| s.project.clone())
    }

    fn move_destination(&mut self, delta: i32, ctx: &Ctx) {
        if self.destinations.is_empty() {
            return;
        }
        let next =
            (self.destination as i32 + delta).clamp(0, self.destinations.len() as i32 - 1) as usize;
        if next == self.destination {
            return;
        }
        self.destination = next;
        self.entries = CardGrid::default();
        self.preset_offset = 0;
        self.filter_editing = false;
        self.search_panel.input.clear();
        self.preview.close();
        self.matrix.close();
        self.quick.reset_for_scope();
        self.refresh_scope(ctx);
    }

    fn scoped_actions(&self, actions: Vec<Action>, ctx: &Ctx) -> Vec<Action> {
        let Some(data) = &self.scoped else {
            return actions;
        };
        let Some(agent) = data.0.config.agent(&self.scope).cloned() else {
            return actions;
        };
        let project = self.project();
        actions
            .into_iter()
            .map(|action| match action {
                Action::SelectAgentSkills { keys, .. } => Action::OpenModal(Box::new(
                    Modal::PresetSkills(Box::new(super::search::SearchView::target_skills(
                        agent.clone(),
                        project.clone(),
                        false,
                        Some(keys),
                        ctx,
                    ))),
                )),

                Action::ApplyLinks { title: _, actions } => {
                    let agent = agent.clone();
                    let project = project.clone();
                    Action::deployment(Action::WriteMeta(Box::new(move |ws| {
                        skills::ops::targets::apply_actions(
                            ws,
                            &agent,
                            project.as_deref(),
                            &actions,
                        )
                    })))
                }
                Action::ConfirmLinks { title, actions } => {
                    let agent = agent.clone();
                    let project = project.clone();
                    Action::OpenModal(Box::new(
                        Modal::confirm_meta(
                            title,
                            actions.iter().map(|a| a.describe()).collect(),
                            Box::new(move |ws| {
                                skills::ops::targets::apply_actions(
                                    ws,
                                    &agent,
                                    project.as_deref(),
                                    &actions,
                                )
                            }),
                        )
                        .deployment_only(),
                    ))
                }
                other => other,
            })
            .collect()
    }

    /// An open matrix owns keyboard input before application shortcuts.
    pub fn handle_matrix_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Option<Vec<Action>> {
        self.matrix.handle_key(k, ctx)
    }

    /// The scope as the planners want it: one key, or nothing at all.
    fn scope_agents(&self) -> Vec<String> {
        if self.scope.is_empty() {
            Vec::new()
        } else {
            vec![self.scope.clone()]
        }
    }

    fn focus(&self) -> Focus {
        self.focus.0
    }

    fn set_focus(&mut self, f: Focus) {
        self.focus = FocusState(f);
        if f != Focus::Entries {
            self.filter_editing = false;
            self.search_panel.completion.close();
        }
    }

    /// Entry rows for the current scope, grouped into ours and theirs.
    /// Ours first, then the agent's own; alphabetical inside each group.
    fn rows<'a>(&self, ctx: &'a Ctx) -> Vec<Row<'a>> {
        self.scope_rows(ctx, true)
    }

    fn scope_rows<'a>(&self, ctx: &'a Ctx, filtered: bool) -> Vec<Row<'a>> {
        let Some(report) = ctx.snap.agent(&self.scope) else {
            return Vec::new();
        };
        let deployed = |record: &skills::reconcile::SkillRecord| {
            matches!(
                record.deploy.get(&self.scope),
                Some(skills::reconcile::DeployState::Deployed)
            )
        };
        let mut rows = report
            .entries
            .iter()
            .map(|(name, state)| {
                let linked = matches!(state, EntryState::Deployed);
                let record = ctx.snap.skills.iter().find(|r| {
                    r.deployment_name() == Some(name.as_str()) && (!linked || deployed(r))
                });
                Row {
                    key: if linked {
                        record.map_or_else(|| name.clone(), |r| r.key.clone())
                    } else {
                        name.clone()
                    },
                    name: name.clone(),
                    state: Some(state),
                    linked,
                    record,
                }
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| (!a.linked, &a.name, &a.key).cmp(&(!b.linked, &b.name, &b.key)));
        if filtered && !self.search_panel.input.value().trim().is_empty() {
            let records = self.search_records(&rows, ctx);
            let hits = self.content_searcher.borrow_mut().search(
                &records,
                &skills::search::Query::parse(self.search_panel.input.value()),
            );
            let order: std::collections::BTreeMap<_, _> = hits
                .iter()
                .enumerate()
                .map(|(i, h)| (records[h.index].key.clone(), i))
                .collect();
            let query = self.search_panel.input.value().to_lowercase();
            rows.retain(|row| {
                order.contains_key(&row.key)
                    || (row.record.is_none()
                        && !report.documents.contains_key(&row.name)
                        && row.key.to_lowercase().contains(&query))
            });
            rows.sort_by_key(|row| {
                (
                    order.get(&row.key).copied().unwrap_or(usize::MAX),
                    usize::from(!row.linked),
                )
            });
        }
        rows
    }

    fn select_skills(&self, ctx: &Ctx, checked: Option<String>) -> Vec<Action> {
        let keys: Vec<_> = self
            .rows(ctx)
            .into_iter()
            .filter(|row| row.linked)
            .filter_map(|row| row.record)
            .map(|skill| skill.key.clone())
            .collect();
        if keys.is_empty() {
            return vec![];
        }
        vec![Action::SelectAgentSkills {
            keys,
            title: format!("Agent: {}", self.scope),
            agent: self.scope.clone(),
            checked,
        }]
    }

    fn preview_entry(&mut self, ctx: &Ctx) {
        let rows = self.rows(ctx);
        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
            return;
        };
        let Some(agent) = ctx.snap.agent(&self.scope) else {
            return;
        };
        self.preview.open_agent(
            row.name.to_string(),
            agent.name.clone(),
            if row.state.is_none() {
                row.record
                    .map(|r| r.path.clone())
                    .unwrap_or_else(|| ctx.ws.root.join(&row.key))
            } else {
                agent.skills_dir.join(&row.name)
            },
            if row.linked {
                "linked from Library".into()
            } else {
                row.state
                    .map(entry_note)
                    .unwrap_or_else(|| "not deployed · preview from Library".into())
            },
        );
    }

    fn selected_caps(&self) -> Caps {
        self.entries
            .selected()
            .and_then(|i| self.caps.get(i))
            .copied()
            .unwrap_or_default()
    }

    /// Plan one repair on the selected entry and put it up for confirmation.
    /// Both repairs delete something — a link with nothing behind it, or a
    /// copy the root already has — so unlike a pill they stop to show the
    /// plan first. The planner is handed the one name, so the plan is exactly
    /// the row in hand and nothing beside it.
    fn repair(&self, ctx: &Ctx, rows: &[Row], clean: bool) -> Vec<Action> {
        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
            return vec![Action::Error("nothing selected".into())];
        };
        let caps = Caps::of(row.state);
        if clean && !caps.clean {
            return vec![Action::Error(format!(
                "{}: clean applies to links whose target is gone",
                row.name
            ))];
        }
        if !clean && !caps.relink {
            return vec![Action::Error(format!(
                "{}: relink applies to the agent's own copies that match the root",
                row.name
            ))];
        }
        let skill = vec![row.name.to_string()];
        let plan = if clean {
            deploy::plan_clean(ctx.ws, ctx.snap, &self.scope, &skill)
        } else {
            deploy::plan_relink(ctx.ws, ctx.snap, &self.scope, &skill)
        };
        match plan {
            Ok(actions) => vec![Action::ConfirmLinks {
                title: format!(
                    "{} {}/{}",
                    if clean { "clean" } else { "relink" },
                    self.scope,
                    row.name
                ),
                actions,
            }],
            Err(e) => vec![Action::Error(format!("{e:#}"))],
        }
    }

    fn move_scope(&mut self, delta: i32, ctx: &Ctx) {
        let keys = ctx.ws.config.agent_keys();
        if keys.is_empty() {
            return;
        }
        let cur = keys.iter().position(|x| *x == self.scope).unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(keys.len() as i32);
        self.scope = keys[next as usize].clone();
        self.entries = CardGrid::default();
    }
}

impl AgentsView {
    /// Re-enter on the agent selector with the complete scope inventory visible.
    fn enter_current(&mut self) {
        self.filter_editing = false;
        self.set_focus(Focus::Agents);
        self.preview.close();
        self.matrix.close();
    }

    fn refresh_current(&mut self, ctx: &Ctx) {
        // A scope pinned to an agent that is gone from the config, or never set,
        // falls back to the first one there is.
        if ctx.ws.config.agent(&self.scope).is_none() {
            self.scope = ctx
                .ws
                .config
                .agent_keys()
                .first()
                .cloned()
                .unwrap_or_default();
        }
        let scope = self.scope_agents();
        self.presets = ctx
            .snap
            .presets
            .by_name
            .values()
            .cloned()
            .map(|p| {
                let st = preset_status(ctx.snap, &p, &scope);
                (p, st)
            })
            .collect();
        self.tags = if ctx.settings.tags_enabled {
            let deployed = ctx
                .snap
                .skills
                .iter()
                .filter(|skill| {
                    skill.deploy.get(&self.scope) == Some(&skills::reconcile::DeployState::Deployed)
                })
                .map(|skill| skill.key.clone())
                .collect();
            tag_coverages(&ctx.ws.config, &deployed)
        } else {
            vec![]
        };
        let rows = self.rows(ctx);
        self.caps = rows.iter().map(|r| Caps::of(r.state)).collect();
        self.entries.clamp(rows.len());
        self.search_panel.completion.close();
        if self.filter_editing {
            self.update_search_completion(ctx);
        }
    }

    fn search_records(&self, rows: &[Row<'_>], ctx: &Ctx) -> Vec<skills::reconcile::SkillRecord> {
        let Some(report) = ctx.snap.agent(&self.scope) else {
            return vec![];
        };
        rows.iter()
            .filter_map(|row| {
                if row.linked || row.state.is_none() {
                    return row.record.cloned();
                }
                let doc = report.documents.get(&row.name)?;
                Some(skills::reconcile::SkillRecord {
                    key: row.key.clone(),
                    path: doc.path.clone(),
                    status: skills::reconcile::SkillStatus::Local,
                    name: Some(doc.name.clone()),
                    description: Some(doc.description.clone()),
                    body: Some(doc.body.clone()),
                    external: doc.external,
                    tags: vec![],
                    presets: vec![],
                    note: None,
                    source: None,
                    source_name: None,
                    current_hash: None,
                    baseline_hash: None,
                    deploy: std::collections::BTreeMap::from([(
                        self.scope.clone(),
                        skills::reconcile::DeployState::Deployed,
                    )]),
                    meta: None,
                })
            })
            .collect::<Vec<_>>()
    }

    fn update_search_completion(&mut self, ctx: &Ctx) {
        let rows = self.scope_rows(ctx, false);
        let records = self.search_records(&rows, ctx);
        let keys = records.iter().map(|r| r.key.clone()).collect();
        let refs = records.iter().collect::<Vec<_>>();
        self.search_panel.update_completion(Some(
            |input: &crate::tui::widgets::Input,
             completion: &mut crate::tui::components::completion::Completion| {
                completion.update_records(input, ctx, &refs, Some(&keys))
            },
        ));
    }

    fn handle_key_current(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        let k = if !self.filter_editing && k.code == KeyCode::Char('q') && k.modifiers.is_empty() {
            KeyEvent::new(KeyCode::Esc, k.modifiers)
        } else {
            k
        };
        if self.preview.handle_key(k) {
            return vec![];
        }
        if let Some(acts) = self.matrix.handle_key(k, ctx) {
            return acts;
        }
        if let Some(actions) = self.filter_key(k) {
            return actions;
        }
        if let Some(actions) = self.quick_key(k, ctx) {
            return actions;
        }
        {
            if self.filter_editing {
                let event = self.search_panel.key(k);
                match event {
                    SearchEvent::Up => {
                        self.filter_editing = false;
                        self.set_focus(if self.quick_visible() {
                            Focus::Groups
                        } else {
                            Focus::Scopes
                        });
                    }
                    SearchEvent::Results => self.focus_skill_filters(),
                    SearchEvent::Escape => {
                        self.filter_editing = false;
                        self.set_focus(Focus::Entries);
                    }
                    SearchEvent::Changed | SearchEvent::CursorMoved | SearchEvent::Accepted => {
                        self.update_search_completion(ctx);
                        if event != SearchEvent::CursorMoved {
                            self.entries.first(0);
                        }
                    }
                    _ => {}
                }
                return vec![];
            }
            if k.code == KeyCode::Char('/') && self.focus() == Focus::Entries {
                self.filter_editing = true;
                return vec![];
            }
        }
        if !self.destinations.is_empty() {
            if self.focus() == Focus::Entries
                && (k.code == KeyCode::Char('i')
                    || (k.code == KeyCode::Char('m') && self.selected_caps().linked))
            {
                let Some(agent) = ctx.ws.config.agent(&self.scope).cloned() else {
                    return vec![];
                };
                let on = k.code == KeyCode::Char('i');
                let keys = (!on).then(|| {
                    self.rows(ctx)
                        .into_iter()
                        .filter(|r| r.linked)
                        .filter_map(|r| r.record.map(|s| s.key.clone()))
                        .collect()
                });
                return vec![Action::OpenModal(Box::new(Modal::PresetSkills(Box::new(
                    super::search::SearchView::target_skills(agent, self.project(), on, keys, ctx),
                ))))];
            }
            if k.code == KeyCode::Char('x')
                && self.focus() == Focus::Entries
                && self.selected_caps().linked
            {
                return self.entry_command(Command::Remove, ctx);
            }
        }
        match k.code {
            KeyCode::Char('M') if matches!(self.focus(), Focus::Agents | Focus::Scopes) => {
                self.matrix.open(ctx);
                return vec![];
            }
            KeyCode::Esc => {
                match self.focus() {
                    Focus::Entries if !self.search_panel.input.is_empty() => {
                        self.search_panel.input.clear();
                        self.entries.clamp(self.rows(ctx).len());
                    }
                    Focus::Entries => self.set_focus(if self.destinations.is_empty() {
                        Focus::Agents
                    } else if self.quick_visible() {
                        Focus::Groups
                    } else {
                        Focus::Scopes
                    }),
                    Focus::Filters => self.focus_skill_search(),
                    Focus::Groups => self.set_focus(Focus::Scopes),
                    Focus::Scopes => self.set_focus(Focus::Agents),
                    Focus::Agents => return vec![Action::BackToParent],
                }
                return vec![];
            }
            // Scope is switchable from anywhere: it frames everything else.
            KeyCode::Char('[') => {
                self.move_scope(-1, ctx);
                return vec![];
            }
            KeyCode::Char(']') => {
                self.move_scope(1, ctx);
                return vec![];
            }
            KeyCode::Char('v' | 'V') if self.focus() == Focus::Entries => {
                return vec![Action::SetLayout {
                    scope: LayoutScope::Agents,
                    layout: ctx.settings.layout_for(LayoutScope::Agents).next(),
                }];
            }
            _ => {}
        }
        match self.focus() {
            Focus::Agents => match k.code {
                KeyCode::Up => vec![Action::BackToParent],
                KeyCode::Left | KeyCode::Char('h') => {
                    self.move_scope(-1, ctx);
                    vec![]
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.move_scope(1, ctx);
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Enter => {
                    self.set_focus(if self.destinations.is_empty() {
                        Focus::Entries
                    } else {
                        Focus::Scopes
                    });
                    vec![]
                }
                _ => vec![],
            },
            Focus::Scopes | Focus::Groups | Focus::Filters => vec![],
            Focus::Entries => {
                let rows = self.rows(ctx);
                let n = rows.len();
                match k.code {
                    KeyCode::Char('m') if self.selected_caps().linked => {
                        return self.select_skills(ctx, None);
                    }
                    // Down and up cross a whole row of cards; left and right
                    // walk along one, and only mean anything once there is more
                    // than one column to walk.
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.entries.move_rows(1, n);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.entries.selected().unwrap_or(0) < self.entries.cols() {
                            if self.has_visible_groups() {
                                self.focus_skill_filters();
                            } else {
                                self.focus_skill_search();
                            }
                        } else {
                            self.entries.move_rows(-1, n);
                        }
                    }
                    KeyCode::Right => self.entries.move_by(1, n),
                    KeyCode::Char('l') => self.entries.move_by(1, n),
                    KeyCode::Char('r') if self.selected_caps().relink => {
                        return self.entry_command(Command::Relink, ctx);
                    }
                    KeyCode::Left | KeyCode::Char('h') => self.entries.move_by(-1, n),
                    KeyCode::Char('x') if self.selected_caps().clean => {
                        return self.entry_command(Command::Remove, ctx);
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.entries.first(n);
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.entries.last(n);
                    }
                    KeyCode::Enter => return self.entry_command(Command::Open, ctx),
                    // Only an entry the root knows nothing about can be taken
                    // in; everything else here is either already ours or the
                    // agent's to keep.
                    KeyCode::Char('a') if self.selected_caps().adopt => {
                        return self.entry_command(Command::Adopt, ctx);
                    }
                    _ => {}
                }
                vec![]
            }
        }
    }

    fn handle_mouse_current(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.preview.handle_mouse(m, ctx) {
            return vec![];
        }
        if let Some(acts) = self.matrix.handle_mouse(m, ctx) {
            return acts;
        }
        if let Some(actions) = self.group_mouse(m, ctx) {
            return actions;
        }
        if self.filter_editing {
            let (consumed, accepted) = self.search_panel.mouse_completion(m);
            if accepted {
                self.entries.first(0);
                self.update_search_completion(ctx);
            }
            if consumed {
                return vec![];
            }
        }
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && self.search_panel.click_input(m.column, m.row)
        {
            self.set_focus(Focus::Entries);
            self.filter_editing = true;
            self.update_search_completion(ctx);
            return vec![];
        }
        let at = (m.column, m.row).into();
        if self.group_rects[2].contains(at) {
            return vec![];
        }
        if let Some(d) = wheel(&m, ctx) {
            if self.left.contains(at) {
                self.set_focus(Focus::Entries);
                self.filter_editing = false;
                // A wheel notch is a row of cards, however many are on it.
                self.entries.move_rows(d.signum(), self.rows(ctx).len());
            }
            return vec![];
        }
        // Dragging the thumb reads the same as clicking the track: both put the
        // selection where the pointer is.
        if matches!(
            m.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
        ) && self.entries_track.contains(at)
        {
            let n = self.entries.grid_rows();
            if n > 0 {
                let span = self.entries_track.height.max(1) as usize;
                let r = (m.row.saturating_sub(self.entries_track.y)) as usize * n / span;
                self.set_focus(Focus::Entries);
                self.filter_editing = false;
                self.entries.select_row(r.min(n - 1));
            }
            return vec![];
        }
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if let Some((_, scope)) = self
                .scope_rects
                .iter()
                .find(|(r, _)| r.contains(at))
                .map(|(r, s)| (*r, s.clone()))
            {
                self.scope = scope;
                self.set_focus(Focus::Agents);
                return vec![];
            }
            if self.left.contains(at) {
                self.set_focus(Focus::Entries);
                self.filter_editing = false;
                if let Some((index, double)) = self.entries.click(m.column, m.row) {
                    if self.entries.cell(index).is_some_and(|cell| {
                        let marker_y = cell.y + u16::from(self.entry_layout == UiLayout::Grid);
                        m.row == marker_y
                            && (cell.x + 2..cell.x + 2 + ctx.settings.layout.marker_width as u16)
                                .contains(&m.column)
                    }) {
                        let rows = self.rows(ctx);
                        if let Some(row) = rows.get(index).filter(|row| row.linked)
                            && let Some(skill) = row.record
                        {
                            return self.select_skills(ctx, Some(skill.key.clone()));
                        }
                    }
                    if double {
                        self.preview_entry(ctx);
                    }
                }
            }
        }
        vec![]
    }

    fn draw_current(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        self.update_scope_counts();
        let th = &ctx.settings.theme;
        let scoped = !self.destinations.is_empty();
        let compact = area.height < 30;
        let groups = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if compact { 3 } else { 5 }),
                Constraint::Length(if scoped && compact {
                    4
                } else if scoped {
                    6 + self
                        .destinations
                        .get(self.destination)
                        .map_or(0, |s| s.links.len() as u16)
                } else {
                    0
                }),
                Constraint::Length(
                    self.quick_height(ctx, area.width)
                        .min(area.height.saturating_sub(18)),
                ),
                Constraint::Min(5),
            ])
            .split(area);
        self.group_rects = [groups[0], groups[1], Rect::default()];
        let mut interiors = [Rect::default(); 2];
        for (i, (title, focused)) in [
            (" Agents ", self.focus() == Focus::Agents),
            (" Scope ", self.focus() == Focus::Scopes),
        ]
        .into_iter()
        .enumerate()
        {
            if groups[i].height == 0 {
                continue;
            }
            let block = th.block(title, focused);
            interiors[i] = block.inner(groups[i]);
            f.render_widget(block, groups[i]);
        }
        self.draw_quick(f, groups[2], ctx);
        let rows = [interiors[0], interiors[1], groups[3]];

        self.scope_rects.clear();
        let mut x = rows[0].x + 1;
        if ctx.ws.config.agents.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(" no agent configured", th.dim())),
                rows[0],
            );
        }
        let selected_agent = ctx
            .ws
            .config
            .agents
            .iter()
            .position(|a| a.key == self.scope)
            .unwrap_or(0);
        let widths: Vec<_> = ctx
            .ws
            .config
            .agents
            .iter()
            .map(|a| {
                let report = ctx.snap.agent(&a.key);
                let sub = match report.map(|r| {
                    (
                        &r.mode,
                        r.documents
                            .keys()
                            .filter(|key| matches!(r.entries.get(*key), Some(EntryState::Deployed)))
                            .count(),
                        r.documents.len(),
                    )
                }) {
                    None | Some((AgentDirMode::Missing, ..)) => "no directory".to_string(),
                    Some((AgentDirMode::ReadOnly { .. }, ..)) => "read-only directory".into(),
                    Some((AgentDirMode::Real, linked, total)) if total > linked => {
                        format!("{linked} linked · {} own", total - linked)
                    }
                    Some((AgentDirMode::Real, linked, _)) => format!("{linked} linked"),
                };
                let sub = self.agent_skill_count(&a.key).unwrap_or(sub);
                (if compact {
                    width(a.display_name()) + 4
                } else {
                    (width(a.display_name()).max(width(&sub)) + 4).max(14)
                }) + 1
            })
            .collect();
        let visible = pill_window(
            &widths,
            selected_agent,
            &mut self.agent_offset,
            rows[0].width.saturating_sub(2) as usize,
        );
        for a in ctx
            .ws
            .config
            .agents
            .iter()
            .skip(visible.start)
            .take(visible.len())
        {
            let report = ctx.snap.agent(&a.key);
            let sub = match report.map(|r| {
                (
                    &r.mode,
                    r.documents
                        .keys()
                        .filter(|key| matches!(r.entries.get(*key), Some(EntryState::Deployed)))
                        .count(),
                    r.documents.len(),
                )
            }) {
                None | Some((AgentDirMode::Missing, ..)) => "no directory".to_string(),
                Some((AgentDirMode::ReadOnly { .. }, ..)) => "read-only directory".into(),
                Some((AgentDirMode::Real, linked, total)) if total > linked => {
                    format!("{linked} linked · {} own", total - linked)
                }
                Some((AgentDirMode::Real, linked, _)) => format!("{linked} linked"),
            };
            let sub = self.agent_skill_count(&a.key).unwrap_or(sub);
            let name = a.display_name().to_string();
            let w = (if compact {
                width(&name) + 4
            } else {
                (width(&name).max(width(&sub)) + 4).max(14)
            }) as u16;
            if x + w > rows[0].right() {
                break;
            }
            let rect = Rect::new(x, rows[0].y, w, 3.min(rows[0].height));
            let on = a.key == self.scope;
            if compact {
                f.render_widget(
                    Paragraph::new(format!("{} {name}", if on { "▸" } else { " " })).style(if on {
                        th.bold()
                    } else {
                        th.dim()
                    }),
                    rect,
                );
                self.scope_rects.push((rect, a.key.clone()));
                x += w + 1;
                continue;
            }
            let border = if on {
                th.accent()
            } else {
                Style::default().fg(th.placeholder)
            };
            let block = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(border)
                .title(Line::from(Span::styled(
                    format!(" {name} "),
                    if on {
                        th.bold().fg(th.accent)
                    } else {
                        th.dim()
                    },
                )));
            let inner = block.inner(rect).inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 0,
            });
            f.render_widget(block, rect);
            f.render_widget(Paragraph::new(sub).style(th.skill_count()), inner);
            self.scope_rects.push((rect, a.key.clone()));
            x += w + 1;
        }

        self.destination_rects.clear();
        if !self.destinations.is_empty() {
            let band = rows[1];
            // Reserve equal slots independently of agent names, paths and counts.
            // Narrow terminals retain the existing horizontal scope scrolling.
            let slot_width = 33;
            let widths = vec![slot_width; self.destinations.len()];
            let visible = pill_window(
                &widths,
                self.destination,
                &mut self.destination_offset,
                band.width.saturating_sub(2) as usize,
            );
            let mut x = band.x + 1;
            for (i, card_width) in widths
                .iter()
                .enumerate()
                .take((visible.end + 1).min(self.destinations.len()))
                .skip(visible.start)
            {
                let scope = &self.destinations[i];
                let w = *card_width as u16 - 1;
                let clipped_width = w.min(band.right().saturating_sub(1).saturating_sub(x));
                if clipped_width == 0 {
                    break;
                }
                let rect = Rect::new(x, band.y, w, 3.min(band.height));
                let clipped = Rect::new(x, band.y, clipped_width, rect.height);
                let mut buffer = ratatui::buffer::Buffer::empty(rect);
                let on = i == self.destination;
                let (tier, kind, path) = scope_card_labels(scope);
                let kind = if ctx.settings.ui.icons == skills::config::Icons::Text {
                    kind.replace("󰌷", "↔")
                } else {
                    kind
                };
                let icon =
                    crate::tui::icons::scope(ctx.settings.ui.icons, scope.project.is_none(), false);
                let subdued = !on && matches!(self.scope_count(scope), "0 skills" | "Not created");
                let muted = Style::default().fg(ratatui::style::Color::Rgb(96, 104, 116));
                let title_style = if on {
                    th.bold()
                } else if subdued {
                    muted
                } else {
                    Style::default().fg(th.placeholder)
                };
                if compact {
                    f.render_widget(
                        Paragraph::new(format!(
                            "{} {icon}{tier} · {kind}",
                            if on { "▸" } else { " " }
                        ))
                        .style(title_style),
                        Rect::new(rect.x, rect.y, clipped.width, 1),
                    );
                    self.destination_rects
                        .push((Rect::new(rect.x, rect.y, clipped.width, 1), i));
                    x += w + 1;
                    continue;
                }
                let prefix = format!(" {icon}{tier}");
                let kind = fit(&kind, (w as usize).saturating_sub(width(&prefix) + 6));
                let title = Line::styled(format!("{prefix} · {kind} "), title_style);
                let block = ratatui::widgets::Block::bordered()
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(if on {
                        th.accent()
                    } else if subdued {
                        muted
                    } else {
                        Style::default().fg(th.placeholder)
                    })
                    .title(title);
                let inner = block.inner(rect).inner(ratatui::layout::Margin {
                    horizontal: 1,
                    vertical: 0,
                });
                block.render(rect, &mut buffer);
                let count = fit(self.scope_count(scope), inner.width as usize);
                let count_width = width(&count) as u16;
                let path_width = inner.width.saturating_sub(count_width + 2);
                Paragraph::new(crate::tui::app::middle_ellipsis(&path, path_width as usize))
                    .style(if subdued { muted } else { th.dim() })
                    .render(
                        Rect::new(inner.x, inner.y, path_width, inner.height),
                        &mut buffer,
                    );
                let count_style = match self.scope_count(scope) {
                    "Unreadable" => th.warn(),
                    "Counting…" => th.dim(),
                    _ => th.skill_count(),
                };
                Paragraph::new(count)
                    .style(if subdued { muted } else { count_style })
                    .render(
                        Rect::new(
                            inner.right().saturating_sub(count_width),
                            inner.y,
                            count_width,
                            inner.height,
                        ),
                        &mut buffer,
                    );
                for y in clipped.y..clipped.bottom() {
                    for x in clipped.x..clipped.right() {
                        f.buffer_mut()[(x, y)] = buffer[(x, y)].clone();
                    }
                }
                self.destination_rects.push((clipped, i));
                x += w + 1;
            }
            if band.height > 1 && band.width > 1 {
                if visible.start > 0 {
                    f.render_widget(
                        Paragraph::new("‹").style(th.accent()),
                        Rect::new(band.x, band.y + u16::from(!compact), 1, 1),
                    );
                }
                if visible.end < self.destinations.len() {
                    f.render_widget(
                        Paragraph::new("›").style(th.accent()),
                        Rect::new(band.right() - 1, band.y + u16::from(!compact), 1, 1),
                    );
                }
            }
            let target = ctx
                .ws
                .config
                .agent(&self.scope)
                .map(|a| skills::paths::contract_tilde(&a.skills_path()))
                .unwrap_or_default();
            if band.height > if compact { 1 } else { 3 } {
                f.render_widget(
                    Paragraph::new(crate::tui::app::middle_ellipsis(
                        &format!(" Target  {target}"),
                        band.width as usize,
                    ))
                    .style(th.dim()),
                    Rect::new(band.x, band.y + if compact { 1 } else { 3 }, band.width, 1),
                );
            }
            if let Some(scope) = self.destinations.get(self.destination) {
                let display = |path: &std::path::Path| {
                    scope
                        .project
                        .as_ref()
                        .and_then(|root| path.strip_prefix(root).ok())
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| skills::paths::contract_tilde(path))
                };
                for (i, (source, target)) in scope.links.iter().enumerate() {
                    if 4 + i >= band.height as usize {
                        break;
                    }
                    f.render_widget(
                        Paragraph::new(crate::tui::app::middle_ellipsis(
                            &format!(" Linked  {} → {}", display(source), display(target)),
                            band.width as usize,
                        ))
                        .style(th.dim()),
                        Rect::new(band.x, band.y + 4 + i as u16, band.width, 1),
                    );
                }
            }
        }

        let left = rows[2];
        self.left = left;
        let rows_data = self.rows(ctx);
        let report = ctx.snap.agent(&self.scope);
        self.caps = rows_data
            .iter()
            .map(|row| {
                let mut caps = Caps::of(row.state);
                caps.adopt &= report.is_some_and(|r| r.documents.contains_key(&row.name));
                caps
            })
            .collect();
        let valid = |row: &&Row<'_>| report.is_some_and(|r| r.documents.contains_key(&row.name));
        let mut counts = match (
            rows_data.iter().filter(valid).filter(|r| r.linked).count(),
            rows_data.iter().filter(valid).filter(|r| !r.linked).count(),
        ) {
            (0, 0) => "nothing here yet".to_string(),
            (n, 0) => format!("{n} linked"),
            (0, m) => format!("{m} the agent's own"),
            (n, m) => format!("{n} linked · {m} the agent's own"),
        };
        let invalid = rows_data
            .iter()
            .filter(|row| row.state.is_some() && !valid(row))
            .count();
        if invalid > 0 {
            if counts == "nothing here yet" {
                counts = "0 skills".into();
            }
            counts.push_str(&format!(" · {invalid} invalid entries"));
        }
        let title = Line::from(vec![
            Span::raw(" skills · "),
            Span::styled(format!("{counts} "), th.skill_count()),
        ]);
        let areas = self.search_panel.draw(
            f,
            left,
            PanelStyle {
                layout: PanelLayout::Unified,
                input_title: Line::default(),
                results_title: title,
                hint: ("/ filter skills…", "   tag:x  preset:y  repo:owner/repo"),
                input_active: self.filter_editing,
                results_active: matches!(self.focus(), Focus::Entries),
                header_height: u16::from(self.has_visible_groups()),
            },
            th,
        );
        self.content_filter_rect = areas.input;
        self.group_rects[2] = areas.header;
        self.draw_groups(f, areas.header, ctx);
        let inner = areas.results;

        let preferred = ctx.settings.layout_for(LayoutScope::Agents);
        let preferred_height = match preferred {
            UiLayout::Grid => ctx.settings.layout.card_height,
            UiLayout::List => 4,
            UiLayout::Compact => 1,
        };
        self.entry_layout = if inner.height < preferred_height {
            UiLayout::Compact
        } else {
            preferred
        };
        let cards = self.entry_layout == UiLayout::Grid;

        if rows_data.is_empty() && !self.destinations.is_empty() {
            let warning = match ctx.settings.ui.icons {
                skills::config::Icons::Nerd => "",
                skills::config::Icons::Text => "!",
            };
            f.render_widget(
                Paragraph::new(format!(
                    " {warning} No matching skills deployed here · press i to install"
                ))
                .style(th.warn()),
                inner,
            );
        }
        let cell_h = match self.entry_layout {
            UiLayout::Grid => ctx.settings.layout.card_height,
            UiLayout::List => 4,
            UiLayout::Compact => 1,
        };
        // One column is held back for the scrollbar so the column count does not
        // shift the moment the list outgrows a screen.
        let usable = inner.width.saturating_sub(1);
        let cols = if cards { cols_for(usable, ctx) } else { 1 };
        let content = Rect {
            width: usable,
            ..inner
        };
        self.entries.layout(
            content,
            cols,
            cell_h,
            if cols > 1 { 1 } else { 0 },
            rows_data.len(),
        );

        let selected = self.entries.selected();
        let visible = if cards {
            self.entries.visible_with_partial()
        } else {
            self.entries.visible()
        };
        for i in visible {
            let Some(cell) = self.entries.cell(i) else {
                continue;
            };
            let row = &rows_data[i];
            let on = selected == Some(i);
            let linked = (row.linked || row.state.is_none())
                .then_some(row.record)
                .flatten();
            let doc = ctx
                .snap
                .agent(&self.scope)
                .and_then(|agent| self.scope_counts.get(&agent.skills_dir))
                .and_then(|inventory| inventory.descriptions.get(&row.name));
            let mut presentation = linked
                .map(|record| SkillPresentation::managed(record, ctx))
                .unwrap_or_else(|| {
                    SkillPresentation::entry(
                        doc.map(|(name, _)| name.as_str()).unwrap_or(&row.name),
                        doc.map(|(_, description)| description.as_str()),
                        row.state,
                    )
                });
            if row.state.is_none() {
                presentation = presentation.not_deployed();
            }
            let render_state = SkillRenderState::default();
            if cards {
                let ci = skill_frame(f, cell, on, self.focus() == Focus::Entries, ctx);
                f.render_widget(
                    Paragraph::new(presentation.card(ctx, ci.width as usize, &render_state)),
                    ci,
                );
            } else {
                let style = if on {
                    if self.focus() == Focus::Entries {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    }
                } else {
                    Style::default()
                };
                let lines = if self.entry_layout == UiLayout::List {
                    presentation.list(ctx, cell.width as usize, on, &render_state)
                } else {
                    presentation.compact(ctx, cell.width as usize, on, &render_state)
                };
                f.render_widget(Paragraph::new(lines).style(style), cell);
            }
        }

        if cards {
            self.search_panel
                .draw_position(f, self.entries.visible(), rows_data.len(), th);
        }

        // The thumb measures grid rows, which is what a click on the track lands on.
        let vis = self.entries.visible_rows();
        self.entries_track = Rect::default();
        if self.entries.grid_rows() > vis && inner.height > 0 {
            let track = Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height);
            self.entries_track = track;
            let mut sb = ScrollbarState::new(self.entries.grid_rows())
                .position(selected.unwrap_or(0) / self.entries.cols())
                .viewport_content_length(vis);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .style(th.dim()),
                track,
                &mut sb,
            );
        }
        if self.filter_editing {
            self.search_panel.completion.draw(f, self.left, ctx);
        }
        self.preview.draw(f, area, ctx);
        self.matrix.draw(f, area, ctx);
        self.draw_group_popup(f, area, ctx);
    }

    fn hints_current(&self) -> Hints {
        if self.focus() == Focus::Filters {
            return &[
                ("←→", "filter badge"),
                ("Enter/Space", "toggle filter"),
                ("↑", "search"),
                ("↓", "skills"),
                ("Esc/q", "search"),
            ];
        }
        if self.focus() == Focus::Groups || self.quick.popup.is_some() {
            return &[
                ("↑↓←→", "group"),
                ("Enter", "install/uninstall / expand"),
                ("Esc/q", "back"),
            ];
        }
        if let Some(hints) = self.matrix.hints() {
            return hints;
        }
        if let Some(hints) = self.preview.hints() {
            return hints;
        }
        if self.filter_editing && self.search_panel.completion.active() {
            return &[
                ("↑↓", "suggestion"),
                ("Enter", "complete"),
                ("Esc", "close suggestions"),
            ];
        }
        if !self.destinations.is_empty() && self.filter_editing {
            return &[
                ("↑", "groups / scope"),
                ("Enter/↓", "results"),
                ("Esc", "results"),
            ];
        }
        if !self.destinations.is_empty() && self.focus() == Focus::Entries && self.caps.is_empty() {
            return &[
                ("/", "filter skills"),
                ("i", "install"),
                ("↑", "scope"),
                ("v", "layout"),
                ("[ ]", "agent"),
                ("Esc/q", "back"),
            ];
        }
        if !self.destinations.is_empty() && self.focus() == Focus::Entries {
            return match self.selected_caps() {
                Caps { linked: true, .. } => &[
                    ("↑↓←→", "skill · ↑ first row: filters/search"),
                    ("/", "filter skills"),
                    ("i", "install"),
                    ("x", "uninstall"),
                    ("m", "multi-uninstall"),
                    ("Enter", "preview"),
                    ("v", "layout"),
                    ("[ ]", "agent"),
                    ("Esc/q", "back"),
                ],
                Caps { clean: true, .. } => &[
                    ("↑↓←→", "skill · ↑ first row: filters/search"),
                    ("/", "filter skills"),
                    ("i", "install"),
                    ("x", "remove broken link"),
                    ("Enter", "preview"),
                    ("v", "layout"),
                    ("[ ]", "agent"),
                    ("Esc/q", "back"),
                ],
                Caps { relink: true, .. } => &[
                    ("↑↓←→", "skill · ↑ first row: filters/search"),
                    ("/", "filter skills"),
                    ("i", "install"),
                    ("r", "relink"),
                    ("Enter", "preview"),
                    ("v", "layout"),
                    ("[ ]", "agent"),
                    ("Esc/q", "back"),
                ],
                Caps { adopt: true, .. } => &[
                    ("↑↓←→", "skill · ↑ first row: filters/search"),
                    ("/", "filter skills"),
                    ("i", "install"),
                    ("a", "adopt"),
                    ("Enter", "preview"),
                    ("v", "layout"),
                    ("[ ]", "agent"),
                    ("Esc/q", "back"),
                ],
                _ => &[
                    ("↑↓←→", "skill · ↑ first row: filters/search"),
                    ("/", "filter skills"),
                    ("i", "install"),
                    ("Enter", "preview"),
                    ("v", "layout"),
                    ("[ ]", "agent"),
                    ("Esc/q", "back"),
                ],
            };
        }
        match self.focus() {
            Focus::Groups | Focus::Filters => &[],
            Focus::Scopes => &[
                ("←→", "scope"),
                ("↑", "agents"),
                ("↓/Enter", "groups / skills"),
                ("[ ]", "agent"),
                ("Esc/q", "back"),
            ],
            // A repair key is shown only on a row it applies to, so the footer
            // never offers something the page would refuse.
            Focus::Entries => match self.selected_caps() {
                Caps { clean: true, .. } => &[
                    ("j/k", "move"),
                    ("↑", "first row: filters/search"),
                    ("Enter", "preview"),
                    ("x", "clean"),
                    ("[ ]", "agent"),
                    ("v", "layout"),
                ],
                Caps { relink: true, .. } => &[
                    ("j/k", "move"),
                    ("↑", "first row: filters/search"),
                    ("Enter", "preview"),
                    ("r", "relink"),
                    ("[ ]", "agent"),
                    ("v", "layout"),
                ],
                Caps { linked: true, .. } => &[
                    ("j/k", "move"),
                    ("↑", "first row: filters/search"),
                    ("Enter", "preview"),
                    ("m", "multi-select"),
                    ("[ ]", "agent"),
                    ("v", "layout"),
                ],
                Caps { adopt: true, .. } => &[
                    ("a", "adopt"),
                    ("j/k", "move"),
                    ("↑", "first row: filters/search"),
                    ("Enter", "preview"),
                    ("[ ]", "agent"),
                    ("v", "layout"),
                ],
                _ => &[
                    ("↑↓←→", "skill"),
                    ("↑", "first row: filters/search"),
                    ("Enter", "preview"),
                    ("v", "layout"),
                    ("Esc/q", "back"),
                ],
            },
            Focus::Agents => &[
                ("←→", "pick agent"),
                ("↓/Enter", "scopes"),
                ("[ ]", "agent"),
                ("Esc/q", "back"),
            ],
        }
    }
}

impl AgentsView {
    fn refresh_scope(&mut self, ctx: &Ctx) {
        if self.destinations.is_empty() {
            self.refresh_current(ctx);
            return;
        }
        let old_name = self
            .scoped
            .as_ref()
            .and_then(|d| d.0.config.agent(&self.scope))
            .map(|a| a.display_name().to_string());
        let previous = self.destinations.get(self.destination).cloned();
        let result = (|| -> anyhow::Result<_> {
            let start = self
                .launch_directory
                .as_ref()
                .context("missing launch directory")?;
            let configured: Vec<_> = ctx
                .ws
                .config
                .agents
                .iter()
                .filter(|a| {
                    ctx.ws.inventory_products.as_ref().is_none_or(|products| {
                        products.contains(skills::ops::targets::product_key(a))
                    })
                })
                .cloned()
                .collect();
            let selected = configured
                .iter()
                .find(|a| Some(a.display_name()) == old_name.as_deref() || a.key == self.scope)
                .or_else(|| configured.first());
            let Some(selected) = selected else {
                let mut ws = ctx.ws.clone();
                ws.config.agents.clear();
                let snap = skills::reconcile::rescope(ctx.snap, &[])?;
                return Ok(std::sync::Arc::new((ws, snap)));
            };
            let mut locations = skills::ops::targets::locations(ctx.ws, selected, start)?;
            locations.sort_by_key(|scope| {
                let shared = scope
                    .directory
                    .as_deref()
                    .and_then(std::path::Path::parent)
                    .and_then(std::path::Path::file_name)
                    .is_some_and(|name| name == ".agents");
                (scope.project.is_some(), shared)
            });
            let index = previous
                .as_ref()
                .and_then(|old| {
                    locations
                        .iter()
                        .position(|s| s.directory == old.directory && s.project == old.project)
                })
                .or_else(|| {
                    previous.as_ref().and_then(|old| {
                        locations
                            .iter()
                            .position(|s| s.project.is_some() == old.project.is_some())
                    })
                })
                .unwrap_or(0);
            let location = locations
                .get(index)
                .context("agent has no deployment locations")?;
            let mut ws = ctx.ws.clone();
            ws.config.agents.clear();
            self.agent_directories.clear();
            for base in &configured {
                let candidates = skills::ops::targets::locations(ctx.ws, base, start)?;
                let chosen = candidates
                    .iter()
                    .find(|s| s.directory == location.directory && s.project == location.project)
                    .or_else(|| {
                        candidates
                            .iter()
                            .find(|s| s.project.is_some() == location.project.is_some())
                    })
                    .or_else(|| candidates.first());
                if let Some(chosen) = chosen {
                    let target = skills::ops::targets::scope_agent(ctx.ws, base, chosen)?;
                    if base.key == selected.key {
                        self.scope = target.key.clone();
                    }
                    let directories = candidates
                        .iter()
                        .filter_map(|scope| scope.directory.as_ref())
                        .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    self.agent_directories
                        .insert(target.key.clone(), directories);
                    ws.config.agents.push(target);
                }
            }
            self.destinations = locations;
            self.destination = index;
            let key: ScopeKey = ws
                .config
                .agents
                .iter()
                .map(|a| (a.key.clone(), a.skills_path()))
                .collect();
            if let Some(cached) = self.scope_cache.get(&key) {
                return Ok(cached.clone());
            }
            let snap = skills::reconcile::rescope(ctx.snap, &ws.config.agents)?;
            let data = std::sync::Arc::new((ws, snap));
            self.scope_cache.insert(key, data.clone());
            Ok(data)
        })();
        match result {
            Ok(data) => {
                if let Some(name) = old_name
                    && let Some(agent) = data
                        .0
                        .config
                        .agents
                        .iter()
                        .find(|a| a.display_name() == name)
                {
                    self.scope = agent.key.clone();
                }
                self.scope_error = None;
                self.scoped = Some(data.clone());
                self.refresh_current(&Ctx {
                    ws: &data.0,
                    snap: &data.1,
                    settings: ctx.settings,
                });
            }
            Err(e) => {
                self.scope_error = Some(format!("{e:#}"));
                self.scoped = None;
            }
        }
    }
}

impl View for AgentsView {
    fn context_menu(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        if self.preview.is_open()
            || self.matrix.hints().is_some()
            || self.quick.popup.is_some()
            || !self.left.contains((x, y).into())
            || self.entries_track.contains((x, y).into())
        {
            return None;
        }
        let index = self.entries.hit(x, y)?;
        let data = self.scoped.clone();
        let scoped = data.as_ref().map(|d| Ctx {
            ws: &d.0,
            snap: &d.1,
            settings: ctx.settings,
        });
        let ctx = scoped.as_ref().unwrap_or(ctx);
        self.rows(ctx).get(index)?;
        self.entries.select(Some(index));
        self.set_focus(Focus::Entries);
        self.filter_editing = false;
        self.search_panel.completion.close();
        self.entry_menu(ctx)
    }
    fn context_execute(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        let data = self.scoped.clone();
        let scoped = data.as_ref().map(|d| Ctx {
            ws: &d.0,
            snap: &d.1,
            settings: ctx.settings,
        });
        let ctx = scoped.as_ref().unwrap_or(ctx);
        let Some(request) = self
            .entry_menu(ctx)
            .filter(|r| &r.target == target && r.allows(command))
        else {
            return vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )];
        };
        let _ = request;
        let actions = self.entry_command(command, ctx);
        self.scoped_actions(actions, ctx)
    }

    fn focus_root(&mut self) {
        self.filter_editing = false;
        self.set_focus(Focus::Agents);
    }

    fn enter(&mut self) {
        self.enter_current();
    }
    fn refresh(&mut self, ctx: &Ctx) {
        self.scope_counts.clear();
        self.scope_count_rx = None;
        self.scope_cache.clear();
        self.content_searcher.borrow_mut().configure(
            ctx.settings.search.clone(),
            skills::dict::Dictionaries::load(&ctx.ws.root, &ctx.settings.search.dictionaries),
        );
        self.content_searcher.borrow_mut().index(&ctx.snap.skills);
        self.refresh_scope(ctx);
    }
    fn handle_key(&mut self, k: KeyEvent, ctx: &Ctx) -> Vec<Action> {
        if self.quick.popup.is_some() {
            let data = self.scoped.clone();
            let scoped = data.as_ref().map(|d| Ctx {
                ws: &d.0,
                snap: &d.1,
                settings: ctx.settings,
            });
            return self
                .quick_key(k, scoped.as_ref().unwrap_or(ctx))
                .unwrap_or_default();
        }
        if !self.preview.is_open()
            && self.matrix.hints().is_none()
            && self.focus() == Focus::Scopes
            && !self.destinations.is_empty()
        {
            match k.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.move_destination(-1, ctx);
                    return vec![];
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.move_destination(1, ctx);
                    return vec![];
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.set_focus(Focus::Agents);
                    return vec![];
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Enter => {
                    self.set_focus(if self.quick_visible() {
                        Focus::Groups
                    } else {
                        Focus::Entries
                    });
                    self.filter_editing = false;
                    return vec![];
                }
                _ => {}
            }
        }
        if let Some(error) = &self.scope_error {
            return if matches!(k.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Up) {
                match self.focus() {
                    Focus::Agents => vec![Action::BackToParent],
                    Focus::Scopes => {
                        self.set_focus(Focus::Agents);
                        vec![]
                    }
                    _ => {
                        self.filter_editing = false;
                        self.set_focus(Focus::Scopes);
                        vec![]
                    }
                }
            } else {
                vec![Action::Error(error.clone())]
            };
        }
        let data = self.scoped.clone();
        let scoped = data.as_ref().map(|d| Ctx {
            ws: &d.0,
            snap: &d.1,
            settings: ctx.settings,
        });
        let previous_scope = self.scope.clone();
        let actions = self.handle_key_current(k, scoped.as_ref().unwrap_or(ctx));
        if self.scope != previous_scope {
            self.refresh_scope(ctx);
        }
        self.scoped_actions(actions, scoped.as_ref().unwrap_or(ctx))
    }
    fn handle_mouse(&mut self, m: MouseEvent, ctx: &Ctx) -> Vec<Action> {
        if self.quick.popup.is_some() {
            let data = self.scoped.clone();
            let scoped = data.as_ref().map(|d| Ctx {
                ws: &d.0,
                snap: &d.1,
                settings: ctx.settings,
            });
            return self
                .group_mouse(m, scoped.as_ref().unwrap_or(ctx))
                .unwrap_or_default();
        }
        if !self.preview.is_open() && self.matrix.hints().is_none() && self.filter_editing {
            let (consumed, accepted) = self.search_panel.mouse_completion(m);
            if consumed {
                if accepted {
                    self.entries.first(0);
                    let data = self.scoped.clone();
                    let scoped = data.as_ref().map(|d| Ctx {
                        ws: &d.0,
                        snap: &d.1,
                        settings: ctx.settings,
                    });
                    self.update_search_completion(scoped.as_ref().unwrap_or(ctx));
                }
                return vec![];
            }
        }

        // The whole group is a focus target. Its controls still handle the
        // click below; focusing blank space never changes a selection or writes.
        if !self.preview.is_open()
            && self.matrix.hints().is_none()
            && m.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(index) = self
                .group_rects
                .iter()
                .take(2)
                .position(|rect| rect.contains((m.column, m.row).into()))
        {
            self.set_focus([Focus::Agents, Focus::Scopes][index]);
            self.filter_editing = false;
            self.search_panel.completion.close();
        }
        if !self.preview.is_open()
            && self.matrix.hints().is_none()
            && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some((_, index)) = self
                .destination_rects
                .iter()
                .find(|(r, _)| r.contains((m.column, m.row).into()))
        {
            let delta = *index as i32 - self.destination as i32;
            self.set_focus(Focus::Scopes);
            self.move_destination(delta, ctx);
            return vec![];
        }
        if self.scope_error.is_some() {
            return vec![];
        }
        let data = self.scoped.clone();
        let scoped = data.as_ref().map(|d| Ctx {
            ws: &d.0,
            snap: &d.1,
            settings: ctx.settings,
        });
        let previous_scope = self.scope.clone();
        let actions = self.handle_mouse_current(m, scoped.as_ref().unwrap_or(ctx));
        if self.scope != previous_scope {
            self.refresh_scope(ctx);
        }
        self.scoped_actions(actions, scoped.as_ref().unwrap_or(ctx))
    }
    fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        if ctx
            .ws
            .inventory_products
            .as_ref()
            .is_some_and(|products| products.is_empty())
        {
            self.scope_rects.clear();
            self.destination_rects.clear();
            f.render_widget(
                Paragraph::new("No installed agents detected.").style(ctx.settings.theme.dim()),
                area,
            );
            return;
        }
        let data = self.scoped.clone();
        let scoped = data.as_ref().map(|d| Ctx {
            ws: &d.0,
            snap: &d.1,
            settings: ctx.settings,
        });
        self.draw_current(f, area, scoped.as_ref().unwrap_or(ctx));
        if let Some(error) = &self.scope_error {
            f.render_widget(
                Paragraph::new(error.clone()).style(ctx.settings.theme.err()),
                self.left,
            );
        }
    }
    fn hints(&self) -> Hints {
        self.hints_current()
    }
}

fn entry_note(state: &EntryState) -> String {
    match state {
        EntryState::Deployed => String::new(),
        EntryState::Broken { .. } => "broken link".into(),
        EntryState::Shadow { same_content: true } => "the agent's own copy, same content".into(),
        EntryState::Shadow {
            same_content: false,
        } => "the agent's own copy, differs".into(),
        EntryState::Foreign { target } => format!("links outside the root → {}", target.display()),
        EntryState::AgentOnly => "only here, not in the root".into(),
    }
}

/// Fit whole pills, moving the start only when selection leaves the viewport.
pub(super) fn pill_window(
    widths: &[usize],
    selected: usize,
    offset: &mut usize,
    budget: usize,
) -> std::ops::Range<usize> {
    if widths.is_empty() {
        *offset = 0;
        return 0..0;
    }
    let selected = selected.min(widths.len() - 1);
    *offset = (*offset).min(selected);
    while *offset < selected && widths[*offset..=selected].iter().sum::<usize>() > budget {
        *offset += 1;
    }
    let mut end = *offset;
    let mut used = 0;
    while end < widths.len() && used + widths[end] <= budget {
        used += widths[end];
        end += 1;
    }
    *offset..end
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};
    use skills::{Workspace, config::AgentConfig};

    #[test]
    fn scope_cards_keep_their_geometry_when_switching_agents() {
        let tmp = skills::ops::DownloadDir::new("scope-card-widths").unwrap();
        let root = tmp.path().join("root");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(project.join(".codex/skills")).unwrap();
        let mut ws = Workspace::open(&root).unwrap();
        ws.config.agents = vec![
            AgentConfig {
                key: "codex".into(),
                name: "Codex".into(),
                skills_dir: "~/.codex/skills".into(),
            },
            AgentConfig {
                key: "trae-cli".into(),
                name: "TraeCode CLI".into(),
                skills_dir: "~/.trae/skills".into(),
            },
        ];
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = AgentsView::default();
        view.discover(&project).unwrap();
        for (w, h) in [(180, 40), (100, 30), (60, 20)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            view.scope = "codex".into();
            view.refresh_scope(&ctx);
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let before = view.destination_rects.clone();
            assert!(!before.is_empty());
            view.scope = "trae-cli".into();
            view.refresh_scope(&ctx);
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            assert_eq!(before, view.destination_rects, "terminal width {w}");
            if w < 132 {
                let (partial, index) = *view.destination_rects.last().unwrap();
                assert!(partial.width > 0 && partial.width < 32);
                if partial.height > 1 {
                    assert_eq!(
                        terminal.backend().buffer()[(partial.right() - 1, partial.bottom() - 1)]
                            .symbol(),
                        "─"
                    );
                }
                view.handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: partial.x,
                        row: partial.y,
                        modifiers: KeyModifiers::NONE,
                    },
                    &ctx,
                );
                terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
                assert_eq!(view.destination, index);
                assert_eq!(
                    view.destination_rects
                        .iter()
                        .find(|(_, i)| *i == index)
                        .unwrap()
                        .0
                        .width,
                    32
                );
                view.move_destination(-(index as i32), &ctx);
            }
        }
        view.move_destination(-(view.destination as i32), &ctx);
        view.move_destination(-1, &ctx);
        assert_eq!(view.destination, 0);
        let last = view.destinations.len() - 1;
        view.move_destination(last as i32, &ctx);
        assert_eq!(view.destination, last);
        view.move_destination(1, &ctx);
        assert_eq!(view.destination, last);
    }

    #[test]
    fn linked_local_skill_keeps_its_health_marker_across_layouts() {
        let root =
            std::env::temp_dir().join(format!("skills-agent-markers-{}", std::process::id()));
        let central = root.join("central");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(central.join("printer")).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            central.join("printer/SKILL.md"),
            "---\nname: printer\ndescription: Print documents\n---\nPrint documents.\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(central.join("printer"), agent_dir.join("printer")).unwrap();
        let mut ws = Workspace::open(&central).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample Agent".into(),
            skills_dir: agent_dir.display().to_string(),
        }];
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
        let mut view = AgentsView {
            scope: "sample".into(),
            ..AgentsView::default()
        };
        view.refresh(&ctx);
        let record = snap.get("printer").unwrap();
        assert!(matches!(
            record.status,
            skills::reconcile::SkillStatus::Local
        ));
        assert!(
            view.rows(&ctx)
                .iter()
                .any(|row| row.name == "printer" && row.linked)
        );
        for layout in [UiLayout::Grid, UiLayout::List, UiLayout::Compact] {
            let mut session = crate::tui::settings::SessionSettings::default();
            session.set_layout(LayoutScope::Agents, layout);
            let settings = crate::tui::settings::RuntimeSettings::resolve(&ws.config, &session);
            let ctx = Ctx {
                settings: &settings,
                ..ctx
            };
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let buf = terminal.backend().buffer();
            let text = (0..30)
                .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            let line = text.lines().find(|line| line.contains("printer")).unwrap();
            assert!(line.contains("●"), "layout={layout:?}: {line}");
            assert!(!line.contains("○"), "layout={layout:?}: {line}");
            assert_eq!(view.entry_layout, layout);
            if layout != UiLayout::Compact {
                assert!(text.contains("Print documents"));
            }
            view.set_focus(Focus::Entries);
            let actions = view
                .handle_key_current(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), &ctx);
            assert!(
                matches!(actions.as_slice(), [Action::SetLayout { scope: LayoutScope::Agents, layout: next }] if *next == layout.next())
            );
        }
        assert!(!central.join(".skills-meta").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_preview_reads_the_selected_entry_instead_of_its_root_namesake() {
        let root =
            std::env::temp_dir().join(format!("skills-agent-preview-{}", std::process::id()));
        let central = root.join("central");
        // Exercise truncation on every platform, even when temp_dir() is short.
        let agent_dir = root.join("long-agent-directory-".repeat(6));
        std::fs::create_dir_all(&central).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        let put = |dir: &std::path::Path, body: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: printer\ndescription: {body}\n---\n{body}\n"),
            )
            .unwrap();
        };
        put(&central.join("printer"), "CENTRAL CONTENT");
        put(&agent_dir.join("printer"), "AGENT COPY CONTENT");
        put(&agent_dir.join("reader"), "AGENT ONLY CONTENT");
        put(&root.join("external"), "EXTERNAL CONTENT");
        std::os::unix::fs::symlink(root.join("external"), agent_dir.join("external")).unwrap();
        std::os::unix::fs::symlink(root.join("absent"), agent_dir.join("broken")).unwrap();
        std::fs::create_dir_all(agent_dir.join("invalid")).unwrap();
        std::fs::write(agent_dir.join("invalid/SKILL.md"), "invalid frontmatter").unwrap();
        let mut ws = Workspace::open(&central).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample Agent".into(),
            skills_dir: agent_dir.display().to_string(),
        }];
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
        let mut view = AgentsView {
            scope: "sample".into(),
            ..AgentsView::default()
        };
        view.refresh(&ctx);
        view.set_focus(Focus::Entries);
        assert!(
            view.select_skills(&ctx, None).is_empty(),
            "agent-owned entries must not enter central skill selection"
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        view.scope_counts
            .insert(agent_dir.clone(), count_scope_skills(&agent_dir));
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let card_text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(card_text.contains("AGENT COPY CONTENT"));
        assert!(card_text.contains("AGENT ONLY CONTENT"));
        assert!(
            !card_text.contains("CENTRAL CONTENT"),
            "cards must use the agent's own description"
        );
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for (name, expected) in [
            ("printer", "AGENT COPY CONTENT"),
            ("reader", "AGENT ONLY CONTENT"),
            ("external", "EXTERNAL CONTENT"),
            ("broken", "missing SKILL.md"),
            ("invalid", "no YAML frontmatter"),
        ] {
            let rows = view.rows(&ctx);
            let index = rows.iter().position(|r| r.name == name).unwrap();
            view.entries.first(rows.len());
            view.entries.move_by(index as i32, rows.len());
            assert!(view.handle_key(key(KeyCode::Enter), &ctx).is_empty());
            terminal
                .draw(|f| view.preview.draw(f, f.area(), &ctx))
                .unwrap();
            let buf = terminal.backend().buffer();
            let text = (0..40)
                .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(expected), "{name}: {text}");
            assert!(text.contains("Sample Agent"));
            let path_line = text
                .lines()
                .find(|line| line.contains("path     "))
                .unwrap();
            assert!(
                path_line.contains('…'),
                "long paths should be collapsed: {path_line}"
            );
            assert!(!text.contains("CENTRAL CONTENT"));

            assert!(view.handle_key(key(KeyCode::Char('e')), &ctx).is_empty());
            terminal
                .draw(|f| view.preview.draw(f, f.area(), &ctx))
                .unwrap();
            let buf = terminal.backend().buffer();
            // Join wrapped rows without terminal padding or the overlay border.
            let expanded = (0..40)
                .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .map(|line| line.trim().trim_matches('│').trim().to_owned())
                .collect::<String>();
            let expected_path = agent_dir.join(name).join("SKILL.md");
            assert!(
                expanded.contains(&expected_path.display().to_string()),
                "{name}: {expanded}"
            );
            assert!(expanded.contains(expected), "{name}: {expanded}");
            assert!(!expanded.contains("CENTRAL CONTENT"));
            assert!(view.handle_key(key(KeyCode::Esc), &ctx).is_empty());
        }
        assert!(!central.join(".skills-meta").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_owned_search_uses_declared_names_and_full_content() {
        let tmp = skills::ops::DownloadDir::new("own-search").unwrap();
        let root = tmp.path().join("library");
        let directory = tmp.path().join("agent/skills");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(directory.join("stored-alias")).unwrap();
        std::fs::write(
            directory.join("stored-alias/SKILL.md"),
            "---\nname: approval-workflow\ndescription: Manage documents\n---\nAnalyze invoices",
        )
        .unwrap();
        let mut ws = Workspace::open(&root).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "example".into(),
            name: "Example".into(),
            skills_dir: directory.display().to_string(),
        }];
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
        let mut view = AgentsView {
            scope: "example".into(),
            destinations: skills::ops::targets::discover_scopes(tmp.path()).unwrap(),
            ..Default::default()
        };
        for query in [
            "Approval",
            "aproval",
            "invoices",
            "stored-alias",
            "agent:example",
        ] {
            view.search_panel.input.set(query);
            let rows = view.rows(&ctx);
            assert_eq!(rows.len(), 1, "{query}");
            assert_eq!(rows[0].name, "stored-alias");
        }
        view.search_panel.input.set("unrelated");
        assert!(view.rows(&ctx).is_empty());
    }

    #[test]
    fn agent_grid_renders_partial_cards_and_hides_zero_coverage_groups() {
        let tmp = skills::ops::DownloadDir::new("agent-partial-cards").unwrap();
        let root = tmp.path().join("library");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        for i in 0..30 {
            let name = format!("skill-{i:02}");
            let path = root.join(&name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Sample description\n---\nBody"),
            )
            .unwrap();
            if i < 29 {
                std::os::unix::fs::symlink(&path, target.join(&name)).unwrap();
            }
        }
        let mut ws = Workspace::open(&root).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample".into(),
            skills_dir: target.display().to_string(),
        }];
        for (name, keys) in [
            ("installed-group", vec!["skill-00".into()]),
            ("empty-group", vec![]),
            ("uninstalled-group", vec!["skill-29".into()]),
        ] {
            ws.presets
                .save(&Preset {
                    name: name.into(),
                    skills: keys,
                    ..Default::default()
                })
                .unwrap();
        }
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = AgentsView {
            scope: "sample".into(),
            ..Default::default()
        };
        view.refresh(&ctx);
        let mut checked_partial = false;
        for height in 28..34 {
            let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("installed-group"));
            let header = view.group_rects[2];
            let text: String = (header.y..header.bottom())
                .flat_map(|y| (header.x..header.right()).map(move |x| (x, y)))
                .map(|p| buffer[p].symbol())
                .collect();
            assert!(!text.contains("empty-group"));
            assert!(!text.contains("uninstalled-group"));
            let full = view.entries.visible();
            if let Some(cell) = view.entries.cell(full.end)
                && cell.height >= 2
            {
                checked_partial = true;
                assert_eq!(buffer[(cell.x, cell.y)].symbol(), "╭");
                let name: String = (cell.x..cell.right())
                    .map(|x| buffer[(x, cell.y + 1)].symbol())
                    .collect();
                assert!(name.contains(&format!("skill-{:02}", full.end)));
            }
        }
        assert!(checked_partial);
    }

    #[test]
    fn coverage_clicks_filter_without_deploying_or_focusing_on_hover() {
        let root = std::env::temp_dir().join(format!("skills-pills-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut ws = Workspace::open(&root).unwrap();
        ws.config.agents = vec![AgentConfig {
            key: "sample".into(),
            name: "Sample".into(),
            skills_dir: "../sample".into(),
        }];
        for i in 0..24 {
            ws.presets
                .save(&Preset {
                    name: format!("group-{i:02}-long-name"),
                    ..Preset::default()
                })
                .unwrap();
        }
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
        let mut view = AgentsView::default();
        view.refresh(&ctx);
        assert!(!view.has_visible_groups());
        for (_, status) in &mut view.presets {
            status.installed = 1;
            status.total = 1;
        }

        for (w, h) in [(100, 30), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let rects = view.preset_rects.clone();
            assert!(!rects.is_empty());
            for (_, rect) in rects {
                assert!(rect.right() <= w && rect.bottom() <= h);
                for focus in [Focus::Agents, Focus::Scopes, Focus::Entries] {
                    view.set_focus(focus);
                    for kind in [
                        MouseEventKind::Down(MouseButton::Left),
                        MouseEventKind::Moved,
                        MouseEventKind::ScrollDown,
                    ] {
                        view.set_focus(focus);
                        let actions = view.handle_mouse(
                            MouseEvent {
                                kind,
                                column: rect.x,
                                row: rect.y,
                                modifiers: KeyModifiers::NONE,
                            },
                            &ctx,
                        );
                        assert!(actions.is_empty());
                        assert_eq!(
                            view.focus(),
                            if kind == MouseEventKind::Down(MouseButton::Left) {
                                Focus::Filters
                            } else {
                                focus
                            },
                            "coverage clicks filter, hover and wheel preserve focus"
                        );
                        assert!(view.rows(&ctx).is_empty());
                    }
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod deployment_scope_tests {
    use super::*;
    use skills::{
        Workspace,
        config::{AgentConfig, Config},
    };

    #[test]
    fn same_named_repository_members_keep_identity_search_and_uninstall_targets() {
        let tmp = skills::ops::DownloadDir::new("agent-group-identity").unwrap();
        let root = tmp.path().join("library");
        let target = tmp.path().join("target");
        let a = "repos/acme--one/shared";
        let b = "repos/acme--two/shared";
        for (key, body) in [(a, "OWNER-ALPHA"), (b, "OWNER-BETA")] {
            std::fs::create_dir_all(root.join(key)).unwrap();
            std::fs::write(
                root.join(key).join("SKILL.md"),
                format!("---\nname: shared\ndescription: shared tools\n---\n{body}"),
            )
            .unwrap();
        }
        Config {
            agents: vec![AgentConfig {
                key: "sample".into(),
                name: "Sample".into(),
                skills_dir: target.display().to_string(),
            }],
            tags: [
                ("only-a", vec![a]),
                ("only-b", vec![b]),
                ("both", vec![a, b]),
            ]
            .into_iter()
            .map(|(name, keys)| skills::config::TagConfig {
                name: name.into(),
                skills: keys.into_iter().map(str::to_owned).collect(),
                color: None,
                description: None,
            })
            .collect(),
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(root.join(b), target.join("shared")).unwrap();
        let ws = Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = AgentsView {
            destinations: skills::ops::targets::discover_scopes(tmp.path()).unwrap(),
            ..Default::default()
        };
        view.refresh_current(&ctx);
        let inventory = view.rows(&ctx);
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].record.unwrap().key, b);
        assert!(inventory[0].linked);
        let rows = view.rows(&ctx);
        assert_eq!(
            rows.len(),
            1,
            "groups do not add missing members to inventory"
        );
        view.search_panel.input.paste("tag:only-a").unwrap();
        let rows = view.rows(&ctx);
        assert!(rows.is_empty(), "search only filters installed inventory");
        assert!(view.select_skills(&ctx, None).is_empty());
        view.search_panel.input.clear();

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        view.set_focus(Focus::Entries);
        terminal
            .draw(|f| view.draw_current(f, f.area(), &ctx))
            .unwrap();
        view.preview_entry(&ctx);
        terminal
            .draw(|f| view.draw_current(f, f.area(), &ctx))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("OWNER-BETA"));
        assert!(!text.contains("OWNER-ALPHA"));
        view.preview.close();
        assert_eq!(
            std::fs::canonicalize(target.join("shared")).unwrap(),
            std::fs::canonicalize(root.join(b)).unwrap()
        );

        terminal
            .draw(|f| view.draw_current(f, f.area(), &ctx))
            .unwrap();
        let selection = view.select_skills(&ctx, None);
        let [Action::SelectAgentSkills { keys, .. }] = selection.as_slice() else {
            panic!("installed member selection");
        };
        assert_eq!(keys, &[b]);
        let Action::OpenModal(modal) = view
            .handle_key_current(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE), &ctx)
            .remove(0)
        else {
            panic!("uninstall confirmation");
        };
        let Modal::ConfirmWrite {
            title,
            write: Some(write),
            ..
        } = *modal
        else {
            panic!("scoped uninstall");
        };
        assert!(title.contains(b));
        write(&ws).unwrap();
        assert!(!target.join("shared").exists());
        assert!(root.join(a).join("SKILL.md").exists());
        assert!(root.join(b).join("SKILL.md").exists());
    }

    #[test]
    fn coverage_keeps_scope_inventory_and_search_independent() {
        let tmp = skills::ops::DownloadDir::new("agent-tag-packages").unwrap();
        let root = tmp.path().join("library");
        let target = tmp.path().join("target");
        for name in ["one", "two", "solo"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(
                root.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name} tools\n---\nbody"),
            )
            .unwrap();
        }
        Config {
            agents: vec![AgentConfig {
                key: "sample".into(),
                name: "Sample".into(),
                skills_dir: target.display().to_string(),
            }],
            tags_enabled: true,
            tags: vec![skills::config::TagConfig {
                name: "tools".into(),
                skills: vec!["one".into(), "two".into()],
                color: None,
                description: None,
            }],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.presets
            .save(&Preset {
                name: "work".into(),
                skills: vec!["one".into(), "two".into()],
                ..Default::default()
            })
            .unwrap();
        let snap = ws.scan().unwrap();
        deploy::apply(
            &deploy::plan_deploy(
                &ws,
                &snap,
                &["one".into(), "solo".into()],
                &["sample".into()],
            )
            .unwrap(),
        )
        .unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = AgentsView::default();
        view.refresh_current(&ctx);
        assert_eq!(
            view.rows(&ctx)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["one", "solo"]
        );
        assert_eq!(
            (view.presets[0].1.installed, view.presets[0].1.total),
            (1, 2)
        );
        assert_eq!((view.tags[0].included, view.tags[0].total), (1, 2));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|f| view.draw_current(f, f.area(), &ctx))
            .unwrap();
        assert!(
            view.preset_rects[0].1.x < view.tag_rects[0].1.x,
            "presets render before tags in the unified group row"
        );
        view.set_focus(Focus::Entries);
        let rect = view.tag_rects[0].1;
        let actions = view.handle_mouse_current(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert!(actions.is_empty());
        assert_eq!(view.focus(), Focus::Filters);
        assert_eq!(view.search_panel.input.value(), "tag:tools");
        assert_eq!(
            view.rows(&ctx)
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["one"]
        );
        terminal
            .draw(|f| view.draw_current(f, f.area(), &ctx))
            .unwrap();
        let rect = view.tag_rects[0].1;
        assert!(
            !terminal.backend().buffer()[(rect.x + 1, rect.y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            terminal.backend().buffer()[(rect.x + 1, rect.y)]
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        view.handle_mouse_current(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert!(view.search_panel.input.is_empty());
        assert_eq!(view.rows(&ctx).len(), 2);
        let buffer = terminal.backend().buffer();
        for x in [rect.x, rect.right() - 1] {
            assert!(!buffer[(x, rect.y)].modifier.intersects(
                ratatui::style::Modifier::BOLD
                    | ratatui::style::Modifier::UNDERLINED
                    | ratatui::style::Modifier::REVERSED
            ));
        }
        view.set_focus(Focus::Groups);
        view.handle_key_current(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
        assert!(view.filter_editing);
        view.handle_key_current(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
        assert_eq!(view.focus(), Focus::Filters);
        view.filter_cursor = 0;
        view.handle_key_current(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(view.search_panel.input.value(), "preset:work");
        view.handle_key_current(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &ctx);
        view.handle_key_current(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert_eq!(view.search_panel.input.value(), "tag:tools");
        view.handle_key_current(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
        assert!(view.search_panel.input.is_empty());
        view.handle_key_current(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
        assert_eq!(view.focus(), Focus::Entries);
        view.handle_key_current(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &ctx);
        assert_eq!(view.focus(), Focus::Filters);
        view.handle_key_current(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &ctx);
        assert!(view.filter_editing);
        view.handle_key_current(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &ctx);
        assert_eq!(view.focus(), Focus::Groups);
        // Deployment uses all members even with an unrelated skill query.
        view.search_panel.input.set("solo");
        let actions = view.toggle_group(
            &groups::GroupKey {
                preset: false,
                name: "tools".into(),
            },
            &ctx,
        );
        let (_, Action::BatchMeta(write, keys)) = actions.into_iter().next().unwrap().into_scoped()
        else {
            panic!("group install")
        };
        assert_eq!(keys, ["one", "two"]);
        assert!(!target.join("two").exists());
        write(&ws).unwrap();
        assert!(target.join("two").is_symlink());
        assert!(target.join("one").is_symlink());
    }

    #[test]
    fn preset_coverage_tracks_overlaps_current_members_and_external_removal() {
        let tmp = skills::ops::DownloadDir::new("preset-current-coverage").unwrap();
        let root = tmp.path().join("root");
        let target = tmp.path().join("target");
        for name in ["one", "two"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(
                root.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: test\n---\nbody"),
            )
            .unwrap();
        }
        Config {
            agents: vec![AgentConfig {
                key: "test".into(),
                name: "Test".into(),
                skills_dir: target.display().to_string(),
            }],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        for name in ["first", "second"] {
            ws.presets
                .save(&Preset {
                    name: name.into(),
                    skills: vec!["one".into()],
                    // An explicitly selected scope takes precedence over legacy defaults.
                    agents: vec!["another-agent".into()],
                    ..Default::default()
                })
                .unwrap();
        }
        let agent = ws.config.agent("test").unwrap();
        skills::ops::targets::set_deployed(&ws, agent, None, &["one".into()], true).unwrap();
        let theme = crate::tui::theme::Theme::default();
        let mut view = AgentsView::default();
        let mut refresh = |expected: &[(usize, usize)]| {
            let snap = ws.scan().unwrap();
            view.refresh_current(&Ctx {
                ws: &ws,
                snap: &snap,
                settings: &{
                    let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                    settings.theme = theme;
                    settings
                },
            });
            let counts: Vec<_> = view
                .presets
                .iter()
                .map(|(_, s)| (s.installed, s.total))
                .collect();
            assert_eq!(counts, expected);
        };
        refresh(&[(1, 1), (1, 1)]);
        let mut second = ws.presets.load("second").unwrap().unwrap();
        second.skills.push("two".into());
        ws.presets.save(&second).unwrap();
        refresh(&[(1, 1), (1, 2)]);
        std::fs::remove_file(target.join("one")).unwrap();
        refresh(&[(0, 1), (0, 2)]);
    }

    #[test]
    fn scope_counts_include_owned_and_linked_skills_but_exclude_invalid_entries() {
        let base = std::env::temp_dir().join(format!("skills-scope-counts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let directory = base.join("skills");
        std::fs::create_dir_all(directory.join("owned")).unwrap();
        std::fs::create_dir_all(base.join("upstream")).unwrap();
        for path in [directory.join("owned"), base.join("upstream")] {
            std::fs::write(
                path.join("SKILL.md"),
                "---\nname: sample\ndescription: test\n---\nbody",
            )
            .unwrap();
        }
        std::os::unix::fs::symlink(base.join("upstream"), directory.join("deployed")).unwrap();
        std::os::unix::fs::symlink(base.join("missing"), directory.join("broken")).unwrap();
        std::fs::create_dir_all(directory.join("invalid")).unwrap();
        std::fs::write(directory.join("invalid/SKILL.md"), "invalid").unwrap();
        std::fs::create_dir_all(directory.join("no-skill")).unwrap();
        std::os::unix::fs::symlink(&directory, base.join("shared")).unwrap();
        assert_eq!(count_scope_skills(&directory).label, "2 skills");
        assert_eq!(count_scope_skills(&base.join("shared")).label, "2 skills");
        assert_eq!(
            count_scope_skills(&base.join("missing")).label,
            "Not created"
        );
        assert_eq!(
            count_scope_skills(&directory.join("broken")).label,
            "Unreadable"
        );
        std::fs::remove_file(directory.join("deployed")).unwrap();
        assert_eq!(count_scope_skills(&directory).label, "1 skills");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn navigation_reuses_scopes_without_rescan_and_refresh_invalidates_cache() {
        let tmp = skills::ops::DownloadDir::new("scope-navigation-cost").unwrap();
        let root = tmp.path().join("root");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(root.join("sample")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            root.join("sample/SKILL.md"),
            "---\nname: sample\ndescription: example\n---\nbody",
        )
        .unwrap();
        Config {
            agents: vec![
                AgentConfig {
                    key: "claude".into(),
                    name: "Claude Code".into(),
                    skills_dir: tmp.path().join("global-claude").display().to_string(),
                },
                AgentConfig {
                    key: "codex".into(),
                    name: "Codex".into(),
                    skills_dir: tmp.path().join("global-codex").display().to_string(),
                },
            ],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let global = tmp.path().join("global-claude");
        std::fs::create_dir_all(&global).unwrap();
        let global = std::fs::canonicalize(global).unwrap();
        std::os::unix::fs::symlink(root.join("sample"), global.join("sample")).unwrap();
        let ws = Workspace::open(&root).unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = AgentsView::default();
        view.discover(&project).unwrap();
        view.refresh(&ctx);
        let directories = view.agent_directories[&view.scope].clone();
        assert!(directories.contains(&global));
        assert!(
            directories.contains(
                &std::fs::canonicalize(&project)
                    .unwrap()
                    .join(".claude/skills")
            )
        );
        for directory in &directories {
            view.scope_counts
                .insert(directory.clone(), ScopeInventory::default());
        }
        view.scope_counts
            .insert(global.clone(), count_scope_skills(&global));
        assert_eq!(
            view.agent_skill_count(&view.scope).as_deref(),
            Some("1 skills total")
        );
        view.move_destination(-1, &ctx);
        assert_eq!(view.agent_directories[&view.scope], directories);
        assert_eq!(
            view.agent_skill_count(&view.scope).as_deref(),
            Some("1 skills total")
        );
        view.move_destination(1, &ctx);
        let first = view.scoped.clone().unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let rect = view
            .scope_rects
            .iter()
            .find(|(_, key)| key.starts_with("codex"))
            .unwrap()
            .0;
        let actions = view.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 1,
                row: rect.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            &ctx,
        );
        assert!(
            actions.is_empty(),
            "clicking an agent must not schedule a library rescan"
        );
        assert!(view.scope.starts_with("codex"));
        let actions = view.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE), &ctx);
        assert!(actions.is_empty());
        assert!(view.scope.starts_with("claude"));
        assert!(std::sync::Arc::ptr_eq(
            &first,
            view.scoped.as_ref().unwrap()
        ));
        view.move_destination(-1, &ctx);
        assert!(view.project().is_none());
        view.move_destination(1, &ctx);
        assert!(std::sync::Arc::ptr_eq(
            &first,
            view.scoped.as_ref().unwrap()
        ));
        // An explicit refresh must observe writes made outside the UI too.
        let target = project.join(".claude/skills");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(ws.skill_path("sample"), target.join("sample")).unwrap();
        view.refresh(&ctx);
        assert!(!std::sync::Arc::ptr_eq(
            &first,
            view.scoped.as_ref().unwrap()
        ));
        assert!(
            view.scoped
                .as_ref()
                .unwrap()
                .1
                .agent(&view.scope)
                .unwrap()
                .entries
                .contains_key("sample")
        );
    }

    #[test]
    fn installed_product_filter_survives_scope_switches() {
        let tmp = skills::ops::DownloadDir::new("installed-scope-filter").unwrap();
        let root = tmp.path().join("root");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Config {
            agents: vec![
                AgentConfig {
                    key: "installed".into(),
                    name: "Installed Agent".into(),
                    skills_dir: tmp.path().join("installed").display().to_string(),
                },
                AgentConfig {
                    key: "absent".into(),
                    name: "Absent Agent".into(),
                    skills_dir: tmp.path().join("absent").display().to_string(),
                },
            ],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let mut ws = Workspace::open(&root).unwrap();
        ws.inventory_products = Some(std::collections::BTreeSet::from(["installed".into()]));
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = AgentsView::default();
        view.discover(&project).unwrap();
        view.refresh(&ctx);
        assert_eq!(view.scoped.as_ref().unwrap().0.config.agents.len(), 1);
        view.move_destination(1, &ctx);
        assert_eq!(
            view.scoped.as_ref().unwrap().0.config.agents[0].name,
            "Installed Agent"
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Installed Agent"));
        assert!(!text.contains("Absent Agent"));
        ws.inventory_products = Some(Default::default());
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        view.refresh(&ctx);
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("No installed agents detected."));
    }

    #[test]
    fn cards_bind_operations_to_the_selected_scope_without_changing_the_library() {
        let base =
            std::env::temp_dir().join(format!("skills-agent-scope-cards-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        let project = base.join("project");
        std::fs::create_dir_all(root.join("sample")).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(
            root.join("sample/SKILL.md"),
            "---\nname: sample-skill\ndescription: a useful skill\n---\nbody\n",
        )
        .unwrap();
        Config {
            agents: vec![AgentConfig {
                key: "claude".into(),
                name: "Claude Code".into(),
                skills_dir: base.join("global/claude").display().to_string(),
            }],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        let ws = Workspace::open(&root).unwrap();
        ws.presets
            .save(&Preset {
                name: "quality".into(),
                skills: vec!["sample".into()],
                ..Default::default()
            })
            .unwrap();
        let snap = ws.scan().unwrap();
        let theme = crate::tui::theme::Theme::default();
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &{
                let mut settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
                settings.theme = theme;
                settings
            },
        };
        let mut view = AgentsView::default();
        view.discover(&project).unwrap();
        view.refresh(&ctx);
        assert_eq!(view.project(), Some(project.canonicalize().unwrap()));
        assert_eq!(view.scoped.as_ref().unwrap().0.root, ws.root);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let search_area = view.content_filter_rect;
        let skills_area = view.left;
        assert!(
            skills_area.height >= 10,
            "compact controls leave room for skills"
        );
        assert!(skills_area.contains((search_area.x, search_area.y).into()));
        for focus in [Focus::Agents, Focus::Scopes, Focus::Entries, Focus::Entries] {
            view.set_focus(focus);
            term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            assert_eq!(
                view.content_filter_rect, search_area,
                "search stays inside Skills for every focus"
            );
            assert_eq!(view.left, skills_area, "focus must not shift the layout");
        }
        view.set_focus(Focus::Entries);
        assert!(
            view.handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: search_area.x,
                    row: search_area.y,
                    modifiers: KeyModifiers::NONE,
                },
                &ctx
            )
            .is_empty()
        );
        assert_eq!(view.focus(), Focus::Entries);
        assert!(view.filter_editing);
        view.filter_editing = false;
        view.set_focus(Focus::Scopes);
        view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &ctx);
        view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &ctx);
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
        assert_eq!(
            view.focus(),
            Focus::Groups,
            "scope change then Down before redraw must not skip groups"
        );
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        view.set_focus(Focus::Agents);
        for (code, focus, editing) in [
            (KeyCode::Down, Focus::Scopes, false),
            (KeyCode::Down, Focus::Groups, false),
            (KeyCode::Down, Focus::Entries, true),
            (KeyCode::Down, Focus::Entries, false),
            (KeyCode::Up, Focus::Entries, true),
            (KeyCode::Up, Focus::Groups, false),
            (KeyCode::Up, Focus::Scopes, false),
            (KeyCode::Up, Focus::Agents, false),
        ] {
            assert!(
                view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
                    .is_empty()
            );
            assert_eq!(view.focus(), focus);
            assert_eq!(view.filter_editing, editing);
        }
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &ctx)
                .as_slice(),
            [Action::BackToParent]
        ));
        let selection = (view.scope.clone(), view.destination);
        for (index, expected) in [Focus::Agents, Focus::Scopes, Focus::Entries]
            .into_iter()
            .enumerate()
        {
            term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
            let rect = view.group_rects[index];
            if rect.is_empty() {
                assert_eq!(index, 2, "only empty coverage is omitted");
                continue;
            }
            for (column, row) in [
                (rect.right() - 2, rect.y + rect.height.saturating_sub(2)),
                (rect.x, rect.y),
            ] {
                view.filter_editing = true;
                let actions = view.handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    },
                    &ctx,
                );
                assert!(
                    actions.is_empty(),
                    "blank group clicks must not deploy or rescan"
                );
                assert_eq!(view.focus(), expected);
                assert!(!view.filter_editing);
                assert_eq!((view.scope.clone(), view.destination), selection);
            }
        }
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Global") && text.contains("project"));
        assert!(text.contains("󰋜") && text.contains("󰉋"));
        assert!(text.contains("Target"));
        assert!(view.destination_rects.iter().all(|(r, _)| r.right() <= 80));
        assert_eq!(
            view.preset_rects.len(),
            0,
            "presets without installations in this scope are hidden"
        );
        assert!(!project.join(".claude").exists(), "browsing must not write");
        for focus in [Focus::Agents, Focus::Scopes] {
            view.set_focus(focus);
            let hints = view.hints_current();
            assert!(
                !hints
                    .iter()
                    .any(|(key, _)| *key == "i" || *key == "m" || *key == "v")
            );
            for code in ['i', 'm', 'v', 'a', 'r'] {
                assert!(
                    view.handle_key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE), &ctx)
                        .is_empty()
                );
            }
        }

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        view.set_focus(Focus::Entries);
        let actions = view.handle_key(key(KeyCode::Char('i')), &ctx);
        let Action::OpenModal(mut modal) = actions.into_iter().next().unwrap() else {
            panic!("install picker")
        };
        let Modal::PresetSkills(picker) = modal.as_mut() else {
            panic!("shared search picker")
        };
        picker.focus_list();
        picker.handle_key(key(KeyCode::Char(' ')), &ctx);
        let actions = picker.handle_key(key(KeyCode::Char('a')), &ctx);
        // The pending install keeps its original target even if the view changes.
        view.move_destination(-1, &ctx);
        assert!(view.project().is_none());
        let write = actions
            .into_iter()
            .find_map(|action| {
                if let (_, Action::BatchMeta(write, _)) = action.into_scoped() {
                    Some(write)
                } else {
                    None
                }
            })
            .unwrap();
        write(&ws).unwrap();
        assert!(project.join(".claude/skills/sample-skill").is_symlink());
        assert!(!base.join("global/claude/sample").exists());
        assert!(root.join("sample/SKILL.md").exists());
        view.move_destination(1, &ctx);
        view.set_focus(Focus::Entries);
        view.refresh(&ctx);
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let scoped = view.scoped.clone().unwrap();
        let scoped_ctx = Ctx {
            ws: &scoped.0,
            snap: &scoped.1,
            settings: ctx.settings,
        };
        view.search_panel.input.set("no match");
        let actions = view.toggle_group(
            &groups::GroupKey {
                preset: true,
                name: "quality".into(),
            },
            &scoped_ctx,
        );
        let (_, Action::BatchMeta(write, keys)) = actions.into_iter().next().unwrap().into_scoped()
        else {
            panic!("preset install")
        };
        assert_eq!(keys, ["sample"]);
        view.move_destination(-1, &ctx);
        write(&ws).unwrap();
        assert!(
            !project.join(".claude/skills/sample").exists(),
            "full preset toggles off in original scope"
        );
        assert!(!base.join("global/claude/sample").exists());
        view.move_destination(1, &ctx);
        view.set_focus(Focus::Entries);
        view.refresh(&ctx);
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let scoped = view.scoped.clone().unwrap();
        let scoped_ctx = Ctx {
            ws: &scoped.0,
            snap: &scoped.1,
            settings: ctx.settings,
        };
        let actions = view.toggle_group(
            &groups::GroupKey {
                preset: true,
                name: "quality".into(),
            },
            &scoped_ctx,
        );
        let (_, Action::BatchMeta(write, _)) = actions.into_iter().next().unwrap().into_scoped()
        else {
            panic!("reinstall preset")
        };
        write(&ws).unwrap();
        view.refresh(&ctx);
        term.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let cell = view.entries.cell(view.entries.selected().unwrap()).unwrap();
        let request = view.context_menu(cell.x + 1, cell.y, &ctx).unwrap();
        let Target::Entry { path, .. } = &request.target else {
            panic!("scoped target")
        };
        assert_eq!(
            *path,
            project
                .canonicalize()
                .unwrap()
                .join(".claude/skills/sample-skill")
        );
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('x')), &ctx).as_slice(),
            [Action::OpenModal(_)]
        ));
        let actions = view.context_execute(&request.target, Command::Remove, &ctx);
        let Action::OpenModal(modal) = actions.into_iter().next().unwrap() else {
            panic!("uninstall confirmation")
        };
        let Modal::ConfirmWrite {
            write: Some(write), ..
        } = *modal
        else {
            panic!("scoped uninstall")
        };
        write(&ws).unwrap();
        assert!(
            !project.join(".claude/skills/sample").exists(),
            "removing a skill affects only the selected scope"
        );
        assert!(root.join("sample/SKILL.md").exists());
        std::fs::remove_dir_all(base).unwrap();
    }
}
