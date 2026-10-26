//! The updater's pacman console: pacman's output as it happens, and its
//! questions answered by the user rather than by `--noconfirm`'s defaults.
//!
//! The worker thread owns the pacman run (see [`crate::utils::console`]). Its
//! [`GuiConsole`] forwards output to the UI thread as [`Msg::Console`], and
//! for a question sends a [`PendingQuestion`] carrying a reply channel, then
//! blocks until the UI answers through it. If the UI goes away first the
//! sender is dropped and the question is cancelled, which pacman reads as
//! "no" — never as an answer the user did not give.

use super::state::Msg;
use crate::utils::console::{Answer, Console, Question, QuestionKind};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::mpsc::{channel, Sender};
use std::time::SystemTime;

/// Where pacman is in a transaction, read from the section headers it prints.
///
/// pacman draws no progress bars when its output is not a terminal, so without
/// this a long download or install shows one line and then nothing for minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Syncing,
    Resolving,
    Downloading,
    Verifying,
    Installing,
    Hooks,
    /// pacman has exited; the rest of the transaction is deploytix's.
    Finishing,
}

impl Phase {
    /// The phase a line of pacman output starts, if it starts one.
    pub fn of(line: &str) -> Option<Self> {
        Some(match line.trim_end() {
            ":: Synchronizing package databases..." => Self::Syncing,
            ":: Starting full system upgrade..." | "resolving dependencies..." => Self::Resolving,
            ":: Retrieving packages..." => Self::Downloading,
            "checking keyring..." | "checking package integrity..." => Self::Verifying,
            ":: Processing package changes..." => Self::Installing,
            ":: Running post-transaction hooks..." => Self::Hooks,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Syncing => "Synchronizing package databases...",
            Self::Resolving => "Resolving the transaction...",
            Self::Downloading => "Downloading packages...",
            Self::Verifying => "Verifying packages...",
            Self::Installing => "Installing packages...",
            Self::Hooks => "Running post-transaction hooks...",
            Self::Finishing => {
                "pacman finished. Rebuilding the initramfs and staging the snapshot..."
            }
        }
    }
}

/// The byte count of a `Total Download Size:   1724.66 MiB` line.
pub fn download_size(line: &str) -> Option<u64> {
    let rest = line.trim().strip_prefix("Total Download Size:")?;
    let mut words = rest.split_whitespace();
    let value: f64 = words.next()?.parse().ok()?;
    let unit = match words.next()? {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * unit) as u64)
}

/// When an inode last changed — including being renamed into place.
fn changed_at(meta: &std::fs::Metadata) -> SystemTime {
    use std::os::unix::fs::MetadataExt;
    let secs = u64::try_from(meta.ctime()).unwrap_or(0);
    let nanos = u32::try_from(meta.ctime_nsec()).unwrap_or(0);
    SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos)
}

/// pacman's package cache as the transaction sees it: `/var` is rbind-mounted
/// into the chroot, so the host path is the same directory.
pub const PKG_CACHE: &str = "/var/cache/pacman/pkg";

/// Bytes pacman has downloaded into `cache` since `since`.
///
/// pacman downloads into `download-*` scratch directories inside the cache and
/// moves each finished file up beside the others, so this counts both: what is
/// in a scratch directory, and package files that arrived since the download
/// began. Arrival is the inode's change time, not its modification time:
/// pacman may stamp a file with the mirror's time, but the move into the cache
/// is a change either way. Unreadable entries count as nothing; this is a
/// progress readout, not an account.
pub fn downloaded_since(cache: &Path, since: SystemTime) -> u64 {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            if entry.file_name().to_string_lossy().starts_with("download-") {
                total += std::fs::read_dir(entry.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|f| f.metadata().ok())
                    .filter(|m| m.is_file())
                    .map(|m| m.len())
                    .sum::<u64>();
            }
        } else if changed_at(&meta) >= since {
            total += meta.len();
        }
    }
    total
}

/// A question pacman is blocked on, waiting for the user.
pub struct PendingQuestion {
    pub question: Question,
    /// What the answer box holds for a number or selection question,
    /// pre-filled with pacman's default.
    pub input: String,
    reply: Sender<Answer>,
}

impl PendingQuestion {
    /// Answer with `answer` and release pacman.
    pub fn answer(self, answer: Answer) {
        // A worker that has already given up has nothing left to unblock.
        let _ = self.reply.send(answer);
    }

    /// Answer with what the answer box holds.
    pub fn submit(self) {
        let reply = self.input.trim().to_string();
        self.answer(Answer::Reply(reply));
    }

    /// The default answer's label, for marking the default button.
    pub fn default_is_yes(&self) -> Option<bool> {
        match self.question.kind {
            QuestionKind::YesNo { default_yes } => Some(default_yes),
            _ => None,
        }
    }
}

/// The worker side: lives on the update thread for the length of a pacman run.
pub struct GuiConsole {
    tx: Sender<Msg>,
    /// The line pacman is part-way through, to read phases from whole lines.
    line: String,
}

impl GuiConsole {
    pub fn new(tx: Sender<Msg>) -> Self {
        Self {
            tx,
            line: String::new(),
        }
    }

    fn line_done(&mut self) {
        let line = std::mem::take(&mut self.line);
        if let Some(phase) = Phase::of(&line) {
            let _ = self.tx.send(Msg::Phase(phase));
        } else if let Some(bytes) = download_size(&line) {
            let _ = self.tx.send(Msg::DownloadSize(bytes));
        }
    }
}

impl Console for GuiConsole {
    fn output(&mut self, text: &str) {
        let _ = self.tx.send(Msg::Console(text.to_string()));
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            self.line.push_str(first);
        }
        for part in parts {
            self.line_done();
            self.line.push_str(part);
        }
    }

    fn finished(&mut self, _status: ExitStatus) {
        self.line_done();
        let _ = self.tx.send(Msg::Phase(Phase::Finishing));
    }

    fn ask(&mut self, question: &Question) -> Answer {
        let (reply, answer) = channel();
        let pending = PendingQuestion {
            question: question.clone(),
            input: question.default_answer(),
            reply,
        };
        if self.tx.send(Msg::Question(Box::new(pending))).is_err() {
            return Answer::Cancel;
        }
        answer.recv().unwrap_or(Answer::Cancel)
    }
}

/// pacman's output, kept as lines for display. Bounded, because a large
/// upgrade prints thousands of lines and the full transcript is kept in the
/// update's history record anyway.
#[derive(Default)]
pub struct ConsoleBuffer {
    lines: Vec<String>,
    /// The line pacman is still writing — a question while one is pending.
    partial: String,
}

/// How many complete lines the console keeps.
pub const CONSOLE_LINES: usize = 5000;

impl ConsoleBuffer {
    pub fn push(&mut self, text: &str) {
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            self.partial.push_str(first);
        }
        for part in parts {
            self.lines.push(std::mem::take(&mut self.partial));
            self.partial.push_str(part);
        }
        if self.lines.len() > CONSOLE_LINES {
            let excess = self.lines.len() - CONSOLE_LINES;
            self.lines.drain(..excess);
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.partial.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.partial.is_empty()
    }

    /// Rows to display: the complete lines, then the partial one if any.
    pub fn row_count(&self) -> usize {
        self.lines.len() + usize::from(!self.partial.is_empty())
    }

    pub fn row(&self, i: usize) -> &str {
        self.lines
            .get(i)
            .map_or(self.partial.as_str(), String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict() -> Question {
        Question::parse(":: a-2-1 and b-1-1 are in conflict. Remove b? [y/N] ").unwrap()
    }

    #[test]
    fn phases_are_read_from_pacmans_section_headers() {
        assert_eq!(
            Phase::of(":: Retrieving packages..."),
            Some(Phase::Downloading)
        );
        assert_eq!(
            Phase::of(":: Processing package changes...\r"),
            Some(Phase::Installing)
        );
        assert_eq!(Phase::of("installing foo..."), None);
    }

    #[test]
    fn the_download_size_is_read_in_pacmans_units() {
        assert_eq!(
            download_size("Total Download Size:   1724.66 MiB"),
            Some((1724.66 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(
            download_size("Total Download Size:   0.01 MiB"),
            Some(10485)
        );
        assert_eq!(
            download_size("Total Download Size:   2.00 GiB"),
            Some(2 << 30)
        );
        assert_eq!(download_size("Total Installed Size:  6273.66 MiB"), None);
    }

    #[test]
    fn the_console_reports_phases_and_size_from_split_output() {
        let (tx, rx) = channel();
        let mut c = GuiConsole::new(tx);
        c.output("Total Download Size:   1.00 Mi");
        c.output("B\n\n:: Retrieving pack");
        c.output("ages...\n");
        c.finished(std::process::Command::new("true").status().unwrap());
        let got: Vec<String> = rx
            .try_iter()
            .filter_map(|m| match m {
                Msg::Phase(p) => Some(format!("{p:?}")),
                Msg::DownloadSize(b) => Some(b.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(got, ["1048576", "Downloading", "Finishing"]);
    }

    #[test]
    fn downloads_are_counted_in_scratch_dirs_and_since_they_began() {
        let cache = std::env::temp_dir().join(format!("deploytix-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        std::fs::create_dir_all(cache.join("download-abc")).unwrap();
        std::fs::write(cache.join("old.pkg.tar.zst"), [0u8; 100]).unwrap();
        let since = SystemTime::now() + std::time::Duration::from_secs(1);
        // Files from before the download began are someone else's.
        assert_eq!(downloaded_since(&cache, since), 0);
        std::fs::write(cache.join("download-abc/a.part"), [0u8; 30]).unwrap();
        assert_eq!(downloaded_since(&cache, since), 30);
        let earlier = SystemTime::now() - std::time::Duration::from_secs(60);
        assert_eq!(downloaded_since(&cache, earlier), 130);
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn output_is_split_into_lines_across_chunks() {
        let mut b = ConsoleBuffer::default();
        b.push("resolving dep");
        b.push("endencies...\nlooking for ");
        assert_eq!(b.row_count(), 2);
        assert_eq!(b.row(0), "resolving dependencies...");
        assert_eq!(b.row(1), "looking for ");
        b.push("conflicting packages...\n");
        assert_eq!(b.row_count(), 2);
        assert_eq!(b.row(1), "looking for conflicting packages...");
    }

    #[test]
    fn the_console_keeps_only_the_newest_lines() {
        let mut b = ConsoleBuffer::default();
        for i in 0..CONSOLE_LINES + 10 {
            b.push(&format!("line {i}\n"));
        }
        assert_eq!(b.row_count(), CONSOLE_LINES);
        assert_eq!(b.row(0), "line 10");
    }

    #[test]
    fn a_question_blocks_until_the_ui_answers() {
        let (tx, rx) = channel();
        let worker = std::thread::spawn(move || GuiConsole::new(tx).ask(&conflict()));
        let Msg::Question(pending) = rx.recv().unwrap() else {
            panic!("expected a question");
        };
        assert_eq!(pending.input, "n");
        assert_eq!(pending.default_is_yes(), Some(false));
        pending.answer(Answer::Reply("y".into()));
        assert_eq!(worker.join().unwrap(), Answer::Reply("y".into()));
    }

    #[test]
    fn a_question_the_ui_drops_is_cancelled_not_answered() {
        let (tx, rx) = channel();
        let worker = std::thread::spawn(move || GuiConsole::new(tx).ask(&conflict()));
        drop(rx.recv().unwrap());
        assert_eq!(worker.join().unwrap(), Answer::Cancel);
    }

    #[test]
    fn a_question_with_no_ui_left_is_cancelled() {
        let (tx, rx) = channel();
        drop(rx);
        assert_eq!(GuiConsole::new(tx).ask(&conflict()), Answer::Cancel);
    }

    #[test]
    fn a_typed_answer_is_submitted_trimmed() {
        let (tx, rx) = channel();
        let q = Question::parse("Enter a number (default=1): ").unwrap();
        let worker = std::thread::spawn(move || GuiConsole::new(tx).ask(&q));
        let Msg::Question(mut pending) = rx.recv().unwrap() else {
            panic!("expected a question");
        };
        assert_eq!(pending.input, "1");
        pending.input = " 2 ".into();
        pending.submit();
        assert_eq!(worker.join().unwrap(), Answer::Reply("2".into()));
    }
}
