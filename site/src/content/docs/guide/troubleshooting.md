---
title: "Troubleshooting"
description: "Resolve root, source, deployment, update, and backup errors."
---

## `skills root not set` or `does not exist`

**Check**

```sh
printf '%s\n' "$SKILLS_HOME"
cat "$HOME/.config/skills-tui/root"
```

**Cause**

Global commands could not resolve an existing Library from `--root`, `SKILLS_HOME`, or the pointer file. CLI `--local` commands instead require a project store at `<project>/.agents/skills`.

**Fix**

Create the directory, select it with `--root`, `SKILLS_HOME`, or the pointer file, then run `skills init`. Edit an existing configuration instead of deleting it. Use `--local init` only for a separate project store. In the TUI, `--project DIR` selects the deployment context while retaining the central Library. See [Configuration](../configuration/) for the complete resolution rules.

## Installation lists choices but adds nothing

**Check** the paths returned by:

```sh
skills install owner/repo --list
```

**Fix**

- Select one or more paths with repeated `--select PATH`.
- Use `--all` for all valid outermost skills.
- Give a new archive URL a `--source-name NAME`.
- Use `--branch` only with Git sources.

Archives preserve their top-level directory. `--name` changes the local directory for one selected skill. Installing a local path copies it directly; `--list` does not preview a local installation. If a release asset or checksum is missing, the installer does not support that tag and target combination. See [Installation](../installation/) and [Library and sources](../library/) for prerequisites and supported source forms.

## An Agent does not see a skill

**Check**

```sh
skills agents status AGENT
skills deploy SKILL_KEY --agent AGENT --dry-run
```

Confirm the Agent, scope, configured directory, Library key, and declared frontmatter name. An Agent entry describes a destination; it does not confirm that the Agent application is installed.

| Reported state | Next command |
| --- | --- |
| `broken` | `skills repair` or `skills agents clean AGENT --dry-run` |
| `foreign` | `skills agents remove-link … --dry-run` or `adopt-link … --dry-run` |
| `shadow`, `agent-only` | `skills agents relink AGENT SKILL --dry-run` after comparing content |

A rescan reports state without repairing links. An Agent directory is classified as read-only when it overlaps the Library, uses a symlinked path, or is not a directory. This safety classification does not test operating-system write bits. [Health and repair](../health/) describes each operation and its apply flag.

## Updates are refused or keep appearing

| Result | Meaning / action |
| --- | --- |
| `needs-resolution` | Preview, then choose `--take local` or `--take upstream` for the complete skill. |
| Update remains after `--take local` | Expected: content, source revision, and baseline remain unchanged. |
| Invalid content or declared-name change | Inspect upstream; the installed copy and deployments remain unchanged. |
| Missing baseline or corrupt TOML | Inspect `skills show` and `skills status`, then fix provenance or syntax. |

`skills accept` records the current hash for a remote-sourced skill; it cannot restore files. `--force` takes the complete upstream version and discards local skill changes. [Updates and baselines](../updates/) covers the full workflow.

## Root backup is pending, paused, or conflicted

`skills sync status` reports configuration without fetching current ahead/behind counts. TUI status probes continue while automatic sync is disabled or paused. They never commit, merge, or push.

**Check**

```sh
git -C "$SKILLS_HOME" status
git -C "$SKILLS_HOME" log -5 --oneline
skills sync --dry-run
```

Use the actual root if `SKILLS_HOME` is unset. Verify Git identity, credentials, network access, checked-out branch, and that the root owns its `.git` directory.

After a conflict, the local backup commit is normally retained and the merge is aborted. Resolve the Git state, then retry `skills sync run`. Do not use force-push or `reset --hard` as routine recovery. Disabling automatic sync preserves history and does not disable independent TUI status probes. [Root backup and sync](../sync/) explains the write policy.

## Missing glyphs or unexpected keys

Set `[ui] icons = "text"` and `pill_caps` to `"block"` or `"none"`. Outside text inputs, `:` opens the command palette; inside a field, it types a colon. Close overlays and leave text input before using page shortcuts. The footer and `Ctrl+G` show controls for the current context.
