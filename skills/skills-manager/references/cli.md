# `skills` CLI reference

Every command accepts these global flags:

| Flag | Effect |
| --- | --- |
| `--root DIR` | Use this Library root. Conflicts with `--local` and `--project`. |
| `--local` | Use `./.agents/skills` as the store. |
| `--project DIR` | Use `DIR/.agents/skills` as the store. |
| `--json` | Machine-readable output on stdout. |
| `--same-name replace` | When deploying, replace a deployment that another skill with the same declared name already owns. |

`skills COMMAND --help` is authoritative for the installed version. Uppercase words below are placeholders, `[...]` is optional, and `[--flag X]...` means the flag can be repeated. `-y` is short for `--yes` wherever `--yes` exists. Three commands have a default subcommand: `skills repos` lists sources, `skills agents` runs `agents list`, and `skills sync` runs `sync run`.

## Inspect

```sh
skills list [QUERY...] [-t TAG]... [-a AGENT]... [-s STATUS]... [--untagged]
skills show KEY
skills status                  # alias: rescan
```

`list` ranks free-text matches across name, tags, description, note, and `SKILL.md` headings and body, with prefix, typo, Chinese/English, and CJK matching. Filters can go in the query:

| Filter | Matches |
| --- | --- |
| `tag:NAME`, `preset:NAME` | Members |
| `agent:KEY` | Currently deployed to that agent |
| `status:STATUS` | That status |
| `source:local`, `source:repository` | Source class |
| `repo:ALIAS` | Exact storage alias, source display name, or source URL (not a substring) |
| `untagged` | Skills with no tag |

Repeated `tag:`, `preset:`, and `agent:` filters must all match; repeated `status:`, `source:`, and `repo:` filters match any one; different kinds combine with AND. Quote a multi-word value inside the query, `skills list 'tag:"team tools"'`; the `-t` flag does not preserve that quoting, so use the query form for multi-word tags.

JSON shapes:

- `skills --json list`: an array of `{key, name, status, tags, presets, deployed, source, source_name, description, score, matched, excerpt}`. `status` is a string here.
- `skills --json show KEY`: one object with `key, path, status, name, description, external, tags, presets, note, source, source_name, current_hash, baseline_hash, deploy`. `status` is an object such as `{"kind": "local"}` or `{"kind": "invalid", "reason": "..."}`; `deploy` maps agent key to a state such as `"deployed"`.
- `skills --json status`: `{root, skills, agents, presets, repositories}`. Each agent has `key, name, skills_dir, mode, entries`, and each entry has a `kind` (`deployed`, `broken`, `foreign`, `shadow`, `agent-only`).

## Create and import

```sh
skills init                                   # write .skills-meta/config.toml in an existing root; refuses to overwrite
skills install REFERENCE [OPTIONS]
skills adopt PATH [--name NAME]
skills repos                                  # list installed sources
skills repos rename ALIAS DISPLAY-NAME        # display name only; nothing moves
```

`install` accepts a local path, `owner/repo`, `owner/repo/subpath`, `owner/repo@branch`, a GitHub tree URL, a Git URL (`https://...git`, `git@...`, `ssh://...`, `file://...`), or an archive URL (zip, tar, tar.gz, tar.bz2, tar.xz).

| Option | Effect |
| --- | --- |
| `--list` | Show discovered skill paths; install nothing |
| `--select PATH` | Install this path in the source; repeatable; `.` is the source root |
| `--all` | Install every outermost skill in the source |
| `--subpath PATH` | Restrict discovery to this path |
| `--name NAME` | Library name when exactly one skill is selected |
| `--local-name PATH=NAME` | Library name per selected path; repeatable |
| `--repo-alias ALIAS` | Storage folder: `repos/ALIAS/` |
| `--source-name NAME` | Display name; required for a new archive URL, defaults to `owner/repo` for Git |
| `--branch REF` | Git branch or tag (not for archives) |
| `--deploy AGENT` | Deploy right after installing; repeatable |

Only `--name` and `--deploy` apply to a local-path install. The other options select from Git repositories and archives; with a local path they are ignored, and `--source-name` is an error. With several skills in a source and no selection, `install` prints the choices and installs nothing; it never prompts.

`adopt PATH` copies a skill directory into the Library and then deletes the original. When PATH is directly inside a configured agent's skills directory, a symlink to the Library copy replaces it. For a skill already directly in the root, `adopt` only writes a local source record, and it refuses `--name` there (use `rename`).

## Maintain skills

```sh
skills rename OLD NEW                        # moves folder, metadata, tags, presets, links
skills remove KEY --yes [--keep-meta]        # undeploys and deletes
skills set-source KEY REFERENCE [--branch B] [--subpath P]
skills accept KEY                            # current content becomes the baseline
```

`set-source` records a remote source for an existing skill without touching its files and without setting a baseline.

`remove --keep-meta` keeps the skill's metadata, tags, and preset membership, leaving a `missing` record you can restore.

### Tags

```sh
skills tag add KEY [TAG...]
skills tag remove KEY [TAG...]
skills tag set KEY [TAG...]      # no tags clears them
skills tag list [KEY]            # all tags with counts, or one skill's tags
skills tag rename OLD NEW [--merge]
skills tag delete TAG --yes
```

### Notes (repository skills only)

Local skills do not store notes; writing one is an error.

```sh
skills note get KEY
skills note set KEY TEXT
printf '%s' "$TEXT" | skills note set KEY -
skills note clear KEY
```

Skip `skills note edit`; it opens `$VISUAL`, `$EDITOR`, or `vi`.

### Upstream updates

```sh
skills check [KEY] [--all] [--repo ALIAS]
skills update [KEY] [--all] [--repo ALIAS] [--take local|upstream] [--force] [--dry-run]
```

Give one of `KEY`, `--all`, or `--repo ALIAS`. `--repo` cannot be combined with the other two; if both `KEY` and `--all` are given, `--all` wins.

`check` compares recorded revisions with upstream and changes nothing; with `--all`, per-skill failures land in the JSON `errors` array and the command still exits 0. `update` on a `modified` skill reports `needs-resolution` unless you choose a side: `--take local` keeps the local files unchanged (still `modified`), `--take upstream` (alias `--force`) replaces the whole directory with upstream and records a new baseline. There is no per-file merge. Batch updates continue past failures and exit 1 if any item failed.

### Presets

```sh
skills preset list
skills preset show NAME
skills preset create NAME [--description TEXT] [--skill KEY]... [--tag TAG]...
skills preset add NAME [KEY...] [--tag TAG]...
skills preset remove NAME [KEY...] [--tag TAG]...
skills preset rename OLD NEW [--merge]
skills preset describe NAME [TEXT]           # no TEXT clears it
skills preset delete NAME --yes
skills preset deploy NAME ([--agent AGENT]... | --all-agents) [--dry-run]
skills preset undeploy NAME ([--agent AGENT]... | --all-agents) [--dry-run]
```

`--tag` copies the tag's current members into the preset once; later tag changes do not follow.

Renaming a tag or preset to an existing name fails; `--merge` combines the skills into the existing one, keeps its colour and description, and removes the old one.

## Deploy

```sh
skills deploy KEY... ([--agent AGENT]... | --all-agents) [--dry-run]
skills undeploy KEY... ([--agent AGENT]... | --all-agents) [--dry-run]
```

Repeat the flag for several agents (`--agent a --agent b`; `-a` also works here); `--agent a b` would read `b` as a skill key. If `--all-agents` is also given, it wins. The agent must be configured (`skills agents list`). Each link is named after the skill's declared `name`. A dry run prints one line per action and a total:

```text
link   claude/pdf-tools -> /path/to/root/pdf-tools
skip   claude/review: already deployed
dry run: 1 change(s) not applied
```

With `--json` the result is `{dry_run, applied, actions, name_conflicts}`. Every action has `op` and `agent`; the rest depends on `op`: `mkdir` has `path`; `link` and `relink` have `skill`, `path`, `target`; `unlink` has `skill`, `path`, and sometimes `target`; `skip` has `skill` and `reason`.

If two skills declare the same name for one agent, a dry run still succeeds and lists the clash in `name_conflicts`, and the real run refuses. `--same-name replace` lets the run unlink the other skill's managed deployment. It cannot replace a foreign link or a real directory in the agent folder, and it cannot pick between two same-name skills selected in one command. Preview with `--dry-run --same-name replace` first.

## Agents and health

```sh
skills agents list
skills agents catalog                        # built-in agent paths
skills agents add BUILTIN
skills agents add KEY --dir DIR
skills agents status [AGENT]
skills agents clean AGENT [KEY...] [--dry-run | --yes]        # remove broken links
skills agents remove-link AGENT NAME [--dry-run | --yes]      # delete a foreign link, keep its target
skills agents adopt-link AGENT NAME [--dry-run | --yes]       # copy a foreign skill in, repoint the link
skills agents relink AGENT [KEY...] [--dry-run | --yes]       # replace identical copies with links
skills repair [--deployment AGENT/LINK=KEY]...                # preview only
skills repair --apply [--deployment AGENT/LINK=KEY]...
```

`remove-link` and `adopt-link` need `--yes` to apply; `clean` and `relink` need it only when there is something to change. `relink` deletes the agent's copies, so preview it. There is no command to unregister an agent; edit `[[agents]]` in `config.toml`.

## Git backup

```sh
skills sync status
skills sync configure URL [--branch BRANCH]  # enables automatic backup and syncs now
skills sync disable
skills sync [run] [--dry-run]                # commit, merge remote, push (configure and disable take no --dry-run)
skills sync push [--dry-run]                 # commit and push without pulling
skills sync pull [--dry-run]                 # commit and merge without pushing
```

## What each write does

| Command | Needs `--yes` | Preview | Triggers automatic backup |
| --- | --- | --- | --- |
| `init`, `install`, `adopt`, `rename`, `set-source`, `accept` | | `install --list` | Yes |
| `remove` | Yes | | Yes |
| `tag` writes, `note` writes, `repos rename` | `tag delete` only | | Yes |
| `update` | | `--dry-run` | Yes, unless `--dry-run` |
| `preset create`, `rename`, `add`, `remove`, `describe`, `delete` | `preset delete` only | | Yes |
| `agents add` | | | Yes |
| `agents adopt-link` | Yes | `--dry-run` | Yes |
| `deploy`, `undeploy`, `preset deploy/undeploy` | | `--dry-run` | No |
| `agents clean`, `remove-link`, `relink` | Yes, to apply | `--dry-run` | No |
| `repair --apply` | | `repair` without `--apply` | No |
| `sync configure`, `disable`, `run`, `push`, `pull` | | `--dry-run` on run, push, pull | Is the backup itself |

Automatic backup runs only when `skills sync status` shows `"enabled": true`, and it commits every pending non-ignored file in the root, not just the command's own changes.

## Exit codes and errors

`0` on success (an empty result is a success), `1` on a runtime or validation error, `2` on a syntax error such as an unknown flag. `check --all` reports per-skill failures in its output and exits 0; a batch `update` prints its report and then exits 1 if any item failed. Errors print as `error: <message>` on stderr; with `--json` they print as `{"error": "<message>"}`, except syntax errors, which stay plain text. Progress and warnings can appear on stderr even when stdout carries JSON.
