#!/bin/bash

# Check if running inside an AppImage
if [ -n "$APPIMAGE" ]; then
    echo "Running inside an AppImage..."
fi

# Ensure sudo works
if ! sudo -v &>/dev/null; then
    echo "This script requires sudo privileges."
    exit 1
fi

# Define the mount points and their corresponding mapper names
declare -A partitions=(
    [Boot]="Crypt-Boot"
    [Swap]="Crypt-Swap"
    [Root]="Crypt-Root"
    [Usr]="Crypt-Usr"
    [Var]="Crypt-Var"
    [Home]="Crypt-Home"
)

# Unmount all mounted filesystems, ignoring errors if not mounted
echo "Unmounting all mounted filesystems..."
for path in /install/boot/efi /install/home /install/var /install/usr /install/boot /install; do
    if mountpoint -q "$path"; then
        sudo /usr/bin/umount "$path" || echo "Failed to unmount $path"
    fi
done

# Turn swap off
sudo /usr/sbin/swapoff /dev/mapper/Crypt-Swap 

# Close all encrypted volumes, ignoring errors if they aren't active
for name in "${!partitions[@]}"; do
    mapper_name="${partitions[$name]}"

    if [ -e "/dev/mapper/$mapper_name" ]; then
        echo "Closing $mapper_name..."
        sudo /usr/sbin/cryptsetup close "$mapper_name" || echo "Failed to close $mapper_name."
    else
        echo "Device $mapper_name is not active, skipping..."
    fi
done

# Prompt the user for the disk to wipe
while true; do
    read -rp "Enter the disk device to wipe (e.g., /dev/sdX or /dev/nvmeXnY): " DEVICE

    if [ -b "$DEVICE" ]; then
        break
    else
        echo "Error: $DEVICE is not a valid block device! Please enter a correct device."
    fi
done

# Confirm before wiping
read -rp "WARNING: This will wipe the partition table on $DEVICE! Type 'yes' to continue: " CONFIRM

if [[ "$CONFIRM" =~ ^[Yy][Ee][Ss]$ ]]; then
    echo "Wiping GPT partition table on $DEVICE..."
    echo -e "g\nw" | sudo /usr/sbin/fdisk "$DEVICE"
    echo "Partition table wiped on $DEVICE."
else
    echo "Operation canceled."
    exit 1
fi

echo "All partitions unmounted and encrypted volumes closed."
echo "Done!"
