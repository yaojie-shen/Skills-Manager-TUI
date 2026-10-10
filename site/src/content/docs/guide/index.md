---
title: "Overview"
description: "Understand the Library, sources, deployments, and safety boundaries."
---

Skills Manager stores Agent Skills in a **Library** that you choose, then deploys them through filesystem links. The `skills` executable includes both the CLI and TUI. Run it without a subcommand to open the TUI.

## Core terms

| Term | Meaning |
| --- | --- |
| Library / root | The directory that contains skill files and `.skills-meta`. Each skill is a directory with a valid, readable `SKILL.md`. |
| Source | A Git repository, archive URL, or local directory. Skills imported from a remote source retain its details and a content baseline. |
| Deployment | A link in an Agent directory that points to a Library skill. Editing files through the link changes the Library copy. |
| Tags / presets | Tags organize skills. Presets store a fixed set of skills and deployment targets; their membership does not change with a tag. |

## Common tasks

- [Install Skills Manager](./installation/) and [create a Library](./quickstart/).
- [Import skills](./library/) and [deploy them to Agents](./deployment/).
- [Organize skills with tags and presets](./tags-presets/).
- [Check updates](./updates/), [repair problems](./health/), and [back up the Library](./sync/).

## Safety boundaries

- Scans and previews do not apply repairs or deployments. Opening metadata written by an older version may still upgrade it, with a backup.
- Git status probes may contact the remote, but never commit, merge, or push.
- Automatic root sync is separate and runs only after configured Library mutations.
