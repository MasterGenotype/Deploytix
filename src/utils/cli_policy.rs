//! CLI implementation of [`InteractivePolicy`].
//!
//! Renders pacman-confirm prompts to stdout/stdin and shells out to
//! `$EDITOR` (falling back to `vi`) for the edit flow.  The extras
//! prompt is a sequential pacman/yay loop.

use crate::utils::error::{DeploytixError, Result};
use crate::utils::interactive::{
    ExtraPackages, InteractivePolicy, PacmanDecision, PacmanInvocation, PacmanKind,
};
use crate::utils::prompt::prompt_confirm;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use tracing::warn;

/// Policy that interacts via the controlling terminal.
pub struct CliInteractivePolicy;

impl CliInteractivePolicy {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CliInteractivePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractivePolicy for CliInteractivePolicy {
    fn confirm_pacman(&self, inv: &PacmanInvocation) -> PacmanDecision {
        match prompt_pacman_confirm(inv) {
            Ok(d) => d,
            Err(e) => {
                warn!("interactive prompt failed ({}); approving", e);
                PacmanDecision::Approve
            }
        }
    }

    fn prompt_extras(&self, can_use_yay: bool) -> (ExtraPackages, bool) {
        match prompt_extras_loop(can_use_yay) {
            Ok(out) => out,
            Err(e) => {
                warn!("extras prompt failed ({}); skipping", e);
                (ExtraPackages::default(), false)
            }
        }
    }
}

// ── Pacman-confirm prompt ────────────────────────────────────────────

fn prompt_pacman_confirm(inv: &PacmanInvocation) -> Result<PacmanDecision> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    writeln!(out, "── {} ──", inv.label)?;
    writeln!(out, "  {}", inv.render())?;
    if let PacmanKind::Yay = inv.kind {
        if let Some(u) = &inv.run_as_user {
            writeln!(out, "  (run as user {})", u)?;
        }
    }
    writeln!(
        out,
        "  packages: {} ({})",
        inv.packages.len(),
        inv.packages.join(" ")
    )?;
    write!(out, "[A]pprove · [E]dit · [S]kip · [C]ancel ▸ ")?;
    out.flush()?;
    drop(out);

    let stdin = std::io::stdin();
    let mut buf = String::new();
    stdin.lock().read_line(&mut buf)?;
    match buf.trim().to_ascii_lowercase().as_str() {
        "" | "a" | "approve" | "y" | "yes" => Ok(PacmanDecision::Approve),
        "e" | "edit" => edit_invocation(inv),
        "s" | "skip" => Ok(PacmanDecision::Skip),
        "c" | "cancel" | "n" | "no" => Ok(PacmanDecision::Cancel),
        other => {
            warn!("unrecognised choice '{}'; approving", other);
            Ok(PacmanDecision::Approve)
        }
    }
}

// ── Editor flow ──────────────────────────────────────────────────────

fn edit_invocation(inv: &PacmanInvocation) -> Result<PacmanDecision> {
    let template = build_edit_template(inv);
    let path = write_temp(&template)?;
    open_editor(&path)?;

    let mut buf = String::new();
    std::fs::File::open(&path)?.read_to_string(&mut buf)?;
    let _ = std::fs::remove_file(&path);

    let (packages, extra_flags) = parse_edited(&buf);
    if packages.is_empty() {
        eprintln!(
            "  → empty package list after edit; treating as Skip. \
             (Cancel the install with [C] next time if that's what you wanted.)"
        );
        return Ok(PacmanDecision::Skip);
    }
    Ok(PacmanDecision::EditedTo {
        packages,
        extra_flags,
    })
}

const PKG_SECTION_HEADER: &str = "# === Packages (one per line) ===";
const FLAG_SECTION_HEADER: &str = "# === Extra flags (one per line) ===";

fn build_edit_template(inv: &PacmanInvocation) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Pacman invocation: {}\n", inv.label));
    s.push_str(&format!("# Kind: {:?}\n", inv.kind));
    if let Some(u) = &inv.run_as_user {
        s.push_str(&format!("# Run as: {}\n", u));
    }
    let core = inv.kind.core_flags();
    if !core.is_empty() {
        s.push_str(&format!(
            "# Core flags (NOT editable, stripped on save): {}\n",
            core.join(" ")
        ));
    }
    if matches!(inv.kind, PacmanKind::Basestrap) {
        s.push_str("#\n");
        s.push_str("# !! WARNING: editing the basestrap package list can brick the install.\n");
        s.push_str("# !! Removing `base`, `linux`, the configured init's package, or the\n");
        s.push_str("# !! bootloader will leave you with an unbootable system.\n");
    }
    s.push_str("#\n");
    s.push_str("# Lines starting with # are ignored. Save and exit to apply.\n");
    s.push_str("# Empty package list = skip this invocation.\n\n");

    s.push_str(PKG_SECTION_HEADER);
    s.push('\n');
    for p in &inv.packages {
        s.push_str(p);
        s.push('\n');
    }
    s.push('\n');
    s.push_str(FLAG_SECTION_HEADER);
    s.push('\n');
    for f in &inv.extra_flags {
        s.push_str(f);
        s.push('\n');
    }
    s
}

fn parse_edited(buf: &str) -> (Vec<String>, Vec<String>) {
    enum Section {
        Packages,
        Flags,
    }
    let mut section = Section::Packages;
    let mut packages = Vec::new();
    let mut flags = Vec::new();
    for raw in buf.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("# === Packages") {
            section = Section::Packages;
            continue;
        }
        if line.starts_with("# === Extra flags") {
            section = Section::Flags;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        match section {
            Section::Packages => packages.push(line.to_string()),
            Section::Flags => flags.push(line.to_string()),
        }
    }
    (packages, flags)
}

fn write_temp(content: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("deploytix-pacman-{pid}-{nanos}.txt"));
    let mut f = std::fs::File::create(&path)?;
    f.write_all(content.as_bytes())?;
    Ok(path)
}

/// Pick an editor.  Honors `$EDITOR`, falls back to `vi` if unset or if
/// `$EDITOR` looks like a known non-blocking GUI editor (`code`, `subl`,
/// etc.) which would race the install thread.
fn pick_editor() -> String {
    let known_non_blocking = ["code", "code-insiders", "subl", "atom", "gnome-text-editor"];
    if let Ok(e) = std::env::var("EDITOR") {
        let bin = e.split_whitespace().next().unwrap_or("");
        let basename = std::path::Path::new(bin)
            .file_name()
            .and_then(|b| b.to_str())
            .unwrap_or(bin);
        if known_non_blocking.contains(&basename) {
            warn!(
                "$EDITOR='{}' is non-blocking; falling back to vi for the pacman edit flow",
                e
            );
            return "vi".to_string();
        }
        if !e.trim().is_empty() {
            return e;
        }
    }
    "vi".to_string()
}

fn open_editor(path: &std::path::Path) -> Result<()> {
    let editor = pick_editor();
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} {}", editor, shell_escape(path)))
        .status()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed {
            command: format!("{} {}", editor, path.display()),
            stderr: format!("editor exited with status {}", status),
        });
    }
    Ok(())
}

fn shell_escape(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    {
        s
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ── Extras prompt ────────────────────────────────────────────────────

fn prompt_extras_loop(can_use_yay: bool) -> Result<(ExtraPackages, bool)> {
    println!();
    println!("== Optional: install extra packages ==");
    let mut extras = ExtraPackages::default();

    loop {
        let pacman_pkgs =
            read_line("Repository packages (pacman -S). Space-separated, empty to skip:\n  > ")?;
        if !pacman_pkgs.trim().is_empty() {
            extras
                .pacman
                .extend(pacman_pkgs.split_whitespace().map(|s| s.to_string()));
        }
        if can_use_yay {
            let aur_pkgs =
                read_line("AUR packages (yay -S). Space-separated, empty to skip:\n  > ")?;
            if !aur_pkgs.trim().is_empty() {
                extras
                    .aur
                    .extend(aur_pkgs.split_whitespace().map(|s| s.to_string()));
            }
        }
        if extras.is_empty() || !prompt_confirm("Add more extras?", false).unwrap_or(false) {
            break;
        }
    }

    let save = if extras.is_empty() {
        false
    } else {
        prompt_confirm(
            "Save these extras to your config so re-runs install them non-interactively?",
            false,
        )
        .unwrap_or(false)
    };
    Ok((extras, save))
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches('\n').to_string())
}
