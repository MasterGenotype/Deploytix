PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin

CLI_BIN      := target/release/deploytix
GUI_BIN      := target/release/deploytix-gui
PORTABLE_BIN := target/x86_64-unknown-linux-musl/release/deploytix

.PHONY: all build gui portable install install-cli install-gui install-portable uninstall clean fmt lint test

## Default: build CLI
all: build

## Build CLI binary (release)
build:
	cargo build --release

## Build GUI binary (release)
gui:
	cargo build --release --features gui

## Build portable static CLI binary (musl, zero dynamic deps)
## Prerequisites: apt install musl-tools
portable:
	rustup target add x86_64-unknown-linux-musl
	cargo build --release --target x86_64-unknown-linux-musl --bin deploytix
	@echo "Portable binary: $(PORTABLE_BIN)"
	@file $(PORTABLE_BIN)

## Install GUI binary to $(BINDIR)  [default: ~/.local/bin]
install: gui
	@mkdir -p $(BINDIR)
	install -m 755 $(GUI_BIN) $(BINDIR)/deploytix-gui
	@echo "Installed deploytix-gui -> $(BINDIR)/deploytix-gui"

## Install CLI binary to $(BINDIR)
install-cli: build
	@mkdir -p $(BINDIR)
	install -m 755 $(CLI_BIN) $(BINDIR)/deploytix
	@echo "Installed deploytix -> $(BINDIR)/deploytix"

## Install both CLI and GUI binaries to $(BINDIR)
install-all: build gui
	@mkdir -p $(BINDIR)
	install -m 755 $(CLI_BIN) $(BINDIR)/deploytix
	install -m 755 $(GUI_BIN) $(BINDIR)/deploytix-gui
	@echo "Installed deploytix      -> $(BINDIR)/deploytix"
	@echo "Installed deploytix-gui  -> $(BINDIR)/deploytix-gui"

## Install portable (musl) CLI binary to $(BINDIR)
install-portable: portable
	@mkdir -p $(BINDIR)
	install -m 755 $(PORTABLE_BIN) $(BINDIR)/deploytix
	@echo "Installed portable deploytix -> $(BINDIR)/deploytix"

## Remove installed binaries
uninstall:
	rm -f $(BINDIR)/deploytix $(BINDIR)/deploytix-gui
	@echo "Uninstalled deploytix and deploytix-gui from $(BINDIR)"

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
