#!/usr/bin/env bash
# scripts/release.sh — Automated GitHub release builder & uploader for deploytix.
#
# Pipeline:
#   1. Derive a release tag from `git describe` (same scheme as PKGBUILD pkgver()).
#   2. Build packages with `makepkg` in pkg/ (CLI, GUI, debug).
#   3. Stage artifacts under releases/<tag>/  (.pkg.tar.zst, PKGBUILD, .SRCINFO, SHA256SUMS, RELEASE_NOTES.md).
#   4. Create + push the git tag if it doesn't already exist on the remote.
#   5. Publish via `gh release create` and upload all staged assets.
#
# Usage:
#   scripts/release.sh                  # build + tag + publish
#   scripts/release.sh --tag v1.3.1     # use an explicit tag
#   scripts/release.sh --no-build       # reuse artifacts already in releases/<tag>/
#   scripts/release.sh --no-tag         # skip tag creation/push (tag must already exist)
#   scripts/release.sh --no-publish     # stage + tag only, skip gh upload
#   scripts/release.sh --draft          # publish as draft
#   scripts/release.sh --prerelease     # mark as pre-release
#   scripts/release.sh --dry-run        # print actions, change nothing
#   scripts/release.sh --notes FILE     # use FILE as release notes (default: auto-generated)
#   scripts/release.sh --remote NAME    # git remote to push tag to (default: origin)
#
# Requires: makepkg, gh (authenticated), git, sha256sum.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd)"
PKG_DIR="$REPO_ROOT/pkg"
RELEASES_DIR="$REPO_ROOT/releases"

# --- args -------------------------------------------------------------------
TAG=""
NOTES_FILE=""
REMOTE="origin"
DO_BUILD=1
DO_TAG=1
DO_PUBLISH=1
DRY_RUN=0
DRAFT=0
PRERELEASE=0

usage() { sed -n '2,30p' "$0"; exit "${1:-0}"; }

while (($#)); do
    case "$1" in
        --tag)        TAG="$2"; shift 2 ;;
        --notes)      NOTES_FILE="$2"; shift 2 ;;
        --remote)     REMOTE="$2"; shift 2 ;;
        --no-build)   DO_BUILD=0; shift ;;
        --no-tag)     DO_TAG=0; shift ;;
        --no-publish) DO_PUBLISH=0; shift ;;
        --draft)      DRAFT=1; shift ;;
        --prerelease) PRERELEASE=1; shift ;;
        --dry-run|-n) DRY_RUN=1; shift ;;
        -h|--help)    usage 0 ;;
        *) echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==> warn:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m==> error:\033[0m %s\n' "$*" >&2; exit 1; }
run()  { if ((DRY_RUN)); then printf '   [dry-run] %s\n' "$*"; else eval "$@"; fi; }

for cmd in git makepkg sha256sum; do
    command -v "$cmd" >/dev/null || die "$cmd is required"
done
((DO_PUBLISH)) && { command -v gh >/dev/null || die "gh CLI is required for --publish"; }

cd "$REPO_ROOT"

# --- derive tag -------------------------------------------------------------
# PKGBUILD pkgver scheme:  v1.2.6-r9-ge34a93c  -->  1.2.6.r9.ge34a93c
# Release tag scheme:      v<pkgver>            (we keep dashes for readability)
if [[ -z "$TAG" ]]; then
    raw="$(git describe --tags --abbrev=7 2>/dev/null || true)"
    [[ -n "$raw" ]] || die "no git tags found; pass --tag explicitly"
    TAG="$raw"
fi
PKGVER="${TAG#v}"; PKGVER="${PKGVER//-/.}"   # for naming staged artifacts
log "release tag:   $TAG"
log "package ver:   $PKGVER"

STAGE_DIR="$RELEASES_DIR/$TAG"

# --- build ------------------------------------------------------------------
if ((DO_BUILD)); then
    log "building packages with makepkg in pkg/"
    run "rm -f '$PKG_DIR'/*.pkg.tar.zst"
    run "cd '$PKG_DIR' && makepkg -sCf --noconfirm"
fi

# --- stage ------------------------------------------------------------------
log "staging artifacts in $STAGE_DIR"
run "mkdir -p '$STAGE_DIR'"

if ((DO_BUILD)); then
    shopt -s nullglob
    pkgs=("$PKG_DIR"/*.pkg.tar.zst)
    shopt -u nullglob
    ((${#pkgs[@]})) || die "makepkg produced no .pkg.tar.zst files in $PKG_DIR"
    for p in "${pkgs[@]}"; do
        run "cp -f '$p' '$STAGE_DIR/'"
    done
    run "cp -f '$PKG_DIR/PKGBUILD' '$STAGE_DIR/PKGBUILD'"
    if (cd "$PKG_DIR" && makepkg --printsrcinfo) >/dev/null 2>&1; then
        if ((DRY_RUN)); then
            printf '   [dry-run] (cd %s && makepkg --printsrcinfo) > %s/.SRCINFO\n' "$PKG_DIR" "$STAGE_DIR"
        else
            (cd "$PKG_DIR" && makepkg --printsrcinfo) > "$STAGE_DIR/.SRCINFO"
        fi
    fi
fi

# --- checksums --------------------------------------------------------------
log "generating SHA256SUMS"
if ((DRY_RUN)); then
    printf '   [dry-run] sha256sum * > %s/SHA256SUMS\n' "$STAGE_DIR"
else
    (
        cd "$STAGE_DIR"
        files=()
        for f in *.pkg.tar.zst PKGBUILD .SRCINFO; do
            [[ -e "$f" ]] && files+=("$f")
        done
        ((${#files[@]})) || die "nothing to checksum in $STAGE_DIR"
        sha256sum "${files[@]}" > SHA256SUMS
    )
fi

# --- release notes ----------------------------------------------------------
NOTES="$STAGE_DIR/RELEASE_NOTES.md"
if [[ -n "$NOTES_FILE" ]]; then
    run "cp -f '$NOTES_FILE' '$NOTES'"
elif [[ ! -f "$NOTES" ]]; then
    log "auto-generating $NOTES (edit before publish if desired)"
    commit="$(git rev-parse --short=7 HEAD)"
    if ((DRY_RUN)); then
        printf '   [dry-run] write %s\n' "$NOTES"
    else
        cat > "$NOTES" <<EOF
# Deploytix $TAG

Snapshot of \`main\` at commit [\`$commit\`](https://github.com/MasterGenotype/Deploytix/commit/$(git rev-parse HEAD)). Built with \`makepkg\` on Arch/Artix Linux.

## Package version

\`$PKGVER-1\` (x86_64)

## Install

\`\`\`sh
# CLI only
sudo pacman -U deploytix-git-${PKGVER}-1-x86_64.pkg.tar.zst

# GUI (depends on the CLI package)
sudo pacman -U \\
  deploytix-git-${PKGVER}-1-x86_64.pkg.tar.zst \\
  deploytix-gui-git-${PKGVER}-1-x86_64.pkg.tar.zst
\`\`\`

## Build it yourself

\`\`\`sh
curl -LO https://github.com/MasterGenotype/Deploytix/releases/download/$TAG/PKGBUILD
makepkg -sCf
\`\`\`

## Checksums

See \`SHA256SUMS\`.
EOF
    fi
else
    log "using existing $NOTES"
fi

# --- tag --------------------------------------------------------------------
if ((DO_TAG)); then
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        log "tag $TAG already exists locally"
    else
        log "creating annotated tag $TAG"
        run "git tag -a '$TAG' -m 'Release $TAG'"
    fi
    if git ls-remote --exit-code --tags "$REMOTE" "$TAG" >/dev/null 2>&1; then
        log "tag $TAG already exists on $REMOTE"
    else
        log "pushing tag $TAG to $REMOTE"
        # Explicit src:dst refspec — git 2.54.0 fails the shorthand forms
        # (`git push origin <tag>` / `git push origin tag <name>`) with
        # "cannot be resolved to branch" when inferring the destination.
        run "git push '$REMOTE' 'refs/tags/$TAG:refs/tags/$TAG'"
    fi
fi

# --- publish ----------------------------------------------------------------
if ((DO_PUBLISH)); then
    log "publishing GitHub release $TAG"
    args=(release create "$TAG" --title "$TAG" --notes-file "$NOTES")
    ((DRAFT))      && args+=(--draft)
    ((PRERELEASE)) && args+=(--prerelease)

    shopt -s nullglob dotglob
    assets=("$STAGE_DIR"/*.pkg.tar.zst "$STAGE_DIR/PKGBUILD" "$STAGE_DIR/.SRCINFO" "$STAGE_DIR/SHA256SUMS")
    shopt -u nullglob dotglob
    real_assets=()
    for a in "${assets[@]}"; do [[ -e "$a" ]] && real_assets+=("$a"); done

    if gh release view "$TAG" >/dev/null 2>&1; then
        warn "release $TAG already exists; uploading assets with --clobber"
        run "gh release upload '$TAG' ${real_assets[*]@Q} --clobber"
    else
        run "gh ${args[*]@Q} ${real_assets[*]@Q}"
    fi
    log "done — https://github.com/MasterGenotype/Deploytix/releases/tag/$TAG"
else
    log "staged release in $STAGE_DIR (skip publish)"
fi
