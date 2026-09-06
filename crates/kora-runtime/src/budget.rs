//! Token budgets.
//!
//! Budgets are denominated in tokens because tokens are what the runtime can
//! measure directly (DECISIONS.md). Money is a display layer applied later
//! from a pricing table; it never affects enforcement.
//!
//! Budgets nest, and a child may only tighten: entering a scope intersects its
//! limits with everything already in force. Exhaustion is a value, never an
//! exception, so partial work always survives.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kora_syntax::ast::BudgetSpec;

/// Which limit ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meter {
    Tokens,
    Calls,
    Steps,
    Seconds,
}

impl Meter {
    pub fn name(&self) -> &'static str {
        match self {
            Meter::Tokens => "tokens",
            Meter::Calls => "calls",
            Meter::Steps => "steps",
            Meter::Seconds => "seconds",
        }
    }
}

/// Shared counters for one budget scope.
///
/// Counters are atomic and the whole scope is shared behind an `Arc` so a
/// `parallel for` fans out over one pot: 500 concurrent agents racing for the
/// same limit stop collectively, with no coordination code in user programs.
#[derive(Debug)]
struct Scope {
    max_tokens: Option<u64>,
    max_calls: Option<u64>,
    max_steps: Option<u64>,
    /// When this scope's time runs out.
    ///
    /// Stored as the moment it expires rather than as a duration, so every
    /// worker in a `parallel for` is measured against one instant instead of
    /// each starting its own clock when the thread happens to begin.
    deadline: Option<Instant>,
    spent_tokens: AtomicU64,
    spent_calls: AtomicU64,
    spent_steps: AtomicU64,
    parent: Option<Arc<Scope>>,
}

impl Scope {
    /// Charge every enclosing scope, innermost first. Returns the meter that
    /// tripped, if any. Charges are applied even when a limit trips: the work
    /// was really done, and hiding it would let a loop spend forever.
    fn charge(&self, tokens: u64, calls: u64, steps: u64) -> Option<Meter> {
        let after_tokens = self.spent_tokens.fetch_add(tokens, Ordering::Relaxed) + tokens;
        let after_calls = self.spent_calls.fetch_add(calls, Ordering::Relaxed) + calls;
        let after_steps = self.spent_steps.fetch_add(steps, Ordering::Relaxed) + steps;

        let mut tripped = None;
        if self.max_tokens.is_some_and(|m| after_tokens > m) {
            tripped = Some(Meter::Tokens);
        } else if self.max_calls.is_some_and(|m| after_calls > m) {
            tripped = Some(Meter::Calls);
        } else if self.max_steps.is_some_and(|m| after_steps > m) {
            tripped = Some(Meter::Steps);
        }

        match &self.parent {
            Some(p) => p.charge(tokens, calls, steps).or(tripped),
            None => tripped,
        }
    }

    /// Would one more call fit? Checked before dispatch so an exhausted budget
    /// stops *before* spending, rather than after.
    fn would_exceed(&self) -> Option<Meter> {
        // Time first: a scope that has run out of it has run out whatever the
        // other meters say, and naming the clock is more useful than naming
        // a token count that merely happens to also be short.
        if self.deadline.is_some_and(|end| Instant::now() >= end) {
            return Some(Meter::Seconds);
        }
        if self
            .max_calls
            .is_some_and(|m| self.spent_calls.load(Ordering::Relaxed) >= m)
        {
            return Some(Meter::Calls);
        }
        if self
            .max_steps
            .is_some_and(|m| self.spent_steps.load(Ordering::Relaxed) >= m)
        {
            return Some(Meter::Steps);
        }
        if self
            .max_tokens
            .is_some_and(|m| self.spent_tokens.load(Ordering::Relaxed) >= m)
        {
            return Some(Meter::Tokens);
        }
        self.parent.as_ref().and_then(|p| p.would_exceed())
    }

    fn remaining_tokens(&self) -> Option<u64> {
        let own = self
            .max_tokens
            .map(|m| m.saturating_sub(self.spent_tokens.load(Ordering::Relaxed)));
        match (&self.parent, own) {
            (Some(p), Some(mine)) => Some(p.remaining_tokens().unwrap_or(u64::MAX).min(mine)),
            (Some(p), None) => p.remaining_tokens(),
            (None, own) => own,
        }
    }

    /// How long the tightest enclosing deadline leaves, if any scope set one.
    ///
    /// A scope whose deadline has already passed answers `Some(ZERO)` rather
    /// than `None`: "no time left" and "no limit" are opposite answers, and a
    /// transport handed the second when the first was true would run on.
    fn remaining_time(&self) -> Option<Duration> {
        let own = self
            .deadline
            .map(|end| end.saturating_duration_since(Instant::now()));
        match (&self.parent, own) {
            (Some(p), Some(mine)) => Some(p.remaining_time().unwrap_or(Duration::MAX).min(mine)),
            (Some(p), None) => p.remaining_time(),
            (None, own) => own,
        }
    }
}

/// A handle to the currently active budget scope.
#[derive(Debug, Clone)]
pub struct Budget {
    scope: Arc<Scope>,
}

impl Default for Budget {
    fn default() -> Self {
        Budget::unlimited()
    }
}

impl Budget {
    /// A root scope with no limits. Budgets are opt-in (DECISIONS.md): a
    /// program with no `budget:` line runs unbounded, by design.
    pub fn unlimited() -> Budget {
        Budget {
            scope: Arc::new(Scope {
                max_tokens: None,
                max_calls: None,
                max_steps: None,
                deadline: None,
                spent_tokens: AtomicU64::new(0),
                spent_calls: AtomicU64::new(0),
                spent_steps: AtomicU64::new(0),
                parent: None,
            }),
        }
    }

    /// Enter a nested scope. The child's limits apply on top of the parent's,
    /// so a child can only ever tighten the total.
    pub fn nested(&self, spec: &BudgetSpec) -> Budget {
        Budget {
            scope: Arc::new(Scope {
                max_tokens: spec.max_tokens,
                max_calls: spec.max_calls,
                max_steps: spec.max_steps,
                // The clock starts when the scope is entered, which is what
                // `max_seconds = 30` reads as to anyone writing it.
                deadline: spec
                    .max_seconds
                    .map(|s| Instant::now() + Duration::from_secs(s)),
                spent_tokens: AtomicU64::new(0),
                spent_calls: AtomicU64::new(0),
                spent_steps: AtomicU64::new(0),
                parent: Some(self.scope.clone()),
            }),
        }
    }

    /// Check before spending: is there room for another model call?
    pub fn check(&self) -> Option<Meter> {
        self.scope.would_exceed()
    }

    /// Record a completed model call.
    pub fn charge_call(&self, tokens_in: u64, tokens_out: u64) -> Option<Meter> {
        self.scope.charge(tokens_in + tokens_out, 1, 0)
    }

    /// Record one step of an agent's tool loop.
    pub fn charge_step(&self) -> Option<Meter> {
        self.scope.charge(0, 0, 1)
    }

    pub fn spent_tokens(&self) -> u64 {
        self.scope.spent_tokens.load(Ordering::Relaxed)
    }

    pub fn spent_calls(&self) -> u64 {
        self.scope.spent_calls.load(Ordering::Relaxed)
    }

    /// Tokens left in the tightest enclosing limit, if any is set.
    pub fn remaining_tokens(&self) -> Option<u64> {
        self.scope.remaining_tokens()
    }

    /// Time left in the tightest enclosing deadline, if any is set.
    ///
    /// This is what makes `max_seconds` bound a call that is *already in
    /// flight*: a transport is given the smaller of its own timeout and this,
    /// so a request already sent dies at the deadline instead of running on
    /// to a timeout the program never asked for. Without it the meter could
    /// only be checked between effects, which is a weaker promise than
    /// `max_seconds = 30` reads as.
    pub fn remaining_time(&self) -> Option<Duration> {
        self.scope.remaining_time()
    }

    /// A timeout that respects both `own` and whatever deadline is in force.
    ///
    /// Never returns zero: a transport handed a zero timeout usually reads it
    /// as "no timeout". A deadline that has already passed is caught by
    /// [`Budget::check`] before dispatch, so the one second floor here is the
    /// margin for a deadline that expires between that check and the send,
    /// not a way to overrun one.
    pub fn bounded_timeout(&self, own: Duration) -> Duration {
        match self.remaining_time() {
            Some(left) => own.min(left).max(Duration::from_secs(1)),
            None => own,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tokens: Option<u64>, calls: Option<u64>, steps: Option<u64>) -> BudgetSpec {
        BudgetSpec {
            max_tokens: tokens,
            max_calls: calls,
            max_steps: steps,
            max_seconds: None,
            span_line: 1,
        }
    }

    fn seconds(limit: u64) -> BudgetSpec {
        BudgetSpec {
            max_seconds: Some(limit),
            span_line: 1,
            ..Default::default()
        }
    }

    #[test]
    fn a_scope_with_no_deadline_leaves_a_timeout_alone() {
        let b = Budget::unlimited().nested(&spec(Some(10), None, None));
        assert_eq!(b.remaining_time(), None);
        assert_eq!(
            b.bounded_timeout(Duration::from_secs(600)),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn a_deadline_cuts_a_longer_timeout_down() {
        let b = Budget::unlimited().nested(&seconds(5));
        let left = b.remaining_time().expect("a deadline is in force");
        assert!(left <= Duration::from_secs(5) && left > Duration::from_secs(3));
        // The transport's own 600s ceiling is irrelevant once the program has
        // said the whole scope gets five seconds.
        assert!(b.bounded_timeout(Duration::from_secs(600)) <= Duration::from_secs(5));
    }

    #[test]
    fn a_timeout_shorter_than_the_deadline_wins() {
        let b = Budget::unlimited().nested(&seconds(300));
        assert_eq!(
            b.bounded_timeout(Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn the_tightest_enclosing_deadline_is_the_one_that_binds() {
        let outer = Budget::unlimited().nested(&seconds(600));
        let inner = outer.nested(&seconds(2));
        assert!(inner.bounded_timeout(Duration::from_secs(600)) <= Duration::from_secs(2));
        // A child cannot buy back time its parent does not have.
        let greedy = inner.nested(&seconds(600));
        assert!(greedy.bounded_timeout(Duration::from_secs(600)) <= Duration::from_secs(2));
    }

    /// "No time left" and "no limit" are opposite answers, and a transport
    /// handed the second when the first is true would run on regardless.
    #[test]
    fn an_expired_deadline_answers_zero_not_none() {
        let b = Budget::unlimited().nested(&seconds(0));
        assert_eq!(b.remaining_time(), Some(Duration::ZERO));
        // Never zero at the transport, which usually reads zero as "forever".
        assert_eq!(
            b.bounded_timeout(Duration::from_secs(600)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn unlimited_never_trips() {
        let b = Budget::unlimited();
        assert_eq!(b.check(), None);
        assert_eq!(b.charge_call(1_000_000, 1_000_000), None);
        assert_eq!(b.check(), None);
        assert_eq!(b.remaining_tokens(), None);
    }

    #[test]
    fn token_limit_trips_after_overspend() {
        let b = Budget::unlimited().nested(&spec(Some(100), None, None));
        assert_eq!(b.charge_call(40, 10), None);
        assert_eq!(b.spent_tokens(), 50);
        assert_eq!(b.remaining_tokens(), Some(50));
        assert_eq!(b.charge_call(40, 30), Some(Meter::Tokens));
    }

    #[test]
    fn check_blocks_once_exhausted() {
        let b = Budget::unlimited().nested(&spec(Some(10), None, None));
        assert_eq!(b.check(), None);
        b.charge_call(6, 6);
        assert_eq!(b.check(), Some(Meter::Tokens));
    }

    #[test]
    fn call_limit_is_independent_of_tokens() {
        let b = Budget::unlimited().nested(&spec(None, Some(2), None));
        assert_eq!(b.charge_call(5, 5), None);
        assert_eq!(b.charge_call(5, 5), None);
        assert_eq!(b.check(), Some(Meter::Calls));
    }

    #[test]
    fn step_limit_trips() {
        let b = Budget::unlimited().nested(&spec(None, None, Some(2)));
        assert_eq!(b.charge_step(), None);
        assert_eq!(b.charge_step(), None);
        assert_eq!(b.charge_step(), Some(Meter::Steps));
    }

    #[test]
    fn child_charges_flow_up_to_parent() {
        let parent = Budget::unlimited().nested(&spec(Some(100), None, None));
        let child = parent.nested(&spec(Some(1000), None, None));
        child.charge_call(60, 0);
        assert_eq!(parent.spent_tokens(), 60, "parent must see child spending");
        // The child's own limit is looser, but the parent's still binds.
        assert_eq!(child.charge_call(60, 0), Some(Meter::Tokens));
    }

    #[test]
    fn child_cannot_loosen_parent() {
        let parent = Budget::unlimited().nested(&spec(Some(50), None, None));
        let child = parent.nested(&spec(Some(1_000_000), None, None));
        assert_eq!(
            child.remaining_tokens(),
            Some(50),
            "the tightest limit in the chain wins"
        );
    }

    #[test]
    fn shared_pot_across_threads() {
        // Mirrors `parallel for`: many workers drawing on one budget.
        let budget = Budget::unlimited().nested(&spec(Some(1000), None, None));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let b = budget.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..10 {
                    b.charge_call(10, 0);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(budget.spent_tokens(), 1000, "no lost updates under races");
    }

    #[test]
    fn a_deadline_that_has_passed_stops_the_next_call() {
        // Zero seconds is the deterministic form of "already out of time",
        // and it is a legitimate thing to write: a scope that must not start
        // any new work.
        let b = Budget::unlimited().nested(&seconds(0));
        assert_eq!(b.check(), Some(Meter::Seconds));
    }

    #[test]
    fn a_deadline_that_has_not_passed_allows_work() {
        let b = Budget::unlimited().nested(&seconds(3600));
        assert_eq!(b.check(), None);
        assert_eq!(b.charge_call(10, 10), None);
        assert_eq!(b.check(), None);
    }

    #[test]
    fn time_is_named_ahead_of_the_other_meters() {
        // A scope out of both time and tokens has run out of time; saying
        // "tokens" would send someone to raise a limit that was not the
        // thing that stopped them.
        let b = Budget::unlimited().nested(&BudgetSpec {
            max_tokens: Some(10),
            max_seconds: Some(0),
            span_line: 1,
            ..Default::default()
        });
        b.charge_call(20, 0);
        assert_eq!(b.check(), Some(Meter::Seconds));
    }

    #[test]
    fn a_parent_deadline_binds_a_child_that_set_none() {
        let parent = Budget::unlimited().nested(&seconds(0));
        let child = parent.nested(&spec(Some(1_000_000), None, None));
        assert_eq!(
            child.check(),
            Some(Meter::Seconds),
            "a child cannot outlive the scope that contains it"
        );
    }

    #[test]
    fn a_child_cannot_extend_its_parents_deadline() {
        // The same rule every other meter follows: entering a scope can only
        // tighten what is already in force.
        let parent = Budget::unlimited().nested(&seconds(0));
        let child = parent.nested(&seconds(3600));
        assert_eq!(child.check(), Some(Meter::Seconds));
    }

    #[test]
    fn one_deadline_is_shared_across_threads() {
        // Mirrors `parallel for`: the deadline is an instant, not a duration
        // each worker starts for itself, so ten branches expire together.
        let budget = Budget::unlimited().nested(&seconds(0));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let b = budget.clone();
            handles.push(std::thread::spawn(move || b.check()));
        }
        for h in handles {
            assert_eq!(h.join().unwrap(), Some(Meter::Seconds));
        }
    }

    #[test]
    fn the_meter_names_itself_for_a_message() {
        assert_eq!(Meter::Seconds.name(), "seconds");
    }
}
