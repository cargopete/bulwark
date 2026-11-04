//! `bulwark dashboard`: a live view of a run, served from the workspace files.

pub mod logbook;
pub mod status;

use crate::workspace::Workspace;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const INDEX: &str = include_str!("index.html");
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST: usize = 16 * 1024;

pub async fn serve(
    ws: Workspace,
    scope: status::Scope,
    bind: &str,
    port: u16,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind((bind, port)).await?;
    eprintln!(
        "  Dashboard on http://{}/  (workspace {})",
        listener.local_addr()?,
        ws.root().display()
    );
    eprintln!(
        "  Scope: {} assets, {} components{}",
        scope.owed.assets.len(),
        scope.owed.components.len(),
        if scope.owed.assets.is_empty() && scope.owed.components.is_empty() {
            " (no scope: every file counts)"
        } else {
            ""
        }
    );
    loop {
        let (sock, _) = listener.accept().await?;
        let ws = ws.clone();
        let scope = scope.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, ws, scope).await {
                tracing::debug!("dashboard connection: {e}");
            }
        });
    }
}

async fn handle(mut sock: TcpStream, ws: Workspace, scope: status::Scope) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST {
        let n = tokio::time::timeout(READ_TIMEOUT, sock.read(&mut chunk)).await??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let (code, ctype, body) = match (method, path) {
        ("GET", "/") => (
            "200 OK",
            "text/html; charset=utf-8",
            INDEX.as_bytes().to_vec(),
        ),
        ("GET", "/api/status") => {
            let s = tokio::task::spawn_blocking(move || {
                status::collect(&ws, &scope, chrono::Utc::now())
            })
            .await?;
            ("200 OK", "application/json", serde_json::to_vec(&s)?)
        }
        ("GET", "/api/logbook") => {
            let label = query_param(query, "label").unwrap_or_default();
            let book = tokio::task::spawn_blocking(move || {
                logbook::read(&ws, &scope, &label, chrono::Utc::now())
            })
            .await?;
            match book {
                Some(b) => ("200 OK", "application/json", serde_json::to_vec(&b)?),
                None => not_found(),
            }
        }
        _ => not_found(),
    };
    let header = format!(
        "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(header.as_bytes()).await?;
    sock.write_all(&body).await?;
    sock.shutdown().await?;
    Ok(())
}

fn not_found() -> (&'static str, &'static str, Vec<u8>) {
    ("404 Not Found", "text/plain", b"not found\n".to_vec())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                Some(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                None => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_survives_url_encoding() {
        assert_eq!(
            query_param("x=1&label=agent%3Ahunt-fees", "label").as_deref(),
            Some("agent:hunt-fees")
        );
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
