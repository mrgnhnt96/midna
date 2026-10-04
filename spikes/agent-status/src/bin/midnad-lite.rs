//! Standalone daemon: `midnad-lite <sock> <events.jsonl>` — prints events + transitions.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let _d = agent_status_spike::Daemon::start(&a[1], a.get(2).map(String::as_str).unwrap_or("/dev/null"), false);
    loop { std::thread::park(); }
}
