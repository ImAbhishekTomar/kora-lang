//! kora-dap: the Debug Adapter Protocol server behind `kora dap`.
//!
//! An editor launches this process, speaks DAP over stdin and stdout, and gets
//! breakpoints, stepping, a call stack, and a variables pane for a Kora
//! program.
//!
//! Three threads, and the split matters:
//!
//! - the **main** thread owns the protocol and answers every request;
//! - a **reader** thread turns stdin into messages on the main thread's queue,
//!   so a request that arrives while the program is running is still seen;
//! - the **program** thread runs the interpreter.
//!
//! The interpreter is not `Send`-friendly by design, so nothing reaches into
//! it from outside. When it stops it *pushes* a complete snapshot of the stack
//! and then blocks; the main thread answers `stackTrace`, `scopes`, and
//! `variables` out of that snapshot. This is why a paused program can be
//! inspected without a single lock around the interpreter.

pub mod protocol;
pub mod variables;

mod session;

pub use session::{run, Adapter, Client};
