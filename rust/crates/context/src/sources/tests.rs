//! Best-effort failing-test extraction.
//!
//! Looks at `.handoff/scratch/lasttest.txt` (written by an optional shell
//! hook: `pytest 2>&1 | tee .handoff/scratch/lasttest.txt`) and applies a
//! handful of regex extractors per framework.

use std::path::Path;

use handoff_common::TestFailure;
use once_cell::sync::Lazy;
use regex::Regex;

static PYTEST_FAILED: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"FAILED ([^:]+)::([\w\[\]:.+-]+)(?: - )?(.*)").unwrap());

static JEST_FAILED: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"●\s+(.+?)\s*$").unwrap());

static CARGO_FAILED: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*test ([\w:]+) \.\.\. FAILED").unwrap());

pub fn failing_from_scratch(scratch_dir: &Path) -> Vec<TestFailure> {
    let path = scratch_dir.join("lasttest.txt");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    parse(&body)
}

pub fn parse(body: &str) -> Vec<TestFailure> {
    let mut out: Vec<TestFailure> = Vec::new();

    // Pytest
    for cap in PYTEST_FAILED.captures_iter(body) {
        let file = cap.get(1).map(|m| m.as_str().to_string());
        let name = cap.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
        let msg = cap.get(3).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        if !name.is_empty() {
            out.push(TestFailure { name, file, message: msg });
        }
    }

    // Cargo
    for cap in CARGO_FAILED.captures_iter(body) {
        let name = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        if !name.is_empty() {
            out.push(TestFailure {
                name,
                file: None,
                message: String::new(),
            });
        }
    }

    // Jest (very rough)
    for cap in JEST_FAILED.captures_iter(body) {
        let name = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        if !name.is_empty() && !out.iter().any(|t| t.name == name) {
            out.push(TestFailure {
                name,
                file: None,
                message: String::new(),
            });
        }
    }

    out
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_pytest_failures() {
        let body = "FAILED tests/test_foo.py::test_bar - AssertionError: nope\n";
        let r = parse(body);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "test_bar");
        assert_eq!(r[0].file.as_deref(), Some("tests/test_foo.py"));
        assert!(r[0].message.contains("AssertionError"));
    }

    #[test]
    fn parses_cargo_failures() {
        let body = "running 3 tests\ntest module::test_one ... ok\ntest module::test_two ... FAILED\n";
        let r = parse(body);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "module::test_two");
    }

    #[test]
    fn empty_input_empty_output() {
        assert!(parse("").is_empty());
    }
}
