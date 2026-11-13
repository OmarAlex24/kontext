//! A small blocking HTTP client (ureq with rustls and the bundled web roots, so
//! a static binary in a container with no CA store still verifies TLS).

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

/// The largest body read back (media goes up to the server's 32 MB).
const MAX_BODY: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Http {
    agent: ureq::Agent,
}

/// What came back: the status and the body, whatever the status.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).with_context(|| {
            format!(
                "the answer ({}) is not JSON: {}",
                self.status,
                String::from_utf8_lossy(&self.body[..self.body.len().min(200)])
            )
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Http {
    /// `timeout` bounds the whole request; connecting gets at most half of it.
    pub fn new(timeout: Duration) -> Http {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(timeout / 2))
            .http_status_as_error(false)
            .user_agent(concat!("kontext/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Http { agent }
    }

    pub fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<Reply> {
        let res = match (method, body) {
            ("GET", _) => {
                let mut r = self.agent.get(url);
                for (k, v) in headers {
                    r = r.header(*k, *v);
                }
                r.call()
            }
            ("DELETE", _) => {
                let mut r = self.agent.delete(url);
                for (k, v) in headers {
                    r = r.header(*k, *v);
                }
                r.call()
            }
            (m, b) => {
                let mut r = match m {
                    "POST" => self.agent.post(url),
                    "PUT" => self.agent.put(url),
                    "PATCH" => self.agent.patch(url),
                    _ => anyhow::bail!("unsupported method {m}"),
                };
                for (k, v) in headers {
                    r = r.header(*k, *v);
                }
                r.send(b.unwrap_or_default())
            }
        };
        let mut res = res.map_err(|e| anyhow::anyhow!("{method} {}: {e}", redact(url)))?;
        let status = res.status().as_u16();
        let body = res
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| anyhow::anyhow!("{method} {}: reading the answer: {e}", redact(url)))?;
        Ok(Reply { status, body })
    }
}

/// A URL without its query, for messages.
fn redact(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}
