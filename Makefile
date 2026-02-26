PREFIX     ?= /usr
BINDIR     := $(PREFIX)/bin
APPDIR     := $(PREFIX)/share/applications
POLKITDIR  := /usr/share/polkit-1/actions

CLI_BIN      := target/release/deploytix
GUI_BIN      := target/release/deploytix-gui
GCC_BIN      := target/x86_64-unknown-linux-gnu/release/deploytix
PORTABLE_BIN := target/x86_64-unknown-linux-musl/release/deploytix

.PHONY: all build gui gcc portable install install-cli install-gcc install-portable uninstall clean fmt lint test

DESKTOP_FILE := deploytix-gui.desktop
POLKIT_FILE  := com.deploytix.gui.policy

## Default: build CLI
all: build

## Build CLI binary (release)
build:
	cargo build --release

## Build GUI binary (release)
gui:
	cargo build --release --features gui

## Build CLI binary with explicit GCC linker (glibc, dynamically linked)
gcc:
	cargo build --release --target x86_64-unknown-linux-gnu --bin deploytix
	@echo "GCC binary: $(GCC_BIN)"
	@file $(GCC_BIN)

## Build portable static CLI binary (musl, zero dynamic deps)
## Prerequisites: apt install musl-tools
portable:
	rustup target add x86_64-unknown-linux-musl
	cargo build --release --target x86_64-unknown-linux-musl --bin deploytix
	@echo "Portable binary: $(PORTABLE_BIN)"
	@file $(PORTABLE_BIN)

## Install GUI binary to $(BINDIR)  [default: /usr/bin]
install: build gui
	sudo mkdir -p $(BINDIR) $(APPDIR)
	sudo install -m 755 $(GUI_BIN) $(BINDIR)/deploytix-gui
	$(CLI_BIN) generate-desktop-file --bindir $(BINDIR) --output /tmp/$(DESKTOP_FILE)
	sudo install -m 644 /tmp/$(DESKTOP_FILE) $(APPDIR)/$(DESKTOP_FILE)
	@rm -f /tmp/$(DESKTOP_FILE)
	sudo install -m 644 $(POLKIT_FILE) $(POLKITDIR)/$(POLKIT_FILE)
	sudo sed -i 's|%BINDIR%|$(BINDIR)|g' $(POLKITDIR)/$(POLKIT_FILE)
	@echo "Installed deploytix-gui -> $(BINDIR)/deploytix-gui"
	@echo "Installed desktop entry  -> $(APPDIR)/$(DESKTOP_FILE)"
	@echo "Installed polkit policy -> $(POLKITDIR)/$(POLKIT_FILE)"

## Install CLI binary to $(BINDIR)
install-cli: build
	sudo mkdir -p $(BINDIR)
	sudo install -m 755 $(CLI_BIN) $(BINDIR)/deploytix
	@echo "Installed deploytix -> $(BINDIR)/deploytix"

## Install both CLI and GUI binaries to $(BINDIR)
install-all: build gui
	sudo mkdir -p $(BINDIR) $(APPDIR)
	sudo install -m 755 $(CLI_BIN) $(BINDIR)/deploytix
	sudo install -m 755 $(GUI_BIN) $(BINDIR)/deploytix-gui
	$(CLI_BIN) generate-desktop-file --bindir $(BINDIR) --output /tmp/$(DESKTOP_FILE)
	sudo install -m 644 /tmp/$(DESKTOP_FILE) $(APPDIR)/$(DESKTOP_FILE)
	@rm -f /tmp/$(DESKTOP_FILE)
	sudo install -m 644 $(POLKIT_FILE) $(POLKITDIR)/$(POLKIT_FILE)
	sudo sed -i 's|%BINDIR%|$(BINDIR)|g' $(POLKITDIR)/$(POLKIT_FILE)
	@echo "Installed deploytix      -> $(BINDIR)/deploytix"
	@echo "Installed deploytix-gui  -> $(BINDIR)/deploytix-gui"
	@echo "Installed desktop entry  -> $(APPDIR)/$(DESKTOP_FILE)"
	@echo "Installed polkit policy -> $(POLKITDIR)/$(POLKIT_FILE)"

## Install GCC CLI binary to $(BINDIR)
install-gcc: gcc
	sudo mkdir -p $(BINDIR)
	sudo install -m 755 $(GCC_BIN) $(BINDIR)/deploytix
	@echo "Installed gcc deploytix -> $(BINDIR)/deploytix"

## Install portable (musl) CLI binary to $(BINDIR)
install-portable: portable
	sudo mkdir -p $(BINDIR)
	sudo install -m 755 $(PORTABLE_BIN) $(BINDIR)/deploytix
	@echo "Installed portable deploytix -> $(BINDIR)/deploytix"

## Remove installed binaries and desktop entry
uninstall:
	rm -f $(BINDIR)/deploytix $(BINDIR)/deploytix-gui
	rm -f $(APPDIR)/$(DESKTOP_FILE)
	sudo rm -f $(POLKITDIR)/$(POLKIT_FILE)
	@echo "Uninstalled deploytix and deploytix-gui from $(BINDIR)"
	@echo "Removed desktop entry from $(APPDIR)"
	@echo "Removed polkit policy from $(POLKITDIR)"

## Format source code
fmt:
	cargo fmt

## Run linter (deny warnings)
lint:
	cargo clippy --all-features -- -D warnings

## Run tests
test:
	cargo test --all-features

## Remove build artifacts
clean:
	cargo clean
