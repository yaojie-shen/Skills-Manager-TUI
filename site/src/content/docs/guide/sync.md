---
title: "Root backup and sync"
description: "Back up the complete Library root with Git."
---

Root backup treats the entire Library root as one Git working tree.

- It includes skill content, metadata, notes, presets, configuration, and deletions.
- A successful Library mutation can trigger it after automatic sync is configured.
- Status probes may contact the remote, but never commit, merge, or push.
- Conflicts stop synchronization; no operation force-pushes or resets the root.

This backup is separate from each skill's upstream source. Configuration may contain machine-specific paths, so use a trusted remote suitable for that data.

## Configure backup

The root must own a real `.git` directory. A parent repository or linked Git worktree is not accepted. For a new backup, use an empty remote. For an existing backup, clone it first and use the clone as the Library root. Git commit identity (`user.name` and `user.email`) and remote credentials must already work.

```sh
skills sync status
skills sync configure git@github.com:YOUR-ACCOUNT/YOUR-BACKUP.git --branch main
```

| Where configured | Immediate effect |
| --- | --- |
| CLI | Enables automatic backup and immediately runs a full sync. |
| TUI | Enables automatic backup without immediately committing, merging, or pushing. |

An existing `origin` with another URL is not silently replaced. The branch defaults to `main`, and the checked-out root branch must pass validation.

Settings live in the root's `.git/config` as `remote.origin.url`, `skills.sync-branch`, and `skills.autosync`. Configure each clone separately. These settings stay in `.git/config` because `.skills-meta/config.toml` is itself synced: a remote or on/off switch stored there would follow every pull to every machine, so turning automatic sync off on one machine would turn it off everywhere. Legacy per-skill backup repositories are not converted automatically.

## Manual commands

| Command | Effect |
| --- | --- |
| `skills sync status` | Prints `url`, `branch`, and `enabled`; it is not a remote-freshness report. |
| `skills sync --dry-run` | Validates the local root and reports changes that would be saved; no remote fetch, merge, or push. |
| `skills sync` / `skills sync run` | Saves local changes, merges remote updates, then pushes without force. |
| `skills sync push` | Saves local changes and pushes without pulling first. |
| `skills sync pull` | Saves local changes and merges remote updates without pushing. It may create a backup commit. |
| `skills sync disable` | Disables automatic sync while retaining history and configuration. |

`skills sync run`, `skills sync push`, and `skills sync pull` support `--dry-run`; `skills sync configure` and `skills sync disable` do not. Manual operations remain available while automatic sync is disabled.

## Status probes and mutating sync

| Operation | May contact remote | Changes Library or Git refs | Commit / merge / push |
| --- | --- | --- | --- |
| TUI status probe | Yes | No | No |
| `skills sync --dry-run` | No | No | No |
| `skills sync pull` | Yes | May save local work and merge | Commit / merge, no push |
| `skills sync run` | Yes | Yes | Commit / merge / push, never force |

The TUI probes status at startup and periodically, whether or not automatic sync is enabled. Probes update cached dirty, ahead, and behind state only. Remote comparison may use a separate cache under `.git`.

Automatic sync follows the command's declared mutation scope, not the final diff. A successful Library-scoped command can trigger it even when no files changed. The sync stages every current non-ignored root change, including pre-existing external files. Startup, reads, previews, and Agent-only deployment changes do not trigger automatic sync. In the TUI, automatic sync also waits until root changes stop changing and input pauses; the `[sync]` table in [configuration](../configuration/) sets both waits.

## Failure behavior

Sync saves local work before merging remote updates. On conflict, it attempts to abort the merge and retains the local backup commit. Check Git status, resolve the repository deliberately, then retry manually. Network or authentication failures may leave local changes or commits pending; a successful Library command does not guarantee that remote backup succeeded.

Runtime paths `.skills-meta/.sync`, `.skills-meta/.staging`, `.skills-meta/.repair-backups`, `.skills-meta/.metadata.lock`, and `.skills-meta/backups` are excluded through local Git excludes. Standard Git ignore rules also apply. Ignored content is not backed up. [Troubleshooting](../troubleshooting/) covers recovery without force-push or `reset --hard`.
