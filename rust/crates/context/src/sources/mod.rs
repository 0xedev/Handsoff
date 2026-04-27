//! Sources that populate a Snapshot.
//!
//! Each module is independent: `git` reads git state, `shell` reads shell
//! history, `tests` parses test artifacts. None of them error fatally —
//! a missing source just means an empty section in the Snapshot.

pub mod git;
pub mod shell;
pub mod tests;
