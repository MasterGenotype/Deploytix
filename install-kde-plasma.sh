#!/bin/bash

# Define the chroot path
CHROOT_DIR="/install"

# Check if the chroot directory exists
if [ ! -d "$CHROOT_DIR" ]; then
    echo "Error: Chroot directory '$CHROOT_DIR' does not exist."
    exit 1
fi

# Function to run commands inside the chroot
chroot_exec() {
    artix-chroot "$CHROOT_DIR" bash -c "$1"
}

# Update system and install Plasma packages
echo "Updating system and installing KDE Plasma..."
chroot_exec "pacman -Syu --noconfirm"
chroot_exec "pacman -S --noconfirm plasma-meta plasma-desktop"

# Install additional recommended packages
echo "Installing additional KDE dependencies..."
chroot_exec "pacman -S --noconfirm konsole dolphin sddm sddm-runit"

# Enable SDDM display manager
echo "Enabling SDDM..."
chroot_exec "ln -s /etc/runit/sv/sddm /run/runit/service/"

# Set up X environment for Plasma
echo "Setting up X environment..."
chroot_exec "echo 'exec startplasma-x11' > /root/.xinitrc"

echo "KDE Plasma installation completed successfully in chroot at $CHROOT_DIR."
