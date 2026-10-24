//! The background add, audit or update: its thread, the questions it puts to
//! the dialogs, and its pane.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};

use super::dialog::{Ask, Dialog};
use super::draw::status_line;
use super::{App, Data};
use crate::audit;
use crate::pick::Picker;
use crate::store::{self, Outcome};
use crate::theme;

/// The most lines the job pane shows: its last ones.
pub(super) const PANE_LINES: usize = 6;

/// How long quitting waits for a cancelled job: longer than a judge has to
/// exit on SIGTERM before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// The frames of a running job's spinner, one per 100 ms.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A question an add or update job puts to the main loop, which answers it
/// in a dialog. A dialog closed without a reply is No, or nothing picked.
enum Question {
    Pick(Vec<(String, String)>, mpsc::Sender<Vec<String>>),
    YesNo { lines: Vec<String>, prompt: String, default: bool, reply: mpsc::Sender<bool> },
}

/// A job's [`store::Io`]: its pane, and the dialogs its questions open.
struct Dialogs<'a> {
    pane: &'a audit::Pane,
    asks: mpsc::Sender<Question>,
}

impl store::Io for Dialogs<'_> {
    fn pane(&self) -> Option<&audit::Pane> {
        Some(self.pane)
    }

    fn line(&self, line: &str) {
        _ = self.pane.lines.send(line.into());
    }

    fn error(&self, line: &str) {
        self.line(line);
    }

    fn cancelled(&self) -> bool {
        self.pane.cancel.load(Ordering::Relaxed)
    }

    fn can_ask(&self) -> bool {
        true
    }

    /// The picker can select several without `--all`.
    fn confirms_calls(&self) -> bool {
        true
    }

    fn yes_no(&self, lines: Vec<String>, prompt: String, default: bool) -> crate::Result<bool> {
        let (reply, answer) = mpsc::channel();
        _ = self.asks.send(Question::YesNo { lines, prompt, default, reply });
        Ok(answer.recv().unwrap_or(false))
    }

    fn pick(&self, items: Vec<(String, String)>) -> crate::Result<Option<Vec<String>>> {
        let (reply, picked) = mpsc::channel();
        _ = self.asks.send(Question::Pick(items, reply));
        Ok(Some(picked.recv().unwrap_or_default()))
    }
}

/// A background add, audit or update and what its pane shows.
pub(super) struct Job {
    /// `add`, `audit` or `update`.
    pub(super) verb: &'static str,
    /// The skill, every store skill, or the add source.
    pub(super) target: String,
    /// What the run reported, then what its CLI command prints.
    pub(super) lines: Vec<String>,
    rx: mpsc::Receiver<String>,
    /// What an add or update asks, each opened as a dialog once none is open.
    asks: Option<mpsc::Receiver<Question>>,
    cancel: Arc<AtomicBool>,
    /// The thread until it ends; it returns whether the run succeeded.
    pub(super) thread: Option<JoinHandle<bool>>,
    ok: bool,
    /// When it started: its spinner's frame and its running time.
    started: Instant,
}

impl App {
    /// Adds from `source` in the background as `mazo add <source>` does, its
    /// picker and y/N questions answered in dialogs, or shows the refusal
    /// `add` gives before reading the source. There is no `--force`.
    pub(super) fn add(&mut self, source: String) {
        if let Err(e) = store::precheck(&self.data.cfg, &source) {
            self.dialog = Some(Dialog { lines: vec![format!("ERROR: {e}")], ask: None });
            return;
        }
        let (tx, rx) = mpsc::channel();
        let target = source.clone();
        self.start("add", target, Some(rx), move |data, pane| {
            let args = store::Add {
                source: &source,
                rename: None,
                pick: None,
                all: false,
                trust: false,
                yes: false,
                force: false,
                project: false,
                io: &Dialogs { pane, asks: tx },
            };
            match store::add(&data.cfg, &data.inv, &data.dir, &args) {
                Ok(outcome) => (String::new(), outcome == Outcome::Done),
                Err(e) => (format!("ERROR: {e}"), false),
            }
        });
    }

    /// Runs `job` on a thread over a copy of the data, as the `verb` of
    /// `target`: the lines it reports, then the text it returns, go to the
    /// job pane, and what it `asks` to the dialogs.
    fn start(
        &mut self,
        verb: &'static str,
        target: String,
        asks: Option<mpsc::Receiver<Question>>,
        job: impl FnOnce(&Data, &audit::Pane) -> (String, bool) + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let pane = audit::Pane { lines: tx, cancel: cancel.clone() };
        let data = self.data.clone();
        let thread = std::thread::spawn(move || {
            let (out, ok) = caught(verb, || job(&data, &pane));
            for line in out.lines() {
                _ = pane.lines.send(line.into());
            }
            ok
        });
        let started = Instant::now();
        self.job =
            Some(Job { verb, target, lines: Vec::new(), rx, asks, cancel, thread: Some(thread), ok: false, started });
    }

    /// Audits `skill` in the background as `mazo audit <skill>` does.
    pub(super) fn run_audit(&mut self, skill: String) {
        let name = skill.clone();
        self.start("audit", skill, None, move |data, pane| {
            let args = audit::Args { limit: Some(audit::LIMIT), resynth: false, dry_run: false, json: false };
            let ran = audit::plan(&data.cfg, &data.inv, &name, None)
                .and_then(|p| audit::command(&p, &args, &data.input(), &data.dir, false, Some(pane)));
            ran.unwrap_or_else(|e| (format!("ERROR: {e}"), false))
        });
    }

    /// Updates `skill`, or every store skill, in the background as `mazo
    /// update [skill]` does, each re-vet's y/N answered in a dialog.
    pub(super) fn run_update(&mut self, skill: Option<String>) {
        let target = skill.clone().unwrap_or_else(|| "every store skill".into());
        let (tx, rx) = mpsc::channel();
        self.start("update", target, Some(rx), move |data, pane| {
            let names = Vec::from_iter(skill);
            let io = &Dialogs { pane, asks: tx };
            let args = store::Update { names: &names, yes: false, force: false, project: false, io };
            match store::update(&data.cfg, &data.inv, &data.dir, &args) {
                Ok(outcome) => (String::new(), outcome == Outcome::Done),
                Err(e) => (format!("ERROR: {e}"), false),
            }
        });
    }

    /// Takes the lines the job reported, and opens its next question when no
    /// dialog is open; once its thread ended, re-reads the data. Called
    /// before each draw.
    pub fn tick(&mut self) {
        let Some(job) = &mut self.job else { return };
        if self.dialog.is_none()
            && let Some(question) = job.asks.as_ref().and_then(|a| a.try_recv().ok())
        {
            self.dialog = Some(match question {
                Question::Pick(items, reply) => {
                    Dialog { lines: Vec::new(), ask: Some(Ask::Pick(Picker::new(items), reply)) }
                }
                Question::YesNo { mut lines, prompt, default, reply } => {
                    lines.push(prompt);
                    Dialog { lines, ask: Some(Ask::Answer(reply, default)) }
                }
            });
        }
        loop {
            match job.rx.try_recv() {
                Ok(line) => job.lines.push(line),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        let Some(thread) = job.thread.take() else { return };
        job.ok = thread.join().unwrap_or(false);
        self.clear = true;
        if let Err(e) = self.reload() {
            self.job.as_mut().expect("the job just ended").lines.push(format!("ERROR: {e}"));
        }
    }

    /// Cancels a running job, which kills its judge call in flight, and waits
    /// up to [`STOP_WAIT`] for its thread.
    pub(super) fn stop(&mut self) {
        let Some(job) = &mut self.job else { return };
        let Some(thread) = job.thread.take() else { return };
        job.cancel.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + STOP_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            // A question the job waits on is dropped unanswered: No.
            while job.asks.as_ref().is_some_and(|a| a.try_recv().is_ok()) {}
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            _ = thread.join();
        }
    }
}

/// The job pane: a spinner and the running time while the job runs, then a
/// check or a cross, in a box of the same colour, over its last lines.
pub(super) fn draw_job(frame: &mut Frame, job: &Job, area: Rect) {
    let elapsed = job.started.elapsed();
    let (icon, state, colour) = match (&job.thread, job.ok) {
        (Some(_), _) => {
            let spin = SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()];
            (spin, format!("running · {}s", elapsed.as_secs()), theme::ACCENT)
        }
        (None, true) => ("✓", "done · Esc closes".into(), theme::GOOD),
        (None, false) => ("✗", "failed · Esc closes".into(), theme::BAD),
    };
    let title = Line::from(vec![
        Span::raw(format!(" {icon}")).fg(colour).bold(),
        Span::raw(format!(" {} {} · {state} ", job.verb, job.target)).bold(),
    ]);
    let lines: Vec<Line> =
        job.lines.iter().skip(job.lines.len().saturating_sub(PANE_LINES)).map(|l| status_line(l)).collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(colour))
        .title(title)
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// What `job`, the `verb` run, returns, or a failure naming its panic: the
/// pane shows it and the screen stays up.
fn caught(verb: &str, job: impl FnOnce() -> (String, bool)) -> (String, bool) {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|panic| {
        let why = panic.downcast_ref::<&str>().copied().or_else(|| panic.downcast_ref::<String>().map(String::as_str));
        (format!("ERROR: the {verb} panicked: {}", why.unwrap_or("no message")), false)
    })
}

#[cfg(test)]
mod tests {
    use super::caught;

    #[test]
    fn a_job_that_panics_fails_with_its_message() {
        assert_eq!(caught("audit", || ("done".into(), true)), ("done".into(), true));
        assert_eq!(caught("audit", || panic!("boom")), ("ERROR: the audit panicked: boom".into(), false));
        let n = 2;
        assert_eq!(caught("update", || panic!("boom {n}")), ("ERROR: the update panicked: boom 2".into(), false));
    }
}
