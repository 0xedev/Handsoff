//! Local HTTPS-MITM proxy.
//!
//! Generates a self-signed CA on first run and uses `hudsucker` to terminate
//! upstream TLS, classify each response by `Host:`, parse provider rate-limit
//! headers via the shared adapter trait, look up the originating PID from
//! `/proc/net/tcp` (Linux) or `lsof` (macOS), and POST a sample to the
//! daemon's `/ingest` endpoint.
//!
//! Public surface:
//!   * [`classify_host`] — pure host -> agent-kind classification (also used
//!     by tests and the CLI).
//!   * [`run`] — bind a listener and serve forever.

pub mod ca;
pub mod pidlookup;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use handoff_adapters::all as all_adapters;
use handoff_common::RateSample;
use http::{Request, Response};
use hudsucker::{
    certificate_authority::RcgenAuthority,
    rcgen::{CertificateParams, KeyPair},
    rustls::crypto::aws_lc_rs,
    Body, HttpContext, HttpHandler, Proxy, RequestOrResponse,
};
use serde_json::json;
use tracing::{debug, info, warn};

const CA_CACHE_SIZE: u64 = 1_000;
const DEFAULT_INGEST: &str = "http://127.0.0.1:7879/ingest";

/// Pure host -> agent-kind classification. Used by the proxy at runtime and
/// by tests.
pub fn classify_host(host: &str) -> Option<&'static str> {
    for adapter in all_adapters() {
        if adapter.classify_host(host) {
            return Some(adapter.kind().as_str());
        }
    }
    None
}

/// Run host -> kind classification AND header parsing in one pass. Returns
/// `(kind, sample)` if the host matches a known adapter; `None` otherwise.
fn classify_and_parse(
    host: &str,
    headers: &BTreeMap<String, String>,
) -> Option<(&'static str, Option<RateSample>)> {
    for adapter in all_adapters() {
        if adapter.classify_host(host) {
            return Some((adapter.kind().as_str(), adapter.parse_headers(headers)));
        }
    }
    None
}

#[derive(Clone)]
struct HandoffHandler {
    daemon_url: String,
    http: reqwest::Client,
    last_host: Arc<tokio::sync::Mutex<Option<String>>>,
}

impl HandoffHandler {
    fn new(daemon_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("client");
        Self {
            daemon_url,
            http,
            last_host: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    async fn post_sample(
        &self,
        kind: &str,
        host: &str,
        status: u16,
        pid: Option<i64>,
        sample: Option<RateSample>,
    ) {
        let body = json!({
            "kind": kind,
            "host": host,
            "status_code": status,
            "pid": pid,
            "sample": sample,
        });
        if let Err(e) = self.http.post(&self.daemon_url).json(&body).send().await {
            debug!("ingest post failed: {e}");
        }
    }
}

impl HttpHandler for HandoffHandler {
    async fn handle_request(
        &mut self,
        _ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        // Stash the host for the response side; hudsucker's HttpContext
        // doesn't always carry the original host through to handle_response.
        if let Some(host) = req
            .uri()
            .host()
            .or_else(|| req.headers().get(http::header::HOST).and_then(|v| v.to_str().ok()))
        {
            *self.last_host.lock().await = Some(host.to_string());
        }
        req.into()
    }

    async fn handle_response(
        &mut self,
        ctx: &HttpContext,
        res: Response<Body>,
    ) -> Response<Body> {
        let host = self.last_host.lock().await.clone().unwrap_or_default();
        let status = res.status();

        // Build a BTreeMap (lower-case keys) for the adapter.
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in res.headers() {
            if let Ok(s) = v.to_str() {
                headers.insert(k.as_str().to_ascii_lowercase(), s.to_string());
            }
        }
        let Some((kind, sample)) = classify_and_parse(&host, &headers) else {
            return res;
        };

        let pid = pidlookup::lookup_pid(ctx.client_addr);
        let kind_owned = kind.to_string();
        let host_owned = host.clone();
        let status_code = status.as_u16();
        let me = self.clone();
        tokio::spawn(async move {
            me.post_sample(&kind_owned, &host_owned, status_code, pid, sample)
                .await;
        });

        res
    }
}

/// Bind on `addr` and serve until the listener is closed.
pub async fn run(addr: SocketAddr, daemon_url: Option<String>) -> Result<()> {
    let (cert_pem, key_pem) =
        ca::load_or_create().context("loading or generating local CA")?;

    // Convert PEM strings into rcgen types via re-parse — hudsucker's
    // `RcgenAuthority` wants a `KeyPair` + `Certificate`. We regenerate from
    // the persisted PEM so the runtime types are correct.
    let key_pair = KeyPair::from_pem(&key_pem).context("parsing CA key.pem")?;
    let mut params = CertificateParams::from_ca_cert_pem(&cert_pem)
        .context("parsing CA cert.pem")?;
    let _ = &mut params; // already populated
    let cert = params.self_signed(&key_pair).context("self-signing")?;

    let provider = aws_lc_rs::default_provider();
    let ca = RcgenAuthority::new(key_pair, cert, CA_CACHE_SIZE, provider.clone());

    let handler = HandoffHandler::new(
        daemon_url.unwrap_or_else(|| DEFAULT_INGEST.to_string()),
    );

    let proxy = Proxy::builder()
        .with_addr(addr)
        .with_ca(ca)
        .with_rustls_client(provider)
        .with_http_handler(handler)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .build()
        .context("building proxy")?;

    info!("handoff-proxy listening on {addr}");
    proxy.start().await.map_err(|e| {
        warn!("proxy stopped: {e}");
        anyhow::anyhow!("proxy: {e}")
    })?;
    Ok(())
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
