//! Shared group colour and merge chooser, independent of page implementations.
//! Colour preview and acceptance both use the selected candidate. Editing the
//! filter selects an exact colour match or exposes a selected custom candidate.
//! Filtering preserves the choice viewport; terminal resizing still refits it.

use super::group::{self, Kind, tag_fill};
use crate::tui::app::{Action, Ctx, Hints};
use crate::tui::theme::Theme;
use crate::tui::widgets::PopupSurface;
use crate::tui::widgets::{Input, ListNav, PopupClass, fit, popup_size, popup_too_small, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph};
use skills::preset::Preset;

/// The row in the colour prompt that takes a tag's colour away again.
const NO_COLOR: &str = "none";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Which group the selected one is merged into.
    Merge,
    /// What colour identifies the selected tag or preset.
    Color,
}

/// Shared chooser for group colours and merge targets.
/// Pages own the write action; the chooser owns filtering, selection and preview.
pub struct Prompt {
    ask: Ask,
    identity: Kind,
    name: String,
    preset: Option<Preset>,
    input: Input,
    choices: Vec<String>,
    custom: Option<String>,
    /// Indices into `choices` matching the filter, in display order;
    /// `choices.len()` denotes the custom candidate.
    shown: Vec<usize>,
    list: ListNav,
    rect: Rect,
}

impl Prompt {
    pub fn for_merge(identity: Kind, name: &str, choices: Vec<String>) -> Self {
        Self::new(Ask::Merge, identity, name, choices)
    }

    pub fn kind(&self) -> Ask {
        self.ask
    }

    pub fn hints(&self) -> Hints {
        match self.ask {
            Ask::Merge => &[
                ("↑↓", "target"),
                ("Enter", "merge"),
                ("type", "filter"),
                ("Esc", "cancel"),
            ],
            Ask::Color => &[
                ("↑↓", "colour"),
                ("Enter", "apply"),
                ("type", "name / #rrggbb"),
                ("Esc", "cancel"),
            ],
        }
    }

    pub fn value(&self) -> &str {
        self.input.value()
    }

    pub fn for_color(identity: Kind, name: &str, current: Option<&str>, theme: &Theme) -> Self {
        let mut choices: Vec<String> = theme.color_choices.iter().map(|c| c.to_string()).collect();
        choices.push(NO_COLOR.into());
        if let Some(c) = current
            && !choices.iter().any(|s| s == c)
        {
            choices.insert(0, c.to_string());
        }
        let selected = choices
            .iter()
            .position(|s| s == current.unwrap_or(NO_COLOR));
        let mut prompt = Self::new(Ask::Color, identity, name, choices);
        prompt.list.state.select(selected);
        prompt
    }

    pub fn for_preset_color(preset: &Preset, theme: &Theme) -> Self {
        let mut prompt =
            Self::for_color(Kind::Preset, &preset.name, preset.color.as_deref(), theme);
        prompt.preset = Some(preset.clone());
        prompt
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn paste(&mut self, text: &str) -> Vec<Action> {
        match self.input.paste(text) {
            Ok(true) => {
                self.refilter();
                vec![]
            }
            Ok(false) => vec![],
            Err(e) => vec![Action::Error(e.into())],
        }
    }

    // Some(true) submits, Some(false) dismisses; None keeps the chooser open.
    pub fn key(&mut self, k: KeyEvent) -> Option<bool> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return Some(false),
            KeyCode::Enter => return Some(true),
            KeyCode::Down => self.list.move_by(1, self.shown.len()),
            KeyCode::Up => self.list.move_by(-1, self.shown.len()),
            KeyCode::Char('n') if ctrl => self.list.move_by(1, self.shown.len()),
            KeyCode::Char('p') if ctrl => self.list.move_by(-1, self.shown.len()),
            KeyCode::PageUp => self.list.first(self.shown.len()),
            KeyCode::PageDown => self.list.last(self.shown.len()),
            _ => {
                if self.input.handle_key(k) {
                    self.refilter();
                }
            }
        }
        None
    }

    pub fn mouse(&mut self, m: MouseEvent) -> Option<bool> {
        match crate::tui::widgets::popup_mouse(m, self.rect) {
            crate::tui::widgets::PopupMouse::OutsidePress => return Some(false),
            crate::tui::widgets::PopupMouse::Inside => {}
            _ => return None,
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.list.move_by(1, self.shown.len()),
            MouseEventKind::ScrollUp => self.list.move_by(-1, self.shown.len()),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.list.rows.contains((m.column, m.row).into())
                    && let Some((_, true)) = self.list.click(m.row, self.shown.len())
                {
                    return Some(true);
                }
                self.input.click(m.column);
            }
            _ => {}
        }
        None
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, ctx: &Ctx) {
        let th = &ctx.settings.theme;
        let p = self;
        let (title, placeholder) = match p.ask {
            Ask::Merge => (format!(" merge {} into ", p.name), "type to filter…"),
            Ask::Color => (format!(" colour of {} ", p.name), "a name, or #rrggbb"),
        };
        let package = (p.identity == Kind::Preset).then(|| p.preview_preset(None));
        let preview_height = package.as_ref().map_or(1, group::preset_card_height);
        let want =
            (p.choices.len().min(u16::MAX as usize) as u16).saturating_add(5 + preview_height);
        p.list.rows = Rect::default();
        p.input.clear_area();
        let size = popup_size(area, PopupClass::Choice, want, (4, 4));
        let rect = size.outer;
        p.rect = rect;
        f.render_widget(PopupSurface, rect);
        let block = th.block(title, true);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
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
        p.input.render(f, field, true, placeholder, th);
        // Preview the same identity shown on the page within the visible swatch.
        let swatch = Rect {
            y: inner.y + 1,
            height: preview_height.min(inner.height.saturating_sub(3)),
            ..field
        };
        let mut preview = vec![Span::raw("  ")];
        let preview_width = swatch.width.saturating_sub(2) as usize;
        match p.ask {
            Ask::Merge => {
                let label_width = preview_width.saturating_sub(3) / 2;
                let badge = |name: &str, width| {
                    group::Badge::for_kind(p.identity, name, p.group_fill(name, ctx))
                        .render(ctx, width)
                };
                preview.extend(badge(&p.name, label_width));
                let used = preview.iter().map(Span::width).sum::<usize>();
                let arrow = fit(" → ", (swatch.width as usize).saturating_sub(used));
                let remaining = (swatch.width as usize).saturating_sub(used + width(&arrow));
                preview.push(Span::styled(arrow, th.dim()));
                match p.chosen() {
                    Some(into) => preview.extend(badge(into, remaining)),
                    None => preview.push(Span::styled(fit("nothing matches", remaining), th.dim())),
                }
            }
            Ask::Color if p.identity == Kind::Preset => match p.color_text() {
                Some(color) => {
                    let preset = p.preview_preset(color);
                    let inside = if swatch.height >= 3 {
                        group::preset_frame(f, swatch, false, false, ctx)
                    } else {
                        swatch
                    };
                    f.render_widget(
                        Paragraph::new(group::fit_preset_card(
                            group::preset_card(&preset, ctx, inside.width as usize, None),
                            inside.height,
                        )),
                        inside,
                    );
                }
                None => preview.push(Span::styled(fit("not a colour", preview_width), th.warn())),
            },
            Ask::Color => match p.color() {
                Some(color) => preview.extend(
                    group::TagLabel::new(&p.name, color.unwrap_or(th.tag))
                        .render(ctx, preview_width),
                ),
                None => preview.push(Span::styled(fit("not a colour", preview_width), th.warn())),
            },
        }
        if p.ask == Ask::Merge || p.identity == Kind::Tag || p.color_text().is_none() {
            f.render_widget(Paragraph::new(Line::from(preview)), swatch);
        }
        let list_area = Rect {
            y: swatch.bottom() + 1,
            height: inner.bottom().saturating_sub(swatch.bottom() + 1),
            ..inner
        };
        p.list.rows = list_area;
        let row_width = list_area.width.saturating_sub(2) as usize;
        let rows: Vec<ListItem> = p
            .shown
            .iter()
            .map(|&i| {
                let name = p.choice(i);
                let spans = match p.ask {
                    Ask::Merge => group::Badge::for_kind(p.identity, name, p.group_fill(name, ctx))
                        .render(ctx, row_width),
                    Ask::Color if name == NO_COLOR => {
                        vec![Span::styled(fit("none — the default", row_width), th.dim())]
                    }
                    Ask::Color => {
                        let fill = group::color(Some(name), th);
                        let label = if i == p.choices.len() {
                            format!("  {name} (custom)")
                        } else {
                            format!("  {name}")
                        };
                        let suffix = fit(&label, row_width / 2);
                        let available = row_width.saturating_sub(width(&suffix));
                        let mut s = group::Badge::for_kind(p.identity, &p.name, fill)
                            .render(ctx, available);
                        s.push(Span::styled(suffix, th.dim()));
                        s
                    }
                };
                ListItem::new(Line::from(spans))
            })
            .collect();
        f.render_stateful_widget(
            List::new(rows)
                .highlight_style(th.selected())
                .highlight_symbol("▸ "),
            list_area,
            &mut p.list.state,
        );
        if p.shown.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("nothing matches", th.dim())),
                list_area,
            );
        }
    }

    fn new(ask: Ask, identity: Kind, name: &str, choices: Vec<String>) -> Self {
        let shown: Vec<usize> = (0..choices.len()).collect();
        let mut list = ListNav::default();
        list.clamp(shown.len());
        Self {
            ask,
            identity,
            name: name.to_string(),
            preset: None,
            input: Input::default(),
            choices,
            custom: None,
            shown,
            list,
            rect: Rect::default(),
        }
    }

    fn preview_preset(&self, color: Option<String>) -> Preset {
        let mut preset = self.preset.clone().unwrap_or_else(|| Preset {
            name: self.name.clone(),
            ..Preset::default()
        });
        preset.color = color;
        preset
    }

    fn group_fill(&self, name: &str, ctx: &Ctx) -> Color {
        if self.identity == Kind::Tag {
            tag_fill(name, ctx)
        } else {
            ctx.snap
                .presets
                .by_name
                .get(name)
                .map_or(ctx.settings.theme.tag, |preset| {
                    group::preset_fill(preset, ctx)
                })
        }
    }

    fn refilter(&mut self) {
        let q = self.input.value().trim().to_lowercase();
        self.shown = (0..self.choices.len())
            .filter(|&i| q.is_empty() || self.choices[i].to_lowercase().contains(&q))
            .collect();
        self.custom = None;
        if self.ask == Ask::Color {
            let exact = self
                .shown
                .iter()
                .position(|&i| self.choices[i].eq_ignore_ascii_case(&q));
            if let Some(i) = exact {
                self.list.select(Some(i));
            } else if self.input.value().trim().parse::<Color>().is_ok() {
                self.custom = Some(self.input.value().trim().to_string());
                self.shown.push(self.choices.len());
                self.list.select(Some(self.shown.len() - 1));
            }
        }
        self.list.clamp(self.shown.len());
    }

    fn choice(&self, i: usize) -> &str {
        if i == self.choices.len() {
            self.custom.as_deref().unwrap_or_default()
        } else {
            &self.choices[i]
        }
    }

    pub fn chosen(&self) -> Option<&str> {
        self.list
            .selected()
            .and_then(|i| self.shown.get(i))
            .map(|&i| self.choice(i))
    }

    /// The selected colour used by both preview and acceptance.
    /// `Some(None)` is "no colour".
    fn color(&self) -> Option<Option<Color>> {
        match self.color_text()? {
            Some(name) => name.parse::<Color>().ok().map(Some),
            None => Some(None),
        }
    }

    /// The selected candidate's text written to the config for `color()`.
    pub fn color_text(&self) -> Option<Option<String>> {
        match self.chosen() {
            Some(NO_COLOR) => Some(None),
            Some(name) => name.parse::<Color>().ok().map(|_| Some(name.to_string())),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(prompt: &mut Prompt, code: KeyCode) -> Option<bool> {
        prompt.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn draw(prompt: &mut Prompt) -> ratatui::buffer::Buffer {
        let tmp = skills::ops::DownloadDir::new("group-colour").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| prompt.draw(f, f.area(), &ctx)).unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn group_filter_geometry_stays_stable_and_selection_scrolls_after_resize() {
        let tmp = skills::ops::DownloadDir::new("group-filter-geometry").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        for identity in [Kind::Tag, Kind::Preset] {
            for ask in [Ask::Color, Ask::Merge] {
                let mut prompt = match ask {
                    Ask::Color => Prompt::for_color(identity, "example", None, &settings.theme),
                    Ask::Merge => Prompt::for_merge(
                        identity,
                        "example",
                        (0..30).map(|i| format!("target-{i:02}")).collect(),
                    ),
                };
                let one = if ask == Ask::Color {
                    "none"
                } else {
                    "target-29"
                };
                let mut previous = None;
                for (width, height) in [(100, 40), (60, 18), (100, 40)] {
                    let mut term =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                            .unwrap();
                    let mut geometry = None;
                    for (query, count) in [
                        ("", prompt.choices.len()),
                        (one, 1),
                        ("no-such-choice", 0),
                        ("", prompt.choices.len()),
                    ] {
                        prompt.input = Input::with_value(query);
                        prompt.refilter();
                        assert_eq!(prompt.shown.len(), count);
                        key(&mut prompt, KeyCode::PageDown);
                        term.draw(|f| prompt.draw(f, Rect::new(0, 0, width, height - 2), &ctx))
                            .unwrap();
                        let current = (
                            prompt.rect,
                            prompt.list.rows,
                            term.get_cursor_position().unwrap().y,
                        );
                        if let Some(expected) = geometry {
                            assert_eq!(current, expected, "{ask:?} {identity:?} {query}");
                        } else {
                            geometry = Some(current);
                        }
                        if let Some(selected) = prompt.list.selected() {
                            let offset = prompt.list.state.offset();
                            assert!(
                                selected >= offset
                                    && selected < offset + prompt.list.rows.height as usize
                            );
                            let y = prompt.list.rows.y + (selected - offset) as u16;
                            assert_eq!(
                                term.backend().buffer()[(prompt.list.rows.x, y)].symbol(),
                                "▸"
                            );
                        }
                    }
                    if let Some(before) = previous {
                        assert_ne!(geometry.unwrap().0, before);
                    }
                    previous = Some(geometry.unwrap().0);
                }
            }
        }
    }

    #[test]
    fn responsive_group_prompts_preserve_selected_color_and_visible_rows() {
        let tmp = skills::ops::DownloadDir::new("group-responsive").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        for identity in [Kind::Tag, Kind::Preset] {
            let mut prompt = Prompt::for_color(identity, "example", Some("red"), &Theme::default());
            let selected = prompt.color_text();
            for (width, height) in [(160, 40), (112, 32), (60, 20), (40, 12), (4, 3), (1, 1)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                let area = Rect::new(0, 0, width, height.saturating_sub(2));
                terminal
                    .draw(|frame| prompt.draw(frame, area, &ctx))
                    .unwrap();
                assert!(
                    prompt.list.rows.is_empty()
                        || prompt.list.rows.intersection(area) == prompt.list.rows
                );
                if width == 40 {
                    assert!(prompt.list.rows.height > 0);
                }
                if width < 4 {
                    assert_eq!(prompt.list.rows, Rect::default());
                }
                assert_eq!(prompt.color_text(), selected);
            }
        }
    }

    #[test]
    fn typed_named_colour_selects_exact_match_then_navigation_controls_acceptance() {
        for identity in [Kind::Tag, Kind::Preset] {
            let mut prompt =
                Prompt::for_color(identity, "test", Some("lightred"), &Theme::default());
            for c in "red".chars() {
                assert_eq!(key(&mut prompt, KeyCode::Char(c)), None);
            }
            assert_eq!(prompt.chosen(), Some("red"));
            assert_eq!(prompt.color(), Some(Some(Color::Red)));
            assert_eq!(key(&mut prompt, KeyCode::Down), None);
            assert_eq!(prompt.value(), "red");
            assert_eq!(prompt.chosen(), Some("lightred"));
            assert_eq!(prompt.color(), Some(Some(Color::LightRed)));
            assert_eq!(key(&mut prompt, KeyCode::Enter), Some(true));
            assert_eq!(prompt.color_text(), Some(Some("lightred".into())));
            key(&mut prompt, KeyCode::Up);
            assert_eq!(prompt.color_text(), Some(Some("red".into())));
        }
    }

    #[test]
    fn pasted_colour_reselects_exact_match_after_navigation() {
        let mut prompt = Prompt::for_color(Kind::Tag, "test", None, &Theme::default());
        prompt.paste("red");
        key(&mut prompt, KeyCode::Down);
        prompt.input.clear();
        prompt.paste(" RED ");
        assert_eq!(prompt.chosen(), Some("red"));
        assert_eq!(prompt.color_text(), Some(Some("red".into())));
    }

    #[test]
    fn custom_colour_is_a_visible_selected_candidate() {
        for identity in [Kind::Tag, Kind::Preset] {
            for (value, color) in [
                ("#b87e54", Color::Rgb(184, 126, 84)),
                ("0", Color::Indexed(0)),
                ("255", Color::Indexed(255)),
                ("reset", Color::Reset),
            ] {
                let mut prompt = Prompt::for_color(identity, "test", None, &Theme::default());
                prompt.paste(value);
                let buffer = draw(&mut prompt);
                let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                assert!(text.contains(&format!("{value} (custom)")));
                assert!(text.contains("▸ "));
                assert_eq!(prompt.chosen(), Some(value));
                assert_eq!(prompt.color(), Some(Some(color)));
                key(&mut prompt, KeyCode::Up);
                key(&mut prompt, KeyCode::Down);
                assert_eq!(key(&mut prompt, KeyCode::Enter), Some(true));
                assert_eq!(prompt.color_text(), Some(Some(value.into())));
            }
        }
    }

    #[test]
    fn mouse_selection_and_double_click_apply_highlighted_colour() {
        for identity in [Kind::Tag, Kind::Preset] {
            let mut prompt = Prompt::for_color(identity, "test", None, &Theme::default());
            prompt.paste("red");
            draw(&mut prompt);
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: prompt.list.rows.x + 2,
                row: prompt.list.rows.y + 1,
                modifiers: KeyModifiers::NONE,
            };
            assert_eq!(prompt.mouse(click), None);
            assert_eq!(prompt.chosen(), Some("lightred"));
            assert_eq!(prompt.color(), Some(Some(Color::LightRed)));
            assert_eq!(key(&mut prompt, KeyCode::Enter), Some(true));
            assert_eq!(prompt.color_text(), Some(Some("lightred".into())));
            assert_eq!(prompt.mouse(click), Some(true));
            assert_eq!(prompt.color_text(), Some(Some("lightred".into())));
        }
    }

    #[test]
    fn invalid_colour_clears_selection_and_preview_without_applying_stale_value() {
        for value in ["#12345z", "256", "not-a-colour"] {
            let mut prompt = Prompt::for_color(Kind::Tag, "test", None, &Theme::default());
            prompt.paste("#b87e54");
            prompt.input.clear();
            prompt.paste(value);
            key(&mut prompt, KeyCode::Down);
            let buffer = draw(&mut prompt);
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("not a colour"));
            assert!(text.contains("nothing matches"));
            assert_eq!(prompt.chosen(), None);
            assert_eq!(prompt.color(), None);
            assert_eq!(key(&mut prompt, KeyCode::Enter), Some(true));
            assert_eq!(prompt.color_text(), None);
        }
    }

    #[test]
    fn default_and_merge_choices_keep_their_meanings() {
        let mut prompt = Prompt::for_color(Kind::Tag, "test", Some("blue"), &Theme::default());
        prompt.paste("none");
        assert_eq!(prompt.color(), Some(None));
        assert_eq!(prompt.color_text(), Some(None));
        let mut merge = Prompt::for_merge(Kind::Tag, "test", vec!["lightred".into(), "red".into()]);
        merge.paste("red");
        assert_eq!(merge.chosen(), Some("lightred"));
        key(&mut merge, KeyCode::Down);
        assert_eq!(merge.chosen(), Some("red"));
        assert_eq!(key(&mut merge, KeyCode::Enter), Some(true));
        merge.input.clear();
        merge.paste("#b87e54");
        assert_eq!(merge.chosen(), None);
        assert!(merge.custom.is_none());
    }

    #[test]
    fn colour_picker_starts_at_current_or_default_colour() {
        for identity in [Kind::Tag, Kind::Preset] {
            let default = Prompt::for_color(identity, "test", None, &Theme::default());
            assert_eq!(default.color(), Some(None));
            assert_eq!(default.color_text(), Some(None));
            for current in ["blue", "#b87e54"] {
                let prompt = Prompt::for_color(identity, "test", Some(current), &Theme::default());
                assert_eq!(prompt.color_text(), Some(Some(current.into())));
            }
        }
    }

    #[test]
    fn colour_and_merge_prompts_survive_tiny_terminal_resize() {
        let tmp = skills::ops::DownloadDir::new("group-resize").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(tmp.path())
        .unwrap();
        let ws = skills::Workspace::open(tmp.path()).unwrap();
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
        for (ask, identity) in [
            (Ask::Color, Kind::Tag),
            (Ask::Color, Kind::Preset),
            (Ask::Merge, Kind::Tag),
        ] {
            let mut prompt = Prompt::new(ask, identity, "test", vec!["blue".into()]);
            for (width, height) in [(40, 12), (40, 11), (20, 8), (4, 3), (1, 1), (80, 24)] {
                let mut term =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                term.draw(|f| prompt.draw(f, f.area(), &ctx)).unwrap();
                assert!(prompt.rect.right() <= width && prompt.rect.bottom() <= height);
            }
        }
    }
}
