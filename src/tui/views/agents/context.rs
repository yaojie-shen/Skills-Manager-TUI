use super::*;
impl AgentsView {
    pub(super) fn entry_menu(&self, ctx: &Ctx) -> Option<Request> {
        let rows = self.rows(ctx);
        let row = rows.get(self.entries.selected()?)?;
        let caps = Caps::of(row.state);
        let path = ctx.snap.agent(&self.scope)?.skills_dir.join(&row.name);
        let target = Target::Entry {
            key: row.key.clone(),
            scope: self.scope.clone(),
            path,
            state: format!("{:?}", row.state),
        };
        Some(Request {
            title: row
                .record
                .map(crate::tui::components::skill::display_name)
                .unwrap_or(&row.name)
                .to_string(),
            detail: format!(
                "Only this entry · {}",
                ctx.snap.agent(&self.scope)?.skills_dir.display()
            ),
            target,
            items: vec![
                Item::new(Command::Open, "View skill", KeyCode::Enter, true, "", 0),
                Item::new(
                    Command::Remove,
                    if row.linked {
                        "Uninstall from this scope"
                    } else {
                        "Remove broken link"
                    },
                    KeyCode::Char('x'),
                    row.linked || caps.clean,
                    "No managed or broken link to remove",
                    1,
                ),
                Item::new(
                    Command::Relink,
                    "Relink",
                    KeyCode::Char('r'),
                    caps.relink,
                    "Requires an identical unmanaged copy",
                    1,
                ),
                Item::new(
                    Command::Adopt,
                    "Adopt skill",
                    KeyCode::Char('a'),
                    caps.adopt,
                    "Entry is already known to the Library",
                    1,
                ),
            ],
        })
    }
    pub(super) fn entry_command(&mut self, command: Command, ctx: &Ctx) -> Vec<Action> {
        if !self.entry_menu(ctx).is_some_and(|r| r.allows(command)) {
            return vec![];
        }
        let rows = self.rows(ctx);
        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
            return vec![];
        };
        match command {
            Command::Open => {
                self.preview_entry(ctx);
                vec![]
            }
            Command::Relink => self.repair(ctx, &rows, false),
            Command::Remove if !row.linked => self.repair(ctx, &rows, true),
            Command::Adopt => {
                let Some(report) = ctx.snap.agent(&self.scope) else {
                    return vec![];
                };
                vec![Action::OpenModal(Box::new(Modal::adopt(
                    &self.scope,
                    &row.name,
                    report.skills_dir.join(&row.name),
                )))]
            }
            Command::Remove => self.uninstall_entry(ctx),
            _ => vec![],
        }
    }
    fn uninstall_entry(&self, ctx: &Ctx) -> Vec<Action> {
        let rows = self.rows(ctx);
        let Some(row) = self.entries.selected().and_then(|i| rows.get(i)) else {
            return vec![];
        };
        let Some(record) = row.record.filter(|_| row.linked) else {
            return vec![];
        };
        let Some(agent) = ctx.ws.config.agent(&self.scope).cloned() else {
            return vec![];
        };
        let project = self.project();
        let key = record.key.clone();
        vec![Action::OpenModal(Box::new(
            Modal::confirm_meta(
                format!("Uninstall {} from {}", record.key, agent.display_name()),
                vec![
                    format!("Remove link from {}", agent.skills_dir),
                    "The central skill is kept.".into(),
                ],
                Box::new(move |ws| {
                    skills::ops::targets::set_deployed(
                        ws,
                        &agent,
                        project.as_deref(),
                        &[key],
                        false,
                    )
                }),
            )
            .deployment_only()
            .in_background(vec![record.key.clone()]),
        ))]
    }
}
