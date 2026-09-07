# Building and releasing

GitHub Actions builds two downloadable packages:

| Platform | Rust target | Runner |
| --- | --- | --- |
| Linux x86_64, static musl | `x86_64-unknown-linux-musl` | Ubuntu 24.04 |
| macOS Apple Silicon, macOS 11+ | `aarch64-apple-darwin` | macOS 15 ARM64 |

The workflows pin Rust 1.98.0 and use `Cargo.lock` with `--locked`. Update the
`RUST_TOOLCHAIN` value in both workflows together when upgrading Rust.

## Continuous integration

Pushes and pull requests to `main` or `dev` run formatting, Clippy, tests, and
an optimized build on Linux and macOS. CI does not create a Release.

## Publish a new version

1. Change `[package].version` in `Cargo.toml`, then run `cargo check` to update
   the package version in `Cargo.lock`. Commit both files on `dev`.
2. Merge the reviewed changes into `main` and push it.
3. Tag the intended commit with exactly `v` plus the Cargo version:

   ```bash
   git switch main
   git tag -a v0.1.0-alpha.2 -m 'Second alpha'
   git push origin v0.1.0-alpha.2
   ```

The Release workflow checks that the tag matches the source version, tests both
targets, builds and smoke-tests the binaries, and verifies that the Linux binary
has no ELF interpreter or shared-library dependencies. Only after both builds
succeed does it create a draft, upload both packages and `SHA256SUMS`, and publish.

Versions with a SemVer prerelease suffix, such as `-alpha.1`, `-beta.1`, or
`-rc.1`, become prereleases and do not replace the latest stable release.
Versions without a suffix become stable releases and are marked latest.

The workflow uses GitHub's automatic `GITHUB_TOKEN`; no personal token or
repository secret is required. Only the publishing job has `contents: write`.
macOS binaries are not Developer ID signed or notarized.

## Build an existing tag

Adding a workflow does not retroactively trigger builds for tags already pushed.
Once these workflow files are on `main`, open **Actions → Release → Run workflow**,
select **main**, and enter the existing tag, for example `v0.1.0-alpha.1`.
This runs the workflow from `main` but builds the exact commit referenced by the
tag, even if that older commit does not contain these workflow files.

A failed run can be rerun; an existing draft can be completed. An already
published Release is deliberately not overwritten. Publish a new version when
changing released binaries. Do not move an existing version tag.

## Download and install

Each `.tar.gz` contains an executable named `skills`, the project license, and
the notices for its embedded dictionaries. Archive names include the version
and Rust target, for example:

```text
skills-v0.1.0-alpha.1-x86_64-unknown-linux-musl.tar.gz
skills-v0.1.0-alpha.1-aarch64-apple-darwin.tar.gz
SHA256SUMS
```

Download both archives to check the complete checksum file with
`sha256sum -c SHA256SUMS` (Linux) or `shasum -a 256 -c SHA256SUMS` (macOS).
If downloading just one platform, compare that archive's SHA-256 with its line
in `SHA256SUMS`. Extract the matching package and install its `skills` executable
into a directory on `PATH`, then run `skills --version`. Git-source operations
also require `git` on `PATH`.

Workflow behavior follows the official [manual trigger documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_dispatch)
and [GitHub CLI release commands](https://cli.github.com/manual/gh_release_create).
