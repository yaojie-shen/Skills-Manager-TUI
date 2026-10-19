//! Lazy, read-only tree and file preview for a skill directory.
//!
//! The browser owns only cached results and navigation state. All filesystem
//! access happens in `event::spawn_file_browser`; requests use root-relative paths and
//! generations so an old result cannot replace a newer request.

use crate::tui::{app::Ctx, keymap, theme::Theme, widgets::fit};
use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use skills::{config::Icons, util::display_safe};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

const MAX_DIR_ENTRIES: usize = 2_000;
const MAX_PREVIEW_BYTES: usize = 256 * 1024;
const BINARY_PROBE_BYTES: usize = 8 * 1024;
const NARROW_WIDTH: u16 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Tree,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Loading,
    Text { text: String, truncated: bool },
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadKind {
    Directory,
    File,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub skill_key: String,
    pub root: PathBuf,
    pub path: PathBuf,
    pub generation: u64,
    pub kind: LoadKind,
}

#[derive(Debug)]
pub enum Loaded {
    Directory {
        entries: Vec<DirEntry>,
        visible_total: usize,
        total: usize,
    },
    File(Content),
}

#[derive(Debug)]
pub struct Output {
    pub skill_key: String,
    pub path: PathBuf,
    pub generation: u64,
    pub result: Result<Loaded>,
}

#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub kind: EntryKind,
    pub last: bool,
}

#[derive(Debug)]
struct Listing {
    entries: Vec<DirEntry>,
    visible_total: usize,
    total: usize,
}

#[derive(Debug)]
pub struct Browser {
    skill_key: String,
    root: PathBuf,
    entries: Vec<TreeEntry>,
    listings: HashMap<PathBuf, Listing>,
    contents: HashMap<PathBuf, Content>,
    pending: HashMap<PathBuf, u64>,
    requests: Vec<Request>,
    expanded: HashSet<PathBuf>,
    selected: usize,
    tree_offset: usize,
    preview_scroll: usize,
    show_hidden: bool,
    pub focus: Focus,
    pending_g: bool,
    tree_rect: Rect,
    preview_rect: Rect,
    tree_height: usize,
    preview_height: usize,
}

impl Browser {
    pub fn open(skill_key: String, root: PathBuf) -> Self {
        let mut browser = Self {
            skill_key,
            root,
            entries: Vec::new(),
            listings: HashMap::new(),
            contents: HashMap::new(),
            pending: HashMap::new(),
            requests: Vec::new(),
            expanded: HashSet::new(),
            selected: 0,
            tree_offset: 0,
            preview_scroll: 0,
            show_hidden: false,
            // The unified preview opens for reading; Tab or `f` moves to files.
            focus: Focus::Preview,
            pending_g: false,
            tree_rect: Rect::default(),
            preview_rect: Rect::default(),
            tree_height: 1,
            preview_height: 1,
        };
        browser.request(PathBuf::new(), LoadKind::Directory);
        browser
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    fn request(&mut self, path: PathBuf, kind: LoadKind) {
        if self.pending.contains_key(&path) {
            return;
        }
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.pending.insert(path.clone(), generation);
        if kind == LoadKind::File {
            self.contents.insert(path.clone(), Content::Loading);
        }
        self.requests.push(Request {
            skill_key: self.skill_key.clone(),
            root: self.root.clone(),
            path,
            generation,
            kind,
        });
    }

    pub fn accepts(&self, output: &Output) -> bool {
        output.skill_key == self.skill_key
            && self.pending.get(&output.path) == Some(&output.generation)
    }

    pub fn apply(&mut self, output: Output) {
        if !self.accepts(&output) {
            return;
        }
        self.pending.remove(&output.path);
        match output.result {
            Ok(Loaded::Directory {
                entries,
                visible_total,
                total,
            }) => {
                self.listings.insert(
                    output.path.clone(),
                    Listing {
                        entries,
                        visible_total,
                        total,
                    },
                );
                self.rebuild(Some(&output.path));
                if output.path.as_os_str().is_empty() {
                    self.select_skill();
                    self.load_selected();
                }
            }
            Ok(Loaded::File(content)) => {
                self.contents.insert(output.path, content);
            }
            Err(error) => match self.entry_kind(&output.path) {
                Some(EntryKind::Directory) | None if output.path.as_os_str().is_empty() => {
                    self.listings.insert(
                        output.path,
                        Listing {
                            entries: vec![DirEntry {
                                name: format!("Unable to read directory: {error:#}"),
                                kind: EntryKind::File,
                            }],
                            visible_total: 1,
                            total: 1,
                        },
                    );
                    self.rebuild(None);
                }
                _ => {
                    self.contents.insert(
                        output.path,
                        Content::Message(format!("Unable to read file: {error:#}")),
                    );
                }
            },
        }
    }

    fn entry_kind(&self, path: &Path) -> Option<EntryKind> {
        self.entries
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.kind.clone())
    }

    fn select_skill(&mut self) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.path == Path::new("SKILL.md"))
        {
            self.selected = index;
        }
    }

    fn rebuild(&mut self, preserve: Option<&Path>) {
        let selected = preserve.map(Path::to_path_buf).or_else(|| {
            self.entries
                .get(self.selected)
                .map(|entry| entry.path.clone())
        });
        self.entries.clear();
        self.collect_cached(Path::new(""), 0);
        self.selected = selected
            .and_then(|path| self.entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0)
            .min(self.entries.len().saturating_sub(1));
    }

    fn collect_cached(&mut self, directory: &Path, depth: usize) {
        let Some(listing) = self.listings.get(directory) else {
            return;
        };
        let visible = listing
            .entries
            .iter()
            .filter(|entry| self.show_hidden || !entry.name.starts_with('.'))
            .take(MAX_DIR_ENTRIES)
            .cloned()
            .collect::<Vec<_>>();
        let shown_total = if self.show_hidden {
            listing.total
        } else {
            listing.visible_total
        };
        let omitted = shown_total.saturating_sub(visible.len());
        for (index, child) in visible.iter().enumerate() {
            let path = directory.join(&child.name);
            self.entries.push(TreeEntry {
                path: path.clone(),
                name: child.name.clone(),
                depth,
                kind: child.kind.clone(),
                last: index + 1 == visible.len() && omitted == 0,
            });
            if child.kind == EntryKind::Directory && self.expanded.contains(&path) {
                self.collect_cached(&path, depth + 1);
            }
        }
        if omitted > 0 {
            self.entries.push(TreeEntry {
                path: directory.join(format!(".skills-omitted-{omitted}")),
                name: format!("… {omitted} more"),
                depth,
                kind: EntryKind::File,
                last: true,
            });
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.entries.len() - 1);
        self.load_selected();
    }

    fn page(&mut self, delta: isize, height: usize) {
        self.move_by(delta.saturating_mul(height.saturating_sub(2).max(1) as isize));
    }

    fn activate(&mut self) {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return;
        };
        match entry.kind {
            EntryKind::Directory => {
                if !self.expanded.remove(&entry.path) {
                    self.expanded.insert(entry.path.clone());
                    if !self.listings.contains_key(&entry.path) {
                        self.request(entry.path.clone(), LoadKind::Directory);
                    }
                }
                self.rebuild(Some(&entry.path));
            }
            EntryKind::File | EntryKind::Symlink(_) => self.focus = Focus::Preview,
        }
        self.load_selected();
    }

    fn collapse_or_parent(&mut self) {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return;
        };
        if entry.kind == EntryKind::Directory && self.expanded.remove(&entry.path) {
            self.rebuild(Some(&entry.path));
            return;
        }
        if let Some(parent) = entry
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            && let Some(index) = self.entries.iter().position(|entry| entry.path == parent)
        {
            self.selected = index;
            self.load_selected();
        }
    }

    pub fn focus_tree(&mut self) {
        self.focus = Focus::Tree;
    }

    pub fn focus_preview(&mut self) {
        self.focus = Focus::Preview;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let pending_g = self.pending_g;
        self.pending_g = false;
        match key.code {
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Tree {
                    Focus::Preview
                } else {
                    Focus::Tree
                };
            }
            KeyCode::Right if self.focus == Focus::Tree => self.focus = Focus::Preview,
            KeyCode::Left if self.focus == Focus::Preview => self.focus = Focus::Tree,
            KeyCode::Char('.') if keymap::plain(key) => {
                let selected = self
                    .entries
                    .get(self.selected)
                    .map(|entry| entry.path.clone());
                self.show_hidden = !self.show_hidden;
                self.rebuild(selected.as_deref());
                self.load_selected();
            }
            KeyCode::Char('g') if keymap::plain(key) && self.focus == Focus::Tree => {
                if pending_g {
                    self.selected = 0;
                    self.load_selected();
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('G') if keymap::plain(key) && self.focus == Focus::Tree => {
                self.selected = self.entries.len().saturating_sub(1);
                self.load_selected();
            }
            KeyCode::Home if self.focus == Focus::Tree => {
                self.selected = 0;
                self.load_selected();
            }
            KeyCode::End if self.focus == Focus::Tree => {
                self.selected = self.entries.len().saturating_sub(1);
                self.load_selected();
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Tree => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Tree => self.move_by(-1),
            KeyCode::PageDown if self.focus == Focus::Tree => self.page(1, self.tree_height),
            KeyCode::PageUp if self.focus == Focus::Tree => self.page(-1, self.tree_height),
            KeyCode::Char('d') if keymap::control(key, 'd') && self.focus == Focus::Tree => {
                self.page(1, self.tree_height)
            }
            KeyCode::Char('u') if keymap::control(key, 'u') && self.focus == Focus::Tree => {
                self.page(-1, self.tree_height)
            }
            KeyCode::Enter | KeyCode::Char('l') if self.focus == Focus::Tree => self.activate(),
            KeyCode::Left | KeyCode::Char('h') if self.focus == Focus::Tree => {
                self.collapse_or_parent()
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == Focus::Preview => {
                self.preview_scroll = self.preview_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') if self.focus == Focus::Preview => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1)
            }
            KeyCode::PageDown if self.focus == Focus::Preview => {
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_add(self.preview_height.saturating_sub(2).max(1))
            }
            KeyCode::PageUp if self.focus == Focus::Preview => {
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_sub(self.preview_height.saturating_sub(2).max(1))
            }
            KeyCode::Char('d') if keymap::control(key, 'd') && self.focus == Focus::Preview => {
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_add(self.preview_height.saturating_sub(2).max(1))
            }
            KeyCode::Char('u') if keymap::control(key, 'u') && self.focus == Focus::Preview => {
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_sub(self.preview_height.saturating_sub(2).max(1))
            }
            KeyCode::Char('g') if keymap::plain(key) && self.focus == Focus::Preview => {
                if pending_g {
                    self.preview_scroll = 0;
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Home if self.focus == Focus::Preview => self.preview_scroll = 0,
            KeyCode::End | KeyCode::Char('G') if self.focus == Focus::Preview => {
                self.preview_scroll = usize::MAX
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent, wheel_rows: i32) {
        let point = (event.column, event.row).into();
        let over_tree = self.tree_rect.contains(point);
        let over_preview = self.preview_rect.contains(point);
        match event.kind {
            MouseEventKind::ScrollDown if over_preview => {
                self.focus = Focus::Preview;
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_add(wheel_rows.max(1) as usize);
            }
            MouseEventKind::ScrollUp if over_preview => {
                self.focus = Focus::Preview;
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_sub(wheel_rows.max(1) as usize);
            }
            MouseEventKind::ScrollDown if over_tree => {
                self.focus = Focus::Tree;
                self.move_by(wheel_rows.max(1) as isize);
            }
            MouseEventKind::ScrollUp if over_tree => {
                self.focus = Focus::Tree;
                self.move_by(-(wheel_rows.max(1) as isize));
            }
            MouseEventKind::Down(MouseButton::Left) if over_preview => {
                self.focus = Focus::Preview;
            }
            MouseEventKind::Down(MouseButton::Left) if over_tree => {
                self.focus = Focus::Tree;
                let row = event.row.saturating_sub(self.tree_rect.y + 1) as usize;
                let index = self.tree_offset.saturating_add(row);
                if index < self.entries.len() {
                    self.selected = index;
                    self.load_selected();
                }
            }
            _ => {}
        }
    }

    fn load_selected(&mut self) {
        self.preview_scroll = 0;
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        if matches!(entry.kind, EntryKind::File | EntryKind::Symlink(_))
            && !self.contents.contains_key(&entry.path)
            && !entry.name.starts_with("… ")
        {
            self.request(entry.path.clone(), LoadKind::File);
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.tree_rect = Rect::default();
        self.preview_rect = Rect::default();
        if area.width < NARROW_WIDTH {
            if self.focus == Focus::Tree {
                self.tree_rect = area;
                self.tree_height = area.height.saturating_sub(2).max(1) as usize;
                self.draw_tree(frame, area, &ctx.settings.theme, ctx.settings.ui.icons);
            } else {
                self.preview_rect = area;
                self.preview_height = area.height.saturating_sub(2).max(1) as usize;
                self.draw_preview(frame, area, &ctx.settings.theme);
            }
            return;
        }
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length((area.width / 4).clamp(22, 32)),
                Constraint::Min(1),
            ])
            .split(area);
        self.tree_rect = panes[0];
        self.preview_rect = panes[1];
        self.tree_height = panes[0].height.saturating_sub(2).max(1) as usize;
        self.preview_height = panes[1].height.saturating_sub(2).max(1) as usize;
        self.draw_tree(frame, panes[0], &ctx.settings.theme, ctx.settings.ui.icons);
        self.draw_preview(frame, panes[1], &ctx.settings.theme);
    }

    fn draw_tree(&mut self, frame: &mut Frame, area: Rect, th: &Theme, icons: Icons) {
        let title = format!(" Files{} ", if self.show_hidden { " · hidden" } else { "" });
        let block = th.block(title, self.focus == Focus::Tree);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = inner.height as usize;
        if self.selected < self.tree_offset {
            self.tree_offset = self.selected;
        }
        if self.selected >= self.tree_offset.saturating_add(height) {
            self.tree_offset = self.selected.saturating_sub(height.saturating_sub(1));
        }
        let mut lines = self
            .entries
            .iter()
            .enumerate()
            .skip(self.tree_offset)
            .take(height)
            .map(|(index, entry)| {
                let branch = if entry.depth == 0 {
                    String::new()
                } else {
                    format!(
                        "{}{}",
                        "│ ".repeat(entry.depth.saturating_sub(1)),
                        if entry.last { "└─" } else { "├─" }
                    )
                };
                let marker = match entry.kind {
                    EntryKind::Directory if self.expanded.contains(&entry.path) => "- ",
                    EntryKind::Directory => "+ ",
                    _ => "  ",
                };
                let icon = if icons == Icons::Nerd && entry.kind == EntryKind::Directory {
                    if self.expanded.contains(&entry.path) {
                        "󰝰"
                    } else {
                        "󰉋"
                    }
                } else {
                    file_icon(icons, entry)
                };
                let mut line = Line::from(vec![
                    Span::styled(branch, th.dim()),
                    Span::styled(marker, th.accent()),
                    Span::styled(format!("{icon} "), icon_style(th, entry)),
                    Span::raw(display_safe(&entry.name).into_owned()),
                ]);
                if let EntryKind::Symlink(target) = &entry.kind {
                    line.push_span(Span::styled(
                        format!(" → {}", display_safe(&target.to_string_lossy())),
                        th.dim(),
                    ));
                }
                if index == self.selected {
                    line = line.style(if self.focus == Focus::Tree {
                        th.selected()
                    } else {
                        th.selected_unfocused()
                    });
                }
                line
            })
            .collect::<Vec<_>>();
        if self.entries.is_empty() {
            lines.push(Line::from(Span::styled("loading…", th.dim())));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_preview(&mut self, frame: &mut Frame, area: Rect, th: &Theme) {
        let selected = self.entries.get(self.selected);
        let title = display_safe(selected.map_or("Preview", |entry| entry.name.as_str()));
        let block = th.block(
            format!(" {} ", fit(&title, area.width.saturating_sub(4) as usize)),
            self.focus == Focus::Preview,
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = match selected {
            None => vec![Line::from(Span::styled("loading…", th.dim()))],
            Some(entry) if entry.kind == EntryKind::Directory => {
                vec![Line::raw("Directory — press l or Enter to expand")]
            }
            Some(entry) => content_lines(
                self.contents.get(&entry.path).unwrap_or(&Content::Loading),
                entry.path.extension().and_then(|value| value.to_str()),
                inner.width as usize,
            ),
        };
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let max = paragraph
            .line_count(inner.width.max(1))
            .saturating_sub(inner.height as usize);
        self.preview_scroll = self.preview_scroll.min(max);
        frame.render_widget(
            paragraph.scroll((self.preview_scroll.min(u16::MAX as usize) as u16, 0)),
            inner,
        );
    }
}

fn content_lines(content: &Content, extension: Option<&str>, width: usize) -> Vec<Line<'static>> {
    match content {
        Content::Loading => vec![Line::raw("loading…")],
        Content::Message(message) => vec![Line::raw(display_safe(message).into_owned())],
        Content::Text { text, truncated } => {
            let mut lines = if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("md")) {
                crate::tui::markdown::render(text, width.max(1))
            } else {
                text.lines()
                    .enumerate()
                    .map(|(index, line)| {
                        Line::from(vec![
                            Span::styled(
                                format!("{:>4} │ ", index + 1),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                            Span::raw(display_safe(line).into_owned()),
                        ])
                    })
                    .collect()
            };
            if *truncated {
                lines.push(Line::raw(format!(
                    "… truncated after {} KiB",
                    MAX_PREVIEW_BYTES / 1024
                )));
            }
            lines
        }
    }
}

pub fn load(request: &Request) -> Output {
    let result = match request.kind {
        LoadKind::Directory => load_directory(&request.root, &request.path),
        LoadKind::File => load_file(&request.root, &request.path),
    };
    Output {
        skill_key: request.skill_key.clone(),
        path: request.path.clone(),
        generation: request.generation,
        result,
    }
}

fn safe_relative(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize()
        .map_err(|error| anyhow!("skill directory: {error}"))
}

fn load_directory(root: &Path, relative: &Path) -> Result<Loaded> {
    if !safe_relative(relative) {
        return Err(anyhow!("path leaves the skill directory"));
    }
    let canonical_root = canonical_root(root)?;
    let directory = canonical_root.join(relative);
    let canonical = directory
        .canonicalize()
        .map_err(|error| anyhow!("{}: {error}", relative.display()))?;
    if !canonical.starts_with(&canonical_root)
        || fs::symlink_metadata(&directory)?.file_type().is_symlink()
    {
        return Err(anyhow!("directory is outside the skill directory"));
    }
    let mut visible = BTreeMap::new();
    let mut hidden = BTreeMap::new();
    let mut visible_total = 0usize;
    let mut total = 0usize;
    for item in fs::read_dir(&canonical)? {
        let item = item?;
        let path = canonical.join(item.file_name());
        let metadata = fs::symlink_metadata(&path)?;
        let kind = if metadata.file_type().is_symlink() {
            EntryKind::Symlink(fs::read_link(&path).unwrap_or_default())
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        let entry = DirEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            kind,
        };
        total += 1;
        if entry.name.starts_with('.') {
            retain_best(&mut hidden, entry);
        } else {
            visible_total += 1;
            retain_best(&mut visible, entry);
        }
    }
    // Cache bounded prefixes for both modes. `collect_cached` applies the one
    // active 2,000-row cap after merging, while hidden-off keeps 2,000 visible.
    let mut entries = visible
        .into_values()
        .chain(hidden.into_values())
        .collect::<Vec<_>>();
    entries.sort_by_key(sort_key);
    Ok(Loaded::Directory {
        entries,
        visible_total,
        total,
    })
}

fn retain_best(entries: &mut BTreeMap<(u8, String, String), DirEntry>, entry: DirEntry) {
    entries.insert(sort_key(&entry), entry);
    if entries.len() > MAX_DIR_ENTRIES {
        entries.pop_last();
    }
}

fn sort_key(entry: &DirEntry) -> (u8, String, String) {
    let rank = if entry.name == "SKILL.md" {
        0
    } else if entry.kind == EntryKind::Directory {
        1
    } else {
        2
    };
    (rank, entry.name.to_lowercase(), entry.name.clone())
}

/// Canonical confinement prevents ordinary escapes; a concurrent parent swap
/// can only redirect this read-only preview during the remaining check/open race.
fn load_file(root: &Path, relative: &Path) -> Result<Loaded> {
    if !safe_relative(relative) {
        return Err(anyhow!("path leaves the skill directory"));
    }
    let canonical_root = canonical_root(root)?;
    let path = canonical_root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.is_dir() {
        return Ok(Loaded::File(Content::Message("Directory".into())));
    }
    let target = metadata
        .file_type()
        .is_symlink()
        .then(|| fs::read_link(&path))
        .transpose()?;
    let canonical = path.canonicalize().map_err(|error| match &target {
        Some(target) => anyhow!("symbolic link → {}: {error}", target.display()),
        None => anyhow!("{}: {error}", relative.display()),
    })?;
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        return Ok(Loaded::File(Content::Message(match target {
            Some(target) => format!(
                "Symbolic link → {} (outside the skill directory or not a file)",
                target.display()
            ),
            None => "File is outside the skill directory or no longer regular".into(),
        })));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&canonical)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Ok(Loaded::File(Content::Message(
            "File is no longer regular".into(),
        )));
    }
    let size = opened.len();
    let mut bytes = Vec::with_capacity(MAX_PREVIEW_BYTES + 1);
    file.by_ref()
        .take((MAX_PREVIEW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.iter().take(BINARY_PROBE_BYTES).any(|byte| *byte == 0) {
        return Ok(Loaded::File(Content::Message(format!(
            "Binary file ({size} bytes)"
        ))));
    }
    let truncated = bytes.len() > MAX_PREVIEW_BYTES;
    bytes.truncate(MAX_PREVIEW_BYTES);
    if truncated {
        while !bytes.is_empty()
            && std::str::from_utf8(&bytes).is_err_and(|error| error.error_len().is_none())
        {
            bytes.pop();
        }
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Ok(Loaded::File(Content::Message(
                "Binary or non-UTF-8 file".into(),
            )));
        }
    };
    Ok(Loaded::File(Content::Text { text, truncated }))
}

pub fn file_icon(mode: Icons, entry: &TreeEntry) -> &'static str {
    if mode == Icons::Text {
        return match entry.kind {
            EntryKind::Directory => "",
            EntryKind::Symlink(_) => "@",
            EntryKind::File => "*",
        };
    }
    if entry.kind == EntryKind::Directory {
        return "󰉋";
    }
    if matches!(entry.kind, EntryKind::Symlink(_)) {
        return "󰌷";
    }
    let name = entry.name.to_ascii_lowercase();
    let ext = entry
        .path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match (name.as_str(), ext.as_str()) {
        ("license" | "license.md" | "copying", _) => "",
        ("cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml", _) => "󰌾",
        (".gitignore" | ".gitattributes" | ".gitmodules", _) => "",
        (_, "md") => "",
        (_, "rs") => "",
        (_, "py") => "",
        (_, "sh" | "bash" | "zsh") => "",
        (_, "js") => "",
        (_, "ts" | "tsx") => "",
        (_, "json") => "",
        (_, "yaml" | "yml") => "",
        (_, "toml") => "",
        (_, "txt") => "󰈙",
        (_, "html") => "",
        (_, "css") => "",
        (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp") => "󰈟",
        (_, "pdf") => "",
        (_, "lock") => "󰌾",
        _ => "󰈔",
    }
}

fn icon_style(th: &Theme, entry: &TreeEntry) -> Style {
    match entry.kind {
        EntryKind::Directory => th.accent(),
        EntryKind::Symlink(_) => th.warn(),
        EntryKind::File => th.source(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use std::fs;

    fn fixture() -> skills::ops::DownloadDir {
        let root = skills::ops::DownloadDir::new("file-browser").unwrap();
        fs::write(root.path().join("z.txt"), "z").unwrap();
        fs::write(root.path().join("SKILL.md"), "# Skill").unwrap();
        fs::create_dir(root.path().join("scripts")).unwrap();
        fs::write(root.path().join(".secret"), "hidden").unwrap();
        root
    }

    fn loaded_browser() -> (skills::ops::DownloadDir, Browser) {
        let root = fixture();
        let mut browser = Browser::open("skill".into(), root.path().to_path_buf());
        let request = browser.take_requests().pop().unwrap();
        browser.apply(load(&request));
        (root, browser)
    }

    #[test]
    fn lazy_root_sorts_skill_then_directories_then_files() {
        let (_root, browser) = loaded_browser();
        assert_eq!(
            browser
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["SKILL.md", "scripts", "z.txt"]
        );
        assert_eq!(browser.selected, 0);
    }

    #[test]
    fn hidden_toggle_and_lazy_directory_request() {
        let (_root, mut browser) = loaded_browser();
        browser.handle_key(KeyEvent::from(KeyCode::Char('.')));
        assert!(browser.entries.iter().any(|entry| entry.name == ".secret"));
        browser.selected = browser
            .entries
            .iter()
            .position(|entry| entry.name == "scripts")
            .unwrap();
        browser.focus_tree();
        browser.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            browser
                .take_requests()
                .iter()
                .any(|request| request.kind == LoadKind::Directory)
        );
    }

    #[test]
    fn hidden_mode_still_caps_one_directory_at_two_thousand_rows() {
        let (_root, mut browser) = loaded_browser();
        let mut entries = (0..MAX_DIR_ENTRIES)
            .map(|index| DirEntry {
                name: format!("visible-{index:04}"),
                kind: EntryKind::File,
            })
            .collect::<Vec<_>>();
        entries.extend((0..MAX_DIR_ENTRIES).map(|index| DirEntry {
            name: format!(".hidden-{index:04}"),
            kind: EntryKind::File,
        }));
        browser.listings.insert(
            PathBuf::new(),
            Listing {
                entries,
                visible_total: MAX_DIR_ENTRIES,
                total: MAX_DIR_ENTRIES * 2,
            },
        );
        browser.show_hidden = true;
        browser.rebuild(None);
        assert_eq!(browser.entries.len(), MAX_DIR_ENTRIES + 1);
        assert_eq!(browser.entries.last().unwrap().name, "… 2000 more");
    }

    #[test]
    fn stale_generation_is_ignored() {
        let (_root, mut browser) = loaded_browser();
        let before = browser.entries.len();
        browser.apply(Output {
            skill_key: "skill".into(),
            path: PathBuf::new(),
            generation: 0,
            result: Ok(Loaded::Directory {
                entries: vec![],
                visible_total: 0,
                total: 0,
            }),
        });
        assert_eq!(browser.entries.len(), before);
    }

    #[test]
    fn navigation_and_icons() {
        let (_root, mut browser) = loaded_browser();
        browser.focus_tree();
        browser.handle_key(KeyEvent::from(KeyCode::Char('G')));
        let bottom = browser.selected;
        browser.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(browser.selected, bottom);
        browser.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(browser.selected, 0);
        browser.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(browser.focus, Focus::Preview);
        browser.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(file_icon(Icons::Nerd, &browser.entries[0]), "");
        assert_eq!(file_icon(Icons::Text, &browser.entries[0]), "*");
    }

    #[test]
    fn arrows_switch_panes_without_activating_directories() {
        let (_root, mut browser) = loaded_browser();
        browser.handle_key(KeyCode::Down.into());
        assert_eq!(browser.selected, 0, "content arrows must not select files");
        assert_eq!(browser.preview_scroll, 1);

        browser.handle_key(KeyCode::Left.into());
        assert_eq!(browser.focus, Focus::Tree);
        browser.handle_key(KeyCode::Right.into());
        assert_eq!(browser.focus, Focus::Preview);
        assert_eq!(browser.selected, 0);

        browser.handle_key(KeyCode::Left.into());
        browser.handle_key(KeyCode::Down.into());
        let directory = browser.entries[browser.selected].path.clone();
        assert_eq!(directory, Path::new("scripts"));
        browser.handle_key(KeyCode::Right.into());
        assert_eq!(browser.focus, Focus::Preview);
        assert_eq!(browser.entries[browser.selected].path, directory);
        assert!(!browser.expanded.contains(&directory));

        browser.handle_key(KeyCode::Left.into());
        browser.handle_key(KeyCode::Enter.into());
        assert!(browser.expanded.contains(&directory));
        browser.handle_key(KeyCode::Left.into());
        assert!(!browser.expanded.contains(&directory));
        assert_eq!(browser.focus, Focus::Tree);
        browser.handle_key(KeyCode::Tab.into());
        assert_eq!(browser.focus, Focus::Preview);
    }

    #[test]
    fn control_characters_in_names_and_text_are_not_rendered() {
        let root = fixture();
        std::fs::write(root.path().join("a\x1b[2J.txt"), "x\x07\x1b[2Jy").unwrap();
        let mut browser = Browser::open("skill".into(), root.path().to_path_buf());
        let request = browser.take_requests().pop().unwrap();
        browser.apply(load(&request));
        browser.selected = browser
            .entries
            .iter()
            .position(|entry| entry.name.starts_with('a'))
            .unwrap();
        browser.load_selected();
        let request = browser.take_requests().pop().unwrap();
        browser.apply(load(&request));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| {
                let [tree, preview] =
                    Layout::horizontal([Constraint::Fill(1); 2]).areas(frame.area());
                browser.draw_tree(frame, tree, &Theme::default(), Icons::default());
                browser.draw_preview(frame, preview, &Theme::default());
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!rendered.contains(['\x1b', '\x07']));
        assert!(rendered.contains("x\u{fffd}\u{fffd}[2Jy"));
    }

    #[test]
    fn preview_scroll_uses_wrapped_visual_lines() {
        let (_root, mut browser) = loaded_browser();
        browser.contents.insert(
            PathBuf::from("SKILL.md"),
            Content::Text {
                text: "x".repeat(1_000),
                truncated: false,
            },
        );
        browser.preview_scroll = 10;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| browser.draw_preview(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(browser.preview_scroll, 10);
    }

    #[test]
    fn bounded_text_binary_and_escape_symlink() {
        let root = fixture();
        fs::write(root.path().join("binary"), b"x\0y").unwrap();
        let binary = Request {
            skill_key: "skill".into(),
            root: root.path().into(),
            path: "binary".into(),
            generation: 1,
            kind: LoadKind::File,
        };
        assert!(
            matches!(load(&binary).result.unwrap(), Loaded::File(Content::Message(message)) if message.contains("Binary"))
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", root.path().join("escape")).unwrap();
            let escape = Request {
                path: "escape".into(),
                ..binary
            };
            assert!(
                matches!(load(&escape).result.unwrap(), Loaded::File(Content::Message(message)) if message.contains("outside"))
            );
        }
    }
}
