//! Minimal HTTP/1.1 client for talking to a local `amara up` daemon.

use crate::config::{DaemonInfo, api_token, daemon_path};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
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

    fn send(&self, method: &str, path: &str, body: Option<&Value>) -> Result<TcpStream> {
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
        Ok(stream)
    }

    /// GET a server-sent event stream until the server closes it.
    pub fn get_events(&self, path: &str, on_event: impl FnMut(&str, &Value)) -> Result<()> {
        self.events("GET", path, None, on_event)
    }

    /// POST and consume a server-sent event stream, calling `on_event(name, data)`
    /// as each event arrives. Plain JSON error responses become errors.
    pub fn post_events(
        &self,
        path: &str,
        body: &Value,
        on_event: impl FnMut(&str, &Value),
    ) -> Result<()> {
        self.events("POST", path, Some(body), on_event)
    }

    fn events(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        mut on_event: impl FnMut(&str, &Value),
    ) -> Result<()> {
        let mut reader = BufReader::new(self.send(method, path, body)?);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                break;
            }
            headers.push_str(&line.to_lowercase());
        }
        let status: u16 = headers
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let chunked = headers.contains("transfer-encoding: chunked");
        if !(200..300).contains(&status) || !headers.contains("text/event-stream") {
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest)?;
            if chunked {
                rest = dechunk(&rest)?;
            }
            let value: Value = serde_json::from_slice(&rest).unwrap_or(Value::Null);
            if !(200..300).contains(&status) {
                bail!(
                    "{}",
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("HTTP {status}"))
                );
            }
            on_event("result", &value);
            return Ok(());
        }
        let mut sse = SseParser::default();
        loop {
            let data = if chunked {
                let mut size_line = String::new();
                if reader.read_line(&mut size_line)? == 0 {
                    break;
                }
                let size =
                    usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)?;
                if size == 0 {
                    break;
                }
                let mut chunk = vec![0; size + 2];
                reader.read_exact(&mut chunk)?;
                chunk.truncate(size);
                chunk
            } else {
                let mut line = Vec::new();
                if reader.read_until(b'\n', &mut line)? == 0 {
                    break;
                }
                line
            };
            for (event, value) in sse.feed(&data) {
                on_event(&event, &value);
            }
        }
        Ok(())
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut stream = self.send(method, path, body)?;
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

/// Incremental `text/event-stream` parser.
#[derive(Default)]
struct SseParser {
    buf: String,
}

impl SseParser {
    /// Feed bytes; returns every event completed by them as `(name, data)`.
    fn feed(&mut self, bytes: &[u8]) -> Vec<(String, Value)> {
        self.buf
            .push_str(&String::from_utf8_lossy(bytes).replace("\r\n", "\n"));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..end + 2).collect();
            let mut event = "message".to_string();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                }
            }
            if !data.is_empty() {
                out.push((
                    event,
                    serde_json::from_str(&data).unwrap_or(Value::String(data)),
                ));
            }
        }
        out
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
    fn parses_sse_across_reads() {
        let mut p = SseParser::default();
        assert!(p.feed(b"event: chunk\ndata: \"a\"\n").is_empty());
        let got = p.feed(b"\nevent: result\r\ndata: {\"ok\":true}\r\n\r\n: keep-alive\n\n");
        assert_eq!(got[0], ("chunk".into(), Value::String("a".into())));
        assert_eq!(got[1].0, "result");
        assert_eq!(got[1].1["ok"], true);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn encodes_segments() {
        assert_eq!(encode_segment("nova"), "nova");
        assert_eq!(encode_segment("a b/c"), "a%20b%2Fc");
    }
}
