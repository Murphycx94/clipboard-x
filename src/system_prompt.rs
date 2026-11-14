//! System prompts of ours (Touch ID) and the keyboard: a prompt takes it
//! from the window, which hides when it loses the keyboard. So while a
//! prompt is up the window stays; and as the prompt closes the system
//! hands the keyboard to the app in front, not back to the window, so for
//! a moment after the window takes it back rather than hide.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long after a prompt the window takes the keyboard back when it
/// loses it. The system moves the keyboard as the prompt's window closes,
/// some tens of milliseconds after the prompt answers, and nothing tells
/// when it is done.
pub(crate) const SETTLE: Duration = Duration::from_secs(1);

/// Where the prompts are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// None up, none just closed.
    Idle,
    /// One is up: it has the keyboard.
    Open,
    /// The last one just closed: the system is moving the keyboard.
    Settling,
}

/// The system prompts up now, and when the last one closed.
#[derive(Debug, Default)]
pub(crate) struct SystemPrompts {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    open: u32,
    settling_until: Option<Instant>,
}

impl SystemPrompts {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A prompt is about to show.
    pub(crate) fn begin(&self) {
        self.state().open += 1;
    }

    /// A prompt closed, at `now`.
    pub(crate) fn end(&self, now: Instant) {
        let mut state = self.state();
        state.open = state.open.saturating_sub(1);
        state.settling_until = Some(now + SETTLE);
    }

    pub(crate) fn phase(&self, now: Instant) -> Phase {
        let state = self.state();
        if state.open > 0 {
            Phase::Open
        } else if state.settling_until.is_some_and(|until| now < until) {
            Phase::Settling
        } else {
            Phase::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_is_open_until_it_ends_then_settles_for_a_moment() {
        let prompts = SystemPrompts::default();
        let start = Instant::now();
        assert_eq!(prompts.phase(start), Phase::Idle);

        prompts.begin();
        assert_eq!(prompts.phase(start), Phase::Open);
        // However long the user takes.
        assert_eq!(prompts.phase(start + SETTLE * 100), Phase::Open);

        let closed = start + Duration::from_secs(5);
        prompts.end(closed);
        assert_eq!(prompts.phase(closed), Phase::Settling);
        assert_eq!(
            prompts.phase(closed + SETTLE - Duration::from_millis(1)),
            Phase::Settling
        );
        assert_eq!(prompts.phase(closed + SETTLE), Phase::Idle);
    }

    #[test]
    fn a_prompt_after_another_is_open_again() {
        let prompts = SystemPrompts::default();
        let start = Instant::now();
        prompts.begin();
        prompts.end(start);
        prompts.begin();
        assert_eq!(prompts.phase(start), Phase::Open);
        prompts.end(start + SETTLE * 2);
        assert_eq!(prompts.phase(start + SETTLE * 2), Phase::Settling);
        assert_eq!(prompts.phase(start + SETTLE * 3), Phase::Idle);
    }

    #[test]
    fn ending_a_prompt_that_never_began_only_settles() {
        let prompts = SystemPrompts::default();
        let start = Instant::now();
        prompts.end(start);
        assert_eq!(prompts.phase(start), Phase::Settling);
        prompts.begin();
        assert_eq!(prompts.phase(start), Phase::Open);
    }
}
