#!/usr/bin/env bash
set -euo pipefail

# restore-disk.sh (Deploytix)
# - In normal mode, Deploytix should select/confirm the target disk BEFORE calling this script.
# - This script will:
#   1) Determine DEVICE from args/env/file (no interactive selection by default)
#   2) Compute dynamic partition sizes deterministically (MiB)
#   3) Generate an fdisk-importable (sfdisk-style) script file
#   4) Apply it via fdisk "I" using expect
#   5) Record /tmp/installer/target_disk (and an optional table dump)

EXECDIR="/tmp/installer"
mkdir -p "$EXECDIR"

# -----------------------------
# Args
# -----------------------------
DEVICE=""
NO_CONFIRM=false

for arg in "$@"; do
  case "$arg" in
    --device=*) DEVICE="${arg#*=}" ;;
    --disk=*) DEVICE="${arg#*=}" ;;
    --no-confirm) NO_CONFIRM=true ;;
  esac
done

# -----------------------------
# Device resolution
# -----------------------------
# Prefer explicit arg; then env; then previously-written file.
if [[ -z "$DEVICE" ]]; then
  DEVICE="${TARGET_DISK:-${DEPLOYTIX_TARGET_DISK:-}}"
fi
if [[ -z "$DEVICE" && -f "$EXECDIR/target_disk" ]]; then
  DEVICE="$(cat "$EXECDIR/target_disk" || true)"
fi

if [[ -z "$DEVICE" ]]; then
  echo "Error: No device provided. Deploytix must pass --device=/dev/XXX (or set TARGET_DISK)." >&2
  exit 1
fi

if [[ ! -b "$DEVICE" ]]; then
  echo "Error: Device $DEVICE does not exist or is not a block device." >&2
  exit 1
fi

# Optional safety confirmation (Deploytix typically already confirms)
if [[ "$NO_CONFIRM" != true ]]; then
  # If stdin is not a tty, do not prompt.
  if [[ -t 0 ]]; then
    read -p "About to WIPE and repartition $DEVICE. Continue? [y/N] " -r CONFIRM
    if [[ ! $CONFIRM =~ ^[Yy]$ ]]; then
      echo "Aborting."
      exit 1
    fi
  fi
fi

# -----------------------------
# Deterministic sizing (MiB)
# -----------------------------
EFI_MIB=512
BOOT_MIB=2048

ROOT_RATIO="0.06441"
USER_RATIO="0.26838"
VAR_RATIO="0.05368"

ROOT_MIN_MIB=20480   # 20 GiB
USER_MIN_MIB=20480   # 20 GiB
VAR_MIN_MIB=8192     # 8 GiB

SWAP_MIN_MIB=4096
SWAP_MAX_MIB=20480

ALIGN_MIB=4

die() { echo "Error: $*" >&2; exit 1; }

clamp_int() {
  # clamp_int <value> <min> <max>
  local v="$1" mn="$2" mx="$3"
  (( v < mn )) && v="$mn"
  (( v > mx )) && v="$mx"
  echo "$v"
}

floor_align() {
  # floor_align <value> <align>
  local v="$1" a="$2"
  echo $(( (v / a) * a ))
}

get_disk_bytes() {
  blockdev --getsize64 "$1" 2>/dev/null || lsblk -bndo SIZE "$1"
}

get_ram_mib() {
  local kb
  kb="$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null || true)"
  if [[ -n "${kb:-}" && "$kb" =~ ^[0-9]+$ && "$kb" -gt 0 ]]; then
    echo $(( kb / 1024 ))
  else
    # deterministic fallback
    echo 8192
  fi
}

DISK_BYTES="$(get_disk_bytes "$DEVICE")"
[[ "$DISK_BYTES" =~ ^[0-9]+$ ]] || die "Failed to read disk size in bytes for $DEVICE"

DISK_MIB=$(( DISK_BYTES / 1024 / 1024 ))
RAM_MIB="$(get_ram_mib)"

SWAP_MIB="$(clamp_int $(( 2 * RAM_MIB )) "$SWAP_MIN_MIB" "$SWAP_MAX_MIB")"
SWAP_MIB="$(floor_align "$SWAP_MIB" "$ALIGN_MIB")"

RESERVED_MIB=$(( EFI_MIB + BOOT_MIB + SWAP_MIB ))
REMAIN_MIB=$(( DISK_MIB - RESERVED_MIB ))

# Must at least fit reserved + minimums + 1MiB HOME
MIN_TOTAL_MIB=$(( RESERVED_MIB + ROOT_MIN_MIB + USER_MIN_MIB + VAR_MIN_MIB + 1 ))
(( DISK_MIB >= MIN_TOTAL_MIB )) || die "Disk too small: ${DISK_MIB}MiB < required minimum ${MIN_TOTAL_MIB}MiB"

# Ratio allocations
ROOT_MIB="$(awk -v r="$REMAIN_MIB" -v p="$ROOT_RATIO" 'BEGIN{printf("%d", r*p)}')"
USER_MIB="$(awk -v r="$REMAIN_MIB" -v p="$USER_RATIO" 'BEGIN{printf("%d", r*p)}')"
VAR_MIB="$(awk -v r="$REMAIN_MIB" -v p="$VAR_RATIO"  'BEGIN{printf("%d", r*p)}')"

# Apply minimums
(( ROOT_MIB < ROOT_MIN_MIB )) && ROOT_MIB="$ROOT_MIN_MIB"
(( USER_MIB < USER_MIN_MIB )) && USER_MIB="$USER_MIN_MIB"
(( VAR_MIB  < VAR_MIN_MIB  )) && VAR_MIB="$VAR_MIN_MIB"

# Align down
ROOT_MIB="$(floor_align "$ROOT_MIB" "$ALIGN_MIB")"
USER_MIB="$(floor_align "$USER_MIB" "$ALIGN_MIB")"
VAR_MIB="$(floor_align "$VAR_MIB" "$ALIGN_MIB")"

HOME_MIB=$(( DISK_MIB - (EFI_MIB + BOOT_MIB + SWAP_MIB + ROOT_MIB + USER_MIB + VAR_MIB) ))

# If HOME is negative, shrink deterministically: USER -> ROOT -> VAR
if (( HOME_MIB < 0 )); then
  deficit=$(( -HOME_MIB ))

  reducible=$(( USER_MIB - USER_MIN_MIB ))
  if (( reducible > 0 && deficit > 0 )); then
    take=$(( deficit < reducible ? deficit : reducible ))
    USER_MIB=$(( USER_MIB - take ))
    USER_MIB="$(floor_align "$USER_MIB" "$ALIGN_MIB")"
    deficit=$(( deficit - take ))
  fi

  reducible=$(( ROOT_MIB - ROOT_MIN_MIB ))
  if (( reducible > 0 && deficit > 0 )); then
    take=$(( deficit < reducible ? deficit : reducible ))
    ROOT_MIB=$(( ROOT_MIB - take ))
    ROOT_MIB="$(floor_align "$ROOT_MIB" "$ALIGN_MIB")"
    deficit=$(( deficit - take ))
  fi

  reducible=$(( VAR_MIB - VAR_MIN_MIB ))
  if (( reducible > 0 && deficit > 0 )); then
    take=$(( deficit < reducible ? deficit : reducible ))
    VAR_MIB=$(( VAR_MIB - take ))
    VAR_MIB="$(floor_align "$VAR_MIB" "$ALIGN_MIB")"
    deficit=$(( deficit - take ))
  fi

  HOME_MIB=$(( DISK_MIB - (EFI_MIB + BOOT_MIB + SWAP_MIB + ROOT_MIB + USER_MIB + VAR_MIB) ))
  (( HOME_MIB >= 0 )) || die "Disk too small after deterministic shrinking (HOME still negative)"
fi

echo "Computed partition sizes (MiB):"
echo "  EFI : ${EFI_MIB}"
echo "  BOOT: ${BOOT_MIB}"
echo "  SWAP: ${SWAP_MIB}   (RAM=${RAM_MIB}MiB)"
echo "  ROOT: ${ROOT_MIB}"
echo "  USER: ${USER_MIB}"
echo "  VAR : ${VAR_MIB}"
echo "  HOME: ${HOME_MIB}   (remainder)"

# -----------------------------
# fdisk script input + apply (expect) + record
# -----------------------------
# fdisk 'I' import can be picky about absolute paths depending on build/environment.
# Use a simple filename in $EXECDIR and run fdisk with cwd=$EXECDIR.
# Keep script filename short/simple for fdisk import.
SCRIPT_BASENAME="DL"
SCRIPT_FILE="$EXECDIR/$SCRIPT_BASENAME"

EFI_TYPE="C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
LINUXFS_TYPE="0FC63DAF-8483-4772-8E79-3D69D8477DE4"
SWAP_TYPE="0657FD6D-A4AB-43C4-84E5-0933C84B4F4F"

# fdisk "I" (import) consumes an sfdisk-script. Use *unit: sectors* form (fdisk is pickier than sfdisk).
# Keep it comment-free.
SECTOR_SIZE="$(blockdev --getss "$DEVICE" 2>/dev/null || cat "/sys/block/${DEVICE##*/}/queue/logical_block_size" 2>/dev/null || echo 512)"
TOTAL_SECTORS="$(blockdev --getsz "$DEVICE" 2>/dev/null || awk -v b="$DISK_BYTES" -v s="$SECTOR_SIZE" 'BEGIN{printf("%d", b/s)}')"
[[ "$SECTOR_SIZE" =~ ^[0-9]+$ ]] || die "Failed to read sector size"
[[ "$TOTAL_SECTORS" =~ ^[0-9]+$ ]] || die "Failed to read total sectors"

FIRST_LBA=2048
LAST_LBA=$(( TOTAL_SECTORS - 34 ))
(( LAST_LBA > FIRST_LBA )) || die "Disk too small after GPT metadata reservation"

mib_to_sectors() {
  local mib="$1"
  awk -v m="$mib" -v ss="$SECTOR_SIZE" 'BEGIN{printf("%d", (m*1024*1024)/ss)}'
}

align_up() {
  # align_up <value> <align>
  local v="$1" a="$2"
  echo $(( ((v + a - 1) / a) * a ))
}

ALIGN_SECTORS=$(( (1024*1024) / SECTOR_SIZE ))  # 1MiB alignment in sectors
(( ALIGN_SECTORS > 0 )) || ALIGN_SECTORS=2048

EFI_SEC="$(mib_to_sectors "$EFI_MIB")"
BOOT_SEC="$(mib_to_sectors "$BOOT_MIB")"
SWAP_SEC="$(mib_to_sectors "$SWAP_MIB")"
ROOT_SEC="$(mib_to_sectors "$ROOT_MIB")"
USER_SEC="$(mib_to_sectors "$USER_MIB")"
VAR_SEC="$(mib_to_sectors "$VAR_MIB")"

# Partition type GUIDs (as provided)
EFI_TYPE="C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
BOOT_TYPE="21686148-6449-6E6F-744E-656564454649"
SWAP_TYPE="0657FD6D-A4AB-43C4-84E5-0933C84B4F4F"
ROOT_TYPE="4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"
USER_TYPE="8484680C-9521-48C6-9C11-B0720656F69E"
VAR_TYPE="4D21B016-B534-45C2-A9FB-5C16E091FD2D"
HOME_TYPE="933AC7E1-2EB4-4F13-B844-0E14E2AEF915"

LABEL_ID="$(uuidgen)"
UUID_EFI="$(uuidgen)"
UUID_BOOT="$(uuidgen)"
UUID_SWAP="$(uuidgen)"
UUID_ROOT="$(uuidgen)"
UUID_USER="$(uuidgen)"
UUID_VAR="$(uuidgen)"
UUID_HOME="$(uuidgen)"

# Compute starts (aligned)
S1=$FIRST_LBA
S2=$(align_up $(( S1 + EFI_SEC )) "$ALIGN_SECTORS")
S3=$(align_up $(( S2 + BOOT_SEC )) "$ALIGN_SECTORS")
S4=$(align_up $(( S3 + SWAP_SEC )) "$ALIGN_SECTORS")
S5=$(align_up $(( S4 + ROOT_SEC )) "$ALIGN_SECTORS")
S6=$(align_up $(( S5 + USER_SEC )) "$ALIGN_SECTORS")
S7=$(align_up $(( S6 + VAR_SEC  )) "$ALIGN_SECTORS")

# Sizes (HOME gets remainder to LAST_LBA)
SZ1=$EFI_SEC
SZ2=$BOOT_SEC
SZ3=$SWAP_SEC
SZ4=$ROOT_SEC
SZ5=$USER_SEC
SZ6=$VAR_SEC
SZ7=$(( (LAST_LBA - S7) + 1 ))
(( SZ7 > 0 )) || die "HOME size computed as non-positive; check sizing/alignment"

cat > "$SCRIPT_FILE" <<EOF
label: gpt
label-id: $LABEL_ID
device: $DEVICE
unit: sectors
first-lba: $FIRST_LBA
last-lba: $LAST_LBA
sector-size: $SECTOR_SIZE

$DEVICE""1 : start=$S1, size=$SZ1, type=$EFI_TYPE,  uuid=$UUID_EFI,  name="EFI"
$DEVICE""2 : start=$S2, size=$SZ2, type=$BOOT_TYPE, uuid=$UUID_BOOT, name="BOOT", attrs="LegacyBIOSBootable"
$DEVICE""3 : start=$S3, size=$SZ3, type=$SWAP_TYPE, uuid=$UUID_SWAP, name="SWAP"
$DEVICE""4 : start=$S4, size=$SZ4, type=$ROOT_TYPE, uuid=$UUID_ROOT, name="ROOT"
$DEVICE""5 : start=$S5, size=$SZ5, type=$USER_TYPE, uuid=$UUID_USER, name="USER"
$DEVICE""6 : start=$S6, size=$SZ6, type=$VAR_TYPE,  uuid=$UUID_VAR,  name="VAR"
$DEVICE""7 : start=$S7, size=$SZ7, type=$HOME_TYPE, uuid=$UUID_HOME, name="HOME"
EOF

echo "fdisk import script written to: $SCRIPT_FILE"

# Pass variables into the expect script via shell expansion.
# Note: fdisk reads full lines; always send "\r" (Enter) after each command/answer to match TTY behavior.
expect << EOF
    set device "$DEVICE"
    set script_file "$SCRIPT_BASENAME"

    # Run fdisk with cwd at $EXECDIR so fdisk can open the script by simple filename.
    spawn sh -c "cd $EXECDIR && fdisk \$device"
    set timeout 60

    # fdisk may print disk info before the prompt
    expect {
        -re {Command \(m for help\):} {}
        timeout {
            puts "Timeout waiting for fdisk prompt"
            exit 1
        }
    }

    # Ensure GPT label (blank disks may default to DOS/MBR)
    send "g\r"
    expect -re {Command \(m for help\):}

    send "I\r"

    expect -re {Enter script file name:}
    
    send "DL\r"
    
    expect {
      -re {(?i)script.*(applied|success).*} {}
      -re {Command \(m for help\):} {}
        timeout { puts "Timeout after loading script"; exit 1 }
    }

    send "w\r"

    expect {
        eof {}
        -re {(?i)partition table has been altered.*} { exp_continue }
        -re {(?i)syncing disks.*} { exp_continue }
        -re {Command \(m for help\):} {
            # only if fdisk stays open, quit explicitly
            send "q\r"
            expect eof
    }
    timeout {
        puts "Timeout waiting for fdisk to finish after write"
        exit 1
    }
}
    
EOF


partprobe "$DEVICE" 2>/dev/null || true
udevadm settle 2>/dev/null || true

# Save the chosen device so the main script can pick it up.
echo "$DEVICE" > "$EXECDIR/target_disk"


# Optional: write a machine-readable dump for debugging
if command -v sfdisk >/dev/null 2>&1; then
  sfdisk -d "$DEVICE" > "$EXECDIR/Artix-Laptop_Disk-Layout" || true
  echo "sfdisk dump written to: $EXECDIR/Artix-Laptop_Disk-Layout"
else
  fdisk -l "$DEVICE" > "$EXECDIR/Artix-Laptop_Disk-Layout" || true
  echo "fdisk listing written to: $EXECDIR/Artix-Laptop_Disk-Layout"
fi
