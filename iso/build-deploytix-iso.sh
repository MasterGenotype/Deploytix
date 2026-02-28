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
REPO_ROOT=""          # Deploytix git repo root
ISO_DIR=""            # iso/ directory inside repo
PKG_DIR=""            # pkg/ directory inside repo
LOCAL_REPO_DIR=""     # pacman repository in artools workspace
PROFILE_SRC=""        # iso/profile/deploytix/
TKG_GUI_DIR="${HOME}/.gitrepos/tkg-gui"   # tkg-gui source repo
TKG_GUI_PKG_DIR="${TKG_GUI_DIR}/pkg"      # tkg-gui PKGBUILD directory
TKG_GUI_PKG=""        # resolved path to the tkg-gui .pkg.tar.zst
WORKSPACE_DIR="${HOME}/artools-workspace"
WORKSPACE_PROFILES="${WORKSPACE_DIR}/iso-profiles"
ARTOOLS_CONF_DIR="${HOME}/.config/artools"
PACMAN_CONF_DIR="${ARTOOLS_CONF_DIR}/pacman.conf.d"
PACMAN_CONF_NAME="iso-x86_64.conf"
SYSTEM_PACMAN_CONF="/usr/share/artools/pacman.conf.d/${PACMAN_CONF_NAME}"

# ── External package sources ─────────────────────────────────────────────────
TKG_GUI_REPO_URL="https://github.com/MasterGenotype/tkg-gui.git"
TKG_GUI_CLONE_DIR=""  # resolved in resolve_paths()
TKG_GUI_PKG_DIR=""    # pkg/ inside the tkg-gui clone

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
    # Find repo root: either we're in it, or in iso/
    if [[ -f "Cargo.toml" && -d "pkg" && -d "iso" ]]; then
        REPO_ROOT="$(pwd)"
    elif [[ -f "../Cargo.toml" && -d "../pkg" ]]; then
        REPO_ROOT="$(cd .. && pwd)"
    else
        die "Cannot find Deploytix repository root. Run from the repo root or iso/ directory."
    fi

    ISO_DIR="${REPO_ROOT}/iso"
    PKG_DIR="${REPO_ROOT}/pkg"
    LOCAL_REPO_DIR="/var/lib/artools/repos/deploytix"
    PROFILE_SRC="${ISO_DIR}/profile/deploytix"
    TKG_GUI_CLONE_DIR="${WORKSPACE_DIR}/tkg-gui-src"
    TKG_GUI_PKG_DIR="${TKG_GUI_CLONE_DIR}/pkg"
}

# ── Prerequisites ────────────────────────────────────────────────────────────
check_prerequisites() {
    msg "Checking prerequisites..."
    local missing=()

    for cmd in buildiso makepkg repo-add yq; do
        if ! command -v "$cmd" &>/dev/null; then
            missing+=("$cmd")
        fi
    done

    if (( ${#missing[@]} > 0 )); then
        die "Missing required commands: ${missing[*]}\n  Install: pacman -S artools iso-profiles base-devel go-yq"
    fi

    [[ -f "${PKG_DIR}/PKGBUILD" ]] || die "PKGBUILD not found at ${PKG_DIR}/PKGBUILD"
    [[ -f "${PROFILE_SRC}/profile.yaml" ]] || die "Profile not found at ${PROFILE_SRC}/profile.yaml"
    [[ -f "${SYSTEM_PACMAN_CONF}" ]] || die "System pacman.conf not found at ${SYSTEM_PACMAN_CONF}"

    msg2 "All prerequisites satisfied"
}

# ── Step B1: Resolve or build tkg-gui package ────────────────────────────────
build_tkg_gui_package() {
    msg "Resolving tkg-gui package..."

    if [[ ! -d "$TKG_GUI_DIR" ]]; then
        die "tkg-gui repo not found at ${TKG_GUI_DIR}"
    fi

    # Look for the most recent tkg-gui .pkg.tar.zst in pkg/ or repo root
    local latest_pkg=""
    latest_pkg=$(
        find "${TKG_GUI_DIR}" -maxdepth 2 -name 'tkg-gui*.pkg.tar.zst' -type f \
            -printf '%T@ %p\n' 2>/dev/null \
        | sort -rn | head -1 | cut -d' ' -f2-
    )

    if [[ -n "$latest_pkg" ]] && "$SKIP_REBUILD"; then
        msg2 "Reusing existing tkg-gui package: $(basename "$latest_pkg")"
        TKG_GUI_PKG="$latest_pkg"
        return 0
    fi

    if [[ -n "$latest_pkg" ]] && ! "$SKIP_REBUILD"; then
        msg2 "Found existing package but rebuilding: $(basename "$latest_pkg")"
    fi

    if [[ ! -f "${TKG_GUI_PKG_DIR}/PKGBUILD" ]]; then
        die "tkg-gui PKGBUILD not found at ${TKG_GUI_PKG_DIR}/PKGBUILD"
    fi

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would build tkg-gui from ${TKG_GUI_PKG_DIR}/PKGBUILD"
        return 0
    fi

    msg2 "Building tkg-gui from PKGBUILD..."
    pushd "${TKG_GUI_PKG_DIR}" >/dev/null
    makepkg -sf --noconfirm
    popd >/dev/null

    # Resolve the newly built package
    latest_pkg=$(
        find "${TKG_GUI_PKG_DIR}" -maxdepth 1 -name 'tkg-gui*.pkg.tar.zst' -type f \
            -printf '%T@ %p\n' 2>/dev/null \
        | sort -rn | head -1 | cut -d' ' -f2-
    )

    if [[ -z "$latest_pkg" ]]; then
        die "tkg-gui makepkg produced no package"
    fi

    TKG_GUI_PKG="$latest_pkg"
    msg2 "Built tkg-gui package: $(basename "$TKG_GUI_PKG")"
}

# ── Step B2: Build deploytix packages ────────────────────────────────────────
build_packages() {
    if "$SKIP_REBUILD"; then
        # Verify packages exist in both source dirs
        local count
        count=$(find "${PKG_DIR}" "${TKG_GUI_PKG_DIR}" \
            -maxdepth 1 -name '*.pkg.tar.zst' 2>/dev/null | wc -l)
        if (( count == 0 )); then
            die "No .pkg.tar.zst found in ${PKG_DIR}/ or ${TKG_GUI_PKG_DIR}/ and -s (skip rebuild) was set"
        fi
        msg "Skipping package build (-s); reusing existing packages"
        return 0
    fi

    # ── Deploytix packages ───────────────────────────────────────────────────
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

    # ── tkg-gui packages ─────────────────────────────────────────────────────
    build_tkg_gui_packages
}

# Clone (or update) tkg-gui and build its pkg/PKGBUILD
build_tkg_gui_packages() {
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would clone/update ${TKG_GUI_REPO_URL} and run makepkg"
        return 0
    fi

    msg "Building tkg-gui packages..."

    if [[ -d "${TKG_GUI_CLONE_DIR}/.git" ]]; then
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

# ── Step C: Create local pacman repository ───────────────────────────────────
create_local_repo() {
    msg "Creating local pacman repository..."

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would create repo at ${LOCAL_REPO_DIR}"
        return 0
    fi

    sudo mkdir -p "${LOCAL_REPO_DIR}"

    # Clean old repo data
    sudo rm -f "${LOCAL_REPO_DIR}"/*.db* "${LOCAL_REPO_DIR}"/*.files*

    # Clean old packages
    sudo rm -f "${LOCAL_REPO_DIR}"/*.pkg.tar.zst

    # Copy packages from all source directories
    local pkg_count=0
    for src_dir in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}"; do
        for pkg in "${src_dir}"/*.pkg.tar.zst; do
            [[ -f "$pkg" ]] || continue
            sudo cp -f "$pkg" "${LOCAL_REPO_DIR}/"
            msg2 "Added $(basename "$pkg")"
            pkg_count=$((pkg_count + 1))
        done
    done

    # Copy tkg-gui package if available
    if [[ -n "$TKG_GUI_PKG" && -f "$TKG_GUI_PKG" ]]; then
        sudo cp -f "$TKG_GUI_PKG" "${LOCAL_REPO_DIR}/"
        msg2 "Added $(basename "$TKG_GUI_PKG") (tkg-gui)"
        pkg_count=$((pkg_count + 1))
    fi

    if (( pkg_count == 0 )); then
        die "No packages found to add to repository"
    fi

    # Make packages world-readable for pacman's alpm user
    sudo chmod 644 "${LOCAL_REPO_DIR}"/*.pkg.tar.zst

    # Create pacman database
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

    # If an override already contains [deploytix], check if it points to the right place
    if [[ -f "$target" ]] && grep -q '^\[deploytix\]' "$target"; then
        if grep -q "Server = file://${LOCAL_REPO_DIR}" "$target"; then
            msg2 "pacman.conf already configured — skipping"
            return 0
        fi
        # Wrong repo path — rebuild it
        msg2 "Updating existing [deploytix] repo path"
    fi

    # Back up existing override if present and not ours
    if [[ -f "$target" ]] && ! grep -q '^\[deploytix\]' "$target"; then
        PACMAN_CONF_BACKUP="${target}.deploytix-bak"
        cp "$target" "$PACMAN_CONF_BACKUP"
        msg2 "Backed up existing ${PACMAN_CONF_NAME} → $(basename "${PACMAN_CONF_BACKUP}")"
    fi

    # Start from the system config
    cp "${SYSTEM_PACMAN_CONF}" "$target"

    # Append the deploytix local repo
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

    # Restore or remove pacman.conf override
    if [[ -f "${target}.deploytix-bak" ]]; then
        mv "${target}.deploytix-bak" "$target"
        msg2 "Restored original ${PACMAN_CONF_NAME}"
    elif [[ -f "$target" ]]; then
        rm -f "$target"
        msg2 "Removed custom ${PACMAN_CONF_NAME}"
    fi

    # Remove installed profile
    if [[ -d "$dest" ]]; then
        rm -rf "$dest"
        msg2 "Removed profile: ${dest}"
    fi

    # Remove local repo
    if [[ -d "${LOCAL_REPO_DIR}" ]]; then
        sudo rm -rf "${LOCAL_REPO_DIR}"
        msg2 "Removed repo: ${LOCAL_REPO_DIR}"
    fi

    # Remove tkg-gui clone
    if [[ -d "${TKG_GUI_CLONE_DIR}" ]]; then
        rm -rf "${TKG_GUI_CLONE_DIR}"
        msg2 "Removed tkg-gui clone: ${TKG_GUI_CLONE_DIR}"
    fi

    msg "Reset complete"
}

# ── Step E: Install ISO profile ──────────────────────────────────────────────
install_profile() {
    msg "Installing deploytix ISO profile..."
    local dest="${WORKSPACE_PROFILES}/deploytix"

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would install profile to ${dest}"
        return 0
    fi

    mkdir -p "${WORKSPACE_PROFILES}"

    # Remove old profile
    rm -rf "$dest"
    mkdir -p "$dest"

    if "$INCLUDE_GUI"; then
        generate_gui_profile "$dest"
    else
        # Copy the base CLI profile
        cp "${PROFILE_SRC}/profile.yaml" "$dest/profile.yaml"
    fi

    # Set root-overlay symlink to common
    ln -sfn "../common/root-overlay" "$dest/root-overlay"

    # Copy live-overlay if it exists in our profile source
    if [[ -d "${PROFILE_SRC}/live-overlay" ]]; then
        cp -a "${PROFILE_SRC}/live-overlay" "$dest/"
    fi

    msg2 "Profile installed at ${dest}"
}

# ── GUI profile generation ───────────────────────────────────────────────────
generate_gui_profile() {
    local dest="$1"
    local de_profile="${WORKSPACE_PROFILES}/${BASE_DE_PROFILE}/profile.yaml"

    if [[ ! -f "$de_profile" ]]; then
        # Fall back to system profiles
        de_profile="/usr/share/artools/iso-profiles/${BASE_DE_PROFILE}/profile.yaml"
    fi

    if [[ ! -f "$de_profile" ]]; then
        die "Desktop profile '${BASE_DE_PROFILE}' not found in workspace or system iso-profiles"
    fi

    msg2 "Merging desktop profile: ${BASE_DE_PROFILE}"

    # Start from the DE profile and inject deploytix packages
    cp "$de_profile" "$dest/profile.yaml"

    # Add deploytix and tkg-gui packages to livefs (live session only) and remove calamares
    yq -i '.livefs.packages += ["deploytix-git", "deploytix-gui-git", "tkg-gui-git"]' "$dest/profile.yaml"
    yq -i '.livefs.packages -= ["calamares-extensions"]' "$dest/profile.yaml"

    # Copy overlays from the DE profile
    local de_dir
    de_dir="$(dirname "$de_profile")"

    if [[ -L "${de_dir}/root-overlay" ]]; then
        # Resolve the symlink target relative to workspace
        local link_target
        link_target="$(readlink "${de_dir}/root-overlay")"
        ln -sfn "$link_target" "$dest/root-overlay"
    elif [[ -d "${de_dir}/root-overlay" ]]; then
        cp -a "${de_dir}/root-overlay" "$dest/"
    fi

    if [[ -L "${de_dir}/live-overlay" ]]; then
        local link_target
        link_target="$(readlink "${de_dir}/live-overlay")"
        ln -sfn "$link_target" "$dest/live-overlay"
    elif [[ -d "${de_dir}/live-overlay" ]]; then
        cp -a "${de_dir}/live-overlay" "$dest/"
    fi

    msg2 "GUI profile generated (${BASE_DE_PROFILE} + deploytix)"
}

# ── Step F: Embed built packages in the live-overlay ─────────────────────────
# The live ISO's pacman.conf (set up by buildiso) points [deploytix] at the
# build machine's LOCAL_REPO_DIR, which does not exist on the booted live
# system.  To let basestrap install deploytix-git and tkg-gui-git onto the
# target disk, we embed the .pkg.tar.zst files and a pacman database directly
# into the live-overlay and include a matching pacman.conf that points to the
# in-ISO path so the live environment's pacman can find them at runtime.
embed_live_repo() {
    msg "Embedding packages in live-overlay for basestrap use..."
    local dest="${WORKSPACE_PROFILES}/deploytix"
    local live_overlay_dir="${dest}/live-overlay"
    local live_repo_path="${live_overlay_dir}/var/lib/deploytix-repo"
    local live_etc_dir="${live_overlay_dir}/etc"

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would embed packages at ${live_repo_path}"
        return 0
    fi

    # If the live-overlay is a symlink (e.g. pointing to a DE profile's overlay),
    # materialise it into a real directory so we can safely add files without
    # modifying the symlink target.
    if [[ -L "${live_overlay_dir}" ]]; then
        local link_target
        link_target="$(readlink -f "${live_overlay_dir}")"
        rm "${live_overlay_dir}"
        if [[ -d "${link_target}" ]]; then
            cp -a "${link_target}" "${live_overlay_dir}"
        else
            mkdir -p "${live_overlay_dir}"
        fi
        msg2 "Materialised live-overlay symlink into real directory"
    fi

    mkdir -p "${live_repo_path}" "${live_etc_dir}"

    # Copy all built packages into the in-ISO repo directory
    local pkg_count=0
    for src_dir in "${PKG_DIR}" "${TKG_GUI_PKG_DIR}"; do
        for pkg in "${src_dir}"/*.pkg.tar.zst; do
            [[ -f "$pkg" ]] || continue
            cp -f "$pkg" "${live_repo_path}/"
            msg2 "Embedded $(basename "$pkg")"
            pkg_count=$((pkg_count + 1))
        done
    done

    (( pkg_count > 0 )) || die "No packages available to embed in live-overlay"

    # Build a pacman database from the embedded packages so pacman/basestrap
    # can resolve and install them inside the live environment.
    repo-add "${live_repo_path}/deploytix.db.tar.zst" \
        "${live_repo_path}"/*.pkg.tar.zst

    # Generate a pacman.conf for the live system:
    #   • base it on the system artools conf so all standard Artix repos are present
    #   • append [deploytix] pointing to the in-ISO path
    # This file, placed in the live-overlay, overrides the default pacman.conf
    # installed by the base Artix system packages.
    cp "${SYSTEM_PACMAN_CONF}" "${live_etc_dir}/pacman.conf"
    cat >> "${live_etc_dir}/pacman.conf" <<EOF

# ── Deploytix local repository (embedded in ISO for basestrap use) ──
[deploytix]
SigLevel = Optional TrustAll
Server = file:///var/lib/deploytix-repo
EOF

    msg2 "Embedded ${pkg_count} package(s); [deploytix] repo available at /var/lib/deploytix-repo"
}

# ── Step H: Run buildiso ─────────────────────────────────────────────────────
run_buildiso() {
    msg "Building ISO (init=${INITSYS}, profile=deploytix)..."

    local args=(-p deploytix -i "$INITSYS")

    if "$CLEAN_BUILD"; then
        args+=()  # buildiso cleans by default unless -c is passed to disable it
    else
        args+=(-c)  # -c disables clean, preserving previous work
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

    # Report ISO location
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

    # Handle reset mode early
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

    build_tkg_gui_package
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
