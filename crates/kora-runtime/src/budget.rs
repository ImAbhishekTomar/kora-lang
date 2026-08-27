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

use kora_syntax::ast::BudgetSpec;

/// Which limit ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meter {
    Tokens,
    Calls,
    Steps,
}

impl Meter {
    pub fn name(&self) -> &'static str {
        match self {
            Meter::Tokens => "tokens",
            Meter::Calls => "calls",
            Meter::Steps => "steps",
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tokens: Option<u64>, calls: Option<u64>, steps: Option<u64>) -> BudgetSpec {
        BudgetSpec {
            max_tokens: tokens,
            max_calls: calls,
            max_steps: steps,
            span_line: 1,
        }
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
}
