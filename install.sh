#!/bin/sh
# Install the prebuilt skills binary from a GitHub Release.
#
# Quick start:
#   curl -fsSL https://raw.githubusercontent.com/yaojie-shen/Skills-Manager-TUI/main/install.sh | sh
#
# Install the latest main build (prerelease tag "nightly") instead:
#   curl -fsSL https://raw.githubusercontent.com/yaojie-shen/Skills-Manager-TUI/main/install.sh | sh -s -- --channel nightly
#
# Optional environment variables:
#   SKILLS_CHANNEL     stable (default) or nightly, the latest main build
#   SKILLS_VERSION     Release tag; default: GitHub's latest stable release
#   SKILLS_TARGET      Rust target triple, for example aarch64-apple-darwin
#   SKILLS_INSTALL_DIR Destination directory (default: ~/.local/bin)
#   SKILLS_REPO        GitHub owner/repository (default: yaojie-shen/Skills-Manager-TUI)
#   SKILLS_NO_PATH_UPDATE Set to 1 to skip shell profile updates

set -eu

[ -n "${HOME:-}" ] || {
    printf '%s\n' "skills installer: HOME is not set" >&2
    exit 1
}

REPO=${SKILLS_REPO:-yaojie-shen/Skills-Manager-TUI}
CHANNEL=${SKILLS_CHANNEL:-stable}
VERSION=${SKILLS_VERSION:-}
TARGET=${SKILLS_TARGET:-}
BIN_DIR=${SKILLS_INSTALL_DIR:-$HOME/.local/bin}

die() {
    printf '%s\n' "skills installer: $*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Install the prebuilt skills binary from GitHub Releases.

Usage:
  install.sh [--channel CHANNEL] [--version TAG] [--target TRIPLE] [--install-dir DIR]

Channels:
  stable  Latest stable release, or the release named by --version (default)
  nightly    Prerelease rebuilt from every push to main; cannot be combined
          with --version

Environment overrides:
  SKILLS_CHANNEL       stable (default) or nightly
  SKILLS_VERSION       Release tag; default: GitHub's latest stable release
  SKILLS_TARGET        Rust target triple, for example aarch64-apple-darwin
  SKILLS_INSTALL_DIR   Destination directory (default: ~/.local/bin)
  SKILLS_REPO          GitHub owner/repository
  SKILLS_NO_PATH_UPDATE Set to 1 to skip shell profile updates
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        --channel)
            [ "$#" -ge 2 ] || die "--channel needs a value (stable or nightly)"
            CHANNEL=$2
            shift 2
            ;;
        --channel=*)
            CHANNEL=${1#*=}
            shift
            ;;
        --version)
            [ "$#" -ge 2 ] || die "--version needs a release tag"
            VERSION=$2
            shift 2
            ;;
        --version=*)
            VERSION=${1#*=}
            shift
            ;;
        --target)
            [ "$#" -ge 2 ] || die "--target needs a Rust target triple"
            TARGET=$2
            shift 2
            ;;
        --target=*)
            TARGET=${1#*=}
            shift
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || die "--install-dir needs a directory"
            BIN_DIR=$2
            shift 2
            ;;
        --install-dir=*)
            BIN_DIR=${1#*=}
            shift
            ;;
        *)
            die "unknown option: $1 (try --help)"
            ;;
    esac
done

case "$REPO" in
    */*) ;;
    *) die "SKILLS_REPO must look like owner/repository" ;;
esac
case "$REPO" in
    *[!A-Za-z0-9._/-]*) die "SKILLS_REPO contains unsupported characters: $REPO" ;;
esac

fetch() {
    url=$1
    dest=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 --connect-timeout 10 \
            -H 'Accept: application/vnd.github+json' \
            -H 'User-Agent: skills-installer' \
            "$url" -o "$dest"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --tries=3 --timeout=10 \
            --header='Accept: application/vnd.github+json' \
            --header='User-Agent: skills-installer' \
            -O "$dest" "$url"
    else
        die "curl or wget is required"
    fi
}

sha256_file() {
    file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        die "sha256sum or shasum is required to verify the download"
    fi
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os:$arch" in
        Darwin:arm64|Darwin:aarch64) printf '%s\n' aarch64-apple-darwin ;;
        Darwin:x86_64|Darwin:amd64) printf '%s\n' x86_64-apple-darwin ;;
        Linux:x86_64|Linux:amd64) printf '%s\n' x86_64-unknown-linux-musl ;;
        Linux:aarch64|Linux:arm64) printf '%s\n' aarch64-unknown-linux-musl ;;
        *) die "unsupported platform: $os $arch (set SKILLS_TARGET to override)" ;;
    esac
}

path_contains() {
    case ":${PATH:-}:" in
        *:"$1":*) return 0 ;;
        *) return 1 ;;
    esac
}

pick_profile() {
    os_name=$(uname -s)
    case "$os_name:${SHELL:-}" in
        Darwin:*/zsh) printf '%s\n' "$HOME/.zprofile" ;;
        Darwin:*/bash) printf '%s\n' "$HOME/.bash_profile" ;;
        Linux:*/zsh) printf '%s\n' "$HOME/.zshrc" ;;
        Linux:*/bash) printf '%s\n' "$HOME/.bashrc" ;;
        *:*/fish) printf '%s\n' "$HOME/.config/fish/config.fish" ;;
        *) printf '%s\n' "$HOME/.profile" ;;
    esac
}

PATH_ACTION=already
PATH_PROFILE=''
ensure_path() {
    PATH_ACTION=already
    PATH_PROFILE=''

    if path_contains "$BIN_DIR"; then
        return
    fi

    if [ "${SKILLS_NO_PATH_UPDATE:-0}" = 1 ]; then
        PATH_ACTION=skipped
        printf '%s\n' "skills installer: $BIN_DIR is not in PATH (profile update skipped)" >&2
        return
    fi

    PATH_PROFILE=$(pick_profile)
    profile_dir=$(dirname "$PATH_PROFILE")
    mkdir -p "$profile_dir" || die "could not create shell profile directory: $profile_dir"
    if [ ! -e "$PATH_PROFILE" ]; then
        : > "$PATH_PROFILE" || die "could not create shell profile: $PATH_PROFILE"
    fi
    [ -w "$PATH_PROFILE" ] || die "shell profile is not writable: $PATH_PROFILE (set SKILLS_NO_PATH_UPDATE=1 to skip)"

    begin_marker="# >>> skills installer >>>"
    end_marker="# <<< skills installer <<<"
    shell_name=$(basename "${SHELL:-sh}")
    case "$shell_name" in
        fish) path_line="fish_add_path -U -- '$BIN_DIR'" ;;
        *) path_line="export PATH=\"$BIN_DIR:\$PATH\"" ;;
    esac

    if [ -f "$PATH_PROFILE" ] && grep -F "$begin_marker" "$PATH_PROFILE" >/dev/null 2>&1; then
        if grep -F "$path_line" "$PATH_PROFILE" >/dev/null 2>&1; then
            PATH_ACTION=configured
            return
        fi
        if grep -F "$end_marker" "$PATH_PROFILE" >/dev/null 2>&1; then
            tmp_profile="$PATH_PROFILE.skills.$$"
            awk -v begin="$begin_marker" -v end="$end_marker" -v line="$path_line" '
                $0 == begin {
                    print begin
                    print line
                    in_block = 1
                    next
                }
                in_block {
                    if ($0 == end) {
                        print end
                        in_block = 0
                    }
                    next
                }
                { print }
            ' "$PATH_PROFILE" > "$tmp_profile" \
                && mv "$tmp_profile" "$PATH_PROFILE" \
                || die "could not update shell profile: $PATH_PROFILE"
            PATH_ACTION=updated
            return
        fi
    fi

    {
        printf '\n%s\n' "$begin_marker"
        printf '%s\n' "$path_line"
        printf '%s\n' "$end_marker"
    } >> "$PATH_PROFILE" || die "could not update shell profile: $PATH_PROFILE"
    PATH_ACTION=added
}

case "$CHANNEL" in
    stable|nightly) ;;
    *) die "invalid channel: '$CHANNEL' (expected stable or nightly)" ;;
esac
if [ "$CHANNEL" = nightly ] && [ -n "$VERSION" ]; then
    die "--channel nightly cannot be combined with --version/SKILLS_VERSION ($VERSION); drop one of them"
fi

case "$VERSION" in
    '') ;;
    v*) ;;
    *) VERSION=v$VERSION ;;
esac

case "$VERSION" in
    *[!A-Za-z0-9._+-]*) die "invalid release tag: $VERSION" ;;
esac

if [ -z "$TARGET" ]; then
    TARGET=$(detect_target)
fi
case "$TARGET" in
    *[!A-Za-z0-9._-]*) die "invalid target triple: $TARGET" ;;
esac

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/skills-install.XXXXXX")
STAGED=""
cleanup() {
    if [ -n "$STAGED" ]; then
        rm -f "$STAGED"
    fi
    rm -rf "$TMP_DIR"
}
trap cleanup 0 1 2 15

ASSET="skills-$TARGET"
ARCHIVE_NAME="$ASSET.tar.gz"
if [ "$CHANNEL" = nightly ]; then
    # The nightly prerelease keeps a fixed tag and has its assets replaced on
    # every push to main.
    BASE_URL="https://github.com/$REPO/releases/download/nightly"
    RELEASE_LABEL='nightly build (latest main)'
elif [ -n "$VERSION" ]; then
    BASE_URL="https://github.com/$REPO/releases/download/$VERSION"
    RELEASE_LABEL=$VERSION
else
    # GitHub's /releases/latest redirect selects the latest non-prerelease
    # release without requiring a GitHub API request.
    BASE_URL="https://github.com/$REPO/releases/latest/download"
    RELEASE_LABEL='latest stable release'
fi
ARCHIVE="$TMP_DIR/$ARCHIVE_NAME"
CHECKSUMS="$TMP_DIR/SHA256SUMS"

NIGHTLY_HINT='the nightly build may not be published yet or is being republished; retry in a few minutes'

download_assets() {
    if ! fetch "$BASE_URL/$ARCHIVE_NAME" "$ARCHIVE"; then
        [ "$CHANNEL" != nightly ] || die "could not download $ARCHIVE_NAME from the nightly release: $NIGHTLY_HINT"
        die "no release asset for $RELEASE_LABEL/$TARGET"
    fi
    if ! fetch "$BASE_URL/SHA256SUMS" "$CHECKSUMS"; then
        [ "$CHANNEL" != nightly ] || die "could not download SHA256SUMS from the nightly release: $NIGHTLY_HINT"
        die "release $RELEASE_LABEL has no SHA256SUMS asset"
    fi
}

# Returns nonzero on a checksum mismatch; dies on any other problem.
archive_matches_checksum() {
    EXPECTED=$(awk -v name="$ARCHIVE_NAME" '
        { file = $2; sub(/^\*/, "", file); sub(/^\.\//, "", file); if (file == name) { print $1; exit } }
    ' "$CHECKSUMS")
    [ -n "$EXPECTED" ] || die "SHA256SUMS does not contain $ARCHIVE_NAME"
    # Callers run this in a condition, where set -e is suspended, so a
    # failing sha256_file must abort explicitly.
    ACTUAL=$(sha256_file "$ARCHIVE") || exit 1
    [ "$ACTUAL" = "$EXPECTED" ]
}

printf '%s\n' "skills installer: downloading $REPO $RELEASE_LABEL ($TARGET)" >&2
download_assets
if ! archive_matches_checksum; then
    [ "$CHANNEL" = nightly ] || die "checksum mismatch for $ARCHIVE_NAME"
    # The nightly release replaces assets in place, so the archive and
    # SHA256SUMS can briefly come from different builds. Refetch both once.
    printf '%s\n' "skills installer: checksum mismatch for $ARCHIVE_NAME on the nightly build; refetching once" >&2
    download_assets
    archive_matches_checksum \
        || die "checksum mismatch for $ARCHIVE_NAME on the nightly build: $NIGHTLY_HINT"
fi

tar -xzf "$ARCHIVE" -C "$TMP_DIR" \
    || die "could not unpack $ARCHIVE_NAME"
BINARY="$TMP_DIR/$ASSET/skills"
[ -f "$BINARY" ] || die "release archive does not contain $ASSET/skills"

mkdir -p "$BIN_DIR" || die "could not create install directory: $BIN_DIR"
ensure_path
STAGED="$BIN_DIR/.skills.tmp.$$"
cp "$BINARY" "$STAGED" || die "could not copy binary to $BIN_DIR"
chmod 755 "$STAGED" || die "could not make binary executable"
mv -f "$STAGED" "$BIN_DIR/skills" || die "could not install $BIN_DIR/skills"
STAGED=""

printf '%s\n' "skills installer: installed $BIN_DIR/skills"
case "$PATH_ACTION" in
    added|updated|configured)
        printf '%s\n' "skills installer: PATH $PATH_ACTION in $PATH_PROFILE" >&2
        printf '%s\n' "skills installer: open a new shell or run: export PATH=\"$BIN_DIR:\$PATH\"" >&2
        ;;
    skipped)
        printf '%s\n' "skills installer: run: export PATH=\"$BIN_DIR:\$PATH\"" >&2
        ;;
    already)
        ;;
esac
