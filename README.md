# Skills Manager

`skills` stores Agent Skills in one Library and links them into supported coding agents. You can manage the Library through the TUI or CLI without maintaining a separate copy for each agent.

## How it works

```text
Skill sources → central Library → symbolic links → coding agents
                        │
                        └── optional Git backup
```

- Run `skills` for the terminal interface or `skills --help` for CLI commands.
- Configure a remote if you want to back up the Library with Git. A merge conflict aborts the merge and leaves the local backup commit intact.

See the [user guide](site/src/content/docs/guide/index.md) or [deployment guide](site/src/content/docs/guide/deployment.mdx).

## Install

For macOS and Linux:

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh
```

Prebuilt binaries are available for macOS and Linux on arm64 and x86_64. This command downloads and runs [install.sh](install.sh). Review the script before running it if required by your environment. See [installation](site/src/content/docs/guide/installation.md) for prerequisites and alternatives.

## Create your Library

Create the directory that will hold your skills:

```sh
mkdir -p ~/.skills
export SKILLS_HOME="$HOME/.skills"
skills init
skills
```

This `SKILLS_HOME` value applies only to the current shell. Add the export to your shell configuration to use the same location in new terminals.

See [Quick start](site/src/content/docs/guide/quickstart.mdx) for the full setup procedure. To list commands, run:

```sh
skills --help
```

## Optional Git backup

Replace `URL` with the URL of your backup repository:

```sh
skills sync configure URL --branch main
```

Review [root backup and sync](site/src/content/docs/guide/sync.md) before enabling it. Sync stages unignored changes across the Library root, while deployment changes links in Agent directories.

## Documentation

| Task | Guide |
| --- | --- |
| Install and initialize | [Installation](site/src/content/docs/guide/installation.md) · [Quick start](site/src/content/docs/guide/quickstart.mdx) |
| Understand sources and deployment | [Library](site/src/content/docs/guide/library.mdx) · [Deployment](site/src/content/docs/guide/deployment.mdx) |
| Organize and maintain | [Tags and presets](site/src/content/docs/guide/tags-presets.mdx) · [Updates](site/src/content/docs/guide/updates.md) · [Health](site/src/content/docs/guide/health.mdx) |
| Configure and troubleshoot | [Configuration](site/src/content/docs/guide/configuration.md) · [Reference](site/src/content/docs/guide/reference.md) · [Troubleshooting](site/src/content/docs/guide/troubleshooting.md) |

## Develop

The application is written in Rust. See `Cargo.toml` for the required toolchain.

```sh
cargo build
cargo test
```

## License

[MIT](LICENSE)
