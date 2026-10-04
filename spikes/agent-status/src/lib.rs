//! midnad-lite: Unix-socket listener + per-terminal agent state machine.
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

#[derive(Debug, Deserialize, Clone)]
pub struct Envelope {
    pub agent: String,
    pub event: String,
    pub term: Option<String>,
    pub started_ms: u64,
    pub sent_ms: u64,
    #[serde(default)]
    pub tty_ok: Value,
    pub payload: Value,
    #[serde(skip)]
    pub recv_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    Unknown,
    Idle,
    Working(String),
    NeedsYou(String),
    Done,
    Failed(String),
    Ended,
}

impl AgentState {
    pub fn tag(&self) -> &'static str {
        match self {
            AgentState::Unknown => "unknown",
            AgentState::Idle => "idle",
            AgentState::Working(_) => "working",
            AgentState::NeedsYou(_) => "needs-you",
            AgentState::Done => "done",
            AgentState::Failed(_) => "failed",
            AgentState::Ended => "ended",
        }
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Pure transition function: (current, event) -> next. Shared by Claude Code and Codex
/// (Codex's lifecycle hooks reuse Claude's event names). Returns None for "no change".
pub fn transition(cur: &AgentState, e: &Envelope) -> Option<AgentState> {
    use AgentState::*;
    let p = &e.payload;
    // Subagent-originated tool events carry agent_id; they keep the parent "working" but
    // must never resolve a parent's needs-you.
    let from_subagent = p.get("agent_id").is_some();
    let next = match e.event.as_str() {
        "SessionStart" => Idle,
        "UserPromptSubmit" => Working("prompt".into()),
        "PreToolUse" => {
            if matches!(cur, NeedsYou(_)) {
                return None; // a parallel tool starting doesn't answer the prompt
            }
            Working(format!("tool {}", s(p, "tool_name")))
        }
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
            if matches!(cur, NeedsYou(_)) && from_subagent {
                return None;
            }
            Working(format!("after {}", s(p, "tool_name")))
        }
        "PermissionRequest" => NeedsYou(format!("permission {}", s(p, "tool_name"))),
        "Elicitation" => NeedsYou("elicitation".into()),
        "Notification" => match s(p, "notification_type") {
            "permission_prompt" | "elicitation_dialog" => NeedsYou(format!("notify {}", s(p, "notification_type"))),
            // idle_prompt fires ~60s after Stop: confirms done, never "needs you" by itself
            "idle_prompt" => {
                if matches!(cur, Done | Idle) {
                    return None;
                }
                Done
            }
            _ => return None,
        },
        "SubagentStart" | "SubagentStop" | "TaskCreated" | "TaskCompleted" => {
            if matches!(cur, NeedsYou(_) | Done | Idle) {
                return None;
            }
            Working(e.event.clone())
        }
        "PreCompact" => Working("compacting".into()),
        "PostCompact" => return None,
        "Stop" => Done,
        "StopFailure" => Failed(format!("{} {}", s(p, "error"), s(p, "error_details"))),
        "SessionEnd" => Ended,
        // Codex legacy notify
        "agent-turn-complete" => Done,
        _ => return None,
    };
    if &next == cur { None } else { Some(next) }
}

#[derive(Debug, Clone)]
pub struct Transition {
    pub term: String,
    pub from: AgentState,
    pub to: AgentState,
    pub event: String,
    pub at: Instant,
    pub hook_started_ms: u64,
    pub recv_ms: u64,
}

pub struct Daemon {
    pub states: Arc<Mutex<HashMap<String, AgentState>>>,
    pub events: Arc<Mutex<Vec<(Instant, Envelope)>>>,
    pub transitions: Arc<Mutex<Vec<Transition>>>,
    pub t0: Instant,
}

impl Daemon {
    /// Spawn the listener thread. `log` gets every raw envelope as a JSON line.
    pub fn start(sock: &str, log_path: &str, quiet_events: bool) -> Daemon {
        let _ = std::fs::remove_file(sock);
        let listener = UnixListener::bind(sock).expect("bind socket");
        let d = Daemon {
            states: Default::default(),
            events: Default::default(),
            transitions: Default::default(),
            t0: Instant::now(),
        };
        let (states, events, trans, t0) =
            (d.states.clone(), d.events.clone(), d.transitions.clone(), d.t0);
        let mut log = std::fs::File::create(log_path).expect("log");
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(conn).lines().map_while(Result::ok) {
                        let _ = tx.send(line);
                    }
                });
            }
        });
        std::thread::spawn(move || {
            for line in rx {
                let recv = now_ms();
                let _ = writeln!(log, "{line}");
                let mut env: Envelope = match serde_json::from_str(&line) {
                    Ok(e) => e,
                    Err(err) => {
                        eprintln!("[midnad] bad envelope: {err}");
                        continue;
                    }
                };
                env.recv_ms = recv;
                let at = Instant::now();
                let term = env.term.clone().unwrap_or_else(|| "?".into());
                let el = at.duration_since(t0).as_secs_f64();
                if !quiet_events {
                    let detail = match env.event.as_str() {
                        "Notification" => format!("type={} msg={:?}", s(&env.payload, "notification_type"), s(&env.payload, "message")),
                        "PreToolUse" | "PostToolUse" | "PermissionRequest" => format!("tool={} agent_id={}", s(&env.payload, "tool_name"), s(&env.payload, "agent_id")),
                        _ => String::new(),
                    };
                    println!("[{el:8.3}s] evt  {term} {:<20} hook-start->recv {:>4}ms {detail}", env.event, recv.saturating_sub(env.started_ms));
                }
                let mut st = states.lock().unwrap();
                let cur = st.get(&term).cloned().unwrap_or(AgentState::Unknown);
                if let Some(next) = transition(&cur, &env) {
                    println!("[{el:8.3}s] STATE {term} {:?} -> {:?}  (via {})", cur, next, env.event);
                    trans.lock().unwrap().push(Transition {
                        term: term.clone(),
                        from: cur,
                        to: next.clone(),
                        event: env.event.clone(),
                        at,
                        hook_started_ms: env.started_ms,
                        recv_ms: recv,
                    });
                    st.insert(term, next);
                }
                drop(st);
                events.lock().unwrap().push((at, env));
            }
        });
        d
    }

    pub fn state(&self, term: &str) -> AgentState {
        self.states.lock().unwrap().get(term).cloned().unwrap_or(AgentState::Unknown)
    }
    pub fn elapsed(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }
}
