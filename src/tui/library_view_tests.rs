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
        assert!(
            lines[29].contains("←→/Tab"),
            "{width} columns: {}",
            lines[29]
        );
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
