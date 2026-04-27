//! Local HTTPS-MITM proxy.
//!
//! v0.4-alpha note: a Rust-native MITM proxy needs the `hudsucker` crate
//! (or equivalent) to terminate TLS with a generated CA. That work is
//! deferred — for now the daemon also accepts ingest from the existing
//! Python mitmproxy addon (under `../../src/handoff/proxy/addon.py`) so
//! we don't lose functionality during the rewrite.
//!
//! When this crate lands its real implementation, it will:
//!   * generate a CA with `rcgen` on first run and write it to
//!     `~/.handoff/ca.pem` for system trust import,
//!   * proxy CONNECT, terminate TLS with `rustls`, classify by
//!     `Host:` header against `handoff-adapters::Adapter::classify_host`,
//!   * for matching responses, parse rate-limit headers and POST the
//!     resulting `RateSample` to the local daemon's `/ingest`.
//!
//! The classification + parsing logic is fully shared with the adapter
//! crate so there's no second source of truth.

pub use handoff_adapters::{all as all_adapters, Adapter};

/// Classify an arbitrary `Host:` value into an agent kind, by asking each
/// adapter. Returns `None` if no adapter claims the host.
pub fn classify_host(host: &str) -> Option<&'static str> {
    for adapter in all_adapters() {
        if adapter.classify_host(host) {
            return Some(adapter.kind().as_str());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_hosts() {
        assert_eq!(classify_host("api.anthropic.com"), Some("claude"));
        assert_eq!(classify_host("api.openai.com"), Some("codex"));
        assert_eq!(classify_host("github.com"), None);
    }
}
