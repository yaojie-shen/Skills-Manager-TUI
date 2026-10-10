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
| `.skills-meta/config.toml` | Agent, search, UI, and sync-wait settings, plus the tag toggle. |
| `.skills-meta/tags/<tag>.toml` | One tag: its name, skills, color, and description. |
| `.skills-meta/repos/<alias>.toml` | Source identity and remote-skill metadata. |
| `.skills-meta/repos/.root.toml` | Standalone remote-skill metadata. |
| `.skills-meta/presets/<name>.toml` | Fixed preset members and target Agents. |
| `.skills-meta/format.toml` | Metadata format version, written by Skills Manager. Do not edit it. |
| `.git/config` | Root backup remote, branch, and enablement. |

The Library stores its data in files. Local skills do not have per-skill notes or baselines. Reconciliation does not change skill content. Opening metadata written by an older version upgrades it first, as described in [metadata format upgrades](#metadata-format-upgrades).

## Metadata format upgrades

`.skills-meta/format.toml` records the format version of the Library's metadata. It is synced with the rest of the root, so keep every machine that syncs the Library on an up-to-date Skills Manager.

When Skills Manager opens metadata written by an older version, it upgrades the files before anything else runs. Older metadata includes tags inside `config.toml`, presets that list tags instead of skills, and files without a `schema` field.

- Every file is checked first. If any file cannot be upgraded, nothing is written and the error names the file.
- Tags with the same name in an old `config.toml` are merged and their skills combined. If their colors or descriptions differ, the upgrade stops so you can choose one.
- If a tag in `config.toml` is defined differently from its file in `.skills-meta/tags/`, for example after a machine with an older Skills Manager synced tags back into `config.toml`, the upgrade stops and names both. Make the two definitions match, or remove the `config.toml` entry you do not want, then open the Library again.
- Before writing, the original files are copied to `.skills-meta/backups/metadata-before-layout-v<from>-to-v<to>-<timestamp>/` under their original paths. The backup holds only the files the upgrade changed or removed.
- The CLI prints the backup path on stderr. The TUI shows a report at startup; until you quit, reopen it with **Show metadata migration report** in the command palette.
- If root sync is running at that moment, the upgrade stops without writing. Open the Library again once the sync finishes.

If an upgrade is interrupted, open the Library again: it continues and reaches the same result, unless the error asks for manual recovery. To run the upgrade again from the original files, copy the originals you need from the backup back into `.skills-meta`. Copy `format.toml` too if the backup has one; otherwise delete `.skills-meta/format.toml`. Then open the Library again, and Skills Manager upgrades those files again. The backup does not contain files the upgrade created, such as `tags/*.toml`; they stay in `.skills-meta`, so delete them first if the upgrade should start from the originals alone.

## UI settings

Add keys to existing TOML tables; TOML does not allow the same table twice.

```toml
schema = 2
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
