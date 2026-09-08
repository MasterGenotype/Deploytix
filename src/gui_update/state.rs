//! State and background workers for the update GUI.

use super::model::{self, Backend, SnapshotRow};
use super::preview::{self, Preview};
use crate::aur::capability::{self, AurCapability};
use crate::immutable::update::{run_update, UpdateOptions};
use crate::immutable::{boot, detect_devices, history, lvm_ab, rollback, snapshot};
use crate::utils::command::{CommandRunner, OperationRecord};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

/// Which worker channel `pump_one` is draining.
#[derive(Debug, Clone, Copy)]
enum ChannelSlot {
    Operation,
    Refresh,
    /// Dependency resolution for the Update tab. Its own channel so a slow
    /// resolve can never displace a running update's messages.
    Resolve,
}

/// How many trailing output lines of each command reach the log pane.
const LOG_TAIL_LINES: usize = 12;

/// Which view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    System,
    Update,
    Snapshots,
    Progress,
}

impl Tab {
    pub const ALL: [Self; 4] = [Self::System, Self::Update, Self::Snapshots, Self::Progress];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Update => "Update",
            Self::Snapshots => "Snapshots",
            Self::Progress => "Progress",
        }
    }
}

/// What the running system looks like right now.
#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub backend: Backend,
    /// Target booted right now.
    pub running: String,
    /// Target the boot pointer selects for next boot.
    pub pointer: String,
}

impl SystemInfo {
    /// An update is built and waiting for a reboot.
    pub fn has_staged_update(&self) -> bool {
        self.pointer != self.running
    }
}

/// Messages from the background workers to the UI thread.
pub enum Msg {
    Status(String),
    Log(String),
    /// A refresh completed.
    Refreshed {
        info: SystemInfo,
        rows: Vec<SnapshotRow>,
        /// What this system can do with the AUR, probed alongside the rest.
        capability: AurCapability,
    },
    /// A dependency preview completed.
    Resolved(Box<Preview>),
    /// A refresh could not read the system.
    RefreshFailed(String),
    /// The operation finished; the string is the user-facing summary.
    Finished(String),
    Error(String),
}

/// Whole application state.
pub struct AppState {
    pub tab: Tab,

    /// `None` until the first refresh lands.
    pub info: Option<SystemInfo>,
    pub rows: Vec<SnapshotRow>,
    /// Target whose package list is expanded in the Snapshots tab.
    pub expanded: Option<String>,

    /// What this system can do with the AUR. `None` until the first refresh.
    pub capability: Option<AurCapability>,

    // Update form
    pub repo_packages: String,
    pub selected_files: Vec<PathBuf>,
    pub keep_sets: usize,
    pub reboot_after: bool,

    // File browser
    pub browser_open: bool,
    pub browser_dir: PathBuf,

    // Rollback confirmation
    pub confirm_rollback: Option<SnapshotRow>,

    // Run state
    pub busy: bool,
    pub status: String,
    pub logs: Vec<String>,
    pub finished: Option<String>,
    pub error: Option<String>,
    /// Messages from the update/rollback worker.
    pub receiver: Option<Receiver<Msg>>,

    // Refresh state, kept separate from the operation above so that reading
    // system state can never displace a running update's message channel.
    pub refreshing: bool,
    pub refresh_error: Option<String>,
    pub refresh_receiver: Option<Receiver<Msg>>,

    // Dependency preview, on its own channel for the same reason.
    pub preview: Option<Preview>,
    pub resolving: bool,
    pub resolve_receiver: Option<Receiver<Msg>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tab: Tab::System,
            info: None,
            rows: Vec::new(),
            expanded: None,
            capability: None,
            repo_packages: String::new(),
            selected_files: Vec::new(),
            keep_sets: UpdateOptions::default().keep_sets,
            reboot_after: false,
            browser_open: false,
            browser_dir: default_browse_dir(),
            confirm_rollback: None,
            busy: false,
            status: String::new(),
            logs: Vec::new(),
            finished: None,
            error: None,
            receiver: None,
            refreshing: false,
            refresh_error: None,
            refresh_receiver: None,
            preview: None,
            resolving: false,
            resolve_receiver: None,
        }
    }
}

/// Where the local-package browser starts.
///
/// Under `pkexec` `$HOME` is root's, so the invoking user's home is resolved
/// explicitly; the pacman cache is the next most likely place to find a
/// package file.
fn default_browse_dir() -> PathBuf {
    if let Some(home) = crate::utils::user::invoking_user_home() {
        let downloads = home.join("Downloads");
        return if downloads.is_dir() { downloads } else { home };
    }
    let cache = PathBuf::from("/var/cache/pacman/pkg");
    if cache.is_dir() {
        cache
    } else {
        PathBuf::from("/")
    }
}

impl AppState {
    /// Drain both worker channels. Returns true if anything changed (so the UI
    /// can request a repaint).
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        for slot in [
            ChannelSlot::Operation,
            ChannelSlot::Refresh,
            ChannelSlot::Resolve,
        ] {
            changed |= self.pump_one(slot);
        }
        changed
    }

    fn pump_one(&mut self, slot: ChannelSlot) -> bool {
        let mut changed = false;
        let mut disconnected = false;
        let mut drained = Vec::new();

        let rx = match slot {
            ChannelSlot::Operation => self.receiver.as_ref(),
            ChannelSlot::Refresh => self.refresh_receiver.as_ref(),
            ChannelSlot::Resolve => self.resolve_receiver.as_ref(),
        };
        if let Some(rx) = rx {
            loop {
                match rx.try_recv() {
                    Ok(msg) => drained.push(msg),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for msg in drained {
            changed = true;
            self.apply(msg);
        }

        if disconnected {
            changed = true;
            match slot {
                ChannelSlot::Operation => {
                    self.receiver = None;
                    // A worker that ended without saying so leaves nothing in
                    // flight; otherwise the UI spins forever.
                    if self.finished.is_none() && self.error.is_none() {
                        self.busy = false;
                    }
                }
                ChannelSlot::Refresh => {
                    self.refresh_receiver = None;
                    self.refreshing = false;
                }
                ChannelSlot::Resolve => {
                    self.resolve_receiver = None;
                    self.resolving = false;
                }
            }
        }
        changed
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Status(s) => self.status = s,
            Msg::Log(l) => self.logs.push(l),
            Msg::Refreshed {
                info,
                rows,
                capability,
            } => {
                self.info = Some(info);
                self.rows = rows;
                self.capability = Some(capability);
                self.refresh_error = None;
                self.refreshing = false;
            }
            Msg::Resolved(p) => {
                self.preview = Some(*p);
                self.resolving = false;
            }
            Msg::RefreshFailed(e) => {
                self.refresh_error = Some(e);
                self.refreshing = false;
            }
            Msg::Finished(s) => {
                self.finished = Some(s);
                self.busy = false;
                // The snapshot list is stale the moment an update lands.
                self.start_refresh();
            }
            Msg::Error(e) => {
                self.error = Some(e);
                self.busy = false;
            }
        }
    }

    /// True when a package selection has been made and an update can start.
    pub fn has_selection(&self) -> bool {
        !model::parse_package_names(&self.repo_packages).is_empty()
            || !self.selected_files.is_empty()
    }

    /// The arguments an update would be run with.
    pub fn update_args(&self) -> Vec<String> {
        let mut args = model::parse_package_names(&self.repo_packages);
        args.extend(
            self.selected_files
                .iter()
                .map(|p| p.to_string_lossy().to_string()),
        );
        args
    }

    fn begin(&mut self, status: &str) -> Sender<Msg> {
        let (tx, rx) = channel();
        self.receiver = Some(rx);
        self.busy = true;
        self.status = status.to_string();
        self.finished = None;
        self.error = None;
        tx
    }

    /// Reload system info and the snapshot list in the background.
    ///
    /// Uses its own channel so it can run alongside an update without
    /// displacing that operation's messages.
    pub fn start_refresh(&mut self) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.refresh_error = None;
        let (tx, rx) = channel();
        self.refresh_receiver = Some(rx);

        thread::spawn(move || {
            let cmd = CommandRunner::new(false);
            let msg = match collect_state(&cmd) {
                // Probed against the live root: the set an update builds is a
                // snapshot of it, so what is installed here is what the build
                // chroot will have.
                Ok((info, rows)) => Msg::Refreshed {
                    info,
                    rows,
                    capability: capability::probe(&cmd, ""),
                },
                Err(e) => Msg::RefreshFailed(e.to_string()),
            };
            let _ = tx.send(msg);
        });
    }

    /// Resolve the typed package names in the background.
    ///
    /// Read-only and unprivileged — `pacman -S --print` and metadata queries —
    /// so it needs no confirmation and cannot affect a running update. Called
    /// when the user finishes editing the package field, not per keystroke.
    ///
    /// A resolve already in flight is left alone rather than cancelled: its
    /// result is discarded by the staleness check in [`Self::preview_matches`]
    /// if the field has moved on.
    pub fn start_resolve(&mut self) {
        let targets = model::parse_package_names(&self.repo_packages);
        if targets.is_empty() {
            self.preview = None;
            return;
        }
        if self.resolving {
            return;
        }
        self.resolving = true;
        let (tx, rx) = channel();
        self.resolve_receiver = Some(rx);

        thread::spawn(move || {
            let msg = match preview::resolve(&targets) {
                Ok(p) => Msg::Resolved(Box::new(p)),
                // A preview is an aid, not a gate: a failure to resolve must
                // not block staging, so it is reported as an empty preview
                // carrying the reason rather than as an error banner.
                Err(e) => Msg::Resolved(Box::new(Preview {
                    targets,
                    notes: vec![format!("Could not resolve dependencies: {e}")],
                    ..Default::default()
                })),
            };
            let _ = tx.send(msg);
        });
    }

    /// Whether the held preview describes what is currently typed.
    ///
    /// The field can change while a resolve is in flight, so the panel checks
    /// this before showing a preview rather than trusting whatever landed last.
    pub fn preview_matches(&self) -> bool {
        match &self.preview {
            Some(p) => p.targets == model::parse_package_names(&self.repo_packages),
            None => false,
        }
    }

    /// The preview to display, if it is still current.
    pub fn current_preview(&self) -> Option<&Preview> {
        if self.preview_matches() {
            self.preview.as_ref()
        } else {
            None
        }
    }

    /// Run a transactional update in the background.
    pub fn start_update(&mut self, args: Vec<String>) {
        let tx = self.begin("Building update...");
        self.logs.clear();
        // The preview described the pre-update system; it is stale the moment
        // the transaction starts.
        self.preview = None;
        self.tab = Tab::Progress;

        let opts = UpdateOptions {
            keep_sets: self.keep_sets,
            reboot: self.reboot_after,
        };

        thread::spawn(move || {
            let cmd = command_runner_logging_to(&tx);
            let backend = Backend::detect();
            let _ = tx.send(Msg::Status(
                "Running pacman — output appears as each step completes.".to_string(),
            ));

            let result = match backend {
                Some(Backend::LvmAb) => lvm_ab::run_update(&cmd, &args, &opts),
                _ => run_update(&cmd, &args, &opts),
            };

            match result {
                Ok(()) => {
                    let _ = tx.send(Msg::Finished(
                        "Update staged. Reboot to activate it.".to_string(),
                    ));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!(
                        "{e}\n\nThe running system is unchanged."
                    )));
                }
            }
        });
    }

    /// Roll the boot pointer back to `target` in the background.
    pub fn start_rollback(&mut self, target: String) {
        let tx = self.begin(&format!("Rolling back to {target}..."));
        self.logs.clear();
        self.tab = Tab::Progress;
        let reboot = self.reboot_after;

        thread::spawn(move || {
            let cmd = command_runner_logging_to(&tx);
            let backend = Backend::detect();
            let result = match backend {
                Some(Backend::LvmAb) => lvm_ab::run_rollback(&cmd, Some(&target), reboot),
                _ => rollback::run_rollback(&cmd, Some(&target), reboot),
            };
            match result {
                Ok(()) => {
                    let _ = tx.send(Msg::Finished(format!(
                        "Rolled back to {target}. Reboot to activate it."
                    )));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("Rollback failed: {e}")));
                }
            }
        });
    }
}

/// A `CommandRunner` whose completed commands are forwarded to `tx` as log
/// lines.
///
/// The runner blocks its own thread for the whole operation, so a second thread
/// drains the recorder channel. It ends on its own when the runner is dropped
/// and the sender closes.
fn command_runner_logging_to(tx: &Sender<Msg>) -> CommandRunner {
    let (op_tx, op_rx): (Sender<OperationRecord>, Receiver<OperationRecord>) = channel();
    let log_tx = tx.clone();
    thread::spawn(move || {
        for record in op_rx {
            let _ = log_tx.send(Msg::Log(model::format_op(&record, LOG_TAIL_LINES)));
        }
    });
    CommandRunner::new(false).with_recorder(op_tx)
}

/// Read the backend, the running and pointed-at targets, and the snapshot rows.
fn collect_state(
    cmd: &CommandRunner,
) -> crate::utils::error::Result<(SystemInfo, Vec<SnapshotRow>)> {
    let backend = Backend::detect().ok_or_else(|| {
        crate::utils::error::DeploytixError::ConfigError(
            "not an immutable deploytix system".to_string(),
        )
    })?;
    let records = history::list_records();

    let (info, targets) = match backend {
        Backend::LvmAb => {
            let state = lvm_ab::read_state()?;
            let running = model::running_slot_from_cmdline(
                &std::fs::read_to_string("/proc/cmdline").unwrap_or_default(),
            )
            // Without a cmdline marker the active slot is the best guess
            // available, and matches what a freshly installed system boots.
            .unwrap_or_else(|| state.active.clone());
            (
                SystemInfo {
                    backend,
                    running,
                    pointer: state.active.clone(),
                },
                vec!["A".to_string(), "B".to_string()],
            )
        }
        Backend::Btrfs => {
            let devices = detect_devices();
            let pointer = boot::pointer_set_id(&boot::current_boot_pointer(cmd)?)
                .unwrap_or_else(|| "@".to_string());
            // From the kernel cmdline, not `findmnt -no FSROOT /`: on an
            // immutable system `/` is an overlayfs whose FSROOT is just "/",
            // which resolved to "@" on every boot and made the GUI report a
            // permanently staged update.
            let running = boot::running_set_id();

            let mut targets = vec!["@".to_string()];
            targets.extend(snapshot::list_sets(cmd, &devices.root_fs)?);
            (
                SystemInfo {
                    backend,
                    running,
                    pointer,
                },
                targets,
            )
        }
    };

    let rows = model::build_snapshot_rows(&targets, &records, &info.running, &info.pointer);
    Ok((info, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_update_is_a_pointer_that_differs_from_the_running_target() {
        let mut info = SystemInfo {
            backend: Backend::Btrfs,
            running: "100".into(),
            pointer: "100".into(),
        };
        assert!(!info.has_staged_update());
        info.pointer = "200".into();
        assert!(info.has_staged_update());
    }

    #[test]
    fn update_args_combine_repo_names_and_local_files() {
        let mut state = AppState::default();
        state.repo_packages = "vim  git".into();
        state.selected_files = vec![PathBuf::from("/tmp/a.pkg.tar.zst")];
        assert_eq!(
            state.update_args(),
            vec!["vim", "git", "/tmp/a.pkg.tar.zst"]
        );
        assert!(state.has_selection());
    }

    #[test]
    fn an_empty_form_is_not_a_selection() {
        let mut state = AppState::default();
        assert!(!state.has_selection());
        // Whitespace alone must not count as a package name, or the update
        // would silently become a full system upgrade.
        state.repo_packages = "   \n ".into();
        assert!(!state.has_selection());
        assert!(state.update_args().is_empty());
    }

    #[test]
    fn pump_applies_messages_and_clears_busy_on_completion() {
        let mut state = AppState::default();
        let (tx, rx) = channel();
        state.receiver = Some(rx);
        state.busy = true;

        tx.send(Msg::Log("first".into())).unwrap();
        tx.send(Msg::Status("working".into())).unwrap();
        tx.send(Msg::Finished("done".into())).unwrap();
        assert!(state.pump());

        assert_eq!(state.logs, vec!["first"]);
        assert_eq!(state.status, "working");
        assert_eq!(state.finished.as_deref(), Some("done"));
        assert!(!state.busy);
    }

    #[test]
    fn a_worker_that_dies_silently_does_not_leave_the_ui_busy() {
        // Without this the UI would show a spinner forever if a worker panicked.
        let mut state = AppState::default();
        let (tx, rx) = channel::<Msg>();
        state.receiver = Some(rx);
        state.busy = true;
        drop(tx);

        state.pump();
        assert!(!state.busy);
        assert!(state.receiver.is_none());
    }

    /// A refresh that fails must leave a visible reason. Without this the
    /// System and Snapshots tabs sat on "Reading system state..." forever
    /// while the real error went nowhere.
    #[test]
    fn a_failed_refresh_records_why_and_stops_refreshing() {
        let mut state = AppState::default();
        let (tx, rx) = channel();
        state.refresh_receiver = Some(rx);
        state.refreshing = true;

        tx.send(Msg::RefreshFailed("no such device".into()))
            .unwrap();
        state.pump();

        assert_eq!(state.refresh_error.as_deref(), Some("no such device"));
        assert!(!state.refreshing);
        // A refresh failure is not an operation failure: the Progress tab must
        // not start reporting an update as failed.
        assert!(state.error.is_none());
    }

    /// A refresh and an update run on separate channels, so starting a refresh
    /// mid-update must not swallow the update's messages.
    #[test]
    fn a_refresh_does_not_displace_a_running_operation() {
        let mut state = AppState::default();
        let (op_tx, op_rx) = channel();
        state.receiver = Some(op_rx);
        state.busy = true;

        state.start_refresh();
        assert!(state.receiver.is_some(), "operation channel must survive");

        op_tx.send(Msg::Log("still here".into())).unwrap();
        state.pump();
        assert_eq!(state.logs, vec!["still here"]);
    }

    #[test]
    fn a_successful_refresh_clears_a_previous_failure() {
        let mut state = AppState::default();
        state.refresh_error = Some("stale".into());
        let (tx, rx) = channel();
        state.refresh_receiver = Some(rx);

        tx.send(Msg::Refreshed {
            info: SystemInfo {
                backend: Backend::Btrfs,
                running: "100".into(),
                pointer: "100".into(),
            },
            rows: Vec::new(),
            capability: AurCapability::default(),
        })
        .unwrap();
        state.pump();

        assert!(state.refresh_error.is_none());
        assert!(state.info.is_some());
        assert!(
            state.capability.is_some(),
            "a refresh must also land the AUR capability"
        );
    }

    #[test]
    fn a_resolve_never_displaces_a_running_update() {
        // The whole reason resolution has its own channel: a slow pacman query
        // must not take over the channel an in-flight update reports on.
        let mut state = AppState::default();
        let (op_tx, op_rx) = channel();
        state.receiver = Some(op_rx);
        state.busy = true;

        let (res_tx, res_rx) = channel();
        state.resolve_receiver = Some(res_rx);
        res_tx
            .send(Msg::Resolved(Box::new(Preview::default())))
            .unwrap();
        op_tx.send(Msg::Log("update output".into())).unwrap();
        state.pump();

        assert_eq!(state.logs, vec!["update output"]);
        assert!(state.preview.is_some());
        assert!(state.busy, "a resolve must not end the update");
    }

    #[test]
    fn a_preview_for_different_packages_is_not_shown() {
        // A resolve in flight while the user keeps typing lands a preview for
        // the old text; showing it would misdescribe what is about to be staged.
        let mut state = AppState::default();
        state.repo_packages = "vim".into();
        state.preview = Some(Preview {
            targets: vec!["neovim".into()],
            ..Default::default()
        });
        assert!(!state.preview_matches());
        assert!(state.current_preview().is_none());

        state.repo_packages = "neovim".into();
        assert!(state.preview_matches());
        assert!(state.current_preview().is_some());
    }

    #[test]
    fn staging_an_update_drops_the_stale_preview() {
        let mut state = AppState::default();
        state.repo_packages = "vim".into();
        state.preview = Some(Preview {
            targets: vec!["vim".into()],
            ..Default::default()
        });
        state.start_update(vec!["vim".to_string()]);
        assert!(
            state.preview.is_none(),
            "the preview described the pre-update system"
        );
    }

    #[test]
    fn clearing_the_field_clears_the_preview() {
        let mut state = AppState::default();
        state.preview = Some(Preview {
            targets: vec!["vim".into()],
            ..Default::default()
        });
        state.repo_packages = String::new();
        state.start_resolve();
        assert!(state.preview.is_none());
        assert!(!state.resolving, "nothing to resolve, no worker spawned");
    }

    #[test]
    fn an_error_message_clears_busy_and_is_retained() {
        let mut state = AppState::default();
        let (tx, rx) = channel();
        state.receiver = Some(rx);
        state.busy = true;
        tx.send(Msg::Error("boom".into())).unwrap();
        state.pump();
        assert_eq!(state.error.as_deref(), Some("boom"));
        assert!(!state.busy);
    }
}
