//! Resolved settings for one UI snapshot.
//!
//! `Config` owns the file format and its user-editable defaults. This module adds
//! application defaults and session overrides, without reading files or knowing
//! about pages. The app resolves settings when configuration changes; components
//! consume these values instead of choosing their own defaults.

use super::theme::Theme;
use skills::config::{Config, Icons, PillCaps, SearchConfig, UiLayout};
use std::time::Duration;

/// Layout preferences have an explicit scope. Fixed-layout controls such as a
/// selection dialog use a component constraint, rather than changing a page's
/// preference or introducing another session override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutScope {
    Library,
    Tags,
    Presets,
    Agents,
    Repositories,
}

impl LayoutScope {
    fn index(self) -> usize {
        match self {
            Self::Library => 0,
            Self::Tags => 1,
            Self::Presets => 2,
            Self::Agents => 3,
            Self::Repositories => 4,
        }
    }
}

/// Only user preferences belong here. Queries, cursors and selection state
/// remain with the controls that own them. Nothing in this type is persisted.
#[derive(Debug, Clone, Default)]
pub struct SessionSettings {
    layouts: [Option<UiLayout>; 5],
}

impl SessionSettings {
    pub fn set_layout(&mut self, scope: LayoutScope, layout: UiLayout) {
        self.layouts[scope.index()] = Some(layout);
    }
}

/// Resolved settings shared by every component. Layout intentionally has no
/// field here: `RuntimeSettings::layout_for` is its only read interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiSettings {
    pub icons: Icons,
    pub pill_caps: PillCaps,
}

/// Shared display geometry. These are application defaults, not new TOML keys.
#[derive(Debug, Clone, Copy)]
pub struct LayoutMetrics {
    pub min_card_width: u16,
    pub card_height: u16,
    pub marker_width: usize,
    pub compact_name_width: usize,
    pub pane_breakpoint: u16,
    pub stacked_pane_percent: u16,
    pub stacked_pane_max_height: u16,
    pub narrow_header_width: u16,
    pub compact_header_width: u16,
}

impl Default for LayoutMetrics {
    fn default() -> Self {
        Self {
            min_card_width: 40,
            card_height: 6,
            marker_width: 4,
            compact_name_width: 26,
            pane_breakpoint: 90,
            stacked_pane_percent: 45,
            stacked_pane_max_height: 14,
            narrow_header_width: 70,
            compact_header_width: 90,
        }
    }
}

/// User-facing timing and navigation policy. Low-level I/O retry timings are
/// implementation details and remain in the modules that perform that I/O.
#[derive(Debug, Clone, Copy)]
pub struct InteractionSettings {
    pub wheel_rows: i32,
    pub toast_lifetime: Duration,
    pub error_toast_lifetime: Duration,
    pub max_visible_toasts: usize,
    pub tick_interval: Duration,
    pub root_poll_interval: Duration,
}

impl Default for InteractionSettings {
    fn default() -> Self {
        Self {
            wheel_rows: 3,
            toast_lifetime: Duration::from_secs(5),
            error_toast_lifetime: Duration::from_secs(20),
            max_visible_toasts: 3,
            tick_interval: Duration::from_millis(100),
            root_poll_interval: Duration::from_secs(2),
        }
    }
}

/// Resolved automatic-sync waits, shared by UI scheduling and sync policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSettings {
    pub quiet_window: Duration,
    pub tui_idle_window: Duration,
}

impl From<skills::config::SyncConfig> for SyncSettings {
    fn from(config: skills::config::SyncConfig) -> Self {
        Self {
            quiet_window: Duration::from_secs(config.quiet_seconds),
            tui_idle_window: Duration::from_secs(config.tui_idle_seconds),
        }
    }
}

impl Default for SyncSettings {
    fn default() -> Self {
        skills::config::SyncConfig::default().into()
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub theme: Theme,
    pub ui: UiSettings,
    pub search: SearchConfig,
    pub tags_enabled: bool,
    pub layout: LayoutMetrics,
    pub interaction: InteractionSettings,
    pub sync: SyncSettings,
    layouts: [UiLayout; 5],
}

impl RuntimeSettings {
    pub fn new(config: &Config) -> Self {
        Self::resolve(config, &SessionSettings::default())
    }

    /// Precedence is application defaults, then file values, then explicit
    /// session preferences. Tags and presets remain workspace data and are not
    /// copied into the settings snapshot.
    pub fn resolve(config: &Config, session: &SessionSettings) -> Self {
        Self {
            theme: Theme::default(),
            ui: UiSettings {
                icons: config.ui.icons,
                pill_caps: config.ui.pill_caps,
            },
            search: config.search.clone(),
            tags_enabled: config.tags_enabled,
            layout: LayoutMetrics::default(),
            interaction: InteractionSettings::default(),
            sync: config.sync.into(),
            layouts: session
                .layouts
                .map(|layout| layout.unwrap_or(config.ui.layout)),
        }
    }

    /// Rebuild with the latest file values while retaining the app's session
    /// preferences. There is no second cache or file-reading path here.
    pub fn reload(&mut self, config: &Config, session: &SessionSettings) {
        *self = Self::resolve(config, session);
    }

    pub fn layout_for(&self, scope: LayoutScope) -> UiLayout {
        self.layouts[scope.index()]
    }
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self::new(&Config::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skills::config::UiConfig;

    #[test]
    fn sync_waits_resolve_and_reload_from_config() {
        let mut config = Config::default();
        let session = SessionSettings::default();
        let mut settings = RuntimeSettings::resolve(&config, &session);
        assert_eq!(settings.sync.quiet_window, Duration::from_secs(120));
        assert_eq!(settings.sync.tui_idle_window, Duration::from_secs(10));

        config.sync.quiet_seconds = 45;
        config.sync.tui_idle_seconds = 3;
        settings.reload(&config, &session);
        assert_eq!(settings.sync.quiet_window, Duration::from_secs(45));
        assert_eq!(settings.sync.tui_idle_window, Duration::from_secs(3));

        config.sync.quiet_seconds = 0;
        config.sync.tui_idle_seconds = 0;
        settings.reload(&config, &session);
        assert_eq!(settings.sync.quiet_window, Duration::ZERO);
        assert_eq!(settings.sync.tui_idle_window, Duration::ZERO);
    }

    #[test]
    fn file_values_apply_to_every_scope_without_session_overrides() {
        let config = Config {
            ui: UiConfig {
                layout: UiLayout::List,
                icons: Icons::Text,
                pill_caps: PillCaps::Block,
            },
            search: SearchConfig {
                fuzzy: false,
                prefix: false,
                ..SearchConfig::default()
            },
            tags_enabled: false,
            ..Config::default()
        };
        let settings = RuntimeSettings::new(&config);

        for scope in [
            LayoutScope::Library,
            LayoutScope::Tags,
            LayoutScope::Presets,
            LayoutScope::Agents,
            LayoutScope::Repositories,
        ] {
            assert_eq!(settings.layout_for(scope), UiLayout::List);
        }
        assert_eq!(settings.ui.icons, config.ui.icons);
        assert_eq!(settings.ui.pill_caps, config.ui.pill_caps);
        assert_eq!(settings.search, config.search);
        assert!(!settings.tags_enabled);
    }

    #[test]
    fn session_override_is_scoped_and_a_new_session_resumes_file_default() {
        let config = Config::default();
        let mut session = SessionSettings::default();
        session.set_layout(LayoutScope::Tags, UiLayout::Compact);
        let mut settings = RuntimeSettings::resolve(&config, &session);

        assert_eq!(settings.layout_for(LayoutScope::Tags), UiLayout::Compact);
        assert_eq!(settings.layout_for(LayoutScope::Library), UiLayout::Grid);
        assert_eq!(settings.layout_for(LayoutScope::Presets), UiLayout::Grid);
        assert_eq!(settings.layout_for(LayoutScope::Agents), UiLayout::Grid);
        assert_eq!(
            settings.layout_for(LayoutScope::Repositories),
            UiLayout::Grid
        );
        assert_eq!(config.ui.layout, UiLayout::Grid);

        session = SessionSettings::default();
        settings.reload(&config, &session);
        assert_eq!(settings.layout_for(LayoutScope::Tags), UiLayout::Grid);
    }

    #[test]
    fn reload_updates_file_settings_and_keeps_only_explicit_overrides() {
        let mut config = Config::default();
        let mut session = SessionSettings::default();
        // An explicit choice equal to the initial default still survives reload.
        session.set_layout(LayoutScope::Library, UiLayout::Grid);
        session.set_layout(LayoutScope::Agents, UiLayout::Compact);
        let mut settings = RuntimeSettings::resolve(&config, &session);

        config.ui.layout = UiLayout::List;
        config.ui.icons = Icons::Text;
        config.search.dictionary = false;
        settings.reload(&config, &session);

        assert_eq!(settings.layout_for(LayoutScope::Library), UiLayout::Grid);
        assert_eq!(settings.layout_for(LayoutScope::Agents), UiLayout::Compact);
        assert_eq!(settings.layout_for(LayoutScope::Tags), UiLayout::List);
        assert_eq!(settings.layout_for(LayoutScope::Presets), UiLayout::List);
        assert_eq!(
            settings.layout_for(LayoutScope::Repositories),
            UiLayout::List
        );
        assert_eq!(settings.ui.icons, Icons::Text);
        assert!(!settings.search.dictionary);

        // A new session sees the current file, not the previous session's choices.
        let reopened = RuntimeSettings::new(&config);
        assert_eq!(reopened.layout_for(LayoutScope::Library), UiLayout::List);
        assert_eq!(reopened.layout_for(LayoutScope::Agents), UiLayout::List);
    }
}
