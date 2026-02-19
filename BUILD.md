# Building Deploytix

Deploytix is a Rust project built with Cargo. It produces two binaries:

| Binary | Description | Feature flag |
|---|---|---|
| `deploytix` | CLI installer / interactive wizard | *(none — always built)* |
| `deploytix-gui` | egui graphical wizard | `--features gui` |

---

## Prerequisites

- **Rust toolchain** ≥ 1.74 (edition 2021)
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **make** (GNU Make) — used for the convenience targets below
- For the GUI: a working X11 or Wayland display server and OpenGL drivers
- For static / portable builds: the `x86_64-unknown-linux-musl` cross-compilation target

---

## Quick start — install the GUI to `~/.local/bin`

```sh
make install
```

This single command:

1. Compiles `deploytix-gui` in release mode (`--features gui`)
2. Creates `~/.local/bin/` if it does not already exist
3. Copies the binary there as `~/.local/bin/deploytix-gui`

Make sure `~/.local/bin` is on your `PATH`:

```sh
# Add to ~/.bashrc or ~/.zshrc if not already present
export PATH="$HOME/.local/bin:$PATH"
```

---

## Makefile targets

| Target | What it does |
|---|---|
| `make` / `make build` | Release build of the CLI binary |
| `make gui` | Release build of the GUI binary |
| `make install` | Build GUI **and** install to `~/.local/bin` |
| `make install-cli` | Build CLI **and** install to `~/.local/bin` |
| `make install-all` | Build **both** binaries and install to `~/.local/bin` |
| `make portable` | Static musl CLI build (zero runtime dependencies) |
| `make uninstall` | Remove both binaries from `~/.local/bin` |
| `make fmt` | Format source with `cargo fmt` |
| `make lint` | Run `cargo clippy --all-features -D warnings` |
| `make test` | Run `cargo test --all-features` |
| `make clean` | Remove the `target/` directory |

### Custom install prefix

The install prefix defaults to `$HOME/.local`. Override with `PREFIX`:

```sh
make install PREFIX=/usr/local        # installs to /usr/local/bin
make install PREFIX=/opt/deploytix    # installs to /opt/deploytix/bin
```

---

## Manual Cargo commands

If you prefer not to use Make:

```sh
# CLI — debug
cargo build

# CLI — release
cargo build --release

# GUI — release
cargo build --release --features gui

# Copy GUI binary manually
mkdir -p ~/.local/bin
cp target/release/deploytix-gui ~/.local/bin/

# Static portable binary (musl)
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# Or via the cargo alias defined in .cargo/config.toml:
cargo portable
```

---

## Release profile

The `[profile.release]` section in `Cargo.toml` is tuned for a small, fast binary:

| Setting | Value | Effect |
|---|---|---|
| `opt-level` | `"z"` | Optimise for binary size |
| `lto` | `true` | Link-time optimisation (cross-crate inlining) |
| `codegen-units` | `1` | Single codegen unit for maximum LTO effectiveness |
| `panic` | `"abort"` | Removes unwinding code, shrinks binary |
| `strip` | `true` | Strips debug symbols from the output binary |

---

## Feature flags

| Flag | Adds |
|---|---|
| `gui` | `eframe` + `egui` for the graphical wizard (`deploytix-gui`) |

Enable with `--features gui` (or `--all-features` during testing).

---

## Linting and formatting

```sh
make fmt      # auto-format
make lint     # clippy with -D warnings
```

CI should run both before merging.

---

## Uninstalling

```sh
make uninstall
```

Removes `~/.local/bin/deploytix` and `~/.local/bin/deploytix-gui`.
