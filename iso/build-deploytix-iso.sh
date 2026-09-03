#!/usr/bin/env bash
#
# build-deploytix-iso.sh — Build a custom Artix Linux ISO with deploytix pre-installed
#
# Usage: ./build-deploytix-iso.sh [OPTIONS]
#
# Requires: artools-iso (buildiso), artools-base, artools-pkg, makepkg,
#           repo-add, go-yq
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
# -w: relocate the buildiso work directory (artools `chroots_dir`). Needed when
# the default /var/lib/artools sits on a filesystem overlayfs refuses as an
# upperdir — a live USB/ISO session, where / is itself an overlay, is the
# common case.
CHROOTS_DIR_OVERRIDE=""

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
ARTOOLS_CONF="${ARTOOLS_CONF_DIR}/artools.conf"
SYSTEM_ARTOOLS_CONF="/etc/artools/artools.conf"
DEFAULT_CHROOTS_DIR="/var/lib/artools"
CHROOTS_DIR=""          # resolved in resolve_chroots_dir()
BUILDISO_WORK_DIR=""    # ${CHROOTS_DIR}/buildiso — what buildiso overlay-mounts
# Rough floor for a full build: chroot trees + squashfs + ISO image.
MIN_WORKDIR_GIB=20

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
  -w <dir>    Build work directory (artools chroots_dir)  [default: /var/lib/artools]
              Use this when the default path is on a filesystem overlayfs cannot
              use as an upperdir — most often a live USB/ISO session, where / is
              itself an overlay. The directory must live on ext4/xfs/btrfs/f2fs.
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
  $(basename "$0") -w /mnt/build      # Live USB: build on a real disk, not the overlay
  $(basename "$0") -r                 # Remove all installed artifacts

EOF
    exit 0
}

# ── Argument parsing ─────────────────────────────────────────────────────────
ORIG_ARGS=("$@")
while getopts ":i:b:w:gscxrnhCK" opt; do
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
        w) CHROOTS_DIR_OVERRIDE="$OPTARG" ;;
        h) usage ;;
        :) die "Option -${OPTARG} requires an argument" ;;
        *) die "Unknown option: -${OPTARG}. Use -h for help." ;;
    esac
done

# ── Validate work directory override ─────────────────────────────────────────
if [[ -n "$CHROOTS_DIR_OVERRIDE" && "$CHROOTS_DIR_OVERRIDE" != /* ]]; then
    die "-w requires an absolute path (got '${CHROOTS_DIR_OVERRIDE}')"
fi

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
    resolve_chroots_dir
    # The repo lives beside the chroots so both sit on the same filesystem —
    # with the default chroots_dir this is the historical /var/lib/artools path.
    LOCAL_REPO_DIR="${CHROOTS_DIR}/repos/deploytix"
    PROFILE_SRC="${ISO_DIR}/profile/deploytix"
    TKG_GUI_PKG_DIR="${REPO_ROOT}/vendor/tkg-gui/pkg"
    GAMESCOPE_PKG_DIR="${REPO_ROOT}/vendor/gamescope/pkg"
}

# ── Build work directory (artools chroots_dir) ───────────────────────────────
#
# buildiso assembles the live filesystem with an overlay mount whose upperdir is
# ${chroots_dir}/buildiso/<profile>/<arch>/livefs. The kernel refuses an upperdir
# on a filesystem that cannot carry trusted.overlay.* xattrs or that is itself an
# overlay, and fails with:
#
#   fsconfig() overlay failed: filesystem on .../livefs not supported as upperdir
#
# That is exactly the situation in a live USB/ISO session: / is squashfs + a COW
# overlay, so the default /var/lib/artools is on an overlay and nesting is
# rejected. Same for tmpfs (a RAM-backed live session), FAT/exFAT/NTFS sticks and
# network mounts. Catch it here, before an hour of package building is wasted.

# Filesystems overlayfs accepts as an upperdir, in practice.
WORKDIR_FS_OK="ext2 ext3 ext4 xfs btrfs f2fs"
# Filesystems known to be rejected, with the reason worth printing.
workdir_fs_reason() {
    case "$1" in
        overlay)          printf '%s\n' "it is itself an overlay (a live USB/ISO session, or a container)" ;;
        tmpfs|ramfs)      printf '%s\n' "it is RAM-backed — older kernels reject it as an upperdir outright, and on newer ones the chroot trees and squashfs would be built in memory" ;;
        vfat|msdos|exfat) printf '%s\n' "FAT-family filesystems have no xattrs or POSIX ownership" ;;
        ntfs|ntfs3|fuseblk|fuse|fuse.*) printf '%s\n' "FUSE/NTFS mounts do not support the required xattrs" ;;
        nfs|nfs4|cifs|smb3|9p|virtiofs) printf '%s\n' "network and shared filesystems cannot be an overlay upperdir" ;;
        squashfs|erofs|iso9660) printf '%s\n' "it is read-only" ;;
        *)                return 1 ;;
    esac
}

# Read a key from an artools config file (shell-style `key=value`).
conf_value() {
    local key="$1" file="$2" v
    [[ -f "$file" ]] || return 1
    v="$(sed -n -E "s/^[[:space:]]*${key}=//p" "$file" | tail -n1)"
    [[ -n "$v" ]] || return 1
    v="${v%\"}"; v="${v#\"}"; v="${v%\'}"; v="${v#\'}"
    printf '%s\n' "$v"
}

resolve_chroots_dir() {
    local v
    if [[ -n "$CHROOTS_DIR_OVERRIDE" ]]; then
        CHROOTS_DIR="${CHROOTS_DIR_OVERRIDE%/}"
    elif v="$(conf_value chroots_dir "$ARTOOLS_CONF")"; then
        CHROOTS_DIR="${v%/}"
    elif v="$(conf_value chroots_dir "$SYSTEM_ARTOOLS_CONF")"; then
        CHROOTS_DIR="${v%/}"
    else
        CHROOTS_DIR="$DEFAULT_CHROOTS_DIR"
    fi
    BUILDISO_WORK_DIR="${CHROOTS_DIR}/buildiso"

    # buildiso writes the finished ISO to ${workspace_dir}/iso, which defaults to
    # ${HOME}/artools-workspace. On a live session that is the overlay (or RAM),
    # so a multi-gigabyte ISO there fails just as surely as the chroot does —
    # move the workspace next to the chroots whenever -w relocates the build.
    if [[ -n "$CHROOTS_DIR_OVERRIDE" ]]; then
        WORKSPACE_DIR="${CHROOTS_DIR}/workspace"
    elif v="$(conf_value workspace_dir "$ARTOOLS_CONF")" \
      || v="$(conf_value workspace_dir "$SYSTEM_ARTOOLS_CONF")"; then
        # Honour an existing workspace_dir; the value may reference ${HOME}.
        eval "WORKSPACE_DIR=\"${v}\""
    fi
    WORKSPACE_DIR="${WORKSPACE_DIR%/}"
    WORKSPACE_PROFILES="${WORKSPACE_DIR}/iso-profiles"
}

# Walk up to the first component that exists, so a not-yet-created work dir can
# still be classified by the filesystem it would land on.
nearest_existing() {
    local path="$1"
    while [[ -n "$path" && ! -e "$path" ]]; do
        path="$(dirname "$path")"
        [[ "$path" == "/" ]] && break
    done
    printf '%s\n' "${path:-/}"
}

fstype_of() {
    local fs
    # findmnt -T can print several lines for stacked mounts (/dev/shm, bind
    # mounts); the first is the one the path actually lands on. Without the
    # head -n1 the value is multi-line and every fstype comparison misses.
    fs="$(findmnt -no FSTYPE -T "$(nearest_existing "$1")" 2>/dev/null | head -n1)"
    printf '%s\n' "${fs:-unknown}"
}

# Mounts that could host the work directory: right filesystem, writable, roomy.
suggest_workdir_candidates() {
    local target fstype avail_kib found=0
    while read -r target fstype avail_kib; do
        [[ " ${WORKDIR_FS_OK} " == *" ${fstype} "* ]] || continue
        [[ -w "$target" ]] || [[ "$(id -u)" == 0 ]] || continue
        (( avail_kib / 1048576 >= MIN_WORKDIR_GIB )) || continue
        printf '    %-30s %-6s %s GiB free\n' "$target" "$fstype" "$(( avail_kib / 1048576 ))"
        found=1
    done < <(findmnt -rno TARGET,FSTYPE,AVAIL --bytes 2>/dev/null \
             | awk '{ printf "%s %s %d\n", $1, $2, $3 / 1024 }' | sort -u)
    (( found )) || printf '    (none found — attach a disk formatted ext4/xfs/btrfs)\n'
}

# Live probe: mount a throwaway overlay with the work dir as upperdir. The fstype
# table above catches the known cases; this catches everything else (a kernel
# without redirect_dir, a nodev/noexec mount, a read-only remount) for the same
# reason buildiso would, but in one second instead of at the end of the build.
probe_overlay_upperdir() {
    local base="$1" probe rc=0
    probe="$(sudo mktemp -d "${base}/.deploytix-overlay-probe.XXXXXX")" || return 2
    sudo mkdir -p "${probe}/lower" "${probe}/upper" "${probe}/work" "${probe}/merged"
    if sudo mount -t overlay deploytix-probe \
            -o "lowerdir=${probe}/lower,upperdir=${probe}/upper,workdir=${probe}/work" \
            "${probe}/merged" 2>/dev/null; then
        sudo umount "${probe}/merged" || true
    else
        rc=1
    fi
    sudo rm -rf "$probe"
    return "$rc"
}

check_workdir() {
    msg "Checking build work directory..."
    local fstype reason root_fstype avail_gib

    fstype="$(fstype_of "$CHROOTS_DIR")"
    root_fstype="$(findmnt -no FSTYPE / 2>/dev/null | head -n1)"
    root_fstype="${root_fstype:-unknown}"

    if reason="$(workdir_fs_reason "$fstype")"; then
        err "The build work directory cannot live on a '${fstype}' filesystem."
        printf '\n'
        printf '  buildiso overlay-mounts %s\n' "${BUILDISO_WORK_DIR}/deploytix/artix/livefs"
        printf '  as the upperdir, which cannot work here: %s.\n' "$reason"
        printf '  That is what the "not supported as upperdir" failure means.\n\n'
        if [[ "$root_fstype" == "overlay" || "$root_fstype" == "tmpfs" ]]; then
            printf '  This looks like a live USB/ISO session (/ is %s), so no path under /\n' "$root_fstype"
            printf '  will work — including /var/lib/artools, /tmp and your home directory.\n'
            printf '  Build onto a real disk partition instead (ext4/xfs/btrfs), mounted\n'
            printf '  from the live session:\n\n'
            printf '      sudo mkdir -p /mnt/build\n'
            printf '      sudo mount /dev/sdXY /mnt/build          # an ext4/xfs/btrfs partition\n'
            printf '      %s -w /mnt/build %s\n\n' "$(basename "$0")" "${ORIG_ARGS[*]-}"
            printf '  A %s GiB-plus filesystem is needed. The ext4 persistence partition written\n' "$MIN_WORKDIR_GIB"
            printf '  by write-deploytix-usb.sh is already mounted directly (typically at\n'
            printf '  /run/artix/cowspace) and works as long as it is that large — but the build\n'
            printf '  then eats the live session%s writable space, so an external disk is safer.\n\n' "'s"
        else
            printf '  Point the build at a directory on ext4/xfs/btrfs with -w <dir>.\n\n'
        fi
        printf '  Candidate mounts on this system:\n'
        suggest_workdir_candidates
        printf '\n'
        die "Unsuitable work directory: ${CHROOTS_DIR} (${fstype})"
    fi

    if [[ " ${WORKDIR_FS_OK} " != *" ${fstype} "* ]]; then
        warn "Work directory ${CHROOTS_DIR} is on '${fstype}', which is untested as an overlay upperdir"
    fi

    local avail_bytes
    # An empty df result would make the arithmetic below a syntax error, so
    # default it before dividing.
    avail_bytes="$(df -B1 --output=avail "$(nearest_existing "$CHROOTS_DIR")" 2>/dev/null \
                   | tail -n1 | tr -dc '0-9')"
    avail_gib=$(( ${avail_bytes:-0} / 1073741824 ))
    if (( avail_gib < MIN_WORKDIR_GIB )); then
        warn "Only ${avail_gib} GiB free on ${CHROOTS_DIR} — a full build needs roughly ${MIN_WORKDIR_GIB} GiB"
    fi

    if "$DRY_RUN"; then
        msg2 "[dry-run] Would probe overlay upperdir support under ${CHROOTS_DIR}"
        msg2 "Work directory: ${CHROOTS_DIR} (${fstype}, ${avail_gib} GiB free)"
        return 0
    fi

    sudo mkdir -p "$CHROOTS_DIR"
    if ! probe_overlay_upperdir "$CHROOTS_DIR"; then
        err "The kernel refused an overlay upperdir under ${CHROOTS_DIR} (filesystem: ${fstype})."
        printf '\n  buildiso would fail the same way at the livefs mount. Pick another\n'
        printf '  directory with -w <dir>. Candidate mounts on this system:\n'
        suggest_workdir_candidates
        printf '\n'
        die "Overlay upperdir probe failed for ${CHROOTS_DIR}"
    fi

    msg2 "Work directory: ${CHROOTS_DIR} (${fstype}, ${avail_gib} GiB free) — overlay upperdir OK"
}

# ── Persist chroots_dir for buildiso ─────────────────────────────────────────
# buildiso reads chroots_dir from artools.conf, so -w has to be written there.
# Both the user and the system config are updated: buildiso re-execs itself under
# sudo, and whether it then reads ~/.config or /etc depends on how sudo handles
# HOME. Both are backed up and restored by -r.
MARKER_COMMENT="# ── Deploytix build work dir (auto-generated by build-deploytix-iso.sh) ──"
CREATED_MARKER="# deploytix-created: this file was created by build-deploytix-iso.sh"

# Run a command as root only when the target path needs it.
as_owner() {
    local target="$1"; shift
    if [[ -w "$(dirname "$target")" && ( ! -e "$target" || -w "$target" ) ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

write_chroots_dir_conf() {
    local target="$1" tmp created=""

    tmp="$(mktemp)"
    if [[ -f "$target" ]]; then
        # Back up only a config we have not already rewritten — otherwise a
        # second run would snapshot our own override and -r would "restore" it.
        if [[ ! -f "${target}.deploytix-bak" ]] && ! grep -qF "$MARKER_COMMENT" "$target"; then
            as_owner "${target}.deploytix-bak" cp "$target" "${target}.deploytix-bak"
            msg2 "Backed up ${target} → $(basename "$target").deploytix-bak"
        fi
        grep -vF -e "$MARKER_COMMENT" "$target" \
            | grep -v -E '^[[:space:]]*(chroots_dir|workspace_dir)=' > "$tmp" || true
    else
        # A user config that only sets chroots_dir would drop every other
        # artools default, so seed it from the system config when there is one.
        created="yes"
        if [[ -f "$SYSTEM_ARTOOLS_CONF" ]]; then
            grep -vF -e "$MARKER_COMMENT" "$SYSTEM_ARTOOLS_CONF" \
                | grep -v -E '^[[:space:]]*(chroots_dir|workspace_dir)=' > "$tmp" || true
        fi
        printf '%s\n' "$CREATED_MARKER" >> "$tmp"
    fi
    printf '%s\nchroots_dir=%s\nworkspace_dir=%s\n' \
        "$MARKER_COMMENT" "$CHROOTS_DIR" "$WORKSPACE_DIR" >> "$tmp"

    as_owner "$target" install -Dm644 "$tmp" "$target"
    rm -f "$tmp"
    msg2 "Set chroots_dir=${CHROOTS_DIR}, workspace_dir=${WORKSPACE_DIR} in ${target}${created:+ (created)}"
}

install_artools_conf() {
    [[ -n "$CHROOTS_DIR_OVERRIDE" ]] || return 0

    msg "Configuring artools work directory..."
    if "$DRY_RUN"; then
        msg2 "[dry-run] Would set chroots_dir=${CHROOTS_DIR} and workspace_dir=${WORKSPACE_DIR}"
        msg2 "[dry-run]   in ${ARTOOLS_CONF} and ${SYSTEM_ARTOOLS_CONF}"
        return 0
    fi

    mkdir -p "$ARTOOLS_CONF_DIR"
    write_chroots_dir_conf "$ARTOOLS_CONF"
    write_chroots_dir_conf "$SYSTEM_ARTOOLS_CONF"
    prepare_workspace_dir
}

# The relocated work dir is created by root (check_workdir), so the workspace
# under it has to be handed back to the invoking user — install_profile() writes
# there without sudo.
prepare_workspace_dir() {
    [[ -n "$CHROOTS_DIR_OVERRIDE" ]] || return 0
    sudo mkdir -p "$WORKSPACE_PROFILES" "${WORKSPACE_DIR}/iso"
    sudo chown "$(id -u):$(id -g)" "$WORKSPACE_DIR" "$WORKSPACE_PROFILES"
    msg2 "Workspace ready at ${WORKSPACE_DIR}"
}

restore_artools_conf() {
    local target
    for target in "$ARTOOLS_CONF" "$SYSTEM_ARTOOLS_CONF"; do
        if [[ -f "${target}.deploytix-bak" ]]; then
            as_owner "$target" mv "${target}.deploytix-bak" "$target"
            msg2 "Restored ${target}"
        elif [[ -f "$target" ]] && grep -qF "$CREATED_MARKER" "$target"; then
            as_owner "$target" rm -f "$target"
            msg2 "Removed ${target} (created by this script)"
        elif [[ -f "$target" ]] && grep -qF "$MARKER_COMMENT" "$target"; then
            local tmp
            tmp="$(mktemp)"
            grep -vF -e "$MARKER_COMMENT" "$target" \
                | grep -v -E '^[[:space:]]*(chroots_dir|workspace_dir)=' > "$tmp" || true
            as_owner "$target" install -Dm644 "$tmp" "$target"
            rm -f "$tmp"
            msg2 "Removed chroots_dir override from ${target}"
        fi
    done
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
  Install: pacman -S artools-base artools-pkg artools-iso iso-profiles base-devel go-yq git"
    fi

    [[ -f "${PKG_DIR}/PKGBUILD" ]] || die "PKGBUILD not found at ${PKG_DIR}/PKGBUILD"
    [[ -f "${PROFILE_SRC}/profile.yaml" ]] || die "Profile not found at ${PROFILE_SRC}/profile.yaml"
    [[ -f "${SYSTEM_PACMAN_CONF}" ]] || die "System pacman.conf not found at ${SYSTEM_PACMAN_CONF}"

    ensure_submodules
    check_workdir

    msg2 "All prerequisites satisfied"
    msg2 "Build source: ${BUILD_SOURCE}"
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

    restore_artools_conf

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

    verify_zram_service "$dest"

    msg2 "Profile installed at ${dest}"
}

# ── zram service sanity check ────────────────────────────────────────────────
#
# The profile lists `zram` in live-session.services, so buildiso will try to
# enable it. If the merge dropped the service definition for the init system we
# are actually building, the ISO ships an enablement pointing at nothing and the
# live session boots with no swap — silently, which is the whole failure mode
# this is meant to catch. Fail the build instead.
verify_zram_service() {
    local dest="$1"
    local overlay="${dest}/root-overlay"
    local worker="${overlay}/usr/local/bin/deploytix-zram-swap"
    local unit

    case "$INITSYS" in
        runit)  unit="${overlay}/etc/runit/sv/zram/run" ;;
        openrc) unit="${overlay}/etc/init.d/zram" ;;
        s6)     unit="${overlay}/etc/s6/adminsv/zram/up" ;;
        dinit)  unit="${overlay}/etc/dinit.d/zram" ;;
        *)      die "verify_zram_service: unhandled init '${INITSYS}'" ;;
    esac

    [[ -f "$unit" ]] \
        || die "zram service for ${INITSYS} missing from the staged profile (${unit})"
    [[ -x "$worker" ]] \
        || die "zram worker missing or not executable (${worker})"

    msg2 "zram swap service staged for ${INITSYS}"
}

# ── GUI profile generation ───────────────────────────────────────────────────
generate_gui_profile() {
    local dest="$1"
    local de_profile="$2"

    msg2 "Merging desktop profile: ${BASE_DE_PROFILE}"

    cp "$de_profile" "$dest/profile.yaml"

    # artools was split into artools-base (basestrap, artix-chroot — what
    # deploytix calls), artools-pkg and artools-iso; the plain `artools` name no
    # longer resolves, so all three are named explicitly.
    yq -i '.livefs.packages += ["deploytix-git", "deploytix-gui-git", "tkg-gui-git", "gamescope-git", "alsa-utils", "artools-base", "artools-pkg", "artools-iso", "iso-profiles", "go-yq", "xorg-xset"]' "$dest/profile.yaml"
    yq -i '.livefs.packages -= ["calamares-extensions"]' "$dest/profile.yaml"
    # Remove packages from the base DE profile that are unavailable in Artix repos
    yq -i '.rootfs.packages -= ["artix-breeze-sddm"]' "$dest/profile.yaml"
    # zram swap. The cp above replaced deploytix's profile.yaml wholesale with the
    # DE one, so the service enablement listed there is gone and has to be
    # re-applied here — this is the path every default build takes.
    # The key is quoted: unlike the unhyphenated paths above, a bare
    # .live-session risks being read as a subtraction.
    yq -i '."live-session".services += ["zram"]' "$dest/profile.yaml"

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
    msg2 "Work dir:      ${CHROOTS_DIR}"
    msg2 "Workspace:     ${WORKSPACE_DIR}"
    msg2 "Profile:       ${WORKSPACE_PROFILES}/deploytix"
    echo

    install_artools_conf
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
