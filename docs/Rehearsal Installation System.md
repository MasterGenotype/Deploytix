# Rehearsal Installation System
## Problem
There is no way to verify a full installation end-to-end on real hardware before committing. The preflight system checks preconditions but doesn't exercise the actual install pipeline. Users need a mode that runs the entire installer for real on the target disk, records every command's output, short-circuits on the first failure, and then wipes the disk to restore pristine state — producing a detailed report of what happened.
## Approach
Instead of duplicating the entire `src/` tree (which creates an unmaintainable parallel codebase), we add **recording** as an opt-in capability to `CommandRunner` and build a thin rehearsal orchestrator in a new `src/rehearsal/` library module. A separate binary entry point at `src-rehearsal/main.rs` provides the isolated binary the user requested. The production installer is unmodified when recording is off — zero behavioral change, zero overhead.
### Why not a full src copy?
The production installer has 20+ phases across ~15 modules (~4000 lines). A full copy means every bug fix or feature change must be applied twice. The recording-wrapper approach gives the same isolation guarantees (recording is opt-in, disabled by default, tested separately) without the duplication.
## Design
### 1. Recording in CommandRunner (`src/utils/command.rs`)
Add an optional `mpsc::Sender<OperationRecord>` to `CommandRunner`. When set, each `run()` / `run_in_chroot()` / `force_run()` call:
1. Captures the command string and start time
2. Executes the command normally
3. Captures stdout, stderr, exit code, and elapsed duration
4. Sends an `OperationRecord` through the channel
5. Returns the `Result` unchanged
When the sender is `None` (production default), behavior is identical to today.
```rust
pub struct OperationRecord {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration: Duration,
    pub success: bool,
}
```
New builder on `CommandRunner`:
```rust
pub fn with_recorder(mut self, tx: Sender<OperationRecord>) -> Self;
```
New builder on `Installer`:
```rust
pub fn with_recorder(mut self, tx: Sender<OperationRecord>) -> Self;
```
### 2. DiskWipeGuard (`src/rehearsal/guard.rs`)
RAII guard that guarantees the target disk is wiped when dropped, even on panic or early return:
```rust
pub struct DiskWipeGuard {
    device: String,
    armed: bool,
}
impl Drop for DiskWipeGuard {
    fn drop(&mut self) {
        if self.armed {
            // emergency_cleanup() then wipefs + blank GPT
        }
    }
}
```
The guard is armed immediately before the installer starts and disarmed only after the wipe succeeds — so double-wipe doesn't happen but missed-wipe can't happen either.
### 3. RehearsalReport (`src/rehearsal/report.rs`)
```rust
pub struct RehearsalReport {
    pub records: Vec<OperationRecord>,
    pub short_circuited_at: Option<String>,   // phase/error where it stopped
    pub disk_wiped: bool,
    pub total_duration: Duration,
}
impl RehearsalReport {
    pub fn print_table(&self);           // colored CLI table
    pub fn to_log_lines(&self) -> Vec<RehearsalLogLine>;  // for GUI
    pub fn write_to_file(&self, path: &Path) -> io::Result<()>;  // structured log file
}
```
The table output groups operations by phase, shows status/duration per operation, and ends with a summary line showing total ops, pass/fail counts, short-circuit location, and total duration.
### 4. Rehearsal Orchestrator (`src/rehearsal/mod.rs`)
```rust
pub fn run_rehearsal(config: &DeploymentConfig) -> RehearsalReport;
```
Internally:
1. Creates `mpsc::channel::<OperationRecord>()`
2. Creates `DiskWipeGuard` for the target device
3. Creates `Installer::new(config, false).with_skip_confirm(true).with_recorder(tx)`
4. Runs `installer.run()` — this is the real installer, executing every phase for real
5. On success or failure, collects all `OperationRecord`s from the receiver
6. If the installer errored, captures the phase/error as `short_circuited_at`
7. `DiskWipeGuard` drops → unmounts everything, wipes disk, writes blank GPT
8. Returns `RehearsalReport`
### 5. CLI Integration (`src/main.rs`)
New subcommand:
```warp-runnable-command
deploytix rehearse [--config deploytix.toml] [--log-file rehearsal.log]
```
Requires root. Loads config, runs rehearsal, prints table, writes log file.
### 6. Separate Binary (`src-rehearsal/main.rs`)
Thin wrapper that imports from the `deploytix` library:
```rust
use deploytix::{config::DeploymentConfig, rehearsal::run_rehearsal};
fn main() { /* parse args, load config, run_rehearsal(), print/save */ }
```
`Cargo.toml` gets `[[bin]] name = "deploytix-rehearsal" path = "src-rehearsal/main.rs"`.
### 7. GUI Integration
* Add `rehearsal_running`, `rehearsal_results`, `rehearsal_log_lines` fields to `InstallState`
* Add `InstallMessage::RehearsalResults` variant
* Add "Rehearsal" button in summary panel (between Preflight and Install)
* Background thread runs `run_rehearsal()`, streams results back via channel
* Results displayed as scrollable colored log with phase grouping
## File Plan
**New files:**
* `src/rehearsal/mod.rs` — orchestrator, `run_rehearsal()` entry point
* `src/rehearsal/report.rs` — `RehearsalReport`, `RehearsalLogLine`, table/file rendering
* `src/rehearsal/guard.rs` — `DiskWipeGuard`
* `src-rehearsal/main.rs` — standalone rehearsal binary
**Modified files:**
* `src/utils/command.rs` — add `OperationRecord`, optional recorder to `CommandRunner`
* `src/install/installer.rs` — add `with_recorder()` builder that passes channel to `CommandRunner`
* `src/lib.rs` — add `pub mod rehearsal`
* `src/main.rs` — add `Rehearse` subcommand
* `src/gui/state.rs` — add rehearsal state fields + message variant
* `src/gui/app.rs` — add `start_rehearsal()` method
* `src/gui/panels/summary.rs` — add Rehearsal button + results display
* `Cargo.toml` — add `[[bin]]` for `deploytix-rehearsal`
