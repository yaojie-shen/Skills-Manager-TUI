# Library structure

How a Skills Manager Library is laid out on disk, how the manager finds skills in it, and which files you may edit directly.

## Layout

```text
<root>/
├── <name>/SKILL.md                 local skill, flat
├── local/                          optional: grouped local skills, any depth
│   └── <group>/.../<name>/SKILL.md
├── repos/                          skills installed from a source
│   └── <alias>/<name>/SKILL.md
├── AGENTS.md, CLAUDE.md, ...       optional files for the Library's own maintainers
├── .git/                           present when Git backup is configured
└── .skills-meta/
    ├── config.toml                 agents, search, UI, sync timing (hand-editable)
    ├── format.toml                 layout version (manager-owned; never edit)
    ├── repos/<alias>.toml          source, revision, baseline, note per installed skill
    ├── repos/.root.toml            source records for skills outside repos/
    ├── tags/<tag>.toml             one tag definition each
    ├── presets/<preset>.toml       one preset each
    ├── dictionary.tsv              optional search dictionary (hand-editable)
    ├── .staging/                   in-progress installs and metadata writes
    ├── .metadata.lock              best-effort metadata lock
    ├── backups/                    copies saved before a layout migration
    ├── .sync/, .repair-backups/    other runtime state
```

Each skill's **key** is its path relative to the root: `pdf-tools`, `local/writing/pdf-tools`, `repos/acme/pdf-tools`. Commands take keys. The **declared name** is the `name` in its `SKILL.md`; deployment links use the declared name, so the two may differ.

A root without `.skills-meta/config.toml` still works with defaults; `skills init` writes a commented config you can edit. The manager writes `format.toml` when it opens existing metadata that needs a layout declaration or migration, so an empty root may have none. A Library whose declared layout is newer than the installed binary refuses to open until the binary is upgraded. The runtime entries are never synced and never edited by hand.

## Where a skill can live

The scan looks at exactly these places:

- **Top level.** Every non-hidden directory directly in the root is a skill candidate, except `local/` and `repos/` (when they have no `SKILL.md` of their own). A top-level folder without `SKILL.md` shows up as `invalid: missing SKILL.md`; the manager does not look inside it for nested skills.
- **Under `local/`.** Folders nest to any depth. The scan stops at each directory that has a `SKILL.md`, so a skill inside another skill is not found. An empty leaf folder with no `SKILL.md` shows up as invalid.
- **Under `repos/`.** Only `repos/<alias>/<name>` is scanned, exactly two levels. `skills install` creates these. A directory you place there by hand has no source record and shows as `missing-source`, so put hand-written skills at the top level or under `local/`.

Hidden names (starting with `.`) are skipped everywhere.

## `SKILL.md` rules

```markdown
---
name: pdf-tools
description: Fill, merge, and split PDF files. Use when a task reads or edits a PDF.
---
```

- The file must start with a `---` line and close the frontmatter with another `---`.
- `name` is required: 1 to 64 lowercase ASCII letters, digits, and hyphens, with no hyphen at the start or end and no `--`.
- `description` is optional to the manager, but agents load skills based on it, so always write one.
- Other top-level fields are allowed and ignored by the manager. The parser handles simple `key: value` lines, quoted strings, and `>`/`|` block scalars, not arbitrary YAML.
- The directory name does not have to equal `name`.

## Skill statuses

`skills show KEY` (`status:` line) and `skills status` report one of:

| Status | Meaning | Usual action |
| --- | --- | --- |
| `local` | Valid skill with no source record | None |
| `repository` | Installed skill whose content matches its recorded baseline | None |
| `modified` | Installed skill whose files changed since the baseline | `accept` to keep, or `update --take upstream` to discard |
| `missing-baseline` | Source recorded but no baseline hash | `accept` |
| `missing-source` | Valid content under `repos/` with no source record | `set-source`, or move it out with `rename` |
| `missing` | Metadata, a tag, or a preset names a key whose directory is gone | Restore the directory, or `remove KEY --yes` |
| `renamed?` | A missing skill's baseline matches content found at another key, usually a hand-moved directory | Move the directory back, then `skills rename OLD NEW` |
| `invalid: <reason>` | `SKILL.md` cannot be loaded | Fix the file |
| `corrupt-meta` | The source record cannot be parsed | Fix or restore `.skills-meta/repos/<alias>.toml` |

Invalid reasons: `missing SKILL.md`, `cannot read SKILL.md: ...`, `no YAML frontmatter`, `unterminated YAML frontmatter`, ``frontmatter has no `name` ``, and `invalid skill name: use 1–64 lowercase letters, digits and single hyphens`.

The content hash behind `modified` covers file paths, file contents, and symlink targets. It ignores timestamps, permissions, `.git`, `__pycache__`, `.DS_Store`, and `*.pyc`.

## Editing by hand

| Change | By hand | Use instead |
| --- | --- | --- |
| Write or edit a local skill's files | Yes | |
| Edit an installed skill's files | Yes, but it becomes `modified` | Then `accept`, or tell the user |
| Rename a skill directory | No: orphans metadata, tags, presets, links | `skills rename OLD NEW` |
| Delete a skill directory | No: leaves `missing` records and broken links | `skills remove KEY --yes` |
| Move a skill into `repos/` | No | `skills install` |
| `config.toml`, `dictionary.tsv` | Yes | |
| Tags and presets TOML | Possible, but easy to get wrong | `skills tag ...`, `skills preset ...` |
| `repos/*.toml` source records | Avoid: a bad edit gives `corrupt-meta` | `set-source`, `accept`, `note` |
| `format.toml`, runtime paths | Never | |

A tag or preset file must stay valid TOML in the current schema. Tag files reject unknown fields. After a Git merge the manager tolerates odd filenames and renames them on its next write.

## Agent directories

`skills agents list` shows each configured agent's key and skills directory. `skills agents catalog` lists the built-in agents that `skills agents add KEY` can register; any other directory needs `skills agents add KEY --dir DIR`. Entries in an agent directory are reported as:

- `deployed`: a symlink to a Library skill under that skill's current declared name.
- `broken`: a symlink whose target is gone. `skills repair` relinks it when its name matches exactly one Library skill; `skills agents clean AGENT` removes it.
- `foreign`: a symlink that is not a current managed deployment, such as a link to somewhere outside the Library or a link left under a skill's old declared name. `skills repair` fixes the renamed case; `skills agents adopt-link` copies an outside skill into the Library and repoints the link; `skills agents remove-link` removes only the link.
- `shadow`: a real directory named like a Library skill's declared name. `skills agents relink` replaces it with a link only when its content is identical.
- `agent-only`: a real directory with no Library counterpart. `skills adopt PATH` moves it into the Library and leaves a link in its place.

Write skills into the Library, never into an agent directory.

## Project stores

`<project>/.agents/skills` is a separate, project-scoped store with the same layout. CLI commands use it with `--local` (current directory) or `--project DIR`; these cannot be combined with `--root`. Read commands fail if the store does not exist yet; `init`, `install`, and `agents add` create it. Agent directories for a project store default to paths inside the project, and the manager refuses paths that escape it.

In the TUI, `--project DIR` and `--local` only choose where deployments go; the TUI still opens the global Library.

## Git backup

`skills sync configure URL [--branch BRANCH]` makes the root a Git working tree and turns on automatic backup. Sync stages every non-ignored file in the root, commits, merges the remote, and pushes without force; a merge conflict stops it and keeps the local commit. Runtime paths are excluded through `.git/info/exclude`; a root `.gitignore`, if present, belongs to the user and is synced like any other file. The remote, branch, and on/off switch live in the root's `.git/config`, so each clone is configured separately.
