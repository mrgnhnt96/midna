//! Webhooks: the local HTTP receiver (`POST /hooks/github`, `POST /hooks/bitbucket` on
//! 127.0.0.1:<webhooks.port>), signing secrets, trigger matching and actions, the delivery
//! path (Tailscale Funnel; relays later), and missed-delivery recovery from GitHub.
//!
//! Threads: one manager loop (1s tick: keeps the receiver bound to the wanted port, detects
//! wake from sleep and kicks reconciliation) and one receiver loop; each request is handled on
//! its own short-lived thread, which answers first and runs the trigger actions afterwards.
pub mod http;
pub mod payload;
pub mod process;
pub mod reconcile;
pub mod secrets;
pub mod sig;
pub mod tailscale;

use crate::daemon::{Config, Daemon};
use midna_proto::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug)]
pub struct WebhookConfig {
    pub secrets: secrets::Mode,
    /// Always run the receiver on this port, whatever webhooks.path says (MIDNA_WEBHOOKS_PORT;
    /// 0 = any free port). Tests use 0.
    pub port_override: Option<u16>,
    /// Tailscale CLI override (MIDNA_TAILSCALE).
    pub tailscale_bin: Option<String>,
}

impl WebhookConfig {
    pub fn from_env() -> WebhookConfig {
        WebhookConfig {
            secrets: secrets::Mode::from_env(),
            port_override: std::env::var("MIDNA_WEBHOOKS_PORT").ok().and_then(|p| p.parse().ok()),
            tailscale_bin: std::env::var("MIDNA_TAILSCALE").ok().filter(|s| !s.is_empty()),
        }
    }
}

/// The running receiver.
pub struct Receiver {
    /// The port asked for (may be 0).
    pub wanted: u16,
    pub bound: u16,
    pub stop: Arc<AtomicBool>,
    pub server: Arc<tiny_http::Server>,
}

#[derive(Default)]
pub struct RtState {
    pub receiver: Option<Receiver>,
    pub error: Option<(u16, String, Instant)>,
    pub reconcile: ReconcileStatus,
    pub tailscale: Option<(Instant, u16, TailscaleStatus)>,
}

pub struct Runtime {
    pub secrets: secrets::Store,
    state: Mutex<RtState>,
    /// Delivery GUIDs being processed right now (dedupe before they're recorded).
    pub inflight: Mutex<HashSet<String>>,
    pub reconciling: AtomicBool,
    /// Unauthenticated rejects recorded in the current minute (window start, count).
    pub rejects: Mutex<(Instant, u32)>,
}

impl Runtime {
    pub fn new(cfg: &Config) -> Runtime {
        Runtime {
            secrets: secrets::Store::new(cfg.webhooks.secrets, &cfg.home),
            state: Mutex::new(RtState::default()),
            inflight: Mutex::new(HashSet::new()),
            reconciling: AtomicBool::new(false),
            rejects: Mutex::new((Instant::now(), 0)),
        }
    }

    pub fn st(&self) -> MutexGuard<'_, RtState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn bound_port(&self) -> Option<u16> {
        self.st().receiver.as_ref().map(|r| r.bound)
    }
}

/// The port the receiver should listen on right now, if any.
fn wanted_port(d: &Daemon) -> Option<u16> {
    if let Some(p) = d.cfg.webhooks.port_override {
        return Some(p);
    }
    let core = d.core();
    if core.state.setting_str("webhooks.path") == "off" {
        return None;
    }
    u16::try_from(core.state.setting_i64("webhooks.port")).ok().filter(|p| *p > 0)
}

/// Make the receiver match the wanted port (start, stop or rebind). Cheap when nothing changed.
pub fn sync_receiver(d: &Arc<Daemon>) {
    let want = wanted_port(d);
    let mut st = d.webhooks.st();
    let current = st.receiver.as_ref().map(|r| r.wanted);
    if current == want {
        return;
    }
    if let Some(r) = st.receiver.take() {
        r.stop.store(true, Ordering::SeqCst);
        r.server.unblock();
    }
    let Some(port) = want else {
        st.error = None;
        return;
    };
    // Don't hammer a port that just failed to bind; retry every 10s.
    if let Some((p, _, at)) = &st.error
        && *p == port
        && at.elapsed() < Duration::from_secs(10)
    {
        return;
    }
    match tiny_http::Server::http(("127.0.0.1", port)) {
        Ok(server) => {
            let bound = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(port);
            let server = Arc::new(server);
            let stop = Arc::new(AtomicBool::new(false));
            http::spawn(Arc::downgrade(d), server.clone(), stop.clone());
            st.receiver = Some(Receiver { wanted: port, bound, stop, server });
            st.error = None;
        }
        Err(e) => st.error = Some((port, format!("could not listen on 127.0.0.1:{port}: {e}"), Instant::now())),
    }
}

/// Start the manager loop (called once from server::start).
pub fn start(d: &Arc<Daemon>) {
    sync_receiver(d);
    let w: Weak<Daemon> = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("webhooks".into()).spawn(move || {
        let mut mono = Instant::now();
        let mut wall = SystemTime::now();
        let mut ticks = 0u64;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                if let Some(r) = d.webhooks.st().receiver.take() {
                    r.stop.store(true, Ordering::SeqCst);
                    r.server.unblock();
                }
                return;
            }
            sync_receiver(&d);
            ticks += 1;
            // macOS's monotonic clock stops while asleep; the wall clock doesn't.
            let wall_elapsed = SystemTime::now().duration_since(wall).unwrap_or_default();
            let woke = wall_elapsed > mono.elapsed() + Duration::from_secs(60);
            mono = Instant::now();
            wall = SystemTime::now();
            if ticks == 3 {
                reconcile::spawn(&d, "startup");
            } else if woke {
                reconcile::spawn(&d, "wake");
            }
        }
    });
}

fn path_name(path: &str) -> &'static str {
    match path {
        "tailscale_funnel" => "Tailscale Funnel",
        "self_relay" => "Self-hosted relay",
        "midna_relay" => "midna relay",
        _ => "Off",
    }
}

/// Tailscale status, cached for 15s (it's two subprocesses).
pub fn tailscale_status(d: &Daemon, port: u16, fresh: bool) -> TailscaleStatus {
    if !fresh
        && let Some((at, p, st)) = &d.webhooks.st().tailscale
        && *p == port
        && at.elapsed() < Duration::from_secs(15)
    {
        return st.clone();
    }
    let st = match tailscale::find_cli(d.cfg.webhooks.tailscale_bin.as_deref()) {
        Some(cli) => tailscale::query(&cli, port),
        None => TailscaleStatus { error: Some("Tailscale CLI not found (install Tailscale.app)".into()), ..Default::default() },
    };
    d.webhooks.st().tailscale = Some((Instant::now(), port, st.clone()));
    st
}

pub fn status(d: &Daemon, fresh: bool) -> WebhooksStatus {
    let (path, port, relay_url, last) = {
        let core = d.core();
        (
            core.state.setting_str("webhooks.path"),
            u16::try_from(core.state.setting_i64("webhooks.port")).unwrap_or(7787),
            core.state.setting_str("webhooks.relay_url"),
            core.state.deliveries.last().cloned(),
        )
    };
    let (receiver, reconcile) = {
        let st = d.webhooks.st();
        let r = WebhookReceiverStatus {
            listening: st.receiver.is_some(),
            port,
            bound_port: st.receiver.as_ref().map(|r| r.bound),
            local_url: st.receiver.as_ref().map(|r| format!("http://127.0.0.1:{}/hooks/github", r.bound)),
            error: st.error.as_ref().map(|(_, e, _)| e.clone()),
        };
        (r, st.reconcile.clone())
    };
    let mut s = WebhooksStatus {
        path: path.clone(),
        path_name: path_name(&path).into(),
        receiver,
        last_delivery_at: last.as_ref().map(|l| l.received_at.clone()),
        last_delivery: last,
        reconcile,
        secret_store: d.cfg.webhooks.secrets.as_str().into(),
        relay_url: Some(relay_url.clone()).filter(|u| !u.is_empty()),
        ..Default::default()
    };
    let recv_err = s.receiver.error.clone();
    match path.as_str() {
        "tailscale_funnel" => {
            let ts = tailscale_status(d, port, fresh);
            if let Some(dns) = &ts.dns_name {
                let base = tailscale::public_base(dns);
                s.host = Some(format!("{dns}:{}", tailscale::FUNNEL_PORT));
                s.public_url = Some(format!("{base}/hooks/github"));
                s.bitbucket_url = Some(format!("{base}/hooks/bitbucket"));
            }
            let (health, detail, fix) = if let Some(e) = &recv_err {
                ("down", e.clone(), Some(format!("Free port {port} or change webhooks.port")))
            } else if let Some(e) = &ts.error {
                ("down", e.clone(), Some("Install and sign in to Tailscale".into()))
            } else if !ts.online {
                ("down", format!("Tailscale is not running ({})", ts.backend_state.clone().unwrap_or_default()), Some("Start Tailscale".into()))
            } else if !ts.funnel_on {
                let why = match &ts.funnel_target {
                    Some(t) => format!("funnel on :8443 points at {t}, not this receiver"),
                    None => "funnel is off for :8443".into(),
                };
                ("down", why, Some("Turn Tailscale Funnel back on for port 8443 (midna webhooks configure tailscale_funnel)".into()))
            } else {
                ("healthy", format!("funnel on :8443 → 127.0.0.1:{port}"), None)
            };
            s.health = health.into();
            s.detail = detail;
            s.fix = fix;
            s.tailscale = Some(ts);
        }
        "self_relay" => {
            s.health = "down".into();
            s.detail = if relay_url.is_empty() {
                "relay not available yet: the relay client isn't built, and webhooks.relay_url is empty".into()
            } else {
                format!("relay not available yet: the relay client isn't built (relay {relay_url})")
            };
            s.fix = Some("Use tailscale_funnel for now".into());
        }
        "midna_relay" => {
            s.health = "down".into();
            s.detail = "relay not available yet: the midna relay is a paid service that hasn't launched".into();
            s.fix = Some("Use tailscale_funnel for now".into());
        }
        _ => {
            s.health = "off".into();
            s.detail = match s.receiver.bound_port {
                Some(p) => format!("No public path; the local receiver is on 127.0.0.1:{p}"),
                None => "Webhooks are off. Pick a delivery path to receive GitHub/Bitbucket events.".into(),
            };
        }
    }
    s
}
