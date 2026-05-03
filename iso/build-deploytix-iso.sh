#!/usr/bin/env bash
#
# build-deploytix-iso.sh — Build a custom Artix Linux ISO with deploytix pre-installed
#
# Usage: ./build-deploytix-iso.sh [OPTIONS]
#
# Requires: artools (buildiso), makepkg, repo-add, go-yq
# Must be run from the Deploytix repository root or the iso/ directory.

set -euo pipefail

# ── Colour helpers ───────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
msg()  { printf "${GREEN}==> %s${NC}\n" "$*"; }
msg2() { printf "${BLUE}  -> %s${NC}\n" "$*"; }
warn() { printf "${YELLOW}==> WARNING: %s${NC}\n" "$*"; }
err()  { printf "${RED}==> ERROR: %s${NC}\n" "$*" >&2; }
die()  { err "$@"; exit 1; }

# ── Defaults ─────────────────────────────────────────────────────────────────
INITSYS="openrc"
INCLUDE_GUI=false
BASE_DE_PROFILE="plasma"
SKIP_REBUILD=false
CLEAN_BUILD=false
CHROOT_ONLY=false
DRY_RUN=false
RESET_MODE=false

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

# ── External package sources ─────────────────────────────────────────────────
TKG_GUI_REPO_URL="https://github.com/MasterGenotype/tkg-gui.git"
TKG_GUI_LOCAL_DIR=""   # Resolved to sibling repo if present
TKG_GUI_CLONE_DIR=""
TKG_GUI_PKG_DIR=""
MODULAR_REPO_URL="https://github.com/MasterGenotype/Modular-1.git"
MODULAR_LOCAL_DIR=""   # Resolved to sibling repo if present
MODULAR_CLONE_DIR=""
MODULAR_PKG_DIR=""
# Gamescope (Bazzite fork). The pkg/PKGBUILD is NOT tracked in the
# upstream bazzite-org/gamescope repository, so we cannot fall back to
# an automatic clone like we do for tkg-gui / Modular. The script
# requires a sibling checkout at <repo-root>/../gamescope/pkg/PKGBUILD.
GAMESCOPE_REPO_URL="https://github.com/bazzite-org/gamescope"
GAMESCOPE_LOCAL_DIR=""
GAMESCOPE_PKG_DIR=""

# ── Usage ────────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Build a custom Artix Linux ISO with deploytix pre-installed.

Options:
  -i <init>   Init system: openrc, runit, dinit, s6  [default: openrc]
  -g          Include GUI (deploytix-gui-git + desktop environment)
  -b <de>     Desktop profile to merge for GUI mode   [default: plasma]
  -s          Skip package rebuild (reuse existing .pkg.tar.zst)
  -c          Clean buildiso work directory before building
  -x          Build chroot only (stop before squash/ISO generation)
  -r          Reset — remove installed profile, repo, and pacman.conf override
  -n          Dry run — show what would be done without executing
  -h          Show this help

Examples:
  $(basename "$0")                    # Base ISO with CLI deploytix, openrc
  $(basename "$0") -i runit           # Base ISO with CLI deploytix, runit
  $(basename "$0") -g -i dinit        # Plasma ISO with GUI deploytix, dinit
  $(basename "$0") -g -b lxqt -i s6   # LXQt ISO with GUI deploytix, s6
  $(basename "$0") -s -c              # Skip rebuild, clean previous build
  $(basename "$0") -r                 # Remove all installed artifacts

EOF
    exit 0
}

# ── Argument parsing ─────────────────────────────────────────────────────────
while getopts ":i:b:gscxrnh" opt; do
    case "$opt" in
        i) INITSYS="$OPTARG" ;;
        g) INCLUDE_GUI=true ;;
        b) BASE_DE_PROFILE="$OPTARG" ;;
        s) SKIP_REBUILD=true ;;
        c) CLEAN_BUILD=true ;;
        x) CHROOT_ONLY=true ;;
        r) RESET_MODE=true ;;
        n) DRY_RUN=true ;;
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
    TKG_GUI_LOCAL_DIR="$(dirname "${REPO_ROOT}")/tkg-gui"
    TKG_GUI_CLONE_DIR="${WORKSPACE_DIR}/tkg-gui-src"
    if [[ -d "${TKG_GUI_LOCAL_DIR}/pkg" && -f "${TKG_GUI_LOCAL_DIR}/pkg/PKGBUILD" ]]; then
        TKG_GUI_PKG_DIR="${TKG_GUI_LOCAL_DIR}/pkg"
    else
        TKG_GUI_PKG_DIR="${TKG_GUI_CLONE_DIR}/pkg"
    fi
    MODULAR_LOCAL_DIR="$(dirname "${REPO_ROOT}")/Modular-1"
    MODULAR_CLONE_DIR="${WORKSPACE_DIR}/modular-src"
    if [[ -d "${MODULAR_LOCAL_DIR}/pkg" && -f "${MODULAR_LOCAL_DIR}/pkg/PKGBUILD" ]]; then
        MODULAR_PKG_DIR="${MODULAR_LOCAL_DIR}/pkg"
    else
        MODULAR_PKG_DIR="${MODULAR_CLONE_DIR}/pkg"
    fi
    GAMESCOPE_LOCAL_DIR="$(dirname "${REPO_ROOT}")/gamescope"
    GAMESCOPE_PKG_DIR="${GAMESCOPE_LOCAL_DIR}/pkg"
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

    msg2 "All prerequisites satisfied"
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

# ── Step B: Build deploytix packages ─────────────────────────────────────────
build_packages() {
    if "$SKIP_REBUILD"; then
        local count=0

        if [[ -d "${PKG_DIR}" ]]; then
            count=$(( count + $(find "${PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' 2>/dev/null | wc -l) ))
        fi
        if [[ -d "${TKG_GUI_PKG_DIR}" ]]; then
            count=$(( count + $(find "${TKG_GUI_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' 2>/dev/null | wc -l) ))
        fi
        if [[ -n "${MODULAR_PKG_DIR}" && -d "${MODULAR_PKG_DIR}" ]]; then
            count=$(( count + $(find "${MODULAR_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' 2>/dev/null | wc -l) ))
        fi
        if [[ -n "${GAMESCOPE_PKG_DIR}" && -d "${GAMESCOPE_PKG_DIR}" ]]; then
            count=$(( count + $(find "${GAMESCOPE_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' 2>/dev/null | wc -l) ))
        fi

        if (( count == 0 )); then
            die "No .pkg.tar.zst found in ${PKG_DIR}/, ${TKG_GUI_PKG_DIR}/, ${MODULAR_PKG_DIR}/, or ${GAMESCOPE_PKG_DIR}/ and -s (skip rebuild) was set"
        fi

        msg "Skipping package build (-s); reusing existing packages"
        return 0
    fi

    msg "Building deploytix packages..."
    pushd "${PKG_DIR}" >/dev/null

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would run: makepkg -sf --noconfirm"
    else
        makepkg -sf --noconfirm
    fi

    popd >/dev/null

    if ! "$DRY_RUN"; then
        local count
        count=$(find "${PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
        (( count > 0 )) || die "makepkg produced no deploytix packages"
        msg2 "Built ${count} deploytix package(s)"
    fi

    build_tkg_gui_packages
    build_modular_packages
    build_gamescope_packages
}

build_tkg_gui_packages() {
    if ! "$INCLUDE_GUI"; then
        return 0
    fi

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would build tkg-gui from ${TKG_GUI_PKG_DIR}"
        return 0
    fi

    msg "Building tkg-gui packages..."

    if [[ "${TKG_GUI_PKG_DIR}" == "${TKG_GUI_LOCAL_DIR}/pkg" ]]; then
        msg2 "Using local tkg-gui repo at ${TKG_GUI_LOCAL_DIR}"
    elif [[ -d "${TKG_GUI_CLONE_DIR}/.git" ]]; then
        msg2 "Updating tkg-gui repository..."
        git -C "${TKG_GUI_CLONE_DIR}" pull --ff-only
    else
        msg2 "Cloning tkg-gui repository..."
        git clone "${TKG_GUI_REPO_URL}" "${TKG_GUI_CLONE_DIR}"
    fi

    [[ -f "${TKG_GUI_PKG_DIR}/PKGBUILD" ]] \
        || die "tkg-gui pkg/PKGBUILD not found at ${TKG_GUI_PKG_DIR}/PKGBUILD"

    pushd "${TKG_GUI_PKG_DIR}" >/dev/null
    makepkg -sf --noconfirm
    popd >/dev/null

    local count
    count=$(find "${TKG_GUI_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    (( count > 0 )) || die "makepkg produced no tkg-gui packages"
    msg2 "Built ${count} tkg-gui package(s)"
}

build_modular_packages() {
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would build modular from ${MODULAR_PKG_DIR}"
        return 0
    fi

    msg "Building Modular packages..."

    if [[ "${MODULAR_PKG_DIR}" == "${MODULAR_LOCAL_DIR}/pkg" ]]; then
        msg2 "Using local Modular repo at ${MODULAR_LOCAL_DIR}"
    elif [[ -d "${MODULAR_CLONE_DIR}/.git" ]]; then
        msg2 "Updating Modular repository..."
        git -C "${MODULAR_CLONE_DIR}" pull --ff-only
    else
        msg2 "Cloning Modular repository..."
        git clone "${MODULAR_REPO_URL}" "${MODULAR_CLONE_DIR}"
    fi

    [[ -f "${MODULAR_PKG_DIR}/PKGBUILD" ]] \
        || die "Modular pkg/PKGBUILD not found at ${MODULAR_PKG_DIR}/PKGBUILD"

    pushd "${MODULAR_PKG_DIR}" >/dev/null
    makepkg -sf --noconfirm
    popd >/dev/null

    local count
    count=$(find "${MODULAR_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    (( count > 0 )) || die "makepkg produced no Modular packages"
    msg2 "Built ${count} Modular package(s)"
}

build_gamescope_packages() {
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would build gamescope from ${GAMESCOPE_PKG_DIR}"
        return 0
    fi

    msg "Building gamescope packages..."

    if [[ ! -f "${GAMESCOPE_PKG_DIR}/PKGBUILD" ]]; then
        die "gamescope pkg/PKGBUILD not found at ${GAMESCOPE_PKG_DIR}/PKGBUILD
  The PKGBUILD is not tracked in ${GAMESCOPE_REPO_URL} upstream; it must
  exist in a sibling checkout at ${GAMESCOPE_LOCAL_DIR}.
  Clone the upstream there and place a pkg/PKGBUILD, or copy from another
  working deploytix checkout."
    fi

    msg2 "Using local gamescope repo at ${GAMESCOPE_LOCAL_DIR}"

    pushd "${GAMESCOPE_PKG_DIR}" >/dev/null
    makepkg -sf --noconfirm
    popd >/dev/null

    local count
    count=$(find "${GAMESCOPE_PKG_DIR}" -maxdepth 1 -name '*.pkg.tar.zst' | wc -l)
    (( count > 0 )) || die "makepkg produced no gamescope packages"
    msg2 "Built ${count} gamescope package(s)"
}

# ── Step C: Create local pacman repository ───────────────────────────────────
create_local_repo() {
    msg "Creating local pacman repository..."

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would create repo at ${LOCAL_REPO_DIR}"
        return 0
    fi

    sudo mkdir -p "${LOCAL_REPO_DIR}"
    sudo rm -f "${LOCAL_REPO_DIR}"/*.db* "${LOCAL_REPO_DIR}"/*.files*
    sudo rm -f "${LOCAL_REPO_DIR}"/*.pkg.tar.zst

    local pkg_count=0
    local src_dir pkg

    for src_dir in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}" "${MODULAR_PKG_DIR}" "${GAMESCOPE_PKG_DIR}"; do
        [[ -d "$src_dir" ]] || continue
        for pkg in "${src_dir}"/*.pkg.tar.zst; do
            [[ -f "$pkg" ]] || continue
            sudo cp -f "$pkg" "${LOCAL_REPO_DIR}/"
            msg2 "Added $(basename "$pkg")"
            pkg_count=$((pkg_count + 1))
        done
    done

    if (( pkg_count == 0 )); then
        die "No packages found to add to repository"
    fi

    sudo chmod 644 "${LOCAL_REPO_DIR}"/*.pkg.tar.zst
    sudo repo-add "${LOCAL_REPO_DIR}/deploytix.db.tar.zst" "${LOCAL_REPO_DIR}"/*.pkg.tar.zst

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

    if [[ -d "${TKG_GUI_CLONE_DIR}" && "${TKG_GUI_PKG_DIR}" != "${TKG_GUI_LOCAL_DIR}/pkg" ]]; then
        rm -rf "${TKG_GUI_CLONE_DIR}"
        msg2 "Removed tkg-gui clone: ${TKG_GUI_CLONE_DIR}"
    fi

    if [[ -d "${MODULAR_CLONE_DIR}" && "${MODULAR_PKG_DIR}" != "${MODULAR_LOCAL_DIR}/pkg" ]]; then
        rm -rf "${MODULAR_CLONE_DIR}"
        msg2 "Removed Modular clone: ${MODULAR_CLONE_DIR}"
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
generate_gui_profile() {
    local dest="$1"
    local de_profile="$2"

    msg2 "Merging desktop profile: ${BASE_DE_PROFILE}"

    cp "$de_profile" "$dest/profile.yaml"

    yq -i '.livefs.packages += ["deploytix-git", "deploytix-gui-git", "tkg-gui-git", "gamescope-git", "alsa-utils"]' "$dest/profile.yaml"
    yq -i '.livefs.packages -= ["calamares-extensions"]' "$dest/profile.yaml"

    msg2 "GUI profile generated (${BASE_DE_PROFILE} + deploytix)"
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
    mkdir -p "${live_repo_path}"

    local pkg_count=0
    local src_dir pkg

    for src_dir in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}" "${MODULAR_PKG_DIR}" "${GAMESCOPE_PKG_DIR}"; do
        [[ -d "$src_dir" ]] || continue
        for pkg in "${src_dir}"/*.pkg.tar.zst; do
            [[ -f "$pkg" ]] || continue
            cp -f "$pkg" "${live_repo_path}/"
            msg2 "Embedded $(basename "$pkg")"
            pkg_count=$((pkg_count + 1))
        done
    done

    (( pkg_count > 0 )) || die "No packages available to embed in live-overlay"

    repo-add "${live_repo_path}/deploytix.db.tar.zst" \
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

# ── Main ─────────────────────────────────────────────────────────────────────
main() {
    resolve_paths

    if "$RESET_MODE"; then
        reset_artifacts
        exit 0
    fi

    check_prerequisites

    msg "Building Deploytix ISO"
    msg2 "Init system: ${INITSYS}"
    msg2 "GUI mode:    ${INCLUDE_GUI}"
    if "$INCLUDE_GUI"; then
        msg2 "Desktop:     ${BASE_DE_PROFILE}"
    fi
    msg2 "Repo:        ${LOCAL_REPO_DIR}"
    msg2 "Profile:     ${WORKSPACE_PROFILES}/deploytix"
    echo

    build_packages
    create_local_repo
    install_pacman_conf
    install_profile
    embed_live_repo
    run_buildiso

    msg "Done!"
    msg2 "The pacman.conf override and profile remain installed."
    msg2 "You can now run 'sudo buildiso -p deploytix -i <init>' directly."
    msg2 "To clean up, run: $(basename "$0") -r"
}

main "$@"
