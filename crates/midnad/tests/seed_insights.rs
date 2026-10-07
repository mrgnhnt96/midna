//! A realistic, deterministic event history for Insights, plus tests that the charts' data
//! (`insights.series`, `insights.summary`, `insights.activity`) comes out of it.
//!
//! The history is what `session.open`, `agent.hook` (UserPromptSubmit / PreToolUse /
//! Notification / Stop / statusline), `needs_you.*`, `trigger.fired` and `rule.added`
//! produce, backdated across 30 days and five projects. It's written straight into
//! `events.jsonl` + `state.json` because live calls are always stamped "now".
//!
//! Seed a dev home for the GUI (keep the path short: Unix socket limit):
//!
//! ```sh
//! MIDNA_SEED_HOME=/tmp/mh-seed cargo test -p midnad --test seed_insights -- --ignored seed_dev_home
//! MIDNA_HOME=/tmp/mh-seed MIDNA_APP_PATH=$PWD/target/debug/midna-app target/debug/midnad --foreground
//! ```
use midna_proto::{Actor, ActorKind, Client, Event, time};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const PROJECTS: &[(&str, &str, u32)] = &[
    // id, name, relative activity weight
    ("p_6d1d4a", "midna", 10),
    ("p_4b2a55", "kass", 6),
    ("p_a91c07", "api", 7),
    ("p_5a6600", "saggar", 3),
    ("p_d0c5e1", "docs-site", 2),
];

const TERMINAL_NAMES: &[&str] = &["settings-ui", "insights", "migrate", "review", "composer", "golden", "auth", "tests", "refactor", "docs", "spike"];

/// xorshift64*: deterministic so screenshots and tests are stable.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo).max(1) as u64)) as i64
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

struct Log {
    events: Vec<(i64, Event)>,
}

impl Log {
    fn push(&mut self, at: i64, kind: &str, actor: Actor, project: Option<&str>, session: Option<&str>, data: Value) {
        let e = Event { seq: 0, at: time::format_unix(at), kind: kind.into(), actor, project_id: project.map(Into::into), session_id: session.map(Into::into), data };
        self.events.push((at, e));
    }
}

fn agent_actor(sid: &str, agent: &str) -> Actor {
    Actor { kind: ActorKind::Agent, session: Some(sid.into()), name: Some(agent.into()) }
}

/// Write `state.json` (projects) and `events.jsonl` (history up to `now`) into `home`.
/// Returns the number of events written.
pub fn seed_history(home: &Path, now: i64) -> usize {
    std::fs::create_dir_all(home).unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut log = Log { events: vec![] };
    let today = time::local_day_start(now);
    let cutoff = now - 30;
    let mut n_session = 0u32;
    for days_ago in (0..30i64).rev() {
        let day = today - days_ago * 86_400;
        let weekday = ((day / 86_400) + 4).rem_euclid(7); // 0 = Sunday
        let weekend = (weekday == 0 || weekday == 6) && days_ago > 0; // today is always busy
        for (pid, _, weight) in PROJECTS {
            let mut n = rng.range(0, (*weight as i64) / 2 + 2);
            if weekend {
                n /= 3;
            }
            for _ in 0..n {
                n_session += 1;
                let sid = format!("{:08x}", 0x5e00_0000u32 + n_session * 7919);
                let agent = if rng.chance(65) { "claude" } else { "codex" };
                let name = TERMINAL_NAMES[rng.range(0, TERMINAL_NAMES.len() as i64) as usize];
                let latest = if days_ago == 0 { (now - day - 1800).clamp(7 * 3600 + 1, 20 * 3600) } else { 20 * 3600 };
                let mut t = day + rng.range(7 * 3600, latest);
                if t >= cutoff {
                    continue;
                }
                let human = Actor::human();
                log.push(t, "session.opened", human.clone(), Some(pid), Some(&sid), json!({
                    "id": sid, "project_id": pid, "name": name, "kind": "agent", "agent": agent,
                    "status": {"state": "idle", "since": time::format_unix(t)}
                }));
                let a = agent_actor(&sid, agent);
                let mut cost = 0.0f64;
                let turns = rng.range(1, 9);
                for _ in 0..turns {
                    t += rng.range(20, 600);
                    if t >= cutoff {
                        break;
                    }
                    log.push(t, "agent.prompt_submitted", a.clone(), Some(pid), Some(&sid), json!({"agent": agent, "prompt": "…"}));
                    log.push(t, "agent.turn_started", a.clone(), Some(pid), Some(&sid), json!({}));
                    log.push(t, "session.status", a.clone(), Some(pid), Some(&sid), json!({"state": "working", "from": "idle", "reason": "prompt"}));
                    t += rng.range(40, 900);
                    if rng.chance(30) && t < cutoff {
                        // a permission prompt the human answers
                        let nid = format!("n_{:06x}", rng.next() & 0xff_ffff);
                        log.push(t, "session.status", a.clone(), Some(pid), Some(&sid), json!({"state": "needs_you", "from": "working", "reason": "Bash(cargo test)"}));
                        log.push(t, "needs_you.raised", a.clone(), Some(pid), Some(&sid), json!({"id": nid, "kind": "permission_prompt", "title": "Allow Bash(cargo test)?"}));
                        t += rng.range(15, 1500);
                        let res = if rng.chance(85) { json!({"kind": "approve", "scope": {"kind": "once"}}) } else { json!({"kind": "deny"}) };
                        log.push(t, "needs_you.resolved", human.clone(), Some(pid), Some(&sid), json!({"id": nid, "resolution": res}));
                        log.push(t, "session.status", a.clone(), Some(pid), Some(&sid), json!({"state": "working", "from": "needs_you", "reason": "approved"}));
                        t += rng.range(30, 600);
                    }
                    let delta = (rng.range(2, 60) as f64) / 100.0 * if agent == "claude" { 1.0 } else { 0.6 };
                    cost += delta;
                    log.push(t, "agent.cost", a.clone(), Some(pid), Some(&sid), json!({"total_usd": cost, "delta_usd": delta, "model": "opus"}));
                    log.push(t, "agent.turn_ended", a.clone(), Some(pid), Some(&sid), json!({"reason": "Stop"}));
                    log.push(t, "session.status", a.clone(), Some(pid), Some(&sid), json!({"state": "done", "from": "working", "reason": "Stop"}));
                }
                t = (t + rng.range(60, 1800)).min(cutoff);
                log.push(t, "session.closed", human.clone(), Some(pid), Some(&sid), json!({}));
            }
        }
        // webhooks: a couple of triggers on the api project
        for _ in 0..rng.range(0, if weekend { 1 } else { 3 }) {
            let t = day + rng.range(9 * 3600, 18 * 3600);
            if t < cutoff {
                let actor = Actor { kind: ActorKind::Trigger, session: None, name: Some("PR review".into()) };
                log.push(t, "trigger.fired", actor, Some("p_a91c07"), None, json!({"trigger_id": "t_review", "event": "pull_request.opened"}));
            }
        }
        if rng.chance(20) {
            let t = day + rng.range(9 * 3600, 18 * 3600);
            if t < cutoff {
                log.push(t, "rule.added", Actor::human(), Some("p_6d1d4a"), None, json!({"id": "r_seed", "effect": "allow", "matcher": {"kind": "command", "pattern": "cargo test*"}}));
            }
        }
    }
    log.push(today + 9 * 3600 + 5, "settings.changed", Actor::human(), None, None, json!({"key": "density", "value": "compact"}));
    log.events.retain(|(t, _)| *t < now);
    log.events.sort_by_key(|(t, _)| *t);
    let mut out = String::new();
    for (i, (_, e)) in log.events.iter_mut().enumerate() {
        e.seq = i as u64 + 1;
        out.push_str(&serde_json::to_string(e).unwrap());
        out.push('\n');
    }
    std::fs::write(home.join("events.jsonl"), out).unwrap();
    let projects: Vec<Value> = PROJECTS
        .iter()
        .enumerate()
        .map(|(i, (id, name, _))| json!({"id": id, "name": name, "path": format!("/tmp/{name}"), "order": i}))
        .collect();
    std::fs::write(home.join("state.json"), serde_json::to_vec_pretty(&json!({"version": 1, "projects": projects})).unwrap()).unwrap();
    log.events.len()
}

fn start(home: &Path) -> midnad::Handle {
    let mut cfg = midnad::Config::for_home(home.to_path_buf());
    cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
    midnad::start(cfg).expect("start daemon")
}

fn call(c: &mut Client, m: &str, p: Value) -> Value {
    c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

#[test]
fn seeded_history_feeds_series() {
    let home = PathBuf::from(format!("/tmp/midna-s-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let now = time::now_unix();
    let n = seed_history(&home, now);
    assert!(n > 500, "seeded {n} events");
    let h = start(&home);
    let mut c = Client::connect(home.join("midnad.sock")).unwrap();
    c.set_caller(None);

    let week = call(&mut c, "insights.series", json!({"range": "week", "metric": "turns", "by": "project"}));
    assert_eq!(week["bucket"], "day");
    assert_eq!(week["buckets"].as_array().unwrap().len(), 7);
    let groups = week["groups"].as_array().unwrap();
    assert!(groups.len() >= 3, "{groups:?}");
    // Which project is busiest depends on where weekends fall relative to today, so only
    // check that every label is a state project name (not a raw id).
    for g in groups {
        let label = g["label"].as_str().unwrap();
        assert!(PROJECTS.iter().any(|(id, name, _)| *name == label && g["key"] == *id), "labels come from state projects: {g}");
    }
    let busy_days = week["buckets"].as_array().unwrap().iter().filter(|b| b["total"].as_f64().unwrap() > 0.0).count();
    assert!(busy_days >= 4);
    let sum: f64 = week["buckets"].as_array().unwrap().iter().map(|b| b["total"].as_f64().unwrap()).sum();
    assert_eq!(sum, week["total"].as_f64().unwrap());
    let summary = call(&mut c, "insights.summary", json!({"range": "week"}));
    assert_eq!(summary["totals"]["turns"].as_f64().unwrap(), week["total"].as_f64().unwrap());

    let month = call(&mut c, "insights.series", json!({"range": "month", "metric": "spend"}));
    assert_eq!(month["unit"], "usd");
    assert_eq!(month["buckets"].as_array().unwrap().len(), 30);
    assert!(month["total"].as_f64().unwrap() > 1.0);
    let wait = call(&mut c, "insights.series", json!({"range": "month", "metric": "waiting", "by": "agent"}));
    assert!(wait["groups"].as_array().unwrap().iter().any(|g| g["label"] == "Claude"));

    // the widgets' detail over the same history
    let detail = call(&mut c, "insights.detail", json!({"range": "week"}));
    assert_eq!(detail["concurrency"]["step_secs"], 3600);
    assert_eq!(detail["concurrency"]["samples"].as_array().unwrap().len(), 7 * 24);
    assert!(detail["concurrency"]["peak"].as_u64().unwrap() >= 1);
    let bins: i64 = detail["turns"]["bins"].as_array().unwrap().iter().map(|b| b.as_i64().unwrap()).sum();
    assert!(bins > 0 && bins <= week["total"].as_f64().unwrap() as i64 + 10, "turns ended in the week: {bins}");
    assert!(!detail["waits"].as_array().unwrap().is_empty());
    assert_eq!(detail["approved"][0]["label"], "Allow Bash(cargo test)?");
    assert_eq!(detail["corrections"].as_array().unwrap().len(), 7);
    assert_eq!(detail["heatmap"].as_array().unwrap().len(), 7);
    assert_eq!(detail["models"][0]["label"], "opus");
    assert!(PROJECTS.iter().any(|(_, name, _)| detail["projects"][0]["label"] == *name));
    assert!(detail["agent_time"].as_array().unwrap().iter().all(|r| TERMINAL_NAMES.contains(&r["label"].as_str().unwrap())));
    assert!(detail["bests"]["busiest_day_secs"].as_i64().unwrap() > 0);

    // live: a real terminal driven by agent.hook shows up in today's hourly series
    let before = call(&mut c, "insights.series", json!({"range": "today", "metric": "turns"}))["total"].as_f64().unwrap();
    let opened = call(&mut c, "session.open", json!({"project_id": "p_4b2a55", "kind": "shell", "command": ["/bin/cat"]}));
    let sid = opened["id"].as_str().or(opened["session"]["id"].as_str()).unwrap().to_string();
    call(&mut c, "agent.hook", json!({"agent": "claude", "event": "UserPromptSubmit", "payload": {"prompt": "hi"}, "session": sid}));
    let today = call(&mut c, "insights.series", json!({"range": "today", "metric": "turns", "by": "project"}));
    assert_eq!(today["buckets"].as_array().unwrap().len(), 24);
    assert_eq!(today["total"].as_f64().unwrap(), before + 1.0);
    assert!(today["groups"].as_array().unwrap().iter().any(|g| g["label"] == "kass"));
    call(&mut c, "session.close", json!({"id": sid, "force": true}));
    drop(c);
    h.shutdown();
    let _ = std::fs::remove_dir_all(&home);
}

/// Dev helper: seed `$MIDNA_SEED_HOME` (must not exist or be a scratch dir) for GUI work.
#[test]
#[ignore]
fn seed_dev_home() {
    let Ok(home) = std::env::var("MIDNA_SEED_HOME") else {
        eprintln!("set MIDNA_SEED_HOME");
        return;
    };
    let home = PathBuf::from(home);
    let real = midna_proto::paths::midna_home();
    assert_ne!(home, real, "refusing to seed the real MIDNA_HOME");
    let n = seed_history(&home, time::now_unix());
    eprintln!("seeded {n} events into {}", home.display());
}
