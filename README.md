# Skills Manager

`skills` manages Agent Skills from one central Library and deploys them to
coding agents with symbolic links. It provides both a terminal UI and a CLI.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/yaojie-shen/Skills-Manager-TUI/main/install.sh | sh
```

Prebuilt binaries are available for macOS and Linux on arm64 and x86_64.

## Set up

Choose an existing directory for the Library:

```sh
mkdir -p ~/.skills
export SKILLS_HOME="$HOME/.skills"
skills init
skills
```

## Root sync

```sh
skills sync configure URL --branch main
```

When enabled, Library changes schedule a Git backup, pull, and push. Startup,
read-only commands, and Agent-only deployment changes do not trigger sync.
Conflicts stop the operation without force-pushing or resetting local work.

## License

[MIT](LICENSE)
