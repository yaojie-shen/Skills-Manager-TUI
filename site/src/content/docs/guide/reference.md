---
title: "CLI and TUI reference"
description: "Command families, global flags, search syntax, and keyboard controls."
---

## Global entry points

```sh
skills --help
skills COMMAND --help
skills --root "$HOME/skills-library" list
skills --json status
```

| Flag | Behavior |
| --- | --- |
| no subcommand | Opens the TUI. |
| `--root DIR` | Overrides global root resolution. |
| `--local`, `--project DIR` | Select a CLI project store or TUI deployment context; [deployment](../deployment/) details the difference. |
| `--json` | Writes machine-readable CLI output; progress and top-level errors may still use stderr. |
| `--same-name replace` | Authorizes replacement for a declared deployment-name conflict. |

Errors return a nonzero status.

## Command families

Uppercase terms are placeholders. Prefix each command in the table with `skills`; examples elsewhere already include it.

| Family | Commands / purpose |
| --- | --- |
| Initialize | `init` creates `.skills-meta/config.toml` in the selected root |
| Inspect | `list [QUERY…]`, `show KEY`, `status` (alias `rescan`) |
| Import and rename | `install REFERENCE`, `adopt PATH`, `repos`, `repos rename ALIAS NAME`, `rename OLD NEW`, `set-source KEY REFERENCE` |
| Remove | `remove KEY --yes`; `--keep-meta` retains metadata, not files |
| Tags | `tag add/remove/set KEY [TAGS…]`, `tag list [KEY]`, `tag rename OLD NEW [--merge]`, `tag delete TAG --yes` |
| Notes | `note get/set/clear/edit`; set accepts `KEY TEXT` or `KEY -`; edit uses `$EDITOR`; remote skills only |
| Upstream | `accept KEY`, `check KEY/--all/--repo ALIAS`, `update KEY/--all/--repo ALIAS` |
| Deployment | `deploy/undeploy KEYS… --agent AGENT` (repeatable), or `--all-agents`; `--dry-run` previews |
| Agents | `agents list/catalog/add/status/clean/remove-link/adopt-link/relink` |
| Presets | `preset list/show/create/delete/add/remove/deploy/undeploy/describe`, `preset rename OLD NEW [--merge]`; deploy/undeploy take `--agent AGENT` or `--all-agents` |
| Health | `repair [--deployment AGENT/LINK=KEY]`; `--apply` executes the plan |
| Root backup | `sync configure/disable/status/push/pull/run`; bare `sync` runs a full sync |

`--dry-run` works only where documented. Removal and cleanup commands commonly require `--yes`; [health](../health/) and [root sync](../sync/) define their safety boundaries.

## Search

```sh
skills list "incident response" tag:operations
```

```text
incident-triage  local                   [operations]  Triage production incidents with safe checklists
                 ↳ [name,description,tag] incident response runbook and safe checklists
```

The bracketed names are the fields that matched the free-text query:

- `name`, `tag`, `description`, `note`
- Markdown `heading` and `body`

Filters include `tag:`, `preset:`, `repo:`, `agent:`, `status:`, and `source:`. `list` also accepts repeatable `--tag/-t`, `--agent/-a`, and `--status/-s`, plus `--untagged`. Mutation commands use the Library keys printed by `list`, not display labels.

## TUI controls

Keyboard hints appear only in the bottom bar and follow the focused page, panel, or dialog.

| Key | Action |
| --- | --- |
| `:` | Open the command palette outside text inputs |
| `a` / right-click | Open actions for the selected item |
| `?` / `Ctrl+G` | Open help; `?` types text while editing |
| `F1` | Show complete help for the current focus, including inside dialogs; press again to close |
| `Tab` / `Shift+Tab` | Move to the next or previous page after closing edit dialogs |
| `1` to `6` | Select a visible tab outside text inputs; hiding Tags changes the order |
| `Enter` / Down | Enter from the tab strip; elsewhere activate the focused control |
| `F2` | Open the Library View menu for layout, grouping, sorting, and group order |
| `Ctrl+←` / `Ctrl+→` | Collapse or expand all current Library groups while results are focused |
| `Esc` / `q` | Go back; quit from the tab strip; `q` types text while editing |
| `Ctrl+R` | Rescan the workspace |
| `Ctrl+Z` / `Ctrl+Y` | Undo or redo supported session actions |
| `m`, Space, `Ctrl+A` | Enter multi-select, toggle one skill, or add the current results |
| `u` / `U` | Check or update upstream content |

The command palette filters as you type. Arrow keys select a command; Enter runs it; Esc closes it. A single left click runs an enabled command in Actions or Commands. In the settings-only Library View menu, use `←`/`→` to switch categories, `Tab` to advance, or `Shift+Tab` to go back. `↑`/`↓`, `j`/`k`, clicks, and the scroll wheel apply settings immediately without closing the menu. `Enter`, `Esc`, or `F2` closes it and keeps applied changes. Collapse-all and expand-all remain in the results-frame border and on `Ctrl+←` / `Ctrl+→`. View-menu shortcuts appear only in the screen footer. Tag-picker changes apply immediately, while staged membership selectors require Apply. Closing a tag picker does not undo changes already written. A left, right, or middle button press outside a popup closes only the top overlay and consumes the event; scrolling outside is ignored. Completion popovers close before their parent dialog. Busy staged operations remain open while writing. Clicking Help or message content keeps it open; press outside or use its keyboard close shortcut. In backup settings, an outside press closes the whole dialog, while Esc returns to the previous step. The single-line bottom bar combines the focused control’s description or disabled reason with its shortcuts. Long messages are shortened to fit. Click the message area or press `F1` to read the full description and shortcuts in a scrollable notification, including while editing or inside another popup. Press `F1`, `Esc`, or `Enter` to close it and return to the previous focus. Keyboard and mouse focus share the same help.
