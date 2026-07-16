#!/usr/bin/env bash
#
# build-deploytix-iso.sh — Build a custom Artix Linux ISO with deploytix pre-installed
#
# Usage: ./build-deploytix-iso.sh [OPTIONS]
#
# Requires: artools (buildiso), makepkg, repo-add, go-yq
# Must be run from the Deploytix repository root or the iso/ directory.
# Run 'git submodule update --init --recursive' once after cloning to populate vendor/.

set -euo pipefail

# ── Colour helpers ───────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
msg()  { printf "${GREEN}==> %s${NC}\n" "$*"; }
msg2() { printf "${BLUE}  -> %s${NC}\n" "$*"; }
warn() { printf "${YELLOW}==> WARNING: %s${NC}\n" "$*"; }
err()  { printf "${RED}==> ERROR: %s${NC}\n" "$*" >&2; }
die()  { err "$@"; exit 1; }

# ── Defaults ─────────────────────────────────────────────────────────────────
INITSYS="runit"
INCLUDE_GUI=true
BASE_DE_PROFILE="plasma"
SKIP_REBUILD=false
CLEAN_BUILD=true
CHROOT_ONLY=false
DRY_RUN=false
RESET_MODE=false
# BUILD_SOURCE controls where makepkg fetches each package's source tree from:
#   local  — git+file:// pointing at the vendor/ submodule on disk (default, no network needed)
#   clone  — fetch fresh from the upstream remote URLs (validates published state; needs SSH keys)
BUILD_SOURCE="local"
KEEP_PACKAGES=false   # -K: keep built .pkg.tar.zst files after ISO creation

# ── Paths (resolved later) ──────────────────────────────────────────────────
REPO_ROOT=""
ISO_DIR=""
PKG_DIR=""
LOCAL_REPO_DIR=""
PROFILE_SRC=""
WORKSPACE_DIR="${HOME}/artools-workspace"
WORKSPACE_PROFILES="${WORKSPACE_DIR}/iso-profiles"
ARTOOLS_CONF_DIR="${HOME}/.config/artools"
PACMAN_CONF_DIR="${ARTOOLS_CONF_DIR}/pacman.conf.d"
PACMAN_CONF_NAME="iso-x86_64.conf"
SYSTEM_PACMAN_CONF="/usr/share/artools/pacman.conf.d/${PACMAN_CONF_NAME}"

# ── Vendor package dirs and remote URLs ──────────────────────────────────────
# Paths are resolved in resolve_paths() once REPO_ROOT is known.
TKG_GUI_PKG_DIR=""
GAMESCOPE_PKG_DIR=""
# Remote URLs used when BUILD_SOURCE=clone.
# tkg-gui's PKGBUILD already carries the correct SSH URL; it is listed here
# for reference only. gamescope requires an explicit rewrite in clone mode.
TKG_GUI_REMOTE="git+ssh://git@github.com/MasterGenotype/tkg-gui.git"
GAMESCOPE_REMOTE="git+ssh://git@github.com/MasterGenotype/gamescope.git#branch=gamescope-ba"
# Staging directory — single source of truth fed to both the local artools repo
# and the live-overlay embedded repo, eliminating version drift between the two.
PKG_STAGE_DIR="/tmp/deploytix-iso-stage-$$"

# ── Usage ────────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Build a custom Artix Linux ISO with deploytix pre-installed.

Options:
  -i <init>   Init system: openrc, runit, dinit, s6  [default: runit]
  -g          Include GUI (deploytix-gui-git + desktop environment)
  -b <de>     Desktop profile to merge for GUI mode   [default: plasma]
  -s          Skip package rebuild (reuse existing .pkg.tar.zst)
  -c          Clean buildiso work directory before building
  -x          Build chroot only (stop before squash/ISO generation)
  -C          Clone mode — fetch package sources from remote URLs instead of
              using the vendor/ submodule checkouts (requires network + SSH keys)
  -K          Keep built .pkg.tar.zst files after ISO creation (skip cleanup)
  -r          Reset — remove installed profile, repo, and pacman.conf override
  -n          Dry run — show what would be done without executing
  -h          Show this help

Build source modes:
  local (default)  makepkg reads source trees from the vendor/ submodule on disk
                   via git+file://.  Fast, reproducible, no network for source.
  clone (-C)       makepkg fetches fresh source from the remote GitHub URLs.
                   Validates the published state of each repo; requires SSH keys.

Examples:
  $(basename "$0")                    # Base ISO, CLI deploytix, runit, local source
  $(basename "$0") -i openrc          # Base ISO, openrc init
  $(basename "$0") -g -i dinit        # Plasma ISO with GUI deploytix, dinit
  $(basename "$0") -g -b lxqt -i s6   # LXQt ISO with GUI deploytix, s6
  $(basename "$0") -s -c              # Skip rebuild, clean previous build artifacts
  $(basename "$0") -C                 # Build with fresh source clones from remote
  $(basename "$0") -K                 # Build and keep .pkg.tar.zst after ISO
  $(basename "$0") -r                 # Remove all installed artifacts

EOF
    exit 0
}

# ── Root-user guard ──────────────────────────────────────────────────────────
# The script itself must NOT run as root: makepkg refuses to run as root, and
# HOME shifts to /root/, silently detaching from the user's artools-workspace.
# Individual privileged steps escalate with sudo internally.
check_not_root() {
    if [[ $EUID -eq 0 ]]; then
        die "Do not run this script with sudo/root.
  makepkg refuses to build as root, and paths shift to /root/ (detaching
  from ~/artools-workspace). Run as your normal user; individual steps that
  need privileges will invoke sudo themselves.
  If you started this via 'sudo ./build-deploytix-iso.sh', re-run without sudo."
    fi
}

# ── Filesystem sanity ────────────────────────────────────────────────────────
# buildiso/makepkg need POSIX perms, xattrs, and case-sensitive names — none of
# vfat / iso9660 / overlay / tmpfs / squashfs meet all three. Refuse early
# instead of failing three steps in when the chroot cannot be assembled.
check_writable_filesystem() {
    local fstype
    fstype="$(stat -f -c %T "$HOME" 2>/dev/null || true)"
    case "$fstype" in
        ext2/ext3|ext4|xfs|btrfs|reiserfs|f2fs|zfs|jfs) ;;
        vfat|msdos|iso9660|isofs|overlay|overlayfs|tmpfs|squashfs)
            die "HOME (${HOME}) is on '${fstype}' — buildiso needs a real filesystem.
  Move ~/artools-workspace and the Deploytix clone onto ext4/btrfs/xfs and re-run.
  (Live-USB / overlay / tmpfs / vfat cannot host the buildiso chroot.)"
            ;;
        "") warn "Could not detect filesystem type at ${HOME}; continuing anyway." ;;
        *)  warn "Unusual filesystem '${fstype}' at ${HOME}; buildiso may misbehave." ;;
    esac
}

# ── Argument parsing ─────────────────────────────────────────────────────────
while getopts ":i:b:gscxrnhCK" opt; do
    case "$opt" in
        i) INITSYS="$OPTARG" ;;
        g) INCLUDE_GUI=true ;;
        b) BASE_DE_PROFILE="$OPTARG" ;;
        s) SKIP_REBUILD=true ;;
        c) CLEAN_BUILD=true ;;
        x) CHROOT_ONLY=true ;;
        r) RESET_MODE=true ;;
        n) DRY_RUN=true ;;
        C) BUILD_SOURCE="clone" ;;
        K) KEEP_PACKAGES=true ;;
        h) usage ;;
        :) die "Option -${OPTARG} requires an argument" ;;
        *) die "Unknown option: -${OPTARG}. Use -h for help." ;;
    esac
done

# ── Validate init system ────────────────────────────────────────────────────
case "$INITSYS" in
    openrc|runit|dinit|s6) ;;
    *) die "Invalid init system '${INITSYS}'. Must be one of: openrc, runit, dinit, s6" ;;
esac

# ── Resolve paths ────────────────────────────────────────────────────────────
resolve_paths() {
    if [[ -f "Cargo.toml" && -d "pkg" && -d "iso" ]]; then
        REPO_ROOT="$(pwd)"
    elif [[ -f "../Cargo.toml" && -d "../pkg" && -d "../iso" ]]; then
        REPO_ROOT="$(cd .. && pwd)"
    else
        die "Cannot find Deploytix repository root. Run from the repo root or iso/ directory."
    fi

    ISO_DIR="${REPO_ROOT}/iso"
    PKG_DIR="${REPO_ROOT}/pkg"
    LOCAL_REPO_DIR="/var/lib/artools/repos/deploytix"
    PROFILE_SRC="${ISO_DIR}/profile/deploytix"
    TKG_GUI_PKG_DIR="${REPO_ROOT}/vendor/tkg-gui/pkg"
    GAMESCOPE_PKG_DIR="${REPO_ROOT}/vendor/gamescope/pkg"
}

# ── Submodule guard ───────────────────────────────────────────────────────────
ensure_submodules() {
    local missing=0
    for sub in vendor/tkg-gui vendor/gamescope; do
        if [[ ! -f "${REPO_ROOT}/${sub}/pkg/PKGBUILD" ]]; then
            warn "Submodule ${sub} not initialised — pkg/PKGBUILD missing"
            missing=1
        fi
    done
    if (( missing )); then
        if "$DRY_RUN"; then
            msg2 "[dry-run] Would run: git submodule update --init --recursive"
        else
            msg "Initialising vendor submodules..."
            git -C "${REPO_ROOT}" submodule update --init --recursive
        fi
    fi
}

# ── Prerequisites ────────────────────────────────────────────────────────────
check_prerequisites() {
    msg "Checking prerequisites..."
    local missing=()

    for cmd in buildiso makepkg repo-add yq git; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done

    if (( ${#missing[@]} > 0 )); then
        die "Missing required commands: ${missing[*]}
  Install: pacman -S artools iso-profiles base-devel go-yq git"
    fi

    [[ -f "${PKG_DIR}/PKGBUILD" ]] || die "PKGBUILD not found at ${PKG_DIR}/PKGBUILD"
    [[ -f "${PROFILE_SRC}/profile.yaml" ]] || die "Profile not found at ${PROFILE_SRC}/profile.yaml"
    [[ -f "${SYSTEM_PACMAN_CONF}" ]] || die "System pacman.conf not found at ${SYSTEM_PACMAN_CONF}"

    validate_profile_yaml "${PROFILE_SRC}/profile.yaml"

    ensure_submodules

    msg2 "All prerequisites satisfied"
    msg2 "Build source: ${BUILD_SOURCE}"
}

# ── profile.yaml sanity ──────────────────────────────────────────────────────
# Empty / tab-indented / missing-section profile.yaml files fail in obscure
# places later in buildiso.  yq will parse (catches YAML syntax + tab-indent
# errors) and we check the required top-level sections are present and
# non-null.
validate_profile_yaml() {
    local yaml="$1"

    [[ -s "$yaml" ]] || die "profile.yaml is empty: ${yaml}"

    if grep -Pq '^\t' "$yaml"; then
        die "profile.yaml uses tab indentation (YAML requires spaces): ${yaml}"
    fi

    yq eval '.' "$yaml" >/dev/null 2>&1 \
        || die "profile.yaml is not valid YAML: ${yaml}
  Run: yq eval '.' '${yaml}' to see the parser error."

    local section
    for section in rootfs livefs; do
        local kind
        kind="$(yq eval ".${section} | type" "$yaml" 2>/dev/null || echo "!!null")"
        case "$kind" in
            "!!map"|"!!seq") ;;
            *) die "profile.yaml missing required section '${section}': ${yaml}" ;;
        esac
    done
}

# ── Overlay helpers ──────────────────────────────────────────────────────────
resolve_de_profile_path() {
    local de_profile="${WORKSPACE_PROFILES}/${BASE_DE_PROFILE}/profile.yaml"

    if [[ -f "$de_profile" ]]; then
        printf '%s\n' "$de_profile"
        return 0
    fi

    de_profile="/usr/share/artools/iso-profiles/${BASE_DE_PROFILE}/profile.yaml"
    if [[ -f "$de_profile" ]]; then
        printf '%s\n' "$de_profile"
        return 0
    fi

    die "Desktop profile '${BASE_DE_PROFILE}' not found in workspace or system iso-profiles"
}

resolve_profile_overlay_dir() {
    local profile_dir="$1"
    local overlay_name="$2"
    local overlay_path="${profile_dir}/${overlay_name}"

    if [[ -L "$overlay_path" ]]; then
        local resolved
        resolved="$(readlink -f "$overlay_path")"
        if [[ -d "$resolved" ]]; then
            printf '%s\n' "$resolved"
            return 0
        fi
        warn "Overlay symlink target missing: ${overlay_path} -> ${resolved}"
        return 1
    elif [[ -d "$overlay_path" ]]; then
        printf '%s\n' "$overlay_path"
        return 0
    fi

    return 1
}

merge_overlay_tree() {
    local src="$1"
    local dest="$2"

    [[ -d "$src" ]] || return 0
    mkdir -p "$dest"

    local path rel target
    while IFS= read -r -d '' path; do
        rel="${path#"$src"/}"
        [[ "$rel" == "$path" ]] && continue
        target="${dest}/${rel}"

        if [[ -e "$target" || -L "$target" ]]; then
            # cp -a preserves symlinks as-is, so treat them as
            # non-directories when checking for type conflicts
            if [[ -d "$path" && ! -L "$path" && ( ! -d "$target" || -L "$target" ) ]]; then
                rm -f "$target"
            elif [[ ( ! -d "$path" || -L "$path" ) && -d "$target" && ! -L "$target" ]]; then
                rm -rf "$target"
            fi
        fi
    done < <(find "$src" -mindepth 1 -print0)

    cp -a "$src"/. "$dest"/

    # Resolve symlinks that became broken after being copied to a new
    # location (e.g. relative symlinks shared between artools profiles)
    local link
    while IFS= read -r -d '' link; do
        [[ -e "$link" ]] && continue
        local link_rel="${link#"$dest"/}"
        local src_link="${src}/${link_rel}"
        local resolved
        if resolved="$(readlink -f "$src_link" 2>/dev/null)" && [[ -e "$resolved" ]]; then
            rm -f "$link"
            if [[ -d "$resolved" ]]; then
                mkdir -p "$link"
                cp -a "$resolved"/. "$link"/
            else
                cp -a "$resolved" "$link"
            fi
        fi
    done < <(find "$dest" -type l -print0)
}

materialize_overlay_symlink() {
    local path="$1"

    if [[ -L "$path" ]]; then
        local link_target tmpdir
        link_target="$(readlink -f "$path")"
        rm -f "$path"

        if [[ -d "$link_target" ]]; then
            tmpdir="$(mktemp -d)"
            cp -aL "$link_target"/. "$tmpdir"/
            mkdir -p "$path"
            cp -a "$tmpdir"/. "$path"/
            rm -rf "$tmpdir"
        else
            mkdir -p "$path"
        fi

        msg2 "Materialised symlinked overlay: $path"
    else
        mkdir -p "$path"
    fi
}

# ── PKGBUILD helpers ──────────────────────────────────────────────────────────

# Create a .iso-bak of a PKGBUILD before modifying it (idempotent).
_backup_pkgbuild() {
    local pkgbuild="$1"
    [[ -f "${pkgbuild}.iso-bak" ]] || cp -f "$pkgbuild" "${pkgbuild}.iso-bak"
}

# Restore a PKGBUILD from its .iso-bak and remove the bak file.
restore_pkgbuild() {
    local pkgbuild="$1"
    [[ -f "${pkgbuild}.iso-bak" ]] && mv "${pkgbuild}.iso-bak" "$pkgbuild"
}

# Rewrite source=("PKG::git+...") to use a local git+file:// path (local mode).
point_pkgbuild_at_submodule() {
    local pkg="$1" pkgbuild="$2" sub_path="$3"
    _backup_pkgbuild "$pkgbuild"
    sed -i "s|source=(\"${pkg}::git+[^\"]*\")|source=(\"${pkg}::git+file://${sub_path}\")|" "$pkgbuild"
}

# Rewrite source=("PKG::git+...") to a remote URL (clone mode).
point_pkgbuild_at_remote() {
    local pkg="$1" pkgbuild="$2" url="$3"
    _backup_pkgbuild "$pkgbuild"
    sed -i "s|source=(\"${pkg}::git+[^\"]*\")|source=(\"${pkg}::${url}\")|" "$pkgbuild"
}

# Stamp pkgrel with a build-time suffix so the buildiso chroot always sees a
# strictly higher version than its cache and reinstalls the package.
stamp_pkgrel() {
    local pkgbuild="$1"
    local stamp
    stamp="$(date -u +%Y%m%d%H%M%S)"
    _backup_pkgbuild "$pkgbuild"
    sed -i "s/^pkgrel=.*/pkgrel=1.${stamp}/" "$pkgbuild"
}

# EXIT / INT / TERM handler: restore any PKGBUILD still carrying a .iso-bak
# (script aborted before the explicit restore ran) and purge the staging dir.
_cleanup_dirty_pkgbuilds() {
    local pb bak
    for pb in \
        "${PKG_DIR}/PKGBUILD" \
        "${TKG_GUI_PKG_DIR}/PKGBUILD" \
        "${GAMESCOPE_PKG_DIR}/PKGBUILD"
    do
        bak="${pb}.iso-bak"
        [[ -f "$bak" ]] && mv "$bak" "$pb"
    done
    [[ -d "${PKG_STAGE_DIR}" ]] && rm -rf "${PKG_STAGE_DIR}"
}

# ── Step B: Build packages ────────────────────────────────────────────────────
build_packages() {
    if "$SKIP_REBUILD"; then
        local count=0 d
        for d in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}" "${GAMESCOPE_PKG_DIR}"; do
            [[ -d "$d" ]] || continue
            count=$(( count + $(find "$d" -maxdepth 1 -name '*.pkg.tar.zst' 2>/dev/null | wc -l) ))
        done
        (( count > 0 )) || die "No .pkg.tar.zst found in vendor pkg dirs and -s (skip rebuild) was set"
        msg "Skipping package build (-s); reusing ${count} existing package(s)"
        return 0
    fi

    msg "Building deploytix packages..."

    local deploytix_pkgbuild="${PKG_DIR}/PKGBUILD"
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would stamp pkgrel and run: makepkg -sf --noconfirm in ${PKG_DIR}"
    else
        stamp_pkgrel "$deploytix_pkgbuild"
        pushd "${PKG_DIR}" >/dev/null
        makepkg -sf --noconfirm
        popd >/dev/null
        restore_pkgbuild "$deploytix_pkgbuild"

        local count
        count=$(find "${PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
        (( count > 0 )) || die "makepkg produced no deploytix packages"
        msg2 "Built ${count} deploytix package(s)"
    fi

    build_tkg_gui_packages
    build_gamescope_packages
}

# tkg-gui (GUI mode only)
#   local:  rewrite source SSH URL → git+file:// pointing at vendor/tkg-gui
#   clone:  PKGBUILD already carries the correct SSH URL — no rewrite needed
build_tkg_gui_packages() {
    if ! "$INCLUDE_GUI"; then
        return 0
    fi

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would build tkg-gui (${BUILD_SOURCE} mode) from ${TKG_GUI_PKG_DIR}"
        return 0
    fi

    msg "Building tkg-gui packages (${BUILD_SOURCE} mode)..."

    local pkgbuild="${TKG_GUI_PKG_DIR}/PKGBUILD"
    [[ -f "$pkgbuild" ]] || die "tkg-gui PKGBUILD not found at ${pkgbuild}"

    rm -rf "${TKG_GUI_PKG_DIR}/tkg-gui" "${TKG_GUI_PKG_DIR}/src"

    if [[ "$BUILD_SOURCE" == "local" ]]; then
        point_pkgbuild_at_submodule "tkg-gui" "$pkgbuild" "${REPO_ROOT}/vendor/tkg-gui"
    fi
    # clone mode: PKGBUILD source already has the correct SSH remote URL.

    stamp_pkgrel "$pkgbuild"
    pushd "${TKG_GUI_PKG_DIR}" >/dev/null
    makepkg -sf --noconfirm
    popd >/dev/null
    restore_pkgbuild "$pkgbuild"

    local count
    count=$(find "${TKG_GUI_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    (( count > 0 )) || die "makepkg produced no tkg-gui packages"
    msg2 "Built ${count} tkg-gui package(s)"
}

# gamescope (always built)
#   local:  no source rewrite — PKGBUILD uses source=("gamescope::git+file://$(cd .. && pwd)")
#           which evaluates to vendor/gamescope when makepkg runs from vendor/gamescope/pkg/
#   clone:  rewrite source → MasterGenotype fork SSH URL on gamescope-ba branch
build_gamescope_packages() {
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would build gamescope (${BUILD_SOURCE} mode) from ${GAMESCOPE_PKG_DIR}"
        return 0
    fi

    msg "Building gamescope packages (${BUILD_SOURCE} mode)..."

    local pkgbuild="${GAMESCOPE_PKG_DIR}/PKGBUILD"
    [[ -f "$pkgbuild" ]] || die "gamescope PKGBUILD not found at ${pkgbuild}"

    rm -rf "${GAMESCOPE_PKG_DIR}/gamescope" "${GAMESCOPE_PKG_DIR}/src"

    if [[ "$BUILD_SOURCE" == "clone" ]]; then
        point_pkgbuild_at_remote "gamescope" "$pkgbuild" "${GAMESCOPE_REMOTE}"
    fi
    # local mode: $(cd .. && pwd) in the source array evaluates to vendor/gamescope
    # at makepkg runtime — no rewrite needed.

    stamp_pkgrel "$pkgbuild"
    pushd "${GAMESCOPE_PKG_DIR}" >/dev/null
    makepkg -sf --noconfirm
    popd >/dev/null
    restore_pkgbuild "$pkgbuild"

    local count
    count=$(find "${GAMESCOPE_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    (( count > 0 )) || die "makepkg produced no gamescope packages"
    msg2 "Built ${count} gamescope package(s)"
}

# ── Step B2: Stage packages ───────────────────────────────────────────────────
# Consolidate all built packages into one directory. Both create_local_repo()
# and embed_live_repo() consume only this dir, so the local artools repo and
# the ISO-embedded repo are always identical — no version drift possible.
stage_packages() {
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would stage packages into ${PKG_STAGE_DIR}"
        return 0
    fi

    msg "Staging packages..."
    rm -rf "${PKG_STAGE_DIR}"
    mkdir -p "${PKG_STAGE_DIR}"

    local src_dir pkg
    for src_dir in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}" "${GAMESCOPE_PKG_DIR}"; do
        [[ -d "$src_dir" ]] || continue
        for pkg in "${src_dir}"/*.pkg.tar.zst; do
            [[ -f "$pkg" ]] || continue
            cp -f "$pkg" "${PKG_STAGE_DIR}/"
        done
    done

    # Sanity gate — these must always be present.
    local r
    for r in deploytix-git gamescope-git; do
        compgen -G "${PKG_STAGE_DIR}/${r}-*.pkg.tar.zst" >/dev/null \
            || die "Stage missing ${r}; rebuild with -s removed"
    done
    if "$INCLUDE_GUI"; then
        compgen -G "${PKG_STAGE_DIR}/tkg-gui-git-*.pkg.tar.zst" >/dev/null \
            || die "Stage missing tkg-gui-git; rebuild with -s removed"
    fi

    local staged_count
    staged_count=$(find "${PKG_STAGE_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    msg2 "Staged ${staged_count} package(s) at ${PKG_STAGE_DIR}"
}

# ── Step C: Create local pacman repository ───────────────────────────────────
create_local_repo() {
    msg "Creating local pacman repository..."

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would create repo at ${LOCAL_REPO_DIR}"
        return 0
    fi

    sudo mkdir -p "${LOCAL_REPO_DIR}"

    # If a prior root-invoked run left this directory owned by root, the
    # non-sudo repo-add call below would still work (we sudo it) but any
    # later `pacman -Sy` from the user account would trip on stale caches.
    # Normalise ownership so subsequent runs behave identically whether the
    # first run was fresh or resumed.
    local repo_owner
    repo_owner="$(stat -c '%U' "${LOCAL_REPO_DIR}" 2>/dev/null || echo "")"
    if [[ "$repo_owner" == "root" ]]; then
        msg2 "Fixing ownership of ${LOCAL_REPO_DIR} (root → ${USER})"
        sudo chown -R "${USER}:${USER}" "${LOCAL_REPO_DIR}"
    fi

    sudo rm -f "${LOCAL_REPO_DIR}"/*.db* "${LOCAL_REPO_DIR}"/*.files*
    sudo rm -f "${LOCAL_REPO_DIR}"/*.pkg.tar.zst

    local pkg pkg_count=0
    for pkg in "${PKG_STAGE_DIR}"/*.pkg.tar.zst; do
        [[ -f "$pkg" ]] || continue
        sudo cp -f "$pkg" "${LOCAL_REPO_DIR}/"
        msg2 "Added $(basename "$pkg")"
        pkg_count=$(( pkg_count + 1 ))
    done

    (( pkg_count > 0 )) || die "No packages found in stage dir to add to repository"

    sudo chmod 644 "${LOCAL_REPO_DIR}"/*.pkg.tar.zst
    sudo repo-add --new "${LOCAL_REPO_DIR}/deploytix.db.tar.zst" "${LOCAL_REPO_DIR}"/*.pkg.tar.zst

    msg2 "Repository created with ${pkg_count} package(s) at ${LOCAL_REPO_DIR}"
}

# ── Step D: Install custom pacman.conf ───────────────────────────────────────
PACMAN_CONF_BACKUP=""

install_pacman_conf() {
    msg "Setting up custom pacman.conf..."
    local target="${PACMAN_CONF_DIR}/${PACMAN_CONF_NAME}"

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would install pacman.conf with [deploytix] repo to ${target}"
        return 0
    fi

    mkdir -p "${PACMAN_CONF_DIR}"

    if [[ -f "$target" ]] && grep -q '^\[deploytix\]' "$target"; then
        if grep -q "Server = file://${LOCAL_REPO_DIR}" "$target"; then
            msg2 "pacman.conf already configured — skipping"
            return 0
        fi
        msg2 "Updating existing [deploytix] repo path"
    fi

    if [[ -f "$target" ]] && ! grep -q '^\[deploytix\]' "$target"; then
        PACMAN_CONF_BACKUP="${target}.deploytix-bak"
        cp "$target" "$PACMAN_CONF_BACKUP"
        msg2 "Backed up existing ${PACMAN_CONF_NAME} → $(basename "${PACMAN_CONF_BACKUP}")"
    fi

    cp "${SYSTEM_PACMAN_CONF}" "$target"

    cat >> "$target" <<EOF

# ── Deploytix local repository (auto-generated by build-deploytix-iso.sh) ──
[deploytix]
SigLevel = Optional TrustAll
Server = file://${LOCAL_REPO_DIR}
EOF

    msg2 "Installed pacman.conf with [deploytix] repo at ${target}"
    msg2 "Repo path: file://${LOCAL_REPO_DIR}"
}

reset_artifacts() {
    msg "Resetting deploytix ISO build artifacts..."
    local target="${PACMAN_CONF_DIR}/${PACMAN_CONF_NAME}"
    local dest="${WORKSPACE_PROFILES}/deploytix"

    if [[ -f "${target}.deploytix-bak" ]]; then
        mv "${target}.deploytix-bak" "$target"
        msg2 "Restored original ${PACMAN_CONF_NAME}"
    elif [[ -f "$target" ]]; then
        rm -f "$target"
        msg2 "Removed custom ${PACMAN_CONF_NAME}"
    fi

    if [[ -d "$dest" ]]; then
        rm -rf "$dest"
        msg2 "Removed profile: ${dest}"
    fi

    if [[ -d "${LOCAL_REPO_DIR}" ]]; then
        sudo rm -rf "${LOCAL_REPO_DIR}"
        msg2 "Removed repo: ${LOCAL_REPO_DIR}"
    fi

    if [[ -d "${PKG_STAGE_DIR}" ]]; then
        rm -rf "${PKG_STAGE_DIR}"
        msg2 "Removed staging dir: ${PKG_STAGE_DIR}"
    fi

    msg "Reset complete"
}

# ── Resolve common/ directory ────────────────────────────────────────────────
resolve_common_dir() {
    if [[ -d "${WORKSPACE_PROFILES}/common" ]]; then
        printf '%s\n' "${WORKSPACE_PROFILES}/common"
    elif [[ -d "/usr/share/artools/iso-profiles/common" ]]; then
        printf '%s\n' "/usr/share/artools/iso-profiles/common"
    else
        die "Cannot find artools common profile directory"
    fi
}

# ── Step E: Install ISO profile ──────────────────────────────────────────────
install_profile() {
    msg "Installing deploytix ISO profile..."
    local dest="${WORKSPACE_PROFILES}/deploytix"
    local common_dir
    local de_profile=""
    local de_dir=""
    local overlay_src=""

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would install profile to ${dest}"
        return 0
    fi

    mkdir -p "${WORKSPACE_PROFILES}"
    rm -rf "$dest"
    mkdir -p "$dest"

    common_dir="$(resolve_common_dir)"

    if "$INCLUDE_GUI"; then
        de_profile="$(resolve_de_profile_path)"
        de_dir="$(dirname "$de_profile")"
        generate_gui_profile "$dest" "$de_profile"
    else
        cp "${PROFILE_SRC}/profile.yaml" "$dest/profile.yaml"
    fi

    mkdir -p "$dest/root-overlay"

    if [[ -d "${common_dir}/root-overlay" ]]; then
        merge_overlay_tree "${common_dir}/root-overlay" "$dest/root-overlay"
    fi

    if "$INCLUDE_GUI"; then
        if overlay_src="$(resolve_profile_overlay_dir "$de_dir" "root-overlay" 2>/dev/null)"; then
            merge_overlay_tree "$overlay_src" "$dest/root-overlay"
        fi
    fi

    if [[ -d "${PROFILE_SRC}/root-overlay" ]]; then
        merge_overlay_tree "${PROFILE_SRC}/root-overlay" "$dest/root-overlay"
    fi

    if "$INCLUDE_GUI"; then
        if overlay_src="$(resolve_profile_overlay_dir "$de_dir" "live-overlay" 2>/dev/null)"; then
            mkdir -p "$dest/live-overlay"
            merge_overlay_tree "$overlay_src" "$dest/live-overlay"
        fi
    fi

    if [[ -d "${PROFILE_SRC}/live-overlay" ]]; then
        mkdir -p "$dest/live-overlay"
        merge_overlay_tree "${PROFILE_SRC}/live-overlay" "$dest/live-overlay"
    fi

    msg2 "Profile installed at ${dest}"
}

# ── GUI profile generation ───────────────────────────────────────────────────
# Display-manager map: which DM package + which init-flavoured DM package
# ships each desktop's session.  Empty DM means the DE doesn't ship a
# graphical login (user chooses lightdm/sddm themselves).
_gui_dm_for_de() {
    case "$1" in
        plasma) echo "sddm" ;;
        gnome)  echo "gdm" ;;
        xfce|lxqt|lxde|mate|cinnamon|budgie) echo "lightdm" ;;
        *)      echo "" ;;
    esac
}

generate_gui_profile() {
    local dest="$1"
    local de_profile="$2"

    msg2 "Merging desktop profile: ${BASE_DE_PROFILE}"

    cp "$de_profile" "$dest/profile.yaml"

    yq -i '.livefs.packages += ["deploytix-git", "deploytix-gui-git", "tkg-gui-git", "gamescope-git", "alsa-utils"]' "$dest/profile.yaml"
    yq -i '.livefs.packages -= ["calamares-extensions"]' "$dest/profile.yaml"
    # Remove packages from the base DE profile that are unavailable in Artix repos.
    # artix-breeze-sddm was a Manjaro-style meta that Artix does not ship.
    yq -i '.rootfs.packages -= ["artix-breeze-sddm"]' "$dest/profile.yaml"

    ensure_display_manager "$dest/profile.yaml"

    msg2 "GUI profile generated (${BASE_DE_PROFILE} + deploytix)"
}

# ── Ensure a compatible display manager is present ───────────────────────────
# After stripping unavailable DM meta packages (e.g. artix-breeze-sddm) a DE
# profile can end up with no display manager at all, causing the live session
# to boot to a TTY. Add the DE's canonical DM (sddm/gdm/lightdm) to rootfs
# packages and the init-flavoured variant to every init's packages-init list
# so the live GUI actually starts.
ensure_display_manager() {
    local yaml="$1"
    local dm
    dm="$(_gui_dm_for_de "$BASE_DE_PROFILE")"

    if [[ -z "$dm" ]]; then
        msg2 "No canonical display manager for '${BASE_DE_PROFILE}'; leaving profile untouched"
        return 0
    fi

    # rootfs.packages: add DM if not already present. Guard against a null or
    # missing rootfs.packages block from an odd base profile.
    yq -i '.rootfs.packages = (.rootfs.packages // [])' "$yaml"
    if ! yq eval ".rootfs.packages | contains([\"${dm}\"])" "$yaml" | grep -qx true; then
        yq -i ".rootfs.packages += [\"${dm}\"]" "$yaml"
        msg2 "Added display manager: ${dm}"
    fi

    # packages-init.<init>: add ${dm}-${init} so the DM has a service unit for
    # every init the profile supports. Skip inits the profile doesn't declare.
    local init
    for init in openrc runit dinit s6; do
        local has_init
        has_init="$(yq eval ".rootfs.\"packages-init\".${init} | type" "$yaml" 2>/dev/null || echo "!!null")"
        [[ "$has_init" == "!!seq" ]] || continue
        if ! yq eval ".rootfs.\"packages-init\".${init} | contains([\"${dm}-${init}\"])" "$yaml" | grep -qx true; then
            yq -i ".rootfs.\"packages-init\".${init} += [\"${dm}-${init}\"]" "$yaml"
            msg2 "Added ${dm}-${init} to packages-init.${init}"
        fi
    done
}

# ── Step F: Embed built packages in the live-overlay ─────────────────────────
embed_live_repo() {
    msg "Embedding packages in live-overlay for basestrap use..."
    local dest="${WORKSPACE_PROFILES}/deploytix"
    local live_overlay_dir="${dest}/live-overlay"
    local live_repo_path="${live_overlay_dir}/var/lib/deploytix-repo"

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would embed packages at ${live_repo_path}"
        return 0
    fi

    materialize_overlay_symlink "${live_overlay_dir}"

    # Wipe any leftovers from a previous run so the db reflects only what
    # is currently in the stage dir.
    rm -rf "${live_repo_path}"
    mkdir -p "${live_repo_path}"

    local pkg pkg_count=0
    for pkg in "${PKG_STAGE_DIR}"/*.pkg.tar.zst; do
        [[ -f "$pkg" ]] || continue
        cp -f "$pkg" "${live_repo_path}/"
        msg2 "Embedded $(basename "$pkg")"
        pkg_count=$(( pkg_count + 1 ))
    done

    (( pkg_count > 0 )) || die "No packages in stage dir to embed in live-overlay"

    # --new combined with the freshly emptied dir ensures no stale entries
    # (e.g. a gamescope-git entry from a prior run) survive in the db.
    repo-add --new "${live_repo_path}/deploytix.db.tar.zst" \
        "${live_repo_path}"/*.pkg.tar.zst

    msg2 "Embedded ${pkg_count} package(s); repo at /var/lib/deploytix-repo"
}

# ── Step H: Run buildiso ─────────────────────────────────────────────────────
run_buildiso() {
    msg "Building ISO (init=${INITSYS}, profile=deploytix)..."

    local args=(-p deploytix -i "$INITSYS")

    if ! "$CLEAN_BUILD"; then
        args+=(-c)
    fi

    if "$CHROOT_ONLY"; then
        args+=(-x)
        msg2 "Chroot-only mode: will stop after building chroot"
    fi

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would run: sudo buildiso ${args[*]}"
        return 0
    fi

    msg2 "Running: sudo buildiso ${args[*]}"
    sudo buildiso "${args[@]}"

    local iso_pool="${WORKSPACE_DIR}/iso/deploytix"
    if [[ -d "$iso_pool" ]] && ! "$CHROOT_ONLY"; then
        local iso_file
        iso_file=$(find "$iso_pool" -maxdepth 1 -name '*.iso' -printf '%f\n' | head -1)
        if [[ -n "$iso_file" ]]; then
            msg "ISO created: ${iso_pool}/${iso_file}"
        fi
    fi
}

# ── Step I: Clean up built packages ──────────────────────────────────────────
# Removes .pkg.tar.zst files from each vendor pkg/ dir and the staging dir once
# they are safely embedded in the ISO and in LOCAL_REPO_DIR. Skip with -K.
cleanup_built_packages() {
    "$DRY_RUN"       && return 0
    "$CHROOT_ONLY"   && return 0
    "$KEEP_PACKAGES" && return 0

    msg "Cleaning up built .pkg.tar.zst files..."
    local d
    for d in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}" "${GAMESCOPE_PKG_DIR}"; do
        [[ -d "$d" ]] || continue
        find "$d" -maxdepth 1 -name '*.pkg.tar.zst'     -delete
        find "$d" -maxdepth 1 -name '*.pkg.tar.zst.sig' -delete
    done
    rm -rf "${PKG_STAGE_DIR}"
    msg2 "Done — packages are embedded in the ISO and in ${LOCAL_REPO_DIR}"
}

# ── Main ─────────────────────────────────────────────────────────────────────
main() {
    check_not_root
    check_writable_filesystem
    resolve_paths

    # Install a global handler that restores any modified PKGBUILDs and removes
    # the staging dir if the script is interrupted or exits on an error.
    trap '_cleanup_dirty_pkgbuilds' EXIT INT TERM

    if "$RESET_MODE"; then
        reset_artifacts
        exit 0
    fi

    check_prerequisites

    msg "Building Deploytix ISO"
    msg2 "Init system:   ${INITSYS}"
    msg2 "GUI mode:      ${INCLUDE_GUI}"
    if "$INCLUDE_GUI"; then
        msg2 "Desktop:       ${BASE_DE_PROFILE}"
    fi
    msg2 "Build source:  ${BUILD_SOURCE}"
    msg2 "Repo:          ${LOCAL_REPO_DIR}"
    msg2 "Profile:       ${WORKSPACE_PROFILES}/deploytix"
    echo

    build_packages
    stage_packages
    create_local_repo
    install_pacman_conf
    install_profile
    embed_live_repo
    run_buildiso
    cleanup_built_packages

    msg "Done!"
    msg2 "The pacman.conf override and profile remain installed."
    msg2 "You can now run 'sudo buildiso -p deploytix -i <init>' directly."
    msg2 "To clean up, run: $(basename "$0") -r"
}

main "$@"
