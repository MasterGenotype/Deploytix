//! Interactive pacman driver, used by `deploytix update`.
//!
//! A transaction used to run `pacman --noconfirm` with stdin closed, so every
//! question pacman asked took its default answer. Most defaults are what a
//! transaction wants, but not all: when an upgrade conflicts with an installed
//! package ("plasma-workspace-6.8.1 and plasma-x11-session-6.7.5 are in
//! conflict. Remove plasma-x11-session? [y/N]") the default is *no*, and the
//! whole transaction fails as "unresolvable package conflicts" with no way for
//! the user to say yes.
//!
//! [`drive`] runs pacman with its stdin and (merged) output on pipes, streams
//! the output to a [`Console`], and hands each question pacman asks to the
//! console to answer. The one exception is `Proceed with installation?`: the
//! user asked for this transaction by starting it, so that is answered here.
//!
//! Questions are recognised by how pacman ends them — `[Y/n] `, `[y/N] `,
//! `Enter a number (default=N): `, `Enter a selection (default=all): ` — with
//! no trailing newline. pacman flushes a question and then blocks reading
//! stdin, so a chunk that ends in one of these is a question pacman is
//! waiting on. The strings are only stable untranslated, so the driver forces
//! `LC_ALL=C`.
//!
//! pacman treats end of input as "no" on every yes/no question, which is what
//! [`Answer::Cancel`] relies on: it closes stdin rather than killing pacman,
//! so pacman declines and unwinds on its own — before a commit, since every
//! question it asks precedes one.

use crate::utils::error::{DeploytixError, Result};
use std::io::{Read, Write};
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The answer format pacman expects for a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionKind {
    /// `[Y/n]` (`default_yes`) or `[y/N]`.
    YesNo { default_yes: bool },
    /// `Enter a number (default=N):` — choosing one provider of several.
    Number { default: u32 },
    /// `Enter a selection (default=all):` — choosing members of a group.
    Selection,
}

/// A question pacman is waiting on an answer to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The question line without pacman's `:: ` prefix or its answer hint,
    /// e.g. `a-2-1 and b-1-1 are in conflict. Remove b?`.
    pub text: String,
    pub kind: QuestionKind,
}

impl Question {
    /// Recognise the question an unterminated output line ends in, if any.
    pub fn parse(line: &str) -> Option<Self> {
        let (text, kind) = if let Some(t) = line.strip_suffix(" [Y/n] ") {
            (t, QuestionKind::YesNo { default_yes: true })
        } else if let Some(t) = line.strip_suffix(" [y/N] ") {
            (t, QuestionKind::YesNo { default_yes: false })
        } else {
            let t = line.strip_suffix(": ")?;
            if t == "Enter a selection (default=all)" {
                (t, QuestionKind::Selection)
            } else {
                let n = t
                    .strip_prefix("Enter a number (default=")?
                    .strip_suffix(')')?
                    .parse()
                    .ok()?;
                (t, QuestionKind::Number { default: n })
            }
        };
        let text = text.trim_start_matches(":: ").trim().to_string();
        Some(Self { text, kind })
    }

    /// What pacman would have picked under `--noconfirm`, spelled out so the
    /// answer is visible in the transcript.
    pub fn default_answer(&self) -> String {
        match self.kind {
            QuestionKind::YesNo { default_yes: true } => "y".into(),
            QuestionKind::YesNo { default_yes: false } => "n".into(),
            QuestionKind::Number { default } => default.to_string(),
            // pacman has no word for "all"; an empty line selects it.
            QuestionKind::Selection => String::new(),
        }
    }

    /// The final go-ahead pacman asks for once the transaction is resolved.
    pub fn is_proceed(&self) -> bool {
        self.text == "Proceed with installation?"
    }
}

/// A console's reply to a [`Question`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Send this line to pacman.
    Reply(String),
    /// Abandon the transaction: pacman sees end of input and declines.
    Cancel,
}

/// Where a driven pacman's output goes and where its questions are answered:
/// the update GUI's console pane, or the terminal.
pub trait Console {
    /// pacman wrote `text`. Chunks are not line-aligned.
    fn output(&mut self, text: &str);
    /// pacman is blocked on `question`. Its text has already been passed to
    /// [`Console::output`].
    fn ask(&mut self, question: &Question) -> Answer;
    /// pacman has exited and all its output has been passed on.
    fn finished(&mut self, _status: ExitStatus) {}
}

/// A console shared with the [`CommandRunner`](crate::utils::command::CommandRunner)
/// that drives pacman, typically from a worker thread.
pub type ConsoleHandle = Arc<Mutex<dyn Console + Send>>;

/// The console used when none is attached: discards output and answers every
/// question as `--noconfirm` would, so an unattended run behaves as it always
/// has — but leaves the questions it declined in the transcript, where an
/// error can show them.
pub struct Unattended;

impl Console for Unattended {
    fn output(&mut self, _text: &str) {}
    fn ask(&mut self, question: &Question) -> Answer {
        Answer::Reply(question.default_answer())
    }
}

/// The CLI's console: pacman's output on stdout as it is written, and its
/// questions put to whoever is at the keyboard.
///
/// Without a keyboard — stdin piped or redirected, as in a script — it still
/// streams the output but answers every question as [`Unattended`] does, so a
/// scripted update never blocks on input that cannot come.
pub struct Terminal {
    interactive: bool,
    /// The answer pacman has yet to echo back. It was already shown as it was
    /// typed, so its echo is swallowed rather than printed a second time.
    echo: String,
    /// The line pacman is part-way through.
    line: String,
}

/// Printed under pacman's download header. pacman draws no progress bars when
/// its output is not a terminal — it is a pipe here, even when deploytix's is
/// one — so a large download is otherwise one line and minutes of nothing.
const DOWNLOAD_NOTE: &str =
    "   (pacman shows no download progress here; a large download takes a while)\n";

impl Terminal {
    /// Ask questions only when stdin is a terminal.
    pub fn for_stdin() -> Self {
        use std::io::IsTerminal;
        Self::new(std::io::stdin().is_terminal())
    }

    pub fn new(interactive: bool) -> Self {
        Self {
            interactive,
            echo: String::new(),
            line: String::new(),
        }
    }
}

impl Console for Terminal {
    fn output(&mut self, text: &str) {
        let text = take_echo(&mut self.echo, text);
        let shown = annotate(&mut self.line, text);
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(shown.as_bytes()).and_then(|_| out.flush());
    }

    fn ask(&mut self, question: &Question) -> Answer {
        if !self.interactive {
            return Answer::Reply(question.default_answer());
        }
        match read_answer() {
            Some(line) => {
                let reply = line.trim().to_string();
                // pacman echoes a non-empty answer read from a pipe, and the
                // driver stands in for the echo of an empty one: either way a
                // line the terminal has already shown.
                self.echo = format!("{reply}\n");
                Answer::Reply(reply)
            }
            None => Answer::Cancel,
        }
    }
}

/// `text` with [`DOWNLOAD_NOTE`] after pacman's download header. `line`
/// carries the unfinished line between chunks.
fn annotate(line: &mut String, text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for (i, part) in text.split('\n').enumerate() {
        if i > 0 {
            shown.push('\n');
            if std::mem::take(line) == ":: Retrieving packages..." {
                shown.push_str(DOWNLOAD_NOTE);
            }
        }
        shown.push_str(part);
        line.push_str(part);
    }
    shown
}

/// Strip from the front of `text` as much of the pending `echo` as it starts
/// with. An echo split across chunks is consumed piecewise; output that does
/// not match it drops the expectation and is shown whole.
fn take_echo<'a>(echo: &mut String, text: &'a str) -> &'a str {
    if echo.is_empty() {
        return text;
    }
    if let Some(rest) = text.strip_prefix(echo.as_str()) {
        echo.clear();
        rest
    } else if echo.starts_with(text) {
        echo.drain(..text.len());
        ""
    } else {
        echo.clear();
        text
    }
}

/// Read one line of input, or `None` on end of input (Ctrl-D) or an interrupt.
///
/// The first Ctrl-C only raises [`crate::utils::signal`]'s flag, and a read
/// restarted after the signal would sit waiting for Enter, so stdin is polled
/// and the flag checked between polls.
fn read_answer() -> Option<String> {
    loop {
        if crate::utils::signal::is_interrupted() {
            return None;
        }
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, and the count passed says so.
        let ready = unsafe { libc::poll(&mut fd, 1, 200) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        if ready > 0 {
            let mut line = String::new();
            return match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(line),
            };
        }
    }
}

/// Everything a driven run produced.
#[derive(Debug)]
pub struct ConsoleRun {
    /// pacman's merged stdout and stderr, answers included (pacman echoes an
    /// answer it read from a pipe).
    pub transcript: String,
    /// Each question asked, with the answer sent; `None` if it was cancelled.
    pub exchanges: Vec<(Question, Option<String>)>,
    pub status: ExitStatus,
    pub cancelled: bool,
}

impl ConsoleRun {
    /// Turn a cancelled or failed run into the matching error.
    pub fn into_result(self, command: &str) -> Result<Self> {
        if self.cancelled {
            return Err(DeploytixError::UserCancelled);
        }
        if !self.status.success() {
            return Err(DeploytixError::CommandFailed {
                command: command.to_string(),
                stderr: failure_summary(&self.transcript),
            });
        }
        Ok(self)
    }
}

/// How long to wait for output that a lingering grandchild might still be
/// holding the pipe open for, once the direct child has exited.
const DRAIN: Duration = Duration::from_millis(200);

/// The longest the driver keeps reading after the child has exited, however
/// steadily a grandchild keeps writing.
const DRAIN_LIMIT: Duration = Duration::from_secs(2);

/// Run `cmd` (pacman, or the chroot wrapper around it), feeding its questions
/// to `console`. Returns once the child exits, whatever its status — see
/// [`ConsoleRun::into_result`].
pub fn drive(mut cmd: Command, console: &mut dyn Console) -> Result<ConsoleRun> {
    let (reader, writer) = std::io::pipe()?;
    cmd.env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    let mut child = cmd.spawn()?;
    // The Command holds the parent's copies of the pipe's write end; while
    // they are open the reader never sees end of file.
    drop(cmd);
    let mut stdin = child.stdin.take();

    // Reading on its own thread lets the loop below notice the child exiting
    // even if something it spawned (a hook's daemon) keeps the pipe open.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut decoder = Utf8Decoder::default();
    let mut transcript = String::new();
    let mut exchanges = Vec::new();
    let mut cancelled = false;
    let mut exited: Option<ExitStatus> = None;
    // Where the text a question can be read from starts: after the last
    // newline, or after the last answer, whichever is later. pacman echoes
    // only a non-empty answer, so a question that follows one answered with
    // an empty line (a selection's "all") continues the same line.
    let mut line_start = 0;
    let mut drain_until = Instant::now();

    loop {
        let wait = if exited.is_some() {
            DRAIN
        } else {
            Duration::from_millis(100)
        };
        match rx.recv_timeout(wait) {
            Ok(bytes) => {
                let text = decoder.push(&bytes);
                if text.is_empty() {
                    continue;
                }
                console.output(&text);
                transcript.push_str(&text);
                let from = transcript.rfind('\n').map_or(0, |i| i + 1).max(line_start);
                if let Some(question) = Question::parse(&transcript[from..]) {
                    let answer = if cancelled {
                        Answer::Cancel
                    } else if question.is_proceed() {
                        Answer::Reply("y".into())
                    } else {
                        console.ask(&question)
                    };
                    match answer {
                        Answer::Reply(reply) => {
                            send(&mut stdin, &reply);
                            if reply.trim().is_empty() {
                                // Stand in for the echo pacman leaves out.
                                console.output("\n");
                                transcript.push('\n');
                            }
                            exchanges.push((question, Some(reply)));
                        }
                        Answer::Cancel => {
                            cancelled = true;
                            stdin = None;
                            exchanges.push((question, None));
                        }
                    }
                    line_start = transcript.len();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if exited.is_some() => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if crate::utils::signal::is_interrupted() && !cancelled {
            cancelled = true;
            stdin = None;
        }
        match exited {
            None => {
                exited = child.try_wait()?;
                drain_until = Instant::now() + DRAIN_LIMIT;
            }
            Some(_) if Instant::now() >= drain_until => break,
            Some(_) => {}
        }
    }

    let status = match exited {
        Some(status) => status,
        None => child.wait()?,
    };
    let rest = decoder.finish();
    if !rest.is_empty() {
        console.output(&rest);
        transcript.push_str(&rest);
    }
    console.finished(status);
    Ok(ConsoleRun {
        transcript,
        exchanges,
        status,
        cancelled,
    })
}

/// Write one answer line. A child that has already gone away is reported by
/// its exit status, not here.
fn send(stdin: &mut Option<ChildStdin>, reply: &str) {
    if let Some(pipe) = stdin {
        let _ = writeln!(pipe, "{reply}").and_then(|_| pipe.flush());
    }
}

/// The part of a failed run's transcript that says why it failed: each
/// question pacman asked with the answer it got, then everything from pacman's
/// first `error:` on, which is the error and the detail pacman prints under it.
/// A transcript without an `error:` line falls back to its tail.
///
/// Everything else — the warnings, the package list, the size totals — is in
/// the console and the history record, and would bury the reason here.
fn failure_summary(transcript: &str) -> String {
    let lines: Vec<&str> = transcript.lines().collect();
    let Some(first_error) = lines.iter().position(|l| l.starts_with("error:")) else {
        return tail(transcript, 40);
    };
    let answered = lines[..first_error]
        .iter()
        .filter(|l| l.contains(" [y/N] ") || l.contains(" [Y/n] "));
    answered
        .chain(&lines[first_error..])
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last `n` lines of `text`.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Decodes a byte stream as UTF-8 across chunk boundaries, holding back a
/// character split between two reads and replacing invalid bytes.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    return out;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.pending[..valid]).unwrap_or_default());
                    match e.error_len() {
                        // An incomplete character at the end: wait for the rest.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                        Some(bad) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            self.pending.drain(..valid + bad);
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> String {
        let out = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers from a script and records what it saw.
    #[derive(Default)]
    struct Scripted {
        answers: Vec<Answer>,
        asked: Vec<Question>,
        output: String,
    }

    impl Console for Scripted {
        fn output(&mut self, text: &str) {
            self.output.push_str(text);
        }
        fn ask(&mut self, question: &Question) -> Answer {
            self.asked.push(question.clone());
            if self.answers.is_empty() {
                Answer::Cancel
            } else {
                self.answers.remove(0)
            }
        }
    }

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[test]
    fn the_download_header_gets_its_note_even_when_split() {
        let mut line = String::new();
        let first = annotate(
            &mut line,
            ":: Proceed with installation? [Y/n] y\n:: Retrieving pack",
        );
        let second = annotate(&mut line, "ages...\n foo downloading...\n");
        assert_eq!(
            format!("{first}{second}"),
            format!(
                ":: Proceed with installation? [Y/n] y\n:: Retrieving packages...\n{DOWNLOAD_NOTE} foo downloading...\n"
            )
        );
    }

    #[test]
    fn a_typed_answer_is_not_shown_twice() {
        let mut echo = "y\n".to_string();
        assert_eq!(
            take_echo(&mut echo, "y\nresolving dependencies...\n"),
            "resolving dependencies...\n"
        );
        assert!(echo.is_empty());
        // Nothing pending: output passes through untouched.
        assert_eq!(take_echo(&mut echo, "y\n"), "y\n");
    }

    #[test]
    fn an_echo_split_across_chunks_is_consumed_piecewise() {
        let mut echo = "12\n".to_string();
        assert_eq!(take_echo(&mut echo, "1"), "");
        assert_eq!(take_echo(&mut echo, "2\nnext"), "next");
        assert!(echo.is_empty());
    }

    #[test]
    fn output_that_is_not_the_echo_is_shown_whole() {
        let mut echo = "y\n".to_string();
        assert_eq!(take_echo(&mut echo, "error: x\n"), "error: x\n");
        assert!(echo.is_empty());
    }

    #[test]
    fn a_terminal_without_a_keyboard_answers_as_noconfirm_would() {
        let mut t = Terminal::new(false);
        let q = Question::parse(":: a and b are in conflict. Remove b? [y/N] ").unwrap();
        assert_eq!(t.ask(&q), Answer::Reply("n".into()));
    }

    #[test]
    fn a_conflict_question_is_recognised() {
        let q = Question::parse(
            ":: plasma-workspace-6.8.1-1.1 and plasma-x11-session-6.7.5-3 are in conflict. \
             Remove plasma-x11-session? [y/N] ",
        )
        .unwrap();
        assert_eq!(q.kind, QuestionKind::YesNo { default_yes: false });
        assert!(q.text.starts_with("plasma-workspace-6.8.1-1.1 and"));
        assert!(q.text.ends_with("Remove plasma-x11-session?"));
        assert_eq!(q.default_answer(), "n");
    }

    #[test]
    fn proceed_is_recognised_as_the_go_ahead() {
        let q = Question::parse(":: Proceed with installation? [Y/n] ").unwrap();
        assert!(q.is_proceed());
        assert_eq!(q.kind, QuestionKind::YesNo { default_yes: true });
        assert!(!Question::parse(":: Replace foo with extra/bar? [Y/n] ")
            .unwrap()
            .is_proceed());
    }

    #[test]
    fn provider_and_group_questions_are_recognised() {
        let n = Question::parse("Enter a number (default=1): ").unwrap();
        assert_eq!(n.kind, QuestionKind::Number { default: 1 });
        assert_eq!(n.default_answer(), "1");
        let s = Question::parse("Enter a selection (default=all): ").unwrap();
        assert_eq!(s.kind, QuestionKind::Selection);
        assert_eq!(s.default_answer(), "");
    }

    #[test]
    fn output_that_only_resembles_a_question_is_not_one() {
        // Not yet complete: pacman has not finished writing the hint.
        assert!(Question::parse(":: Proceed with installation? [Y/").is_none());
        assert!(Question::parse(":: Proceed with installation? [Y/n]").is_none());
        // Ordinary progress output that happens to end in ": ".
        assert!(Question::parse("checking keyring: ").is_none());
        assert!(Question::parse("Enter a number (default=x): ").is_none());
    }

    #[test]
    fn a_question_on_stderr_is_answered_by_the_console() {
        let mut console = Scripted {
            answers: vec![Answer::Reply("y".into())],
            ..Default::default()
        };
        let run = drive(
            sh(
                "printf ':: a-2-1 and b-1-1 are in conflict. Remove b? [y/N] ' >&2; \
                read a; echo \"got:$a\"",
            ),
            &mut console,
        )
        .unwrap();
        assert!(run.status.success());
        assert_eq!(console.asked.len(), 1);
        assert_eq!(
            console.asked[0].text,
            "a-2-1 and b-1-1 are in conflict. Remove b?"
        );
        assert!(run.transcript.contains("got:y"), "{}", run.transcript);
        assert_eq!(run.exchanges[0].1.as_deref(), Some("y"));
    }

    #[test]
    fn proceed_is_answered_without_asking() {
        let mut console = Scripted::default();
        let run = drive(
            sh("printf ':: Proceed with installation? [Y/n] '; read a; echo \"proceed:$a\""),
            &mut console,
        )
        .unwrap();
        assert!(console.asked.is_empty());
        assert!(run.transcript.contains("proceed:y"));
        assert!(!run.cancelled);
    }

    #[test]
    fn questions_are_answered_in_order() {
        let mut console = Scripted {
            answers: vec![Answer::Reply("y".into()), Answer::Reply("2".into())],
            ..Default::default()
        };
        let run = drive(
            sh("printf ':: Replace a with extra/b? [Y/n] '; read r; \
                printf '\\nEnter a number (default=1): '; read n; \
                printf ':: Proceed with installation? [Y/n] '; read p; \
                echo \"$r $n $p\""),
            &mut console,
        )
        .unwrap();
        assert_eq!(console.asked.len(), 2);
        assert!(run.transcript.contains("y 2 y"), "{}", run.transcript);
        assert_eq!(console.output, run.transcript);
    }

    #[test]
    fn a_question_after_an_unechoed_empty_answer_is_still_recognised() {
        // pacman echoes only a non-empty answer, so after "all" (an empty
        // line) the next question would otherwise share the previous line.
        let mut console = Scripted {
            answers: vec![Answer::Reply(String::new()), Answer::Reply("y".into())],
            ..Default::default()
        };
        let run = drive(
            sh("printf 'Enter a selection (default=all): '; read s; \
                printf ':: Remove b? [y/N] '; read r; echo \"[$s] $r\""),
            &mut console,
        )
        .unwrap();
        assert_eq!(console.asked.len(), 2);
        assert_eq!(console.asked[1].text, "Remove b?");
        assert!(run
            .transcript
            .contains("Enter a selection (default=all): \n:: Remove b?"));
        assert!(run.transcript.contains("[] y"), "{}", run.transcript);
    }

    #[test]
    fn a_grandchild_writing_after_exit_does_not_stall_the_driver() {
        let start = Instant::now();
        let run = drive(
            sh(
                "(i=0; while [ $i -lt 120 ]; do echo spam; i=$((i+1)); sleep 0.05; done) & \
                echo done",
            ),
            &mut Scripted::default(),
        )
        .unwrap();
        assert!(run.status.success());
        assert!(start.elapsed() < Duration::from_millis(3500));
    }

    #[test]
    fn cancel_closes_stdin_and_reports_cancelled() {
        let mut console = Scripted {
            answers: vec![Answer::Cancel],
            ..Default::default()
        };
        let run = drive(
            sh("printf ':: x and y are in conflict. Remove y? [y/N] '; \
                if read a; then echo answered; else echo eof; fi"),
            &mut console,
        )
        .unwrap();
        assert!(run.cancelled);
        assert!(run.transcript.contains("eof"), "{}", run.transcript);
        assert_eq!(run.exchanges[0].1, None);
        assert!(matches!(
            run.into_result("pacman"),
            Err(DeploytixError::UserCancelled)
        ));
    }

    #[test]
    fn a_failed_run_carries_the_transcript_tail() {
        let mut console = Scripted::default();
        let run = drive(
            sh("echo 'error: unresolvable package conflicts detected' >&2; exit 1"),
            &mut console,
        )
        .unwrap();
        match run.into_result("pacman -Syu") {
            Err(DeploytixError::CommandFailed { command, stderr }) => {
                assert_eq!(command, "pacman -Syu");
                assert!(stderr.contains("unresolvable package conflicts"));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_is_summarised_by_its_question_and_errors() {
        // The real transcript of the Plasma 6.8 update, declined.
        let transcript = "\
:: Starting full system upgrade...
warning: tkg-gui-git: local (r52.g9bbac7e-1) is newer than galaxy (r39-1)
resolving dependencies...
looking for conflicting packages...
:: plasma-workspace-6.8.1-1.1 and plasma-x11-session-6.7.5-3 are in conflict. Remove plasma-x11-session? [y/N] n
error: unresolvable package conflicts detected
error: failed to prepare transaction (conflicting dependencies)
:: plasma-workspace-6.8.1-1.1 and plasma-x11-session-6.7.5-3 are in conflict
";
        assert_eq!(
            failure_summary(transcript),
            "\
:: plasma-workspace-6.8.1-1.1 and plasma-x11-session-6.7.5-3 are in conflict. Remove plasma-x11-session? [y/N] n
error: unresolvable package conflicts detected
error: failed to prepare transaction (conflicting dependencies)
:: plasma-workspace-6.8.1-1.1 and plasma-x11-session-6.7.5-3 are in conflict"
        );
    }

    #[test]
    fn a_failure_without_an_error_line_keeps_the_tail() {
        let transcript: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let summary = failure_summary(&transcript);
        assert!(summary.starts_with("line 11\n") && summary.ends_with("line 50"));
    }

    #[test]
    fn a_grandchild_holding_the_pipe_does_not_stall_the_driver() {
        let start = Instant::now();
        let run = drive(sh("sleep 5 & echo done"), &mut Scripted::default()).unwrap();
        assert!(run.status.success());
        assert!(run.transcript.contains("done"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn the_driver_runs_pacman_untranslated() {
        let run = drive(sh("echo \"$LC_ALL\""), &mut Scripted::default()).unwrap();
        assert_eq!(run.transcript.trim(), "C");
    }

    #[test]
    fn a_character_split_across_reads_is_decoded_whole() {
        let mut d = Utf8Decoder::default();
        let bytes = "─x".as_bytes();
        assert_eq!(d.push(&bytes[..1]), "");
        assert_eq!(d.push(&bytes[1..]), "─x");
        assert_eq!(d.push(&[0xff, b'a']), "\u{fffd}a");
        assert_eq!(d.push(&[0xe2]), "");
        assert_eq!(d.finish(), "\u{fffd}");
    }
}
