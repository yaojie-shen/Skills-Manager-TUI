---
title: "Configuration and paths"
description: "Select the Library root and configure UI, Agent, and search behavior."
---

## Select the Library root

Global commands resolve an existing directory in this order:

1. `skills --root DIR …`
2. `SKILLS_HOME`
3. The path stored in `~/.config/skills-tui/root`

There is no implicit default. Missing paths and non-directories are rejected.

```sh
mkdir -p "$HOME/skills-library" "$HOME/.config/skills-tui"
printf '%s\n' "$HOME/skills-library" > "$HOME/.config/skills-tui/root"
skills init
```

When neither `--root` nor `SKILLS_HOME` is set, the program reads the pointer file. `init` creates `.skills-meta/config.toml` with commented defaults and refuses to overwrite it. Environment variables and the pointer file may contain paths that start with `~`. For a shell `--root` argument, use `"$HOME/path"` rather than a quoted tilde.

In the CLI, `--local` and `--project DIR` use `<project>/.agents/skills` as a separate project store. Init, install, and Agent registration may create this directory. Read commands require it to exist. In the TUI, the same flags select the deployment context while the central Library remains in use. See [Agent deployment](../deployment/) for the scope rules.

## Files in the Library

| Path relative to root | Purpose |
| --- | --- |
| `hello-skill/SKILL.md` | Local skill; valid content is recognized without a metadata record. |
| `repos/<alias>/<local-name>/` | Remote skill content. |
| `.skills-meta/config.toml` | Agent, tag, search, and UI settings. |
| `.skills-meta/repos/<alias>.toml` | Source identity and remote-skill metadata. |
| `.skills-meta/repos/.root.toml` | Standalone remote-skill metadata. |
| `.skills-meta/presets/<name>.toml` | Fixed preset members and target Agents. |
| `.git/config` | Root backup remote, branch, and enablement. |

The Library stores its data in files. Local skills do not have per-skill notes or baselines. Reconciliation does not change skill content. If it opens legacy preset data, however, it may create a backup and migrate the file format.

## UI settings

Add keys to existing TOML tables; TOML does not allow the same table twice.

```toml
schema = 1
tags_enabled = true

[ui]
layout = "grid"
pill_caps = "block"
icons = "text"
```

| Setting | Values / behavior |
| --- | --- |
| `layout` | `grid`, `list`, or `compact`; `split` is a compatibility alias. |
| `pill_caps` | Round Powerline caps by default; use `block` or `none` if glyphs fail. |
| `icons` | Nerd Font icons by default; use `text` when glyphs are unavailable. |
| `tags_enabled` | Shows or hides tag classification without deleting tag or preset data. |

A session-only layout change does not update startup configuration.

## Agent paths

Each `[[agents]]` entry has a `key`, an optional display `name`, and a `skills_dir`. Paths may start with `~`. New global configurations include Claude and Codex. Project paths must remain inside the project.

```sh
skills agents catalog
skills agents add cursor
skills agents add custom --dir /path/to/skills
```

## Search settings

```toml
[search]
prefix = true
fuzzy = true
dictionary = true
```

Prefix, fuzzy, and dictionary search are enabled by default.

| Field weight | Default |
| --- | ---: |
| name | 6 |
| tag | 4 |
| description | 2 |
| note | 2 |
| heading | 1.5 |
| body | 1 |

The dictionary weights are tech 0.7, common 0.4, and user 1. Unknown fields in supported tables cause validation to fail.

## Automatic sync waits

```toml
[sync]
quiet_seconds = 120
tui_idle_seconds = 10
```

| Setting | Behavior |
| --- | --- |
| `quiet_seconds` | Waits until the root's uncommitted changes have stayed the same for this many seconds. Default 120. |
| `tui_idle_seconds` | Waits this many seconds after the last keyboard, paste, mouse, or resize input. Default 10. |

Values are whole seconds. `0` disables only that wait; safety checks still apply. The waits apply only to automatic sync in the TUI, and manual sync runs immediately. The remote, branch, and enablement are not set here; configure them with `skills sync configure` as described in [root backup](../sync/).
