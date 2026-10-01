---
title: "Updates and baselines"
description: "Check upstream changes and choose whether to keep local or upstream content."
---

`skills check` compares installed remote skills with their recorded upstream revisions. It never changes Library content. The examples below continue with the `review` skill installed in [Library and sources](../library/); its full Library key is `repos/toolbox/review`.

## 1. Check

```sh
skills check repos/toolbox/review
skills check --repo toolbox
skills check --all
```

Repository-wide and all-skills checks update the status of installed skills only. They may report newly discovered upstream skills, but do not install them.

## 2. Preview

```sh
skills update repos/toolbox/review --dry-run
```

The preview may download upstream content for comparison, but does not apply it to the Library.

## 3. Resolve local changes

If both the upstream and local copies changed, choose which complete copy to keep:

| Choice | Content after update | Revision and baseline | Later checks |
| --- | --- | --- | --- |
| `--take local` | Keeps local content | Unchanged | May report the same upstream update again |
| `--take upstream` | Replaces the complete skill and removes local-only files | Updated | Compares against the new upstream state |

```sh
skills update repos/toolbox/review --take local
skills update repos/toolbox/review --take upstream
```

Without either choice, the result is `needs-resolution`. `--force` is an alias for `--take upstream`. Per-file merging with `--take-file` is not supported. Back up any local edits you need before taking the upstream copy.

Invalid upstream content or an unexpected declared-name change stops replacement and preserves the installed copy and deployments.

## 4. Apply

```sh
skills update repos/toolbox/review
```

If the revision did not change, Skills Manager leaves the skill in place instead of reinstalling it. An applied update may trigger the configured [root backup](../sync/).

## Accept local content or change its source

```sh
skills accept KEY
skills set-source KEY REFERENCE --subpath PATH
```

`skills accept` records the current content as the baseline without changing the source revision or uploading the edits. `skills set-source` changes the recorded source without replacing content or creating a new baseline. Run `skills show` and `skills status` afterward to inspect the result.

## What the baseline records

Remote skills store an installed revision and a content hash. Git sources use repository revisions; archives use content hashes. A newer repository revision does not imply that every installed skill changed.

The hash covers relative paths, file contents, and symlink targets. It ignores timestamps, permissions, `.git`, `__pycache__`, `.DS_Store`, and `*.pyc`. The hash detects changes but cannot restore files. Local skills have no remote baseline until a source is recorded.
