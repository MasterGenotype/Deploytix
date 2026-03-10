#!/usr/bin/env bash
#
# write-deploytix-usb.sh — Write a Deploytix ISO to USB with persistence
#
# Usage: ./write-deploytix-usb.sh [-d /dev/sdX] [-i /path/to/iso] [-l LABEL] [-y]
#
# If no ISO path is given, the most recent .iso in the default ISO output
# directory is used automatically.

set -euo pipefail

# ── Colour helpers ───────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
msg()  { printf "${GREEN}==> %s${NC}\n" "$*"; }
msg2() { printf "${BLUE}  -> %s${NC}\n" "$*"; }
warn() { printf "${YELLOW}==> WARNING: %s${NC}\n" "$*"; }
err()  { printf "${RED}==> ERROR: %s${NC}\n" "$*" >&2; }
die()  { err "$@"; exit 1; }

# ── Defaults ─────────────────────────────────────────────────────────────────
ISO_DIR="${HOME}/artools-workspace/iso/deploytix"
ISO_PATH=""
DEVICE=""
COW_LABEL="cow_persistence"
SKIP_CONFIRM=false

# ── Usage ────────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Write a Deploytix ISO to a USB device with persistent storage.

The script will:
  1. Write the ISO image to the target device (dd)
  2. Create a persistence partition (ext4, labeled '${COW_LABEL}')
  3. Patch the GRUB config to enable persistence automatically

Options:
  -d <device>   Target block device (e.g. /dev/sdb)          [required]
  -i <iso>      Path to ISO file                              [auto-detect]
  -l <label>    Persistence partition label                    [default: ${COW_LABEL}]
  -y            Skip confirmation prompt
  -h            Show this help

Examples:
  $(basename "$0") -d /dev/sdb
  $(basename "$0") -d /dev/sdb -i /path/to/artix-deploytix-runit-20260309-x86_64.iso
  $(basename "$0") -d /dev/sdb -y

EOF
    exit 0
}

# ── Argument parsing ─────────────────────────────────────────────────────────
while getopts ":d:i:l:yh" opt; do
    case "$opt" in
        d) DEVICE="$OPTARG" ;;
        i) ISO_PATH="$OPTARG" ;;
        l) COW_LABEL="$OPTARG" ;;
        y) SKIP_CONFIRM=true ;;
        h) usage ;;
        :) die "Option -${OPTARG} requires an argument" ;;
        *) die "Unknown option: -${OPTARG}. Use -h for help." ;;
    esac
done

# ── Validation ───────────────────────────────────────────────────────────────
[[ "$(id -u)" -eq 0 ]] || die "This script must be run as root (use sudo)"

[[ -n "${DEVICE}" ]] || die "No target device specified. Use -d /dev/sdX"
[[ -b "${DEVICE}" ]] || die "'${DEVICE}' is not a block device"

# Safety: refuse to target mounted devices
if grep -qs "^${DEVICE}" /proc/mounts; then
    die "'${DEVICE}' or a partition on it is currently mounted. Unmount first."
fi
for part in "${DEVICE}"?*; do
    if [[ -b "$part" ]] && grep -qs "^${part}" /proc/mounts; then
        die "'${part}' is currently mounted. Unmount all partitions on ${DEVICE} first."
    fi
done

# Safety: refuse to target the system disk
SYS_DISK=$(lsblk -ndo PKNAME "$(findmnt -n -o SOURCE /)" 2>/dev/null || true)
if [[ -n "${SYS_DISK}" && "${DEVICE}" == "/dev/${SYS_DISK}" ]]; then
    die "'${DEVICE}' appears to be the system disk. Refusing to continue."
fi

# ── Auto-detect ISO ─────────────────────────────────────────────────────────
if [[ -z "${ISO_PATH}" ]]; then
    if [[ -d "${ISO_DIR}" ]]; then
        ISO_PATH=$(find "${ISO_DIR}" -maxdepth 1 -name '*.iso' -type f -printf '%T@ %p\n' \
            | sort -rn | head -1 | cut -d' ' -f2-)
    fi
    [[ -n "${ISO_PATH}" ]] || die "No ISO found in ${ISO_DIR}. Use -i to specify one."
    msg2 "Auto-detected ISO: ${ISO_PATH}"
fi

[[ -f "${ISO_PATH}" ]] || die "ISO file not found: ${ISO_PATH}"

ISO_SIZE=$(stat -c%s "${ISO_PATH}")
ISO_NAME=$(basename "${ISO_PATH}")
DEV_SIZE=$(blockdev --getsize64 "${DEVICE}")
DEV_MODEL=$(lsblk -ndo MODEL "${DEVICE}" 2>/dev/null | sed 's/[[:space:]]*$//' || echo "unknown")

if (( ISO_SIZE > DEV_SIZE )); then
    die "ISO ($(numfmt --to=iec ${ISO_SIZE})) is larger than device ($(numfmt --to=iec ${DEV_SIZE}))"
fi

# ── Confirmation ─────────────────────────────────────────────────────────────
msg "Deploytix USB Writer"
echo ""
printf "  ISO:        %s (%s)\n" "${ISO_NAME}" "$(numfmt --to=iec ${ISO_SIZE})"
printf "  Device:     %s (%s, %s)\n" "${DEVICE}" "${DEV_MODEL}" "$(numfmt --to=iec ${DEV_SIZE})"
printf "  Persistence: ext4, label=%s\n" "${COW_LABEL}"
echo ""
warn "ALL DATA ON ${DEVICE} WILL BE DESTROYED"
echo ""

if ! "${SKIP_CONFIRM}"; then
    read -rp "Type 'yes' to continue: " answer
    [[ "${answer}" == "yes" ]] || die "Aborted by user"
fi

# ── Step 1: Write ISO ───────────────────────────────────────────────────────
msg "Writing ISO to ${DEVICE}..."
dd bs=4M if="${ISO_PATH}" of="${DEVICE}" conv=fsync status=progress
sync
msg2 "ISO written successfully"

# Wait for kernel to re-read partition table
partprobe "${DEVICE}" 2>/dev/null || true
udevadm settle --timeout=5 2>/dev/null || sleep 3

# ── Step 2: Determine partition layout ───────────────────────────────────────
msg "Inspecting partition layout..."

# Find the last sector used by the ISO's partitions
LAST_END=$(sfdisk -d "${DEVICE}" 2>/dev/null \
    | grep "^${DEVICE}" \
    | sed -n 's/.*start=\s*\([0-9]*\).*size=\s*\([0-9]*\).*/\1 \2/p' \
    | awk '{print $1 + $2}' \
    | sort -rn | head -1)

TOTAL_SECTORS=$(blockdev --getsz "${DEVICE}")
FREE_SECTORS=$(( TOTAL_SECTORS - LAST_END ))

if (( FREE_SECTORS < 2048 )); then
    die "Not enough free space on ${DEVICE} for a persistence partition"
fi

msg2 "Free space after ISO: $(( FREE_SECTORS * 512 / 1024 / 1024 )) MiB"

# ── Step 3: Create persistence partition ─────────────────────────────────────
msg "Creating persistence partition..."

# Append partition 3 (Linux, type 83) using all remaining space
echo ',,83,;' | sfdisk --append "${DEVICE}" --no-reread \
    || die "sfdisk --append failed; is the partition table intact?"
partprobe "${DEVICE}" 2>/dev/null || true
udevadm settle --timeout=5 2>/dev/null || sleep 3

# Find the new partition (usually ${DEVICE}3, but handle nvme-style names too)
PERSIST_PART=""
for candidate in "${DEVICE}3" "${DEVICE}p3"; do
    if [[ -b "${candidate}" ]]; then
        PERSIST_PART="${candidate}"
        break
    fi
done

# If the device node hasn't appeared yet, give the kernel another chance
if [[ -z "${PERSIST_PART}" ]]; then
    msg2 "Waiting for partition device node..."
    partprobe "${DEVICE}" 2>/dev/null || true
    udevadm settle --timeout=5 2>/dev/null || sleep 3
    for candidate in "${DEVICE}3" "${DEVICE}p3"; do
        if [[ -b "${candidate}" ]]; then
            PERSIST_PART="${candidate}"
            break
        fi
    done
fi

[[ -n "${PERSIST_PART}" ]] || die "Could not find persistence partition after creation"
msg2 "Persistence partition: ${PERSIST_PART}"

# ── Step 4: Format persistence partition ─────────────────────────────────────
msg "Formatting ${PERSIST_PART} as ext4 (label=${COW_LABEL})..."
mkfs.ext4 -q -L "${COW_LABEL}" "${PERSIST_PART}"
msg2 "Formatted successfully"

# ── Step 5: Patch GRUB kernels.cfg for persistence ──────────────────────────
msg "Patching GRUB kernel command line for persistence..."

# The ISO's first partition contains an ISO9660 filesystem with the GRUB config.
# We patch kernels.cfg in-place on the raw device, adding cow_label to the
# kernel parameters while trimming a GRUB comment to keep the file size identical.
ISO_PART="${DEVICE}1"
[[ -b "${ISO_PART}" ]] || ISO_PART="${DEVICE}p1"
[[ -b "${ISO_PART}" ]] || die "Cannot find ISO partition (${DEVICE}1)"

python3 - "${ISO_PART}" "${COW_LABEL}" <<'PYEOF'
import sys, os

dev = sys.argv[1]
cow_label = sys.argv[2]

# Read the ISO partition to find kernels.cfg
# Search in 2MiB chunks for the target string
CHUNK = 2 * 1024 * 1024
target = b'overlay=livefs; do'
new_params_insert = f' cow_label={cow_label}'.encode()

found_offset = -1
with open(dev, 'rb') as f:
    offset = 0
    while True:
        # Read with overlap to handle boundary cases
        data = f.read(CHUNK)
        if not data:
            break
        idx = data.find(target)
        if idx >= 0:
            found_offset = offset + idx
            break
        offset += len(data) - len(target)
        f.seek(offset)

if found_offset < 0:
    # Check if already patched
    with open(dev, 'rb') as f:
        offset = 0
        already = f'cow_label={cow_label}'.encode()
        while True:
            data = f.read(CHUNK)
            if not data:
                break
            if already in data:
                print(f'  -> Already patched: cow_label={cow_label} found in kernel params')
                sys.exit(0)
            offset += len(data) - len(already)
            f.seek(offset)
    print('ERROR: Could not find kernel params (overlay=livefs) in ISO partition', file=sys.stderr)
    sys.exit(1)

# Align to ISO block boundary (2048) to read the full file block
block_start = (found_offset // 2048) * 2048
with open(dev, 'rb') as f:
    f.seek(block_start)
    block_data = f.read(4096)  # Read 2 blocks to cover the whole file

# The target is: "overlay=livefs; do"
# We insert " cow_label=<label>" before "; do"
insert_point = b'overlay=livefs'
insert_after = insert_point + new_params_insert
delta = len(new_params_insert)

# Compensate by trimming a GRUB comment in the same file
# Pattern: {# set arguments above with the editor
old_comment = b'{# set arguments above with the editor'
if old_comment not in block_data:
    print('WARNING: Could not find GRUB comment to trim; file size will change', file=sys.stderr)
    new_comment = old_comment
else:
    # Trim the comment by exactly delta bytes
    trim_len = len(old_comment) - delta
    if trim_len < 2:
        trim_len = 2
    new_comment = old_comment[:trim_len]

# Apply patches
patched = block_data.replace(insert_point, insert_after, 1)
patched = patched.replace(old_comment, new_comment, 1)

if len(patched) != len(block_data):
    # If sizes don't match, try trimming the second comment occurrence too
    remaining = len(patched) - len(block_data)
    if remaining > 0 and old_comment in patched:
        trim2 = old_comment[:len(old_comment) - remaining]
        patched = patched.replace(old_comment, trim2, 1)

if len(patched) != len(block_data):
    print(f'WARNING: Patched size ({len(patched)}) differs from original ({len(block_data)})', file=sys.stderr)
    print('         Padding/truncating to match', file=sys.stderr)
    if len(patched) < len(block_data):
        patched = patched + b'\x00' * (len(block_data) - len(patched))
    else:
        patched = patched[:len(block_data)]

# Write back
fd = os.open(dev, os.O_WRONLY | os.O_SYNC)
os.lseek(fd, block_start, os.SEEK_SET)
os.write(fd, patched)
os.close(fd)

print(f'  -> Patched: added cow_label={cow_label} to kernel command line')
PYEOF

sync

# ── Done ─────────────────────────────────────────────────────────────────────
msg "USB drive is ready!"
echo ""
printf "  Device:      %s\n" "${DEVICE}"
printf "  ISO:         %s\n" "${ISO_NAME}"
printf "  Persistence: %s (label=%s)\n" "${PERSIST_PART}" "${COW_LABEL}"
echo ""
msg2 "Boot from this USB and changes will persist across reboots."
msg2 "To reset persistence, format ${PERSIST_PART}: mkfs.ext4 -L ${COW_LABEL} ${PERSIST_PART}"
