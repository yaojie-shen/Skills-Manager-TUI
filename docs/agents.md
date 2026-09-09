# Agent directories and project installation

`skills --local` uses `<current directory>/.agents/skills` as the skill root.
`skills --project /path/to/project` uses that project's `.agents/skills`, regardless
of the current working directory. Existing skills there are discovered directly.
Metadata, presets, history and configuration live inside this root's `.skills-meta`.
The project must exist. `init`, `install` and `agents add` create its skills root.
Run `init` once before opening the TUI in a new project.

```sh
skills --local init
skills --local agents add cursor
skills --local install ./my-skill --deploy cursor --deploy codex
skills --local

skills --project /path/to/project agents list
skills --project /path/to/project deploy my-skill --agent claude --dry-run
skills --project /path/to/project sync --dry-run
```

Without either flag, global behavior is unchanged: `--root`, then `SKILLS_HOME`,
then `~/.config/skills-tui/root`. `--root` cannot be combined with project scope.
Local scope does not read the global root pointer or global configuration.
Relative agent paths in local configuration are relative to the project, including
after a TUI refresh. Paths escaping the project, including existing symlink
ancestors, are rejected. This tool still deploys symlinks: moving a project to
another machine may require repairing links; this is not a portable copy installer.

## Built-in agents

```sh
skills agents catalog                  # works without a configured root
skills agents add windsurf             # opt in for the global store
skills --local agents add trae         # opt in for this project
skills --local agents add custom --dir .custom/skills
```

The catalog includes Claude Code, Codex, Cursor, GitHub Copilot, Gemini CLI,
OpenCode, Windsurf, Trae, Trae CN, Cline, Roo Code, Continue, Kilo Code, Amp,
Qwen Code, Kimi Code CLI, Kiro CLI, Droid and Augment. `agents catalog --json`
lists every default global/project path. Existing configurations are preserved;
new stores retain Claude and Codex as their default configured agents. Adding an
agent only writes configuration; the existing all-to-all sync policy will include
it on the next `sync`. Review with `sync --dry-run`.

Use `--dir` for customized locations, including installations configured with
`CODEX_HOME`, `CLAUDE_CONFIG_DIR` or `XDG_CONFIG_HOME`; catalog paths are defaults
and do not automatically follow those environment variables.

Agents sharing `.agents/skills` see the same source skills. A source skill cannot
be disabled for just one of those agents; `undeploy` preserves it and explains
why. Use `remove` only when you intend to delete it from the shared store.
Repository skills keep their existing `repos/<repository>/<skill>` identities;
deploying one to a reader of the shared root creates a top-level alias. The alias
is not indexed as a second skill. Sync combines desired skills for agents that
read the same directory.

## Directory sources

Checked on 2026-09-08. These are skill locations, not rules or commands directories.
Some agents also recognize compatibility paths; the catalog chooses one location.

- [Claude Code skills](https://code.claude.com/docs/en/skills)
- [Cursor skills](https://prod.cursor.com/docs/skills)
- [VS Code / GitHub Copilot skills](https://code.visualstudio.com/docs/agent-customization/agent-skills)
- [OpenCode skills](https://opencode.ai/v2/docs/skills)
- [Vercel Skills agent registry implementation](https://github.com/vercel-labs/skills/blob/main/src/agents.ts)

The registry supplies the remaining path mappings, including Codex's project
`.agents/skills`, Windsurf's global `.codeium/windsurf/skills` and Trae's project
`.trae/skills`. Native Cursor, Copilot and OpenCode project paths are chosen from
their documentation instead of the registry's shared compatibility path.

## TUI scope and agent selection

Click **Global** / **Local** in the TUI header, or press **F6**, to switch the
workspace being viewed. Local uses the project supplied by `--project`, or the
working directory from which the TUI was launched. The Agents page discovers
existing built-in agent directories inside that project, including agent-owned
skills absent from the shared root. Each workspace keeps its own undo history.
Switching waits for background work to finish.

Right-click a skill (or press `d`) to open **Install to agents**. This also opens
after importing a new skill. Choose Global (home) or Local (project), edit the
project path with `p` if needed, and select agents with Space or a mouse click.
The dialog shows the source store and the selected agent's target directory.
Click Apply or press Ctrl+Enter to write; Esc/Cancel makes no deployment changes.
Switching the destination scope discards the pending selection in that dialog.
Agents sharing a target directory toggle together because their installations
cannot be independent.

This dialog deploys symlinks from the currently viewed skill store; choosing
Global does not copy a local source into a second central store. Keep that source
project available while using those global links. Explicit destinations are
recorded in `.skills-meta/deployment-targets.toml` so subsequent scans, removals
and undo can find them. `sync` preserves these destinations' existing manual
selections instead of adding every skill merely because a new target was chosen.
