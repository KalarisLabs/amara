//! Minimal HTTP/1.1 client for talking to a local `amara up` daemon.

use crate::config::{DaemonInfo, api_token, daemon_path};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Client {
    host: String,
    token: String,
}

impl Client {
    /// Connect to the daemon recorded in `~/.amara/daemon.json`.
    pub fn discover() -> Result<Self> {
        let info: DaemonInfo = std::fs::read_to_string(daemon_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(|| {
                anyhow!("no running node found. Start one with `amara up` (or use `amara chat`)")
            })?;
        let host = info
            .url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            host,
            token: api_token()?,
        })
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.request("GET", path, None)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.request("POST", path, Some(body))
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut stream = TcpStream::connect(&self.host).with_context(|| {
            format!(
                "cannot reach node at {} (is `amara up` running?)",
                self.host
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(3700)))?;
        let payload = body
            .map(|b| serde_json::to_vec(b).unwrap_or_default())
            .unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.host,
            self.token,
            payload.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(&payload)?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| anyhow!("malformed HTTP response"))?;
        let headers = String::from_utf8_lossy(&raw[..split]).to_lowercase();
        let mut body = raw[split + 4..].to_vec();
        if headers.contains("transfer-encoding: chunked") {
            body = dechunk(&body)?;
        }
        let status: u16 = headers
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let msg = value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("HTTP {status}"));
            bail!("{msg}");
        }
        Ok(value)
    }
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow!("bad chunk"))?;
        let size_str = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            bail!("truncated chunk");
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// Percent-encode a path segment.
pub fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dechunks() {
        assert_eq!(
            dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap(),
            b"Wikipedia"
        );
        assert!(dechunk(b"ff\r\nshort").is_err());
    }

    #[test]
    fn encodes_segments() {
        assert_eq!(encode_segment("nova"), "nova");
        assert_eq!(encode_segment("a b/c"), "a%20b%2Fc");
    }
}
