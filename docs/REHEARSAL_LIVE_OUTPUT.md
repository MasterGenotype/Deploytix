# Real-time output during rehearsal installations

## Problem

`run_rehearsal()` collects all `OperationRecord`s from the `mpsc::channel` **after** `installer.run()` completes, then renders a single batch report. The user sees zero output while the rehearsal is running — which can take many minutes.

## Current flow

`src/rehearsal/mod.rs`:

1. Creates `mpsc::channel::<OperationRecord>()`
2. Passes `tx` into the Installer via `with_recorder()`
3. `installer.run()` blocks — records stream into channel
4. **After** run completes: `rx.iter().collect()` drains all records at once
5. Builds `RehearsalReport` and returns it
6. `src-rehearsal/main.rs` calls `report.print_table()` — all output is post-hoc

## Proposed changes

### 1. Live consumer thread in `run_rehearsal()` (`src/rehearsal/mod.rs`)

Spawn a thread before `installer.run()` that reads from `rx` in real-time. For each `OperationRecord` received, it immediately prints a one-line status to stderr and appends the record to a `Vec`. When the installer finishes and the `tx` is dropped, `rx.iter()` terminates and the thread joins — returning the collected `Vec<OperationRecord>` for the final report.

This keeps the existing `mpsc::channel` plumbing intact; we just move the drain from a post-hoc `.collect()` to a real-time consuming thread.

### 2. New `print_live_record()` helper in `src/rehearsal/report.rs`

A standalone function that formats and prints a single `OperationRecord` as one line to stderr. Format:

```
[42] ✓  0.3s  sfdisk /dev/sda
[43] ✗  1.2s  mkfs.btrfs /dev/mapper/Crypt-Root
         └─ mkfs.btrfs: invalid argument
```

Uses a running counter (passed in) since total ops isn't known ahead of time. Prints to **stderr** so it doesn't interfere with any structured stdout output. Shows stderr snippet for failures, same as the existing `to_log_lines()` pattern.

### 3. Update `src-rehearsal/main.rs`

No significant changes needed — `run_rehearsal()` will now print live output during execution, and the existing `report.print_table()` call at the end still provides the summary table. Add a small note before the rehearsal starts indicating that live output will follow.

### 4. Optional: live callback for GUI consumers

Add an optional `on_record` callback parameter to `run_rehearsal()` (or a new `run_rehearsal_live()` variant) so the GUI/TKG layer can receive records as they arrive without coupling to stderr. This keeps the existing `run_rehearsal()` signature backward-compatible. Lower priority — can be a follow-up.
