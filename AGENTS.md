# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 package builds `skills`, a CLI and Ratatui terminal interface for managing shared agent skills. Keep reusable behavior in `src/lib.rs` and its modules; keep `src/cli.rs` and `src/tui/` focused on presentation and interaction. Filesystem operations live in `src/ops/`, and individual TUI screens in `src/tui/views/`.

Integration tests live in `tests/`; unit tests also live alongside source code. Embedded dictionaries and license notices are in `data/dict/`, with their generator in `scripts/build-dictionary.py`. CI workflows are in `.github/workflows/`; release instructions are in `docs/releasing.md`.

## Build, Test, and Development Commands

Use the Rust toolchain pinned in the workflows (currently `1.98.0`). Git must be on `PATH` for repository operations and related tests.

- `cargo run --locked -- --help`: inspect CLI commands.
- `cargo run --locked -- --root /path/to/skills`: launch the TUI against an existing skills directory.
- `cargo build --locked --release`: build `target/release/skills`.
- `cargo fmt --all -- --check`: check formatting; use `cargo fmt --all` to apply it.
- `cargo clippy --locked --all-targets -- -D warnings`: run lint checks with warnings treated as errors.
- `cargo test --locked --all-targets`: run the CI test suite.
- `cargo test --locked --test install`: run one integration test target.

## Coding Style & Naming Conventions

Follow rustfmt defaults and four-space indentation. Use `snake_case` for modules, functions, and variables; `PascalCase` for types; and `SCREAMING_SNAKE_CASE` for constants. Follow existing `anyhow::Result` and contextual error handling patterns. Keep shared operations out of UI handlers.

## Testing Guidelines

Use Rust's built-in `#[test]` framework and descriptive behavior-based test names. Add regression tests for changed behavior, especially filesystem mutations, symlink safety, and update conflicts. Use isolated temporary fixtures with cleanup, never real agent directories. No numeric coverage threshold is configured. CI checks Linux and macOS.

## Commit & Pull Request Guidelines

Recent commits use forms such as `fix(tui): ...` and `feat(health): ...`; follow that style for focused changes. For PRs, describe the problem, resulting behavior, and validation performed; link relevant issues and include screenshots for visible TUI changes. Follow `docs/releasing.md` for version changes and releases.

## Configuration & Data Safety

Root resolution uses `--root`, then `SKILLS_HOME`, then `~/.config/skills-tui/root`. Metadata lives under `<root>/.skills-meta/`. Use disposable roots and agent paths when exercising install, deploy, repair, or removal commands.
