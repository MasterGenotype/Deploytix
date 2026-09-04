# Building Deploytix

Deploytix is a Rust project built with Cargo. It produces four binaries:

| Binary | Description | Feature flag |
|---|---|---|
| `deploytix` | CLI installer / interactive wizard / all subcommands | *(none — always built)* |
| `deploytix-rehearsal` | Standalone rehearsal-install entry point | *(none — always built)* |
| `deploytix-gui` | egui graphical installation wizard | `--features gui` |
| `deploytix-update-gui` | egui transactional updater (immutable installs) | `--features gui` |

---

## Prerequisites

- **Rust toolchain** (edition 2021; CI builds on `stable`)
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **A C compiler** — `build.rs` compiles `src/resources/alsa_noop.c` (an ABI-correct
  shim that silences ALSA's error spew) into every binary via the `cc` crate.
- **ALSA headers** — the CLI links `libasound` for theme audio playback (`rodio`).
  Artix/Arch: `pacman -S alsa-lib`; Debian/Ubuntu: `apt install libasound2-dev`.
- **make** (GNU Make) — used for the convenience targets below
- For the GUI: a working X11 or Wayland display server and OpenGL drivers, plus
  the development headers below.

### GUI build dependencies

Artix/Arch:

```sh
pacman -S libxcb libxkbcommon libxkbcommon-x11 libx11 libxcursor wayland mesa
```

Debian/Ubuntu (as installed by CI):

```sh
apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
            libxkbcommon-dev libwayland-dev libgl-dev
```

At *runtime* on an X11 session, winit additionally loads `libX11`, `libXcursor`,
and `libxkbcommon-x11` dynamically.

> **No static musl build.** The CLI links `libasound` for theme audio, and there
> is no static ALSA to link against, so a fully self-contained musl binary is not
> achievable. Build for glibc.

---

## Quick start — install the GUI

```sh
make install
```

This single command:

1. Compiles `deploytix` and `deploytix-gui` in release mode (`--features gui`)
2. Installs the GUI binary to `/usr/bin/deploytix-gui`
3. Generates and installs the `.desktop` launcher into `/usr/share/applications`
4. Installs the polkit policy into `/usr/share/polkit-1/actions`

Do **not** prefix this with `sudo`: the install steps invoke `sudo` themselves, so
the compile stays under your own user and only the file copies escalate.

---

## Makefile targets

| Target | What it does |
|---|---|
| `make` / `make build` | Release build of the CLI binary |
| `make gui` | Release build with `--features gui` (all four binaries) |
| `make gcc` | CLI built through the explicit glibc/GCC linker target |
| `make install` | Build CLI + GUI, install the **GUI** with desktop entry and polkit policy |
| `make install-cli` | Build CLI **and** install `deploytix` only |
| `make install-all` | Install **both** binaries plus desktop entry and polkit policy |
| `make install-gcc` | Install the GCC/glibc-linked CLI as `deploytix` |
| `make install-update-gui` | Install `deploytix-update-gui` with its desktop entry and polkit policy |
| `make uninstall` | Remove installed binaries, desktop entries, and polkit policies |
| `make fmt` | Format source with `cargo fmt` |
| `make lint` | Run `cargo clippy --all-features -- -D warnings` |
| `make test` | Run `cargo test --all-features` |
| `make clean` | Remove the `target/` directory |

`make install-update-gui` is deliberately **not** part of `install` or
`install-all`. The updater only functions on an immutable root, and deployed
systems receive it from the separate `deploytix-update-gui-git` package, which
the installer withholds unless `immutable_root` is set. Use this target only on
a machine that is already immutable.

### Custom install prefix

The install prefix defaults to `/usr`. Override with `PREFIX`:

```sh
make install PREFIX=/usr/local          # installs to /usr/local/bin
make install PREFIX="$HOME/.local"      # installs to ~/.local/bin
```

Polkit policies always go to `/usr/share/polkit-1/actions`, which is where
polkit reads them from regardless of `PREFIX`.

---

## Manual Cargo commands

If you prefer not to use Make:

```sh
# CLI — debug
cargo build

# CLI — release
cargo build --release

# All binaries including the GUIs — release
cargo build --release --features gui

# A single binary
cargo build --release --features gui --bin deploytix-update-gui

# Copy a binary manually
sudo install -m 755 target/release/deploytix-gui /usr/bin/

# Via the cargo alias defined in .cargo/config.toml (explicit glibc linker):
cargo gcc-build
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
| `gui` | `eframe` + `egui`, enabling `deploytix-gui` and `deploytix-update-gui` |

Enable with `--features gui` (or `--all-features` during testing). `default = []`,
so a plain `cargo build` produces the CLI and rehearsal binaries only.

---

## Linting and formatting

```sh
make fmt      # auto-format
make lint     # clippy with -D warnings
make test     # cargo test --all-features
```

The release workflow (`.github/workflows/release.yml`) gates every build on
`cargo fmt -- --check` and `cargo clippy --all-features -- -D warnings`, so run
both before pushing.

---

## Packaging

`pkg/PKGBUILD` is a split PKGBUILD producing three Artix/Arch packages:

| Package | Contents |
|---|---|
| `deploytix-git` | `deploytix` binary, licence, `README.md` |
| `deploytix-gui-git` | `deploytix-gui`, desktop entry, polkit policy |
| `deploytix-update-gui-git` | `deploytix-update-gui`, desktop entry, polkit policy |

Each package `provides`/`conflicts` its unpinned name, and the two GUI packages
depend on `deploytix-git`. `deploytix-rehearsal` is not packaged — it is a
build-tree tool. `iso/` holds the scripts and profile for building a bootable
Artix ISO with deploytix preinstalled.

---

## Releases

Pushing a `v*` tag — or a `release/v*` branch, for environments that cannot push
tags directly — triggers `.github/workflows/release.yml`, which lints, builds the
CLI and GUI binaries on `ubuntu-latest`, and publishes them as a GitHub release
with generated notes. `workflow_dispatch` accepts an explicit tag input.

---

## Uninstalling

```sh
make uninstall
```

Removes `deploytix`, `deploytix-gui`, and `deploytix-update-gui` from
`$(PREFIX)/bin`, their desktop entries from `$(PREFIX)/share/applications`, and
their polkit policies from `/usr/share/polkit-1/actions`.
