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
| Tags | `tag add/remove/set KEY [TAGS…]`, `tag list [KEY]`, `tag rename OLD NEW`, `tag delete TAG --yes` |
| Notes | `note get/set/clear/edit`; set accepts `KEY TEXT` or `KEY -`; edit uses `$EDITOR`; remote skills only |
| Upstream | `accept KEY`, `check KEY/--all/--repo ALIAS`, `update KEY/--all/--repo ALIAS` |
| Deployment | `deploy/undeploy KEYS… --agent AGENT` (repeatable), or `--all-agents`; `--dry-run` previews |
| Agents | `agents list/catalog/add/status/clean/remove-link/adopt-link/relink` |
| Presets | `preset list/show/create/delete/add/remove/deploy/undeploy/rename/describe` |
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

| Key | Action |
| --- | --- |
| `:` | Open the command palette outside text inputs |
| `a` / right-click | Open actions for the selected item |
| `?` / `Ctrl+G` | Open help; `?` types text while editing |
| `Tab` / `Shift+Tab` | Move to the next or previous page after closing edit dialogs |
| `1` to `6` | Select a visible tab outside text inputs; hiding Tags changes the order |
| `Enter` / Down | Enter from the tab strip; elsewhere activate the focused control |
| `Esc` / `q` | Go back; quit from the tab strip; `q` types text while editing |
| `Ctrl+R` | Rescan the workspace |
| `Ctrl+Z` / `Ctrl+Y` | Undo or redo supported session actions |
| `m`, Space, `Ctrl+A` | Enter multi-select, toggle one skill, or add the current results |
| `u` / `U` | Check or update upstream content |

The command palette filters as you type. Arrow keys select a command; Enter runs it; Esc closes it. Tag-picker changes apply immediately, while staged membership selectors require Apply. Closing a tag picker does not undo changes already written. The footer shows controls for the current context.
