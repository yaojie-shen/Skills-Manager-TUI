# Editing the library outside the TUI

The TUI polls the root's supported directory layout, SKILL.md files, metadata,
and configured agent directory entries every two seconds while idle. It rescans
only when these change. Polling waits while a dialog or background task is active.
Ctrl+R remains available for an immediate full rescan, including changes made only
to scripts or other payload files inside a skill.

Deleted skills disappear from the normal Library and cannot be newly selected in
deployment or preset pickers. Their metadata remains in Health so tags, notes and
source information are not lost during a temporary move. Use Health's `x` action
on a missing record when you want to discard that information permanently.

If a missing skill's baseline uniquely matches an unmanaged directory, Health
shows `renamed?` with the destination. Press `m` on that record to complete the
move in one operation: transfer metadata, repair existing links that point exactly
to the old skill path, and update preset and registered installation references.
Real agent directories, unrelated links and occupied destinations are not replaced.
Destination conflicts are checked before writes. If a later filesystem write
fails, earlier repairs can remain; resolve the reported error and retry the same
old/new pair. The old metadata is retained until the references are updated.

Matching content is a suggestion, not proof of identity, so migration still
requires choosing the pair. With changed contents or ambiguous copies, specify it
explicitly:

```sh
skills --root /path/to/root migrate old-key local/new-key
```

The new directory must be discoverable in the supported root layout (a flat skill,
`local/name`, `local/group/name`, or `repos/repository/name`). Moving outside the
root is treated as removal. Migration does not silently delete old metadata or
unrelated broken links. Scanning itself remains read-only.
