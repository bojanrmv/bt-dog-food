//! What a command is doing while it works, on stderr: one line per step,
//! ending with what the step found and how long it took, and a clock on the
//! step under way. Only when stderr is a terminal, so reports, pipes, CI
//! logs and agents get nothing extra. `DGF_PROGRESS=0` turns it off;
//! `DGF_PROGRESS=1` also prints the finished steps when stderr is not a
//! terminal.

use std::io::{IsTerminal, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// How often the clock on the step under way moves.
const TICK: Duration = Duration::from_millis(250);
/// A step shows its clock only once it has taken this long.
const SLOW: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Off,
    /// Finished steps, one line each.
    Plain,
    /// Finished steps, and the step under way redrawn in place.
    Live,
}

struct State {
    mode: Mode,
    step: Option<(String, Instant)>,
    /// Characters on the live line now, so a shorter one covers it.
    drawn: usize,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn mode(setting: Option<&str>, terminal: bool) -> Mode {
    match setting {
        Some("0") => Mode::Off,
        _ if terminal => Mode::Live,
        Some("1") => Mode::Plain,
        _ => Mode::Off,
    }
}

fn state() -> MutexGuard<'static, State> {
    let state = STATE.get_or_init(|| {
        let mode = mode(
            std::env::var("DGF_PROGRESS").ok().as_deref(),
            std::io::stderr().is_terminal(),
        );
        if mode == Mode::Live {
            std::thread::spawn(tick);
        }
        Mutex::new(State {
            mode,
            step: None,
            drawn: 0,
        })
    });
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts a step, ending the one under way.
pub fn step(what: impl Into<String>) {
    let mut state = state();
    state.end(None);
    if state.mode != Mode::Off {
        state.step = Some((what.into(), Instant::now()));
        state.draw();
    }
}

/// Ends the step under way with what it found.
pub fn done(found: impl AsRef<str>) {
    state().end(Some(found.as_ref()));
}

/// Ends the step under way: before a report or an error is printed.
pub fn finish() {
    state().end(None);
}

/// Prints a line of its own on stderr (a warning) without breaking the
/// step under way.
pub fn note(line: &str) {
    let mut state = state();
    state.clear();
    let _ = writeln!(std::io::stderr(), "{line}");
    state.draw();
}

fn tick() {
    loop {
        std::thread::sleep(TICK);
        state().draw();
    }
}

impl State {
    fn draw(&mut self) {
        if self.mode != Mode::Live {
            return;
        }
        let Some((what, started)) = &self.step else {
            return;
        };
        let elapsed = started.elapsed();
        let line = if elapsed < SLOW {
            format!("  {what}...")
        } else {
            format!("  {what}... {}", seconds(elapsed))
        };
        self.put(&line, "");
    }

    fn end(&mut self, found: Option<&str>) {
        let Some((what, started)) = self.step.take() else {
            return;
        };
        let line = finished(&what, found, started.elapsed());
        self.put(&line, "\n");
        self.drawn = 0;
    }

    fn clear(&mut self) {
        if self.drawn > 0 {
            self.put("", "");
            let _ = write!(std::io::stderr(), "\r");
            self.drawn = 0;
        }
    }

    /// Writes `line` over the live line, then `end`.
    fn put(&mut self, line: &str, end: &str) {
        let width = line.chars().count();
        let cover = " ".repeat(self.drawn.saturating_sub(width));
        let start = if self.mode == Mode::Live { "\r" } else { "" };
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{start}{line}{cover}{end}");
        let _ = err.flush();
        self.drawn = width;
    }
}

/// A finished step: what it did, what it found, and how long it took.
fn finished(what: &str, found: Option<&str>, took: Duration) -> String {
    match found {
        Some(found) => format!("  {what}: {found} ({})", seconds(took)),
        None => format!("  {what} ({})", seconds(took)),
    }
}

fn seconds(took: Duration) -> String {
    let secs = took.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1} s")
    } else if secs < 60.0 {
        format!("{secs:.0} s")
    } else {
        format!("{} min {} s", took.as_secs() / 60, took.as_secs() % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_on_a_terminal_unless_turned_off() {
        assert_eq!(mode(None, true), Mode::Live);
        assert_eq!(mode(Some("1"), true), Mode::Live);
        assert_eq!(mode(Some("0"), true), Mode::Off);
    }

    #[test]
    fn elsewhere_only_when_asked_for() {
        assert_eq!(mode(None, false), Mode::Off);
        assert_eq!(mode(Some("1"), false), Mode::Plain);
        assert_eq!(mode(Some("0"), false), Mode::Off);
    }

    #[test]
    fn a_finished_step_says_what_it_found_and_how_long_it_took() {
        let took = Duration::from_millis(2_340);
        assert_eq!(
            finished("finding copies", Some("213 groups in 1204 files"), took),
            "  finding copies: 213 groups in 1204 files (2.3 s)"
        );
        assert_eq!(
            finished("reading coverage", None, took),
            "  reading coverage (2.3 s)"
        );
    }

    #[test]
    fn times_read_at_a_glance() {
        assert_eq!(seconds(Duration::from_millis(420)), "0.4 s");
        assert_eq!(seconds(Duration::from_millis(23_400)), "23 s");
        assert_eq!(seconds(Duration::from_secs(125)), "2 min 5 s");
    }
}
