//! kora-runtime: interpreter for Kora (Stage 1: tree-walking).
//!
//! Agents, scheduler, budgets, and cassettes arrive in Phases 2-3.

pub mod interp;
pub mod value;

pub use interp::{Interpreter, RuntimeError};
pub use value::Value;
