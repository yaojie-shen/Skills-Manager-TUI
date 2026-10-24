//! Exercise Library view-menu routing through the application, where global
//! shortcuts, tabs and the sync button could otherwise receive the same event.

use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn app() -> (skills::ops::DownloadDir, App) {
    let root = skills::ops::DownloadDir::new("library-view-menu").unwrap();
    Config {
        agents: vec![],
        ..Default::default()
    }
    .save(root.path())
    .unwrap();
    std::fs::create_dir_all(root.path().join("sample")).unwrap();
    std::fs::write(
        root.path().join("sample/SKILL.md"),
        "---\nname: sample\ndescription: Sample skill\n---\nBody",
    )
    .unwrap();
    let (tx, _) = std::sync::mpsc::channel();
    let app = App::new_with_launch_directory(
        Workspace::open(root.path()).unwrap(),
        tx,
        Some(root.path()),
    )
    .unwrap();
    (root, app)
}

fn draw(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle(Msg::Key(KeyEvent::new(code, modifiers)));
}

fn open_menu(app: &mut App) {
    key(app, KeyCode::F(2), KeyModifiers::NONE);
    assert!(app.search.view_menu_open());
    draw(app);
}

fn click(app: &mut App, rect: Rect, button: MouseButton) {
    app.handle(Msg::Mouse(MouseEvent {
        kind: MouseEventKind::Down(button),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    }));
}

#[test]
fn popup_background_is_neutral_footer_stays_active_and_close_restores_colors() {
    use ratatui::style::Color;

    for width in [60, 120, 200] {
        let (_root, mut app) = app();
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let before = terminal.backend().buffer().clone();
        key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let popup = app.search.view_menu_rect();
        let buffer = terminal.backend().buffer();
        for y in 0..29 {
            for x in 0..width {
                let cell = &buffer[(x, y)];
                if !popup.contains((x, y).into()) && !cell.symbol().trim().is_empty() {
                    assert_eq!(cell.fg, Color::Reset, "background at ({x}, {y})");
                    assert_eq!(cell.bg, Color::Reset, "background at ({x}, {y})");
                    assert!(cell.modifier.contains(Modifier::DIM));
                    assert!(!cell.modifier.contains(Modifier::BOLD));
                }
            }
        }
        assert_eq!(
            buffer[(popup.x, popup.y)].fg,
            app.settings.theme.border_focus
        );
        assert!(!buffer[(popup.x, popup.y)].modifier.contains(Modifier::DIM));
        assert!((0..width).any(|x| {
            let cell = &buffer[(x, 29)];
            cell.fg == app.settings.theme.accent
                && !cell.modifier.contains(Modifier::DIM)
                && !cell.symbol().trim().is_empty()
        }));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.backend().buffer(), &before);
    }
}

#[test]
fn app_popups_dim_page_overlays_and_restore_them_when_dismissed() {
    use ratatui::style::Color;

    let (_root, mut app) = app();
    open_menu(&mut app);
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let parent = terminal.backend().buffer().clone();
    let parent_corner = app.search.view_menu_rect().as_position();
    for modal in [
        Modal::new_preset(),
        Modal::message("Message", vec!["Body".into()]),
        Modal::confirm("Confirm".into(), vec![]),
    ] {
        app.modal = Some(modal);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let cell = &terminal.backend().buffer()[parent_corner];
        assert_eq!(cell.symbol(), parent[parent_corner].symbol());
        assert_eq!(cell.fg, Color::Reset);
        assert!(cell.modifier.contains(Modifier::DIM));
        app.apply(Action::CloseModal);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.backend().buffer(), &parent);
    }
}

#[test]
fn nested_skill_preview_dims_member_picker_buttons() {
    let (_root, mut app) = app();
    let ctx = Ctx {
        ws: &app.ws,
        snap: &app.snap,
        settings: &app.settings,
    };
    let mut picker = SearchView::preset_members("example", &ctx);
    picker.focus_list();
    app.modal = Some(Modal::PresetSkills(Box::new(picker)));
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let before = terminal.backend().buffer().clone();
    let cancel = before
        .content
        .iter()
        .enumerate()
        .find(|(i, _)| {
            before.content[*i..]
                .iter()
                .take(6)
                .map(|c| c.symbol())
                .collect::<String>()
                == "Cancel"
        })
        .map(|(i, _)| i)
        .expect("member picker Cancel button");
    key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
    let Some(Modal::PresetSkills(picker)) = &app.modal else {
        panic!("picker must stay open");
    };
    assert!(picker.overlay_open(), "o opens the nested skill preview");
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let cell = &terminal.backend().buffer().content[cancel];
    assert_eq!(cell.symbol(), "C");
    assert!(cell.modifier.contains(Modifier::DIM));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(terminal.backend().buffer(), &before);
}

#[test]
fn menu_open_from_input_contains_global_shortcuts_and_restores_search() {
    let (_root, mut app) = app();
    for letter in "sample".chars() {
        key(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    open_menu(&mut app);
    let tags_enabled = app.settings.tags_enabled;
    key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
    assert_eq!(app.settings.tags_enabled, tags_enabled);
    assert!(app.search.view_menu_open());
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(app.modal.is_none());
    assert!(app.search.view_menu_open());
    app.handle(Msg::Paste("ignored".into()));
    assert_eq!(app.search.query(), "sample");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.search.view_menu_open());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(
        app.search.query(),
        "samplze",
        "input focus, text and cursor survive the menu"
    );
}

#[test]
fn menu_navigation_applies_layout_live_and_shows_hints_only_in_the_app_footer() {
    use crate::tui::settings::LayoutScope;
    use skills::config::UiLayout;

    let (_root, mut app) = app();
    app.handle(Msg::Paste("sample".into()));
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    open_menu(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert!(app.search.view_menu_open());
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::List
    );
    assert!(draw(&mut app).contains("› ● List"));
    key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::Compact
    );
    app.handle(Msg::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 60,
        row: 15,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::List
    );
    app.handle(Msg::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::List,
        "wheel events outside the popup are contained and ignored"
    );
    assert!(app.search.view_menu_open());
    assert_eq!(app.settings.layout_for(LayoutScope::Tags), UiLayout::Grid);
    assert_eq!(app.ws.config.ui.layout, UiLayout::Grid);

    for width in [60, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let lines: Vec<String> = (0..30)
            .map(|y| {
                (0..width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect()
            })
            .collect();
        assert!(lines[29].contains("←→"), "{width} columns: {}", lines[29]);
        assert!(lines[..29].iter().all(|line| !line.contains("←→/Tab")));
        assert!(lines.iter().all(|line| !line.contains("Enter apply")));
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.search.view_menu_open());
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::List
    );
    key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(app.search.query(), "samplze");
}

#[test]
fn menu_outside_click_dismisses_without_switching_tabs_or_opening_sync() {
    let (_root, mut app) = app();
    draw(&mut app);
    let tags_tab = app
        .tab_rects
        .iter()
        .find(|(_, tab)| *tab == Tab::Tags)
        .unwrap()
        .0;
    open_menu(&mut app);
    click(&mut app, tags_tab, MouseButton::Left);
    assert_eq!(app.tab, Tab::Search);
    assert!(!app.search.view_menu_open());

    open_menu(&mut app);
    let sync_button = app.sync_button;
    assert!(sync_button.width > 0);
    click(&mut app, sync_button, MouseButton::Left);
    assert!(app.modal.is_none());
    assert!(!app.search.view_menu_open());
    assert_eq!(app.tab, Tab::Search);
}

#[test]
fn option_click_applies_live_and_enter_f2_close_without_rollback() {
    use crate::tui::settings::LayoutScope;
    use skills::config::UiLayout;

    let (_root, mut app) = app();
    open_menu(&mut app);
    let menu = app.search.view_menu_rect();
    click(
        &mut app,
        Rect::new(menu.x + 3, menu.y + 4, 1, 1),
        MouseButton::Left,
    );
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::List
    );
    assert!(app.search.view_menu_open());
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.search.view_menu_open());
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::List
    );

    open_menu(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::Compact
    );
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert!(!app.search.view_menu_open());
    assert_eq!(
        app.settings.layout_for(LayoutScope::Library),
        UiLayout::Compact
    );
}

#[test]
fn outside_right_and_middle_clicks_close_without_rollback_or_context_menu() {
    use crate::tui::settings::LayoutScope;
    use skills::config::UiLayout;

    for button in [MouseButton::Right, MouseButton::Middle] {
        let (_root, mut app) = app();
        open_menu(&mut app);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            app.settings.layout_for(LayoutScope::Library),
            UiLayout::List
        );

        click(&mut app, Rect::new(0, 0, 1, 1), button);

        assert!(!app.search.view_menu_open(), "{button:?} outside");
        assert_eq!(
            app.settings.layout_for(LayoutScope::Library),
            UiLayout::List,
            "{button:?} outside must not roll back the applied setting"
        );
        assert!(app.context_menu.is_none());
        assert!(app.modal.is_none());
        assert_eq!(app.tab, Tab::Search);
    }
}

#[test]
fn selection_count_stays_visible_ahead_of_content_verification_status() {
    let (_root, mut app) = app();
    app.snap.skills[0].status = skills::reconcile::SkillStatus::Unchecked;
    app.on_snapshot();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    for width in [60, 80, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let footer: String = (0..width)
            .map(|x| terminal.backend().buffer()[(x, 23)].symbol())
            .collect();
        assert!(footer.contains("1 selected"), "{width} columns: {footer}");
    }
}

#[test]
fn control_arrows_fold_results_without_affecting_the_selection() {
    let (_root, mut app) = app();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
    let collapsed = draw(&mut app);
    assert!(collapsed.contains("▸ Local"));
    assert!(!collapsed.contains("Sample skill"));
    assert!(collapsed.contains("1 selected"));
    key(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
    let expanded = draw(&mut app);
    assert!(expanded.contains("▾ Local"));
    assert!(expanded.contains("Sample skill"));
    assert!(expanded.contains("1 selected"));
    assert!(app.modal.is_none());
}

#[test]
fn control_arrows_in_search_input_keep_word_editing() {
    let (_root, mut app) = app();
    app.handle(Msg::Paste("sample sample".into()));
    key(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(app.search.query(), "sample zsample");
    assert!(app.search.input_focused());
    assert!(!app.search.view_menu_open());
}

#[test]
fn group_header_footer_and_actions_follow_selection_at_all_widths() {
    for width in [40, 60, 120] {
        for selecting in [false, true] {
            for collapsed in [false, true] {
                let (_root, mut app) = app();
                key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                if selecting {
                    key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
                    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
                }
                key(&mut app, KeyCode::Home, KeyModifiers::NONE);
                if collapsed {
                    key(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
                }
                let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
                terminal.draw(|f| app.draw(f)).unwrap();
                let footer: String = (0..width)
                    .map(|x| terminal.backend().buffer()[(x, 23)].symbol())
                    .collect();
                assert!(footer.contains("Esc"), "{width}: {footer}");
                assert!(
                    footer.contains("Space")
                        && (footer.contains("toggle") || footer.contains("collapse")),
                    "{width}: {footer}"
                );
                assert!(!footer.contains("preview") && !footer.contains("Space select"));
                if selecting {
                    assert!(
                        app.footer_hints()
                            .contains(&("Ctrl+A", "select all results"))
                    );
                    assert!(app.footer_hints().contains(&("a", "actions")));
                }
                for code in [KeyCode::Enter, KeyCode::Char(' ')] {
                    key(&mut app, code, KeyModifiers::NONE);
                    assert!(!app.search.overlay_open());
                    let screen = draw(&mut app);
                    assert_eq!(screen.contains("▸ Local"), !collapsed);
                    key(&mut app, code, KeyModifiers::NONE);
                }
                key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                assert_eq!(
                    app.focus,
                    if selecting {
                        AppFocus::Page
                    } else {
                        AppFocus::Tabs
                    }
                );
                if selecting {
                    assert!(!app.footer_hints().contains(&("Esc", "cancel selection")));
                }
            }
        }
    }
}

#[test]
fn completion_dims_app_and_split_badges_and_restores_parent() {
    use ratatui::style::Color;
    for tab in [Tab::Search, Tab::Tags, Tab::Presets] {
        for width in [60, 120] {
            let (_root, mut app) = app();
            app.ws.config.tags.push(skills::config::TagConfig {
                name: "work".into(),
                skills: vec!["sample".into()],
                color: Some("blue".into()),
                description: None,
            });
            app.snap.skills[0].tags = vec!["work".into()];
            app.snap.skills[0].presets = vec!["office".into()];
            app.snap.presets = skills::preset::PresetIndex::new(vec![skills::preset::Preset {
                name: "office".into(),
                skills: vec!["sample".into()],
                color: Some("green".into()),
                ..Default::default()
            }]);
            app.on_snapshot();
            app.apply(Action::SwitchTab(tab));
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            if tab != Tab::Search {
                key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                key(&mut app, KeyCode::Home, KeyModifiers::NONE);
                key(&mut app, KeyCode::Right, KeyModifiers::NONE);
                key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
            }
            app.handle(Msg::Paste("tag:".into()));
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let active = terminal.backend().buffer().clone();
            let text: String = active.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("filters"), "{tab:?}: {text}");
            // The Library header and the split-view classification caps are outside completion.
            for y in 0..27 {
                for x in 0..width {
                    let cell = &active[(x, y)];
                    if y == 0 {
                        assert_eq!(cell.fg, Color::Reset, "{tab:?} ({x},{y})");
                        assert_eq!(cell.bg, Color::Reset);
                        assert!(cell.modifier.contains(Modifier::DIM));
                        assert!(!matches!(cell.symbol(), "\u{e0b6}" | "\u{e0b4}"));
                    }
                }
            }
            assert!((0..width).any(|x| {
                let c = &active[(x, 29)];
                c.fg == app.settings.theme.accent && !c.modifier.contains(Modifier::DIM)
            }));
            assert!((0..27).any(|y| (0..width).any(|x| {
                let c = &active[(x, y)];
                c.symbol() == "›" && !c.modifier.contains(Modifier::DIM)
            })));
            // Filled classification badges outside the child lose their fills and caps.
            let mut badge_cells = 0;
            for cell in &active.content {
                if matches!(cell.symbol(), "(" | ")" | "/") && cell.modifier.contains(Modifier::DIM)
                {
                    badge_cells += 1;
                    assert_eq!(cell.fg, Color::Reset);
                    assert_eq!(cell.bg, Color::Reset);
                }
            }
            assert!(badge_cells > 0, "{tab:?}: expected classification badges");
            app.modal = Some(Modal::message("Higher overlay", vec!["Body".into()]));
            terminal.draw(|f| app.draw(f)).unwrap();
            let higher = terminal.backend().buffer();
            assert!(higher.content.iter().any(|c| {
                c.fg == app.settings.theme.border_focus && !c.modifier.contains(Modifier::DIM)
            }));
            app.apply(Action::CloseModal);
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(terminal.backend().buffer(), &active);
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            terminal.draw(|f| app.draw(f)).unwrap();
            let parent = terminal.backend().buffer().clone();
            assert!(!parent[(0, 0)].modifier.contains(Modifier::DIM));
            // Reopening and closing the child restores the exact same parent frame.
            key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            terminal.draw(|f| app.draw(f)).unwrap();
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(terminal.backend().buffer(), &parent);
        }
    }
}

#[test]
fn short_app_popup_geometry_explanation_leaves_keys_to_footer() {
    let (_root, mut app) = app();
    app.command_palette = Some(CommandPalette::default());
    let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let lines: Vec<String> = (0..5)
        .map(|y| {
            (0..40)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect()
        })
        .collect();
    assert!(lines[..4].iter().any(|line| line.contains("resize")));
    assert!(lines[..4].iter().all(|line| !line.contains("Esc")));
    assert!(lines[4].contains("Esc close"), "{}", lines[4]);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.command_palette.is_none());
}

#[test]
fn plain_f2_opens_library_view_from_tabs_and_restores_origin() {
    for width in [40, 60, 120] {
        for dismiss in [KeyCode::Esc, KeyCode::Enter, KeyCode::F(2)] {
            let (_root, mut app) = app();
            app.focus = AppFocus::Tabs;
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let before = terminal.backend().buffer().clone();
            key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
            assert!(app.search.view_menu_open());
            assert_eq!(app.focus, AppFocus::Tabs);
            terminal.draw(|f| app.draw(f)).unwrap();
            let footer: String = (0..width)
                .map(|x| terminal.backend().buffer()[(x, 23)].symbol())
                .collect();
            assert!(footer.contains("Esc"), "{width}: {footer}");
            assert!(!footer.contains("quit") && !footer.contains(": commands"));
            assert!((0..width).any(|x| {
                let c = &terminal.backend().buffer()[(x, 23)];
                c.fg == app.settings.theme.accent && !c.modifier.contains(Modifier::DIM)
            }));
            key(&mut app, dismiss, KeyModifiers::NONE);
            assert!(!app.search.view_menu_open());
            assert_eq!(app.focus, AppFocus::Tabs);
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(terminal.backend().buffer(), &before);
        }
    }
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        let (_root, mut app) = app();
        for tabs in [true, false] {
            app.focus = if tabs { AppFocus::Tabs } else { AppFocus::Page };
            app.search.focus_list();
            key(&mut app, KeyCode::F(2), modifiers);
            assert!(!app.search.view_menu_open(), "{modifiers:?}, tabs={tabs}");
        }
        app.search.focus_input();
        key(&mut app, KeyCode::F(2), modifiers);
        assert!(app.search.view_menu_open(), "retain modified input routing");
    }
    let (_root, mut shifted) = app();
    shifted.focus = AppFocus::Tabs;
    key(&mut shifted, KeyCode::F(2), KeyModifiers::SHIFT);
    assert!(!shifted.search.view_menu_open());
    shifted.focus = AppFocus::Page;
    key(&mut shifted, KeyCode::F(2), KeyModifiers::SHIFT);
    assert!(
        shifted.search.view_menu_open(),
        "retain existing shifted input routing"
    );
    for tab in [
        Tab::Tags,
        Tab::Presets,
        Tab::Agents,
        Tab::Repos,
        Tab::Health,
    ] {
        let (_root, mut app) = app();
        app.apply(Action::SwitchTab(tab));
        app.focus = AppFocus::Tabs;
        key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        assert!(!app.search.view_menu_open());
        assert_eq!(app.tab, tab);
    }
}

#[test]
fn f2_retains_library_input_results_preview_and_higher_overlay_routing() {
    for path in 0..3 {
        let (_root, mut app) = app();
        if path > 0 {
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        if path == 2 {
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        assert!(app.search.view_menu_open());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.search.view_menu_open());
        assert_eq!(app.focus, AppFocus::Page);
        assert_eq!(app.search.input_focused(), path == 0);
    }
    let (_root, mut app) = app();
    app.focus = AppFocus::Tabs;
    app.modal = Some(Modal::help());
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert!(!app.search.view_menu_open());
    app.modal = None;
    app.command_palette = Some(CommandPalette::default());
    key(&mut app, KeyCode::F(2), KeyModifiers::NONE);
    assert!(!app.search.view_menu_open());
    assert!(app.command_palette.is_some());
}
