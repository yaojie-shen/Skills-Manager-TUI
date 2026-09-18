use super::*;
impl HealthView {
    pub(super) fn issue_menu(&self, ctx: &Ctx) -> Option<Request> {
        let row = self.selected_row().filter(|r| r.heading.is_none())?;
        let mut items = vec![Item::new(
            Command::Open,
            if row.agent.is_some() {
                "Open / resolve entry"
            } else {
                "View skill"
            },
            KeyCode::Enter,
            true,
            "",
            0,
        )];
        let target = if let Some(agent) = &row.agent {
            let foreign = matches!(row.state, Some(EntryState::Foreign { .. }));
            items.extend([
                Item::new(
                    Command::Remove,
                    "Remove link",
                    KeyCode::Char('x'),
                    foreign || matches!(row.state, Some(EntryState::Broken { .. })),
                    "No foreign or broken link to remove",
                    1,
                ),
                Item::new(
                    Command::Relink,
                    "Relink",
                    KeyCode::Char('r'),
                    matches!(row.state, Some(EntryState::Shadow { same_content: true })),
                    "Requires an identical unmanaged copy",
                    1,
                ),
                Item::new(
                    Command::Adopt,
                    if foreign { "Adopt copy" } else { "Adopt skill" },
                    KeyCode::Char('a'),
                    foreign || matches!(row.state, Some(EntryState::AgentOnly)),
                    "Entry is already managed",
                    1,
                ),
            ]);
            Target::Entry {
                key: row.key.clone(),
                scope: agent.clone(),
                path: ctx.snap.agent(agent)?.skills_dir.join(&row.key),
                state: format!("{:?}", row.state),
            }
        } else {
            let caps = row.caps;
            items.extend([
                Item::new(
                    Command::Update,
                    "Update",
                    KeyCode::Char('U'),
                    caps.update,
                    "No applicable update",
                    1,
                ),
                Item::new(
                    Command::Accept,
                    "Accept baseline",
                    KeyCode::Char('a'),
                    caps.accept,
                    "Requires changes or a missing baseline",
                    1,
                ),
                Item::new(
                    Command::Remove,
                    "Clean up invalid / missing skill",
                    KeyCode::Char('x'),
                    caps.clean,
                    "Skill does not require cleanup",
                    2,
                ),
            ]);
            Target::Entry {
                key: row.key.clone(),
                scope: String::new(),
                path: ctx.ws.skill_path(&row.key),
                state: format!("{:?}", ctx.snap.get(&row.key)?.status),
            }
        };
        Some(Request {
            title: self
                .selected(ctx)
                .map(crate::tui::components::skill::display_name)
                .unwrap_or_else(|| row.key.rsplit('/').next().unwrap_or(&row.key))
                .to_string(),
            detail: row
                .agent
                .as_ref()
                .map(|a| format!("Only this entry · {a}"))
                .unwrap_or_default(),
            target,
            items,
        })
    }
    pub(super) fn issue_command(&mut self, command: Command, ctx: &Ctx) -> Vec<Action> {
        let caps = self.selected_row().map(|r| r.caps).unwrap_or_default();
        match command {
            Command::Open => self.open_selected(ctx),
            // The guards below mirror `Caps::of`: a key the footer does not
            // show is simply ignored, never answered with an error toast.
            Command::Update if caps.update => match self.selected(ctx) {
                Some(r) => vec![
                    Action::Spawn(Task::Prepare(r.key.clone())),
                    Action::Toast(format!("fetching {}…", r.key)),
                ],
                None => vec![],
            },
            Command::Accept if caps.accept => match self.selected(ctx) {
                Some(r) => {
                    let key = r.key.clone();
                    vec![Action::Write(Box::new(move |ws| {
                        edit::accept(ws, &key).map(|_| format!("baseline updated for {key}"))
                    }))]
                }
                None => vec![],
            },
            // Invalid on-disk content may be discarded explicitly. Missing
            // records remain informational in Repair v1.
            Command::Remove if caps.clean => match self
                .selected(ctx)
                .map(|r| (r.key.clone(), r.status.clone()))
            {
                Some((key, SkillStatus::Invalid { reason })) => {
                    vec![Action::OpenModal(Box::new(Modal::discard_invalid(
                        &key, &reason,
                    )))]
                }
                _ => vec![],
            },
            _ => vec![],
        }
    }
}
