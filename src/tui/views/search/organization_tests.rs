use super::grouped_grid::{Entry, GroupedGrid};
use super::organization::{GroupId, GroupMode, GroupOrder, Organized, SortMode, natural_cmp};
use super::{Focus, SearchView, SkillPanelOptions, ViewControl};
use crate::tui::app::{Action, Ctx};
use crate::tui::components::context_menu::{Command, Target};
use crate::tui::settings::{LayoutScope, RuntimeSettings};
use crate::tui::views::View;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use skills::config::{Config, Icons};
use skills::reconcile::{SkillRecord, SkillStatus};
use skills::repository::Repository;
use skills::search::Hit;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

fn record(key: &str, name: &str, tags: &[&str], presets: &[&str]) -> SkillRecord {
    SkillRecord {
        key: key.into(),
        path: key.into(),
        status: if key.starts_with("repos/") {
            SkillStatus::Repository
        } else {
            SkillStatus::Local
        },
        name: Some(name.into()),
        description: Some(format!("{name} description")),
        body: None,
        document: None,
        external: false,
        tags: tags.iter().map(|tag| (*tag).into()).collect(),
        presets: presets.iter().map(|preset| (*preset).into()).collect(),
        note: None,
        source: None,
        source_name: None,
        current_hash: None,
        baseline_hash: None,
        deploy: BTreeMap::new(),
        meta: None,
    }
}

fn hit(index: usize, key: &str, score: f32) -> Hit {
    Hit {
        key: key.into(),
        index,
        score,
        fields: Vec::new(),
        terms: Vec::new(),
        excerpt: None,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn workspace(label: &str) -> (skills::ops::DownloadDir, skills::Workspace) {
    let root = skills::ops::DownloadDir::new(label).unwrap();
    Config {
        agents: vec![],
        tags: vec![
            skills::config::TagConfig {
                name: "A".into(),
                skills: vec!["alpha".into()],
                color: None,
                description: None,
            },
            skills::config::TagConfig {
                name: "B".into(),
                skills: vec!["alpha".into(), "beta".into()],
                color: None,
                description: None,
            },
        ],
        ..Default::default()
    }
    .save(root.path())
    .unwrap();
    for name in ["alpha", "beta", "gamma"] {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} tools\n---\nBody"),
        )
        .unwrap();
    }
    let ws = skills::Workspace::open(root.path()).unwrap();
    ws.presets
        .save(&skills::preset::Preset {
            name: "Daily".into(),
            skills: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        })
        .unwrap();
    (root, ws)
}

fn clipping_workspace(label: &str) -> (skills::ops::DownloadDir, skills::Workspace) {
    let root = skills::ops::DownloadDir::new(label).unwrap();
    Config {
        agents: vec![],
        ..Config::default()
    }
    .save(root.path())
    .unwrap();
    for index in 0..19 {
        let name = format!("skill-{index:02}");
        let dir = root.path().join(&name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: shared {name} details\n---\nBody"),
        )
        .unwrap();
    }
    let ws = skills::Workspace::open(root.path()).unwrap();
    (root, ws)
}

fn render(view: &mut SearchView, ctx: &Ctx<'_>, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view.draw(frame, frame.area(), ctx))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn draw_buffer(
    view: &mut SearchView,
    ctx: &Ctx<'_>,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view.draw(frame, frame.area(), ctx))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn draw_results_buffer(
    view: &mut SearchView,
    ctx: &Ctx<'_>,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view.draw_results(frame, Rect::new(0, 0, width, height), ctx))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn buffer_row(buffer: &ratatui::buffer::Buffer, rect: Rect, y: u16) -> String {
    (rect.x..rect.right())
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

fn ordered_keys<'a>(
    organized: &Organized,
    hits: &[Hit],
    records: &'a [SkillRecord],
) -> Vec<&'a str> {
    organized
        .flat
        .iter()
        .map(|&hit_index| records[hits[hit_index].index].key.as_str())
        .collect()
}

#[test]
fn natural_name_and_path_sorts_are_case_insensitive_stable_and_overflow_safe() {
    let records = vec![
        record("z/tool-10", "Tool 10", &[], &[]),
        record("b/tool-2", "TOOL 2", &[], &[]),
        record("a/tool-2", "tool 2", &[], &[]),
        record(
            "huge-999999999999999999999999999999999999",
            "Huge 999999999999999999999999999999999999",
            &[],
            &[],
        ),
        record(
            "huge-1000000000000000000000000000000000000",
            "Huge 1000000000000000000000000000000000000",
            &[],
            &[],
        ),
        record("unicode-10", "工具 10", &[], &[]),
        record("unicode-2", "工具 2", &[], &[]),
    ];
    let hits: Vec<_> = records
        .iter()
        .enumerate()
        .map(|(index, record)| hit(index, &record.key, index as f32))
        .collect();
    let build = |sorting| {
        Organized::build(
            &hits,
            &records,
            &BTreeMap::new(),
            GroupMode::None,
            sorting,
            GroupOrder::Name,
            false,
            true,
        )
    };

    assert_eq!(
        ordered_keys(&build(SortMode::NameAsc), &hits, &records),
        [
            "huge-999999999999999999999999999999999999",
            "huge-1000000000000000000000000000000000000",
            "a/tool-2",
            "b/tool-2",
            "z/tool-10",
            "unicode-2",
            "unicode-10",
        ]
    );
    assert_eq!(
        ordered_keys(&build(SortMode::NameDesc), &hits, &records),
        [
            "unicode-10",
            "unicode-2",
            "z/tool-10",
            "a/tool-2",
            "b/tool-2",
            "huge-1000000000000000000000000000000000000",
            "huge-999999999999999999999999999999999999",
        ],
        "equal display names retain the stable key tie-break in descending order"
    );
    assert_eq!(
        ordered_keys(&build(SortMode::PathAsc), &hits, &records),
        [
            "a/tool-2",
            "b/tool-2",
            "huge-999999999999999999999999999999999999",
            "huge-1000000000000000000000000000000000000",
            "unicode-2",
            "unicode-10",
            "z/tool-10",
        ]
    );
    assert_eq!(natural_cmp("Éclair 2", "éCLAIR 10"), Ordering::Less);
}

#[test]
fn tag_and_preset_projection_duplicates_rows_but_keeps_unique_hit_counts() {
    let records = vec![
        record(
            "alpha",
            "Alpha",
            &["Blue", "Red", "Red"],
            &["Daily", "Team"],
        ),
        record("beta", "Beta", &["Red"], &[]),
        record("gamma", "Gamma", &[], &["Team"]),
        // Not matched: contributes only to the total side of Red's 2/3 count.
        record("delta", "Delta", &["Red"], &["Daily"]),
    ];
    let hits = vec![
        hit(0, "alpha", 3.0),
        hit(1, "beta", 2.0),
        hit(2, "gamma", 1.0),
    ];
    let tags = Organized::build(
        &hits,
        &records,
        &BTreeMap::new(),
        GroupMode::Tag,
        SortMode::NameAsc,
        GroupOrder::Count,
        false,
        true,
    );
    assert_eq!(hits.len(), 3, "search results remain the unique source set");
    assert_eq!(
        tags.groups
            .iter()
            .map(|group| (group.title.as_str(), group.hits.len(), group.total))
            .collect::<Vec<_>>(),
        [("Red", 2, 3), ("Blue", 1, 1), ("Untagged (no tags)", 1, 1)]
    );
    let alpha_appearances = tags
        .groups
        .iter()
        .flat_map(|group| &group.hits)
        .filter(|&&hit_index| hits[hit_index].key == "alpha")
        .count();
    assert_eq!(alpha_appearances, 2);

    let presets = Organized::build(
        &hits,
        &records,
        &BTreeMap::new(),
        GroupMode::Preset,
        SortMode::NameAsc,
        GroupOrder::Name,
        false,
        true,
    );
    assert_eq!(
        presets
            .groups
            .iter()
            .map(|group| (group.title.as_str(), group.hits.len(), group.total))
            .collect::<Vec<_>>(),
        [
            ("Daily", 1, 2),
            ("No preset (unassigned)", 1, 1),
            ("Team", 2, 2)
        ]
    );
}

#[test]
fn auto_modes_distinguish_text_search_from_filter_only_search() {
    assert_eq!(
        GroupMode::Auto.effective(false, true),
        GroupMode::Repository
    );
    assert_eq!(GroupMode::Auto.effective(true, true), GroupMode::None);
    assert_eq!(SortMode::Auto.effective(false), SortMode::NameAsc);
    assert_eq!(SortMode::Auto.effective(true), SortMode::Relevance);
    assert_eq!(
        GroupMode::Tag.effective(false, false),
        GroupMode::Repository
    );

    let records = vec![
        record("first", "First", &[], &[]),
        record("second", "Second", &[], &[]),
    ];
    let hits = vec![hit(0, "first", 1.0), hit(1, "second", 10.0)];
    let browsing = Organized::build(
        &hits,
        &records,
        &BTreeMap::new(),
        GroupMode::None,
        SortMode::Auto,
        GroupOrder::Name,
        false,
        true,
    );
    let text_search = Organized::build(
        &hits,
        &records,
        &BTreeMap::new(),
        GroupMode::None,
        SortMode::Auto,
        GroupOrder::Name,
        true,
        true,
    );
    assert_eq!(
        ordered_keys(&browsing, &hits, &records),
        ["first", "second"]
    );
    assert_eq!(
        ordered_keys(&text_search, &hits, &records),
        ["second", "first"]
    );
}

#[test]
fn repositories_with_the_same_display_name_stay_distinct() {
    let records = vec![
        record("repos/one/alpha", "Alpha", &[], &[]),
        record("repos/two/beta", "Beta", &[], &[]),
    ];
    let hits = vec![
        hit(0, "repos/one/alpha", 1.0),
        hit(1, "repos/two/beta", 1.0),
    ];
    let repositories = BTreeMap::from([
        (
            "one".into(),
            Repository {
                alias: "one".into(),
                name: Some("Shared".into()),
                url: "https://example.test/one.git".into(),
                kind: Default::default(),
                branch: String::new(),
            },
        ),
        (
            "two".into(),
            Repository {
                alias: "two".into(),
                name: Some("Shared".into()),
                url: "https://example.test/two.git".into(),
                kind: Default::default(),
                branch: String::new(),
            },
        ),
    ]);
    let organized = Organized::build(
        &hits,
        &records,
        &repositories,
        GroupMode::Repository,
        SortMode::NameAsc,
        GroupOrder::Name,
        false,
        true,
    );
    assert_eq!(organized.groups.len(), 2);
    assert_eq!(organized.groups[0].title, "Shared (one)");
    assert_eq!(organized.groups[1].title, "Shared (two)");
    assert_ne!(organized.groups[0].id, organized.groups[1].id);
    assert_eq!(organized.groups[0].id, GroupId::Repository("one".into()));
    assert_eq!(organized.groups[1].id, GroupId::Repository("two".into()));
}

#[test]
fn repository_name_order_pins_local_and_disambiguates_a_remote_named_local() {
    let records = vec![
        record("z-local", "Local skill", &[], &[]),
        record("repos/remote/alpha", "Alpha", &[], &[]),
        record("repos/earlier/beta", "Beta", &[], &[]),
    ];
    let hits = vec![
        hit(0, "z-local", 1.0),
        hit(1, "repos/remote/alpha", 1.0),
        hit(2, "repos/earlier/beta", 1.0),
    ];
    let repositories = BTreeMap::from([
        (
            "remote".into(),
            Repository {
                alias: "remote".into(),
                name: Some("Local".into()),
                url: "https://example.test/remote.git".into(),
                kind: Default::default(),
                branch: String::new(),
            },
        ),
        (
            "earlier".into(),
            Repository {
                alias: "earlier".into(),
                name: Some("Aardvark".into()),
                url: "https://example.test/earlier.git".into(),
                kind: Default::default(),
                branch: String::new(),
            },
        ),
    ]);
    let organized = Organized::build(
        &hits,
        &records,
        &repositories,
        GroupMode::Repository,
        SortMode::NameAsc,
        GroupOrder::Name,
        false,
        true,
    );
    assert_eq!(
        organized
            .groups
            .iter()
            .map(|group| (group.id.clone(), group.title.as_str()))
            .collect::<Vec<_>>(),
        [
            (GroupId::Local, "Local"),
            (GroupId::Repository("earlier".into()), "Aardvark"),
            (GroupId::Repository("remote".into()), "Local (remote)"),
        ]
    );
}

#[test]
fn sentinel_buckets_are_distinguishable_from_real_group_names() {
    let records = vec![
        record("unassigned", "Unassigned", &[], &[]),
        record(
            "named-like-sentinel",
            "Named",
            &["Untagged (no tags)"],
            &["No preset (unassigned)"],
        ),
    ];
    let hits = vec![
        hit(0, "unassigned", 1.0),
        hit(1, "named-like-sentinel", 1.0),
    ];
    for grouping in [GroupMode::Tag, GroupMode::Preset] {
        let organized = Organized::build(
            &hits,
            &records,
            &BTreeMap::new(),
            grouping,
            SortMode::NameAsc,
            GroupOrder::Name,
            false,
            true,
        );
        assert_eq!(organized.groups.len(), 2);
        assert_ne!(
            organized.groups[0].title, organized.groups[1].title,
            "a real group must not render exactly like its sentinel bucket"
        );
    }
}

#[test]
fn fully_collapsed_headers_keep_full_width_hit_targets_without_skill_selection() {
    use ratatui::layout::Rect;

    let records = vec![
        record("alpha", "Alpha", &["A"], &[]),
        record("beta", "Beta", &["B"], &[]),
    ];
    let hits = vec![hit(0, "alpha", 1.0), hit(1, "beta", 1.0)];
    let organized = Organized::build(
        &hits,
        &records,
        &BTreeMap::new(),
        GroupMode::Tag,
        SortMode::NameAsc,
        GroupOrder::Name,
        false,
        true,
    );
    let collapsed: BTreeSet<_> = organized
        .groups
        .iter()
        .map(|group| group.id.clone())
        .collect();
    let mut grid = GroupedGrid::default();
    grid.rebuild(&organized, &collapsed);
    grid.layout(Rect::new(4, 7, 37, 4), 3, 5, 1);

    assert_eq!(grid.entries.len(), 2);
    assert!(
        grid.entries
            .iter()
            .all(|entry| matches!(entry, Entry::Header { .. }))
    );
    assert_eq!(grid.first_skill(), None);
    assert_eq!(grid.selected_hit(), None);
    assert_eq!(grid.cell(0), Some(Rect::new(4, 7, 37, 1)));
    assert_eq!(grid.cell(1), Some(Rect::new(4, 8, 37, 1)));
    assert_eq!(
        grid.hit(40, 7),
        Some(0),
        "the entire heading row is clickable"
    );
}

#[test]
fn view_menu_contains_input_keys_scrolls_in_a_tiny_frame_and_accepts_mouse() {
    let (_root, ws) = workspace("organization-view-menu");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.set_query("alpha", &ctx);
    assert_eq!(view.focus, Focus::Input);

    view.handle_key(key(KeyCode::F(2)), &ctx);
    assert!(view.view_menu.is_open());
    for code in [KeyCode::Char(':'), KeyCode::Char('s')] {
        assert!(view.handle_key(key(code), &ctx).is_empty());
        assert!(view.view_menu.is_open());
        assert_eq!(view.query(), "alpha");
    }
    assert!(view.paste("-pasted", &ctx).is_empty());
    assert_eq!(
        view.query(),
        "alpha",
        "the open menu must contain paste input as well as key input"
    );
    let layout_screen = render(&mut view, &ctx, 64, 8);
    assert!(layout_screen.contains("Grid"), "{layout_screen}");
    assert!(layout_screen.contains("List"), "{layout_screen}");
    assert!(layout_screen.contains("Compact"), "{layout_screen}");
    assert!(!layout_screen.contains("Repository"));
    assert!(!layout_screen.contains("Name ↑"));
    assert!(!layout_screen.contains("Count ↓"));
    assert!(matches!(
        view.handle_key(key(KeyCode::Down), &ctx).as_slice(),
        [Action::SetLayout {
            scope: LayoutScope::Library,
            layout: skills::config::UiLayout::List,
        }]
    ));
    assert!(view.view_menu.is_open(), "live changes keep the menu open");
    view.handle_key(key(KeyCode::Enter), &ctx);
    assert!(
        !view.view_menu.is_open(),
        "keyboard choice applies within the active category"
    );
    assert_eq!(view.query(), "alpha");
    view.handle_key(key(KeyCode::Char('z')), &ctx);
    assert_eq!(view.query(), "alphaz", "closing restores the input cursor");

    view.set_query("", &ctx);
    view.handle_key(key(KeyCode::F(2)), &ctx);
    render(&mut view, &ctx, 64, 8);
    let menu = view.view_menu.rect;
    let group_tab_x = menu.x + 15;
    view.handle_mouse(click(group_tab_x, menu.y + 1), &ctx);
    assert_eq!(view.view_menu.section(), super::view_menu::Section::Group);
    let group_screen = render(&mut view, &ctx, 64, 8);
    assert!(group_screen.contains("Auto"));
    assert!(!group_screen.contains("Grid"));
    assert!(!group_screen.contains("Compact"));

    // The second visible Group choice is None; clicking it must not address a
    // hidden Layout/Sort row at the same display position.
    assert!(
        view.handle_mouse(click(menu.x + 3, menu.y + 3), &ctx)
            .is_empty()
    );
    assert_eq!(view.organization.grouping, GroupMode::None);
    assert!(
        view.view_menu.is_open(),
        "mouse changes apply without dismissing the settings popup"
    );
    view.handle_key(key(KeyCode::Enter), &ctx);
    assert!(!view.view_menu.is_open());

    view.handle_key(key(KeyCode::F(2)), &ctx);
    render(&mut view, &ctx, 64, 8);

    // In a clipped Group category, wheel input stays within Group choices.
    view.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: menu.x + 1,
            row: menu.y + 3,
            modifiers: KeyModifiers::NONE,
        },
        &ctx,
    );
    assert_eq!(view.organization.grouping, GroupMode::Repository);
    assert!(
        view.view_menu.is_open(),
        "wheel changes apply before confirmation"
    );
    assert!(view.handle_key(key(KeyCode::Enter), &ctx).is_empty());
    assert_eq!(view.organization.grouping, GroupMode::Repository);
    assert!(!view.view_menu.is_open());

    render(&mut view, &ctx, 120, 30);
    let view_button = view
        .view_buttons
        .iter()
        .find(|(_, control)| *control == ViewControl::View)
        .map(|(rect, _)| *rect)
        .unwrap();
    view.handle_mouse(click(view_button.x, view_button.y), &ctx);
    assert!(view.view_menu.is_open());
    assert_eq!(view.view_menu.section(), super::view_menu::Section::Group);
    view.handle_key(key(KeyCode::Right), &ctx);
    assert_eq!(view.view_menu.section(), super::view_menu::Section::Sort);
    view.handle_key(key(KeyCode::Tab), &ctx);
    assert_eq!(view.view_menu.section(), super::view_menu::Section::Order);
    let tiny = render(&mut view, &ctx, 12, 4);
    assert!(view.view_menu.rect.right() <= 12);
    assert!(view.view_menu.rect.bottom() <= 4);
    assert!(!tiny.contains("Grid"));
    assert!(!tiny.contains("Repository"));
    view.handle_key(key(KeyCode::Esc), &ctx);
    assert!(!view.view_menu.is_open());
}

#[test]
fn actual_view_menu_diff_repairs_wide_background_edges_and_closing_restores_them() {
    let (root, _) = workspace("organization-view-menu-wide-edges");
    for name in ["alpha", "beta", "gamma"] {
        let dir = root.path().join(name);
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: A{}\n---\nBody",
                "中文技能说明🙂".repeat(20)
            ),
        )
        .unwrap();
    }
    let ws = skills::Workspace::open(root.path()).unwrap();
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);

    let background = draw_buffer(&mut view, &ctx, 82, 24);
    let expected_menu = Rect::new(9, 6, 64, 11);
    let left_crossing_rows: Vec<_> = (expected_menu.top() + 1..expected_menu.bottom() - 1)
        .filter(|y| {
            background[(expected_menu.x, *y)].symbol() == " "
                && !background[(expected_menu.x.saturating_sub(1), *y)]
                    .symbol()
                    .is_ascii()
        })
        .collect();
    let right_crossing_rows: Vec<_> = (expected_menu.top() + 1..expected_menu.bottom() - 1)
        .filter(|y| {
            !background[(expected_menu.right() - 1, *y)]
                .symbol()
                .is_ascii()
                && background[(expected_menu.right(), *y)].symbol() == " "
        })
        .collect();
    assert!(
        !left_crossing_rows.is_empty() && !right_crossing_rows.is_empty(),
        "fixture must place wide glyphs across both future popup edges: left={left_crossing_rows:?}, right={right_crossing_rows:?}"
    );
    view.handle_key(key(KeyCode::F(2)), &ctx);
    let overlay = draw_buffer(&mut view, &ctx, 82, 24);
    let menu = view.view_menu.rect;
    assert_eq!(menu, expected_menu);
    let opening = background.diff(&overlay);
    for y in menu.top() + 1..menu.bottom() - 1 {
        assert!(
            opening
                .iter()
                .any(|(x, row, cell)| *x == menu.x && *row == y && cell.symbol() == "│"),
            "left border missing from actual SearchView diff at row {y}"
        );
        assert!(
            opening.iter().any(|(x, row, cell)| {
                *x == menu.right() - 1 && *row == y && cell.symbol() == "│"
            }),
            "right border missing from actual SearchView diff at row {y}"
        );
    }

    view.handle_key(key(KeyCode::Esc), &ctx);
    let restored = draw_buffer(&mut view, &ctx, 82, 24);
    let closing = overlay.diff(&restored);
    for y in left_crossing_rows {
        assert!(
            closing.iter().any(|(x, row, cell)| {
                *x == menu.x.saturating_sub(1) && *row == y && *cell == &background[(*x, *row)]
            }),
            "closing diff must restore the wide leading cell left of the popup at row {y}"
        );
    }
    for y in right_crossing_rows {
        assert!(
            closing.iter().any(|(x, row, cell)| {
                *x == menu.right() - 1
                    && *row == y
                    && *cell == &background[(menu.right() - 1, *row)]
            }),
            "closing diff must restore the wide leading cell crossing the popup's right edge at row {y}"
        );
    }
    assert_eq!(restored, background);
}

#[test]
fn actual_view_menu_keeps_common_geometry_across_categories_and_icon_modes() {
    let (_root, ws) = workspace("organization-view-menu-stable-geometry");
    let snap = ws.scan().unwrap();
    let mut settings = RuntimeSettings::new(&ws.config);
    settings.ui.icons = Icons::Nerd;
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.handle_key(key(KeyCode::F(2)), &ctx);

    for (width, height) in [(80, 24), (42, 16), (30, 8)] {
        let mut geometry = Vec::new();
        for _ in 0..4 {
            let buffer = draw_buffer(&mut view, &ctx, width, height);
            geometry.push((view.view_menu.rect, view.view_menu.first_row_y()));
            if width >= 64 {
                let rect = view.view_menu.rect;
                assert_eq!(rect.height, 11);
                assert_eq!(view.view_menu.first_row_y(), Some(rect.y + 3));
                let content = Rect::new(rect.x + 3, rect.y + 1, rect.width - 6, 1);
                assert!(
                    (content.x..content.right())
                        .any(|x| buffer[(x, content.y)].symbol() == "\u{f0570}"),
                    "Nerd mode should draw the Layout icon"
                );
                let screen: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                assert_eq!(
                    screen.contains("requires search"),
                    view.view_menu.section() == super::view_menu::Section::Sort
                );
            }
            view.handle_key(key(KeyCode::Right), &ctx);
        }
        assert!(
            geometry.windows(2).all(|pair| pair[0] == pair[1]),
            "{width}x{height} category geometry shifted: {geometry:?}"
        );
    }
}

#[test]
fn view_menu_applies_settings_live_and_preserves_library_state() {
    let (_root, ws) = workspace("organization-live-menu");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.focus_list();
    view.handle_key(key(KeyCode::Char('m')), &ctx);
    view.handle_key(key(KeyCode::Char(' ')), &ctx);
    let checked = view.checked.clone();
    let selected = view.selected(&ctx).unwrap().key.clone();
    view.handle_key(key(KeyCode::F(2)), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.grouping, GroupMode::None);
    assert!(view.organized.groups.is_empty());
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.grouping, GroupMode::Repository);
    assert!(!view.organized.groups.is_empty());
    assert_eq!(view.selected(&ctx).unwrap().key, selected);
    assert_eq!(view.checked, checked);

    assert!(view.view_menu.is_open());
    view.handle_key(key(KeyCode::Enter), &ctx);
    assert!(!view.view_menu.is_open());
    assert!(view.collapsed.is_empty());

    view.handle_key(key(KeyCode::F(2)), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.sorting, SortMode::NameAsc);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.sorting, SortMode::NameDesc);
    view.handle_key(key(KeyCode::End), &ctx);
    assert_eq!(
        view.organization.sorting,
        SortMode::PathDesc,
        "disabled Relevance is skipped"
    );
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.group_order, GroupOrder::Count);
    view.handle_key(key(KeyCode::Esc), &ctx);
    assert_eq!(
        view.organization.group_order,
        GroupOrder::Count,
        "closing does not revert live changes"
    );
    assert_eq!(view.checked, checked);
}

#[test]
fn flat_view_disables_group_order_but_restores_its_value_when_grouping_returns() {
    let (_root, ws) = workspace("organization-flat-order");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.organization.group_order = GroupOrder::Count;
    view.focus_list();
    view.handle_key(key(KeyCode::F(2)), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.grouping, GroupMode::None);
    assert_eq!(view.organization.group_order, GroupOrder::Count);

    view.handle_key(key(KeyCode::Right), &ctx);
    assert_eq!(view.view_menu.section(), super::view_menu::Section::Sort);
    view.handle_key(key(KeyCode::Right), &ctx);
    assert_eq!(
        view.view_menu.section(),
        super::view_menu::Section::Layout,
        "disabled Group order is skipped while the projection is flat"
    );

    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.organization.grouping, GroupMode::Repository);
    view.handle_key(key(KeyCode::Right), &ctx);
    view.handle_key(key(KeyCode::Right), &ctx);
    assert_eq!(view.view_menu.section(), super::view_menu::Section::Order);
    assert_eq!(view.organization.group_order, GroupOrder::Count);
    view.handle_key(key(KeyCode::Up), &ctx);
    assert_eq!(view.organization.group_order, GroupOrder::Name);
}

#[test]
fn header_keyboard_and_mouse_folding_preserve_original_skill_actions() {
    let (_root, ws) = workspace("organization-header-actions");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.organization.grouping = GroupMode::Tag;
    view.rebuild_organization(&ctx, None);
    view.focus_list();

    view.handle_key(key(KeyCode::Char('g')), &ctx);
    let first_group = match view.grouped.selected_entry().unwrap() {
        Entry::Header { group } => view.organized.groups[*group].id.clone(),
        Entry::Skill { .. } => panic!("g must reach the first heading"),
    };
    assert!(
        view.hints()
            .iter()
            .any(|(key, action)| *key == "Enter/Space" && action.contains("collapse"))
    );
    assert!(view.handle_key(key(KeyCode::Char('s')), &ctx).is_empty());
    view.handle_key(key(KeyCode::Char(' ')), &ctx);
    assert!(view.collapsed.contains(&first_group));
    view.handle_key(key(KeyCode::Char(' ')), &ctx);
    assert!(!view.collapsed.contains(&first_group));
    view.handle_key(key(KeyCode::Left), &ctx);
    assert!(view.collapsed.contains(&first_group));
    view.handle_key(key(KeyCode::Enter), &ctx);
    assert!(!view.collapsed.contains(&first_group));

    render(&mut view, &ctx, 120, 30);
    let header = view.grouped.cell(view.grouped.selected().unwrap()).unwrap();
    view.handle_mouse(click(header.right() - 1, header.y), &ctx);
    assert!(
        view.collapsed.contains(&first_group),
        "a click anywhere on the heading folds it"
    );
    render(&mut view, &ctx, 120, 30);
    let header = view.grouped.cell(view.grouped.selected().unwrap()).unwrap();
    view.handle_mouse(click(header.x + 1, header.y), &ctx);
    assert!(!view.collapsed.contains(&first_group));

    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.selected(&ctx).unwrap().key, "alpha");
    assert!(matches!(
        view.handle_key(key(KeyCode::Char('v')), &ctx).as_slice(),
        [Action::SetLayout {
            scope: LayoutScope::Library,
            ..
        }]
    ));
    assert!(matches!(
        view.handle_key(key(KeyCode::Char('s')), &ctx).as_slice(),
        [Action::OpenModal(_)]
    ));
    assert!(view.handle_key(key(KeyCode::Enter), &ctx).is_empty());
    assert!(view.overlay.is_open());
    view.handle_key(key(KeyCode::Esc), &ctx);
    assert!(!view.overlay.is_open());
    view.handle_key(key(KeyCode::Char('G')), &ctx);
    assert!(
        view.selected(&ctx).is_some(),
        "G reaches the last expanded skill"
    );
    view.handle_key(key(KeyCode::Char('/')), &ctx);
    assert_eq!(view.focus, Focus::Input);
}

#[test]
fn duplicate_occurrences_share_checks_and_ctrl_a_includes_collapsed_matches() {
    let (_root, ws) = workspace("organization-duplicate-checks");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.organization.grouping = GroupMode::Tag;
    view.rebuild_organization(&ctx, None);
    view.focus_list();
    view.handle_key(key(KeyCode::Char('m')), &ctx);

    let alpha_entries: Vec<_> = view
        .grouped
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            Entry::Skill { hit, .. } if view.hits[*hit].key == "alpha" => Some(index),
            _ => None,
        })
        .collect();
    assert_eq!(alpha_entries.len(), 2);
    view.grouped.select(Some(alpha_entries[0]));
    view.handle_key(key(KeyCode::Char(' ')), &ctx);
    assert_eq!(view.checked, BTreeSet::from(["alpha".into()]));
    view.grouped.select(Some(alpha_entries[1]));
    assert_eq!(view.selected(&ctx).unwrap().key, "alpha");
    view.handle_key(key(KeyCode::Char(' ')), &ctx);
    assert!(
        view.checked.is_empty(),
        "either occurrence toggles the one key state"
    );

    view.collapsed = view
        .organized
        .groups
        .iter()
        .map(|group| group.id.clone())
        .collect();
    view.rebuild_organization(&ctx, None);
    assert!(
        view.grouped
            .entries
            .iter()
            .all(|entry| matches!(entry, Entry::Header { .. }))
    );
    view.handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &ctx,
    );
    assert_eq!(
        view.checked,
        BTreeSet::from(["alpha".into(), "beta".into(), "gamma".into()])
    );
    assert_eq!(
        view.checked.len(),
        view.hits.len(),
        "duplicate rows do not inflate bulk targets"
    );

    view.refresh(&ctx);
    assert!(
        matches!(view.grouped.selected_entry(), Some(Entry::Header { .. })),
        "refreshing an all-collapsed view must leave a navigable heading selected"
    );
}

#[test]
fn collapse_expand_all_preserves_filter_counts_checks_and_selection_anchor() {
    let (_root, ws) = workspace("organization-fold-all");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.organization.grouping = GroupMode::Tag;
    view.set_query("tag:B", &ctx);
    view.focus_list();
    view.handle_key(key(KeyCode::Char('m')), &ctx);
    view.handle_key(key(KeyCode::Char(' ')), &ctx);

    let query = view.query();
    let hits: Vec<_> = view.hits.iter().map(|hit| hit.key.clone()).collect();
    let checked = view.checked.clone();
    let selected_group = match view.grouped.selected_entry().unwrap() {
        Entry::Header { group } => view.organized.groups[*group].id.clone(),
        Entry::Skill {
            group: Some(group), ..
        } => group.clone(),
        Entry::Skill { group: None, .. } => panic!("tag grouping must attach the selected skill"),
    };
    let current_groups: BTreeSet<_> = view
        .organized
        .groups
        .iter()
        .map(|group| group.id.clone())
        .collect();
    assert_eq!(hits, ["alpha", "beta"]);
    assert_eq!(checked, BTreeSet::from(["alpha".into()]));

    // This is intentionally before the first draw: fold actions cannot depend
    // on card geometry.
    view.set_all_groups_collapsed(true, &ctx);
    assert!(
        current_groups
            .iter()
            .all(|group| view.collapsed.contains(group))
    );
    let selected_header = match view.grouped.selected_entry().unwrap() {
        Entry::Header { group } => view.organized.groups[*group].id.clone(),
        Entry::Skill { .. } => panic!("the hidden skill must anchor to its own heading"),
    };
    assert_eq!(selected_header, selected_group);
    assert_eq!(view.query(), query);
    assert_eq!(
        view.hits
            .iter()
            .map(|hit| hit.key.clone())
            .collect::<Vec<_>>(),
        hits
    );
    assert_eq!(view.checked, checked);
    assert!(view.status(&ctx).contains("1 selected"));
    assert!(!view.control_enabled(ViewControl::CollapseAll));
    assert!(view.control_enabled(ViewControl::ExpandAll));

    let collapsed = view.collapsed.clone();
    view.set_all_groups_collapsed(true, &ctx);
    assert_eq!(view.collapsed, collapsed, "collapse-all is idempotent");
    assert_eq!(
        match view.grouped.selected_entry().unwrap() {
            Entry::Header { group } => &view.organized.groups[*group].id,
            Entry::Skill { .. } => panic!("idempotent collapse must retain the header"),
        },
        &selected_group
    );

    view.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL), &ctx);
    assert!(
        current_groups
            .iter()
            .all(|group| !view.collapsed.contains(group))
    );
    let expanded = view.collapsed.clone();
    view.set_all_groups_collapsed(false, &ctx);
    assert_eq!(view.collapsed, expanded, "expand-all is idempotent");
    assert_eq!(view.query(), query);
    assert_eq!(view.checked, checked);

    // Ctrl+Left/Right remain editing keys while the filter owns focus.
    view.focus_input();
    view.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL), &ctx);
    assert_eq!(view.query(), query);
    assert_eq!(view.collapsed, expanded);

    view.focus_list();
    render(&mut view, &ctx, 120, 30);
    assert!(
        view.view_buttons
            .iter()
            .all(|(rect, _)| rect.bottom() <= view.list_rect.y)
    );
    assert_eq!(
        view.view_buttons
            .iter()
            .map(|(_, control)| *control)
            .collect::<Vec<_>>(),
        [
            ViewControl::CollapseAll,
            ViewControl::ExpandAll,
            ViewControl::View,
        ]
    );
    let expand = view
        .view_buttons
        .iter()
        .find(|(_, control)| *control == ViewControl::ExpandAll)
        .unwrap()
        .0;
    assert!(!view.control_enabled(ViewControl::ExpandAll));
    view.handle_mouse(click(expand.x, expand.y), &ctx);
    assert_eq!(view.collapsed, expanded, "disabled header control is inert");
    let collapse = view
        .view_buttons
        .iter()
        .find(|(_, control)| *control == ViewControl::CollapseAll)
        .unwrap()
        .0;
    view.handle_mouse(click(collapse.x, collapse.y), &ctx);
    assert!(
        current_groups
            .iter()
            .all(|group| view.collapsed.contains(group))
    );
    render(&mut view, &ctx, 120, 30);
    let expand = view
        .view_buttons
        .iter()
        .find(|(_, control)| *control == ViewControl::ExpandAll)
        .unwrap()
        .0;
    view.handle_mouse(click(expand.x, expand.y), &ctx);
    assert!(
        current_groups
            .iter()
            .all(|group| !view.collapsed.contains(group))
    );

    let compact_header = render(&mut view, &ctx, 60, 20);
    assert!(compact_header.contains("▸ All"));
    assert!(compact_header.contains("▾ All"));
    assert!(compact_header.contains("View F2"));
    assert!(!compact_header.contains("Collapse all"));
    assert!(!compact_header.contains("Expand all"));

    for width in [36, 40] {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), &ctx))
            .unwrap();
        let narrow_header: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(
            view.view_buttons
                .iter()
                .map(|(_, control)| *control)
                .collect::<Vec<_>>(),
            [
                ViewControl::CollapseAll,
                ViewControl::ExpandAll,
                ViewControl::View,
            ],
            "outer width {width} must retain all fold controls"
        );
        assert!(narrow_header.contains('▸'));
        assert!(narrow_header.contains('▾'));
        assert!(narrow_header.contains(" View "));
        for (rect, control) in &view.view_buttons {
            assert_eq!(
                rect.y + rect.height,
                view.list_rect.y,
                "outer width {width} control hit rect must stay in the rendered header"
            );
            assert!(
                rect.right() <= width,
                "outer width {width} control hit rect must stay inside the frame"
            );
            if view.control_enabled(*control) {
                assert_eq!(
                    terminal.backend().buffer()[(rect.x, rect.y)].fg,
                    ratatui::style::Color::Reset,
                    "enabled border controls must not inherit the dim border foreground"
                );
            }
        }

        let collapse = view
            .view_buttons
            .iter()
            .find(|(_, control)| *control == ViewControl::CollapseAll)
            .unwrap()
            .0;
        view.handle_mouse(click(collapse.x, collapse.y), &ctx);
        assert!(
            current_groups
                .iter()
                .all(|group| view.collapsed.contains(group)),
            "outer width {width} collapse control must remain clickable"
        );
        render(&mut view, &ctx, width, 20);
        let expand = view
            .view_buttons
            .iter()
            .find(|(_, control)| *control == ViewControl::ExpandAll)
            .unwrap()
            .0;
        view.handle_mouse(click(expand.x, expand.y), &ctx);
        assert!(
            current_groups
                .iter()
                .all(|group| !view.collapsed.contains(group)),
            "outer width {width} expand control must remain clickable"
        );
    }
}

#[test]
fn embedded_panels_keep_the_flat_grid_and_ignore_library_view_controls() {
    let (_root, ws) = workspace("organization-embedded-panel");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut panel = SearchView::panel(
        SkillPanelOptions::new(
            vec!["alpha".into(), "beta".into()],
            "Tag: shared".into(),
            LayoutScope::Tags,
        ),
        &ctx,
    );
    assert!(panel.grouped.entries.is_empty());
    assert_eq!(panel.hits.len(), 2);
    assert!(panel.handle_key(key(KeyCode::F(2)), &ctx).is_empty());
    assert!(!panel.view_menu.is_open());
    let screen = render(&mut panel, &ctx, 100, 30);
    assert!(panel.view_buttons.is_empty());
    assert!(!screen.contains("Group:"));
    panel.focus_list();
    assert!(
        panel.hints().iter().all(|(key, _)| *key != "F2"),
        "embedded panels must not advertise an ignored Library-only control"
    );
    panel.handle_key(key(KeyCode::Char('m')), &ctx);
    assert!(
        panel.hints().iter().all(|(key, _)| *key != "Ctrl+←/→"),
        "embedded multi-select panels must not advertise Library fold controls"
    );
    panel.handle_key(key(KeyCode::Char('G')), &ctx);
    assert_eq!(panel.selected(&ctx).unwrap().key, "beta");
}

#[test]
fn context_execution_selects_the_requested_visible_key_and_rejects_collapsed_targets() {
    let (_root, ws) = workspace("organization-context-target");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.organization.grouping = GroupMode::Tag;
    view.rebuild_organization(&ctx, None);
    view.focus_list();
    assert_eq!(view.selected(&ctx).unwrap().key, "alpha");

    assert!(
        view.context_execute(&Target::Skill("beta".into()), Command::Open, &ctx)
            .is_empty()
    );
    assert_eq!(view.selected(&ctx).unwrap().key, "beta");
    assert!(view.overlay.is_open());
    view.overlay.close();

    view.collapsed.insert(GroupId::Tag("B".into()));
    view.rebuild_organization(&ctx, None);
    let before = view.selected(&ctx).map(|record| record.key.clone());
    let actions = view.context_execute(&Target::Skill("beta".into()), Command::Open, &ctx);
    assert!(
        matches!(actions.as_slice(), [Action::Error(message)] if message.contains("Target changed"))
    );
    assert_eq!(view.selected(&ctx).map(|record| record.key.clone()), before);
    assert!(!view.overlay.is_open());
}

#[test]
fn library_navigation_survives_pre_draw_rebuild_and_zero_size_resize() {
    let (_root, ws) = workspace("organization-pre-draw-lifecycle");
    let snap = ws.scan().unwrap();
    let settings = RuntimeSettings::new(&ws.config);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut view = SearchView::default();
    view.refresh(&ctx);
    view.focus_list();

    // Refresh builds a display projection before any frame has supplied rows
    // or columns. Navigation must still advance by display entry safely.
    assert_eq!(view.selected(&ctx).unwrap().key, "alpha");
    view.handle_key(key(KeyCode::Down), &ctx);
    assert_eq!(view.selected(&ctx).unwrap().key, "beta");

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| view.draw_results(frame, Rect::new(0, 0, 120, 24), &ctx))
        .unwrap();
    assert!(
        view.grouped
            .cell(view.grouped.selected().unwrap())
            .is_some()
    );

    // Changing the projection invalidates the old geometry. Page navigation
    // before the next draw must use the new entries rather than stale row maps.
    let anchor = view.selection_anchor();
    view.organization.grouping = GroupMode::Tag;
    view.rebuild_organization(&ctx, anchor);
    assert!(
        view.grouped
            .cell(view.grouped.selected().unwrap())
            .is_none()
    );
    view.handle_key(key(KeyCode::PageDown), &ctx);
    assert!(view.grouped.selected_entry().is_some());

    // A temporarily empty pane can occur during resize. It must clear hit
    // geometry, retain logical selection, and recover on the following frame.
    terminal
        .draw(|frame| view.draw_results(frame, Rect::new(0, 0, 0, 0), &ctx))
        .unwrap();
    let selected = view.grouped.selected_entry().cloned();
    assert!(selected.is_some());
    assert!(view.grouped.visible_entries().next().is_none());
    view.handle_key(key(KeyCode::Down), &ctx);
    assert!(view.grouped.selected_entry().is_some());
    terminal
        .draw(|frame| view.draw_results(frame, Rect::new(0, 0, 20, 3), &ctx))
        .unwrap();
    assert!(
        view.grouped
            .cell(view.grouped.selected().unwrap())
            .is_some()
    );
    terminal
        .draw(|frame| view.draw_results(frame, Rect::new(0, 0, 120, 24), &ctx))
        .unwrap();
    assert!(
        view.grouped
            .cell(view.grouped.selected().unwrap())
            .is_some()
    );
}

#[test]
fn actual_search_view_renders_grouped_and_flat_card_source_rows_after_top_clipping() {
    let (_root, ws) = clipping_workspace("organization-card-top-clipping");
    let snap = ws.scan().unwrap();
    let mut session = crate::tui::settings::SessionSettings::default();
    session.set_layout(LayoutScope::Library, skills::config::UiLayout::Grid);
    let settings = RuntimeSettings::resolve(&ws.config, &session);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };

    for grouping in [GroupMode::None, GroupMode::Repository] {
        let mut view = SearchView::default();
        view.refresh(&ctx);
        view.organization.grouping = grouping;
        view.rebuild_organization(&ctx, None);
        view.focus_list();

        let first = view.grouped.first_skill().unwrap();
        view.grouped.select(Some(first));
        let full = draw_results_buffer(&mut view, &ctx, 60, 30);
        let full_cell = view.grouped.cell(first).unwrap();
        assert_eq!(full_cell.height, ctx.settings.layout.card_height);
        let source_rows: Vec<_> = (full_cell.y..full_cell.bottom())
            .map(|y| buffer_row(&full, full_cell, y))
            .collect();

        let fourth = view
            .grouped
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry, Entry::Skill { .. }))
            .nth(3)
            .map(|(index, _)| index)
            .unwrap();
        view.grouped.select(Some(fourth));
        let clipped = draw_results_buffer(&mut view, &ctx, 60, 22);
        let clipped_first = view.grouped.cell(first).unwrap();
        let top_clip = view.grouped.top_clip(first);
        assert!(top_clip > 0);
        assert_eq!(
            (clipped_first.y..clipped_first.bottom())
                .map(|y| buffer_row(&clipped, clipped_first, y))
                .collect::<Vec<_>>(),
            source_rows[usize::from(top_clip)..]
        );

        let tail = draw_results_buffer(&mut view, &ctx, 60, 19);
        let tail_cell = view.grouped.cell(first).unwrap();
        assert_eq!(view.grouped.top_clip(first), 5);
        assert_eq!(tail_cell.height, 1);
        assert_eq!(
            buffer_row(&tail, tail_cell, tail_cell.y),
            source_rows[5],
            "{grouping:?} must render the logical bottom border, not a new top"
        );
        assert_eq!(tail[(tail_cell.x, tail_cell.y)].symbol(), "╰");
        assert_eq!(tail[(tail_cell.right() - 1, tail_cell.y)].symbol(), "╯");
        assert_eq!(
            view.grouped.click(tail_cell.x + 1, tail_cell.y).unwrap().0,
            first
        );

        view.grouped.select(Some(fourth));
        let bottom_open = draw_results_buffer(&mut view, &ctx, 60, 20);
        let partial_entry = view
            .grouped
            .visible_entries()
            .find(|&entry| {
                let cell = view.grouped.cell(entry).unwrap();
                matches!(view.grouped.entries[entry], Entry::Skill { .. })
                    && view.grouped.top_clip(entry) == 0
                    && cell.height < ctx.settings.layout.card_height
            })
            .unwrap();
        let partial = view.grouped.cell(partial_entry).unwrap();
        assert!(partial.height < ctx.settings.layout.card_height);
        assert_eq!(view.grouped.top_clip(partial_entry), 0);
        assert_eq!(bottom_open[(partial.x, partial.y)].symbol(), "╭");
        assert_ne!(bottom_open[(partial.x, partial.bottom() - 1)].symbol(), "╰");

        let adjacent = view
            .grouped
            .visible_entries()
            .find(|&entry| {
                let cell = view.grouped.cell(entry).unwrap();
                matches!(view.grouped.entries[entry], Entry::Skill { .. })
                    && view.grouped.top_clip(entry) == 0
                    && cell.height == ctx.settings.layout.card_height
            })
            .unwrap();
        let adjacent_cell = view.grouped.cell(adjacent).unwrap();
        assert_eq!(
            bottom_open[(adjacent_cell.x, adjacent_cell.y)].symbol(),
            "╭"
        );
    }
}

#[test]
fn actual_search_view_list_and_compact_rows_do_not_restart_after_top_clipping() {
    let (_root, ws) = clipping_workspace("organization-density-top-clipping");
    let snap = ws.scan().unwrap();

    for layout in [
        skills::config::UiLayout::List,
        skills::config::UiLayout::Compact,
    ] {
        let mut session = crate::tui::settings::SessionSettings::default();
        session.set_layout(LayoutScope::Library, layout);
        let settings = RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        view.organization.grouping = GroupMode::None;
        view.set_query("shared", &ctx);
        view.focus_list();

        let first = view.grouped.first_skill().unwrap();
        view.grouped.select(Some(first));
        let full = draw_results_buffer(&mut view, &ctx, 60, 30);
        let full_cell = view.grouped.cell(first).unwrap();
        let source_rows: Vec<_> = (full_cell.y..full_cell.bottom())
            .map(|y| buffer_row(&full, full_cell, y))
            .collect();

        let selected = view
            .grouped
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry, Entry::Skill { .. }))
            .nth(if layout == skills::config::UiLayout::List {
                3
            } else {
                8
            })
            .map(|(index, _)| index)
            .unwrap();
        view.grouped.select(Some(selected));
        let height = if layout == skills::config::UiLayout::List {
            18
        } else {
            17
        };
        let clipped = draw_results_buffer(&mut view, &ctx, 60, height);
        let clipped_first = view.grouped.cell(first).unwrap();
        let top_clip = view.grouped.top_clip(first);
        assert!(top_clip > 0, "{layout:?}");
        assert_eq!(
            (clipped_first.y..clipped_first.bottom())
                .map(|y| buffer_row(&clipped, clipped_first, y))
                .collect::<Vec<_>>(),
            source_rows[usize::from(top_clip)..],
            "{layout:?} must preserve the logical source row"
        );
    }
}

#[test]
fn actual_search_view_mouse_uses_logical_rows_for_clipped_markers_and_list_separator() {
    let (_root, ws) = clipping_workspace("organization-clipped-mouse-rows");
    let snap = ws.scan().unwrap();

    let mut session = crate::tui::settings::SessionSettings::default();
    session.set_layout(LayoutScope::Library, skills::config::UiLayout::Grid);
    let settings = RuntimeSettings::resolve(&ws.config, &session);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut grid = SearchView::default();
    grid.refresh(&ctx);
    grid.organization.grouping = GroupMode::None;
    grid.rebuild_organization(&ctx, None);
    grid.focus_list();
    draw_results_buffer(&mut grid, &ctx, 60, 18);
    grid.list_rect = Rect::new(0, 0, 60, 18);

    grid.grouped.scroll_to(1);
    let first = grid.grouped.first_skill().unwrap();
    let cell = grid.grouped.cell(first).unwrap();
    assert_eq!(grid.grouped.top_clip(first), 1);
    let key = grid.selected(&ctx).unwrap().key.clone();
    grid.handle_mouse(click(cell.x + 2, cell.y), &ctx);
    assert_eq!(grid.checked, BTreeSet::from([key]));

    grid.clear_selection();
    grid.grouped.scroll_to(1);
    let cell = grid.grouped.cell(first).unwrap();
    grid.handle_mouse(click(cell.x + 2, cell.y + 1), &ctx);
    assert!(
        grid.checked.is_empty(),
        "the description row below a shifted Grid marker must not toggle it"
    );

    grid.clear_selection();
    grid.grouped.scroll_to(2);
    let cell = grid.grouped.cell(first).unwrap();
    assert_eq!(grid.grouped.top_clip(first), 2);
    grid.handle_mouse(click(cell.x + 2, cell.y + 1), &ctx);
    assert!(
        grid.checked.is_empty(),
        "a fully clipped Grid marker must not retain a hotspot"
    );

    for layout in [
        skills::config::UiLayout::List,
        skills::config::UiLayout::Compact,
    ] {
        for top_clip in [0, 1] {
            let mut session = crate::tui::settings::SessionSettings::default();
            session.set_layout(LayoutScope::Library, layout);
            let settings = RuntimeSettings::resolve(&ws.config, &session);
            let ctx = Ctx {
                ws: &ws,
                snap: &snap,
                settings: &settings,
            };
            let mut view = SearchView::default();
            view.refresh(&ctx);
            view.organization.grouping = GroupMode::None;
            view.rebuild_organization(&ctx, None);
            view.focus_list();
            if layout == skills::config::UiLayout::Compact {
                view.set_query("shared", &ctx);
            }
            draw_results_buffer(&mut view, &ctx, 60, 18);
            view.list_rect = Rect::new(0, 0, 60, 18);
            view.grouped.scroll_to(top_clip);
            let first = view.grouped.first_skill().unwrap();
            let cell = view.grouped.cell(first).unwrap();
            assert_eq!(usize::from(view.grouped.top_clip(first)), top_clip);
            let marker_x = cell.x
                + if layout == skills::config::UiLayout::List {
                    1
                } else {
                    2
                };
            let key = view.selected(&ctx).unwrap().key.clone();
            view.handle_mouse(click(marker_x, cell.y), &ctx);
            if top_clip == 0 {
                assert_eq!(view.checked, BTreeSet::from([key]));
            } else {
                assert!(view.checked.is_empty());
            }
        }
    }

    let mut session = crate::tui::settings::SessionSettings::default();
    session.set_layout(LayoutScope::Library, skills::config::UiLayout::List);
    let settings = RuntimeSettings::resolve(&ws.config, &session);
    let ctx = Ctx {
        ws: &ws,
        snap: &snap,
        settings: &settings,
    };
    let mut list = SearchView::default();
    list.refresh(&ctx);
    list.organization.grouping = GroupMode::None;
    list.rebuild_organization(&ctx, None);
    list.focus_list();
    draw_results_buffer(&mut list, &ctx, 60, 18);
    list.list_rect = Rect::new(0, 0, 60, 18);
    list.grouped.scroll_to(2);
    let first = list.grouped.first_skill().unwrap();
    let cell = list.grouped.cell(first).unwrap();
    assert_eq!(list.grouped.top_clip(first), 2);
    let second = list
        .grouped
        .entries
        .iter()
        .enumerate()
        .find(|(index, entry)| {
            *index != first
                && matches!(entry, Entry::Skill { .. })
                && list.grouped.cell(*index).is_some()
        })
        .map(|(index, _)| index)
        .unwrap();
    list.grouped.select(Some(second));
    let selected = list.selected(&ctx).unwrap().key.clone();
    let checked = list.checked.clone();
    let preview_scroll = list.preview_scroll;
    let overlay_open = list.overlay.is_open();
    let separator_y = cell.y + 4 - list.grouped.top_clip(first);
    for _ in 0..2 {
        list.handle_mouse(click(cell.x + 3, separator_y), &ctx);
    }
    assert_eq!(list.selected(&ctx).unwrap().key, selected);
    assert_eq!(list.checked, checked);
    assert_eq!(list.preview_scroll, preview_scroll);
    assert_eq!(list.overlay.is_open(), overlay_open);
}

#[test]
fn library_page_navigation_resets_scrolled_split_preview_for_new_skill() {
    let root = skills::ops::DownloadDir::new("organization-page-preview-reset").unwrap();
    for index in 0..32 {
        let name = format!("skill-{index:02}");
        let dir = root.path().join(&name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {name} tools\n---\n{}",
                (0..80)
                    .map(|line| format!("{name} body line {line:03}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
    }
    let ws = skills::Workspace::open(root.path()).unwrap();
    let snap = ws.scan().unwrap();

    let cases = [
        (
            skills::config::UiLayout::List,
            GroupMode::None,
            key(KeyCode::PageDown),
            false,
        ),
        (
            skills::config::UiLayout::Compact,
            GroupMode::Repository,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            false,
        ),
        (
            skills::config::UiLayout::List,
            GroupMode::Repository,
            key(KeyCode::PageUp),
            true,
        ),
        (
            skills::config::UiLayout::Compact,
            GroupMode::None,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            true,
        ),
    ];

    for (layout, grouping, page_key, from_end) in cases {
        let mut session = crate::tui::settings::SessionSettings::default();
        session.set_layout(LayoutScope::Library, layout);
        let settings = RuntimeSettings::resolve(&ws.config, &session);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        view.organization.grouping = grouping;
        view.rebuild_organization(&ctx, None);
        if from_end {
            view.grouped.last();
        } else {
            view.grouped.select(view.grouped.first_skill());
        }
        view.focus_list();

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), &ctx))
            .unwrap();
        let previous = view.selected(&ctx).unwrap().key.clone();

        view.handle_key(key(KeyCode::Right), &ctx);
        assert_eq!(view.focus, Focus::Preview);
        view.handle_key(key(KeyCode::PageDown), &ctx);
        assert!(
            view.preview_scroll > 0,
            "{layout:?} {grouping:?} must reach a nonzero preview scroll"
        );
        view.handle_key(key(KeyCode::Esc), &ctx);
        assert_eq!(view.focus, Focus::List);

        view.handle_key(page_key, &ctx);
        let selected = view.selected(&ctx).unwrap().key.clone();
        assert_ne!(
            selected, previous,
            "{layout:?} {grouping:?} page navigation must select another skill"
        );
        assert_eq!(
            view.preview_scroll, 0,
            "{layout:?} {grouping:?} must reset the new skill preview"
        );

        terminal
            .draw(|frame| view.draw(frame, frame.area(), &ctx))
            .unwrap();
        let first_preview_row = (view.preview_rect.x + 1..view.preview_rect.right() - 2)
            .map(|x| terminal.backend().buffer()[(x, view.preview_rect.y + 1)].symbol())
            .collect::<String>();
        assert!(
            first_preview_row.contains(&selected),
            "{layout:?} {grouping:?} preview must restart at metadata for {selected}: \
             {first_preview_row:?}"
        );
    }
}
