use super::*;
use crate::tui::components::context_menu::{Command, Item, Request, Target};

impl SearchView {
    fn skill_items(&self, ctx: &Ctx) -> Vec<Item> {
        use Command::*;
        let Some(r) = self.selected(ctx) else {
            return vec![];
        };
        let present = r.status.is_present();
        let remote = r
            .source
            .as_ref()
            .is_some_and(skills::meta::Source::is_remote);
        let mut items = vec![];
        let mut add = |cmd, label, key, enabled, reason, group| {
            items.push(Item::new(cmd, label, key, enabled, reason, group))
        };
        add(Open, "View skill", KeyCode::Enter, true, "", 0);
        if ctx.settings.tags_enabled {
            add(
                Tags,
                "Edit tags",
                KeyCode::Char('t'),
                present,
                "Skill files are unavailable",
                1,
            );
        }
        add(Presets, "Manage presets", KeyCode::Char('p'), true, "", 1);
        add(
            Deploy,
            "Deploy",
            KeyCode::Char('d'),
            present,
            "Skill files are unavailable",
            1,
        );
        add(
            Note,
            "Edit note",
            KeyCode::Char('n'),
            present && r.source_kind() == "repository",
            "Notes are only available for repository skills",
            2,
        );
        add(
            Rename,
            "Rename",
            KeyCode::Char('r'),
            present,
            "Skill files are unavailable",
            2,
        );
        add(
            Source,
            "Set source",
            KeyCode::Char('s'),
            present,
            "Skill files are unavailable",
            2,
        );
        add(
            Check,
            "Check updates",
            KeyCode::Char('u'),
            remote,
            "No remote source",
            3,
        );
        add(
            Update,
            "Update",
            KeyCode::Char('U'),
            remote,
            "No remote source",
            3,
        );
        add(
            Accept,
            "Accept baseline",
            KeyCode::Char('a'),
            matches!(
                r.status,
                SkillStatus::Modified | SkillStatus::MissingBaseline
            ),
            "Requires changes or a missing baseline",
            3,
        );
        add(Remove, "Delete skill", KeyCode::Char('x'), true, "", 4);
        items
    }
    pub(super) fn skill_command(&mut self, cmd: Command, ctx: &Ctx) -> Vec<Action> {
        if let Some(item) = self.skill_items(ctx).iter().find(|i| i.command == cmd) {
            if let Some(reason) = &item.disabled {
                return vec![Action::Error(reason.clone())];
            }
        } else {
            return vec![];
        }
        match cmd {
            Command::Open => {
                self.open_preview(ctx);
                vec![]
            }
            Command::Tags => self.act_tags(ctx),
            Command::Note => self.act_note(ctx),
            Command::Deploy => self.act_deploy(ctx),
            Command::Rename => self.act_rename(ctx),
            Command::Source => self.act_set_source(ctx),
            Command::Check => self.act_check(ctx),
            Command::Update => self.act_update(ctx),
            Command::Accept => self.act_accept(ctx),
            Command::Remove => self.act_remove(ctx),
            Command::Presets => self
                .selected(ctx)
                .map(|r| {
                    vec![Action::OpenModal(Box::new(Modal::batch_presets(
                        vec![r.key.clone()],
                        ctx,
                    )))]
                })
                .unwrap_or_default(),
            _ => vec![],
        }
    }
    pub(super) fn batch_command(&self, cmd: Command, keys: Vec<String>, ctx: &Ctx) -> Vec<Action> {
        if keys.is_empty() {
            return vec![Action::Error("No selected skills".into())];
        }
        let modal = match cmd {
            Command::Tags if ctx.settings.tags_enabled => Modal::batch_tags(keys, ctx),
            Command::Presets => Modal::batch_presets(keys, ctx),
            Command::Deploy => match &self.scope_agent {
                Some(agent) => Modal::batch_deploy_agent(keys, agent, ctx),
                None => Modal::batch_deploy(keys, ctx),
            },
            _ => return vec![],
        };
        vec![Action::OpenModal(Box::new(modal))]
    }
    pub(super) fn menu_at(&mut self, x: u16, y: u16, ctx: &Ctx) -> Option<Request> {
        if self.is_picker()
            || self.overlay.is_open()
            || !self.list_rect.contains((x, y).into())
            || self.list_track.hit(x, y)
        {
            return None;
        }
        let i = self.grid.hit(x, y)?;
        self.hits.get(i)?;
        self.grid.select(Some(i));
        self.focus = Focus::List;
        self.preview_scroll = 0;
        self.search_panel.completion.close();
        self.menu_current(ctx)
    }
    pub(super) fn menu_current(&self, ctx: &Ctx) -> Option<Request> {
        if self.is_picker() || self.overlay.is_open() || self.focus != Focus::List {
            return None;
        }
        let key = self.selected(ctx)?.key.clone();
        if self.multi && self.checked.contains(&key) {
            let all: Vec<_> = self.checked.iter().cloned().collect();
            let visible = self.visible_checked(ctx);
            let mut items = vec![];
            for (cmd, label, key, count) in [
                (Command::Tags, "Batch edit tags", 't', all.len()),
                (Command::Presets, "Batch manage presets", 'p', all.len()),
                (Command::Deploy, "Batch deploy", 'd', all.len()),
            ] {
                if cmd == Command::Tags && !ctx.settings.tags_enabled {
                    continue;
                }
                items.push(Item::new(
                    cmd,
                    format!("{label} · {count} skills"),
                    KeyCode::Char(key),
                    count > 0,
                    "No selected skills",
                    0,
                ));
            }
            Some(Request {
                title: format!("{} skills selected", all.len()),
                detail: if all.len() > visible.len() {
                    format!("Includes {} hidden by filter", all.len() - visible.len())
                } else {
                    String::new()
                },
                target: Target::Batch { all, visible },
                items,
            })
        } else {
            Some(Request {
                title: self
                    .selected(ctx)
                    .map(crate::tui::components::skill::display_name)
                    .unwrap_or(&key)
                    .to_string(),
                detail: if self.checked.is_empty() {
                    String::new()
                } else {
                    format!(
                        "Only this skill · {} other selections kept",
                        self.checked.len()
                    )
                },
                target: Target::Skill(key),
                items: self.skill_items(ctx),
            })
        }
    }
    pub(super) fn run_menu(&mut self, target: &Target, command: Command, ctx: &Ctx) -> Vec<Action> {
        let stale = || {
            vec![Action::Error(
                "Target changed; reopen the context menu".into(),
            )]
        };
        match target {
            Target::Skill(key) => {
                let Some(i) = self.hits.iter().position(|h| &h.key == key) else {
                    return stale();
                };
                if ctx.snap.get(key).is_none() {
                    return stale();
                }
                self.grid.select(Some(i));
                if !self
                    .skill_items(ctx)
                    .iter()
                    .any(|i| i.command == command && i.disabled.is_none())
                {
                    return stale();
                }
                self.skill_command(command, ctx)
            }
            Target::Batch { all, visible } => {
                if all.iter().any(|k| ctx.snap.get(k).is_none())
                    || all.iter().cloned().collect::<BTreeSet<_>>() != self.checked
                    || *visible != self.visible_checked(ctx)
                {
                    return stale();
                }
                self.batch_command(command, all.clone(), ctx)
            }
            _ => stale(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_menu_hit_batch_counts_stale_targets_and_keyboard_parity() {
        let root = skills::ops::DownloadDir::new("context-search").unwrap();
        skills::config::Config {
            agents: vec![],
            ..Default::default()
        }
        .save(root.path())
        .unwrap();
        for name in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
            std::fs::write(
                root.path().join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: test\n---\nBody"),
            )
            .unwrap();
        }
        let ws = skills::Workspace::open(root.path()).unwrap();
        let snap = ws.scan().unwrap();
        let settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        let ctx = Ctx {
            ws: &ws,
            snap: &snap,
            settings: &settings,
        };
        let mut view = SearchView::default();
        view.refresh(&ctx);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let cell = view.grid.cell(1).unwrap();
        let request = view.menu_at(cell.x + 1, cell.y, &ctx).unwrap();
        assert_eq!(request.target, Target::Skill("beta".into()));
        assert_eq!(view.selected(&ctx).unwrap().key, "beta");
        assert!(view.checked.is_empty());
        assert!(!view.overlay.is_open());
        let menu_actions = view.run_menu(&request.target, Command::Presets, &ctx);
        let key_actions =
            view.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE), &ctx);
        assert!(matches!(menu_actions.as_slice(), [Action::OpenModal(_)]));
        assert!(matches!(key_actions.as_slice(), [Action::OpenModal(_)]));
        view.multi = true;
        view.checked = BTreeSet::from(["alpha".into(), "beta".into()]);
        view.set_query("beta", &ctx);
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let cell = view.grid.cell(0).unwrap();
        let request = view.menu_at(cell.x + 1, cell.y, &ctx).unwrap();
        assert!(request.title.contains('2'));
        assert!(request.detail.contains("1 hidden"));
        assert!(request.items.iter().all(|i| i.label.starts_with("Batch")));
        assert!(
            request
                .items
                .iter()
                .find(|i| i.command == Command::Presets)
                .unwrap()
                .label
                .contains("2 skills")
        );
        assert!(
            request
                .items
                .iter()
                .find(|i| i.command == Command::Deploy)
                .unwrap()
                .label
                .contains("2 skills")
        );
        let before = view.checked.clone();
        assert!(matches!(
            view.run_menu(&request.target, Command::Presets, &ctx)
                .as_slice(),
            [Action::OpenModal(_)]
        ));
        assert_eq!(view.checked, before);
        view.checked.remove("alpha");
        assert!(matches!(
            view.run_menu(&request.target, Command::Presets, &ctx)
                .as_slice(),
            [Action::Error(_)]
        ));
        view.set_query("gamma", &ctx);
        terminal.draw(|f| view.draw(f, f.area(), &ctx)).unwrap();
        let cell = view.grid.cell(0).unwrap();
        let request = view.menu_at(cell.x + 1, cell.y, &ctx).unwrap();
        assert_eq!(request.target, Target::Skill("gamma".into()));
        assert!(view.checked.contains("beta"));
        assert!(view.menu_at(0, 0, &ctx).is_none());
        for layout in [UiLayout::List, UiLayout::Compact, UiLayout::Grid] {
            let mut session = crate::tui::settings::SessionSettings::default();
            session.set_layout(LayoutScope::Library, layout);
            let settings = crate::tui::settings::RuntimeSettings::resolve(&ws.config, &session);
            let layout_ctx = Ctx {
                settings: &settings,
                ..ctx
            };
            terminal
                .draw(|f| view.draw(f, f.area(), &layout_ctx))
                .unwrap();
            let cell = view.grid.cell(0).unwrap();
            assert_eq!(
                view.menu_at(cell.x + 1, cell.y, &layout_ctx)
                    .unwrap()
                    .target,
                Target::Skill("gamma".into())
            );
        }
        let mut disabled_settings = crate::tui::settings::RuntimeSettings::new(&ws.config);
        disabled_settings.tags_enabled = false;
        let disabled_ctx = Ctx {
            settings: &disabled_settings,
            ..ctx
        };
        assert!(
            !view
                .skill_items(&disabled_ctx)
                .iter()
                .any(|i| i.command == Command::Tags)
        );
        let mut picker = SearchView::tag_members("tools", &ctx);
        terminal.draw(|f| picker.draw(f, f.area(), &ctx)).unwrap();
        assert!(picker.menu_at(5, 6, &ctx).is_none());

        assert!(matches!(
            view.run_menu(&Target::Skill("gone".into()), Command::Remove, &ctx)
                .as_slice(),
            [Action::Error(_)]
        ));
    }
}
