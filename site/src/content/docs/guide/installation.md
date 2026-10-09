---
title: "Installation"
description: "Install a release binary or build Skills Manager from source."
---

## Install a release

Install the latest release on macOS or Linux:

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh
```

Install a specific release:

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh -s -- --version TAG
```

Install the latest `main` build:

```sh
curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh -s -- --channel nightly
```

This is a prerelease rebuilt on every push to `main`, intended for testing.

The installer checks the archive against the release's `SHA256SUMS`. It stops when the platform is unsupported or the release asset is missing.

Confirm the installation:

```sh
skills --version
skills --help
```

### Supported targets

| System | Architectures / targets |
| --- | --- |
| macOS | ARM64 (`aarch64-apple-darwin`), x86-64 (`x86_64-apple-darwin`) |
| Linux | ARM64 (`aarch64-unknown-linux-musl`), x86-64 (`x86_64-unknown-linux-musl`) |

The installer requires `curl` or `wget`, `tar`, and `sha256sum` or `shasum`. Native Windows is not supported, and unsupported targets do not fall back to a source build.

## Choose the destination

The default destination is `~/.local/bin/skills`. If that directory is not on `PATH`, open a new shell after installation or update the current one:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Available overrides:

| Option | Environment variable | Purpose |
| --- | --- | --- |
| `--channel CHANNEL` | `SKILLS_CHANNEL` | `stable` (default) or `nightly`, the latest `main` build. |
| `--version TAG` | `SKILLS_VERSION` | Install a specific release. |
| `--target TRIPLE` | `SKILLS_TARGET` | Select a release target. |
| `--install-dir DIR` | `SKILLS_INSTALL_DIR` | Change the install directory. |
| — | `SKILLS_REPO` | Download from another release repository. |
| — | `SKILLS_NO_PATH_UPDATE=1` | Leave shell profiles unchanged. |

## Build from source

From a repository checkout:

```sh
cargo build --release --locked
cargo install --path . --locked
```

The project requires Rust 1.98 or newer and uses edition 2024. Cargo chooses the install destination from its own configuration.

Git-backed imports and root backup require `git`. Archive URL imports require `curl` and support ZIP, TAR, TAR.GZ, TAR.BZ2, and TAR.XZ. Git processes launched by Skills Manager do not prompt for credentials in the terminal.

Node is needed only to maintain this documentation site, not to run `skills`.
