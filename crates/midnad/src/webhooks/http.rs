//! The local receiver: tiny_http on 127.0.0.1. Each request gets its own thread, which reads
//! the body, verifies and matches it, answers (202 / 401 / 200 duplicate) and only then runs
//! the trigger actions.
use super::process::{self, Request};
use crate::daemon::Daemon;
use midna_proto::TriggerSource;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// GitHub caps payloads at 25 MB.
const MAX_BODY: u64 = 25 * 1024 * 1024;
/// Requests handled at once. The receiver may be public (Funnel); beyond this, answer 503
/// without reading the body, so a flood can't hold 25 MB × N in memory.
pub const MAX_CONCURRENT: usize = 16;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Slot;
impl Slot {
    fn take() -> Option<Slot> {
        if ACTIVE.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn spawn(d: Weak<Daemon>, server: Arc<tiny_http::Server>, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("webhooks-http".into()).spawn(move || {
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let req = match server.recv_timeout(Duration::from_millis(250)) {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(_) => return,
            };
            let Some(daemon) = d.upgrade() else { return };
            if daemon.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            let Some(slot) = Slot::take() else {
                reply(req, 503, r#"{"error":"busy, retry later"}"#);
                continue;
            };
            let _ = std::thread::Builder::new().name("webhook-req".into()).spawn(move || {
                let _slot = slot;
                handle(&daemon, req)
            });
        }
    });
}

fn reply(req: tiny_http::Request, status: u16, body: &str) {
    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("static header");
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(status).with_header(ct));
}

fn handle(d: &Arc<Daemon>, mut req: tiny_http::Request) {
    let path = req.url().split('?').next().unwrap_or("").trim_end_matches('/').to_string();
    let source = match path.as_str() {
        "/hooks/github" => Some(TriggerSource::Github),
        "/hooks/bitbucket" => Some(TriggerSource::Bitbucket),
        "/hooks/health" | "/hooks" => {
            reply(req, 200, r#"{"ok":true,"service":"midnad"}"#);
            return;
        }
        _ => None,
    };
    let Some(source) = source else {
        reply(req, 404, r#"{"error":"not found; POST /hooks/github or /hooks/bitbucket"}"#);
        return;
    };
    if *req.method() != tiny_http::Method::Post {
        reply(req, 405, r#"{"error":"use POST"}"#);
        return;
    }
    let headers: Vec<(String, String)> =
        req.headers().iter().map(|h| (h.field.as_str().as_str().to_ascii_lowercase(), h.value.as_str().to_string())).collect();
    if req.body_length().is_some_and(|n| n as u64 > MAX_BODY) {
        reply(req, 413, r#"{"error":"body too large"}"#);
        return;
    }
    let mut body = Vec::new();
    if req.as_reader().take(MAX_BODY + 1).read_to_end(&mut body).is_err() || body.len() as u64 > MAX_BODY {
        reply(req, 413, r#"{"error":"body too large or unreadable"}"#);
        return;
    }
    let out = process::receive(d, Request { source, headers, body });
    reply(req, out.status, &out.body);
    if let Some(work) = out.work {
        process::complete(d, work);
    }
}
