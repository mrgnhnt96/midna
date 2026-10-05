//! What an agent has in flight, from its hooks (pure: hook payload in, `AgentInfo` out).
//!
//! Claude Code (verified on 2.1.289) reports:
//! - its conversation id (`session_id`) and `permission_mode` in every main-thread hook
//!   (hooks fired inside a subagent carry `agent_id`; those ids are not the conversation);
//! - the running `version` and `model.id` in its status line;
//! - a full snapshot of background work, `background_tasks`, and of scheduled wakeups,
//!   `session_crons`, in every `Stop` and `SubagentStop` (REPLACE semantics);
//! - subagents as `SubagentStart` / `SubagentStop`. Its internal prompt-suggestion agent fires
//!   a `SubagentStop` with an empty `agent_type` (and no start) after most turns: ignored;
//! - a shell sent to the background as PostToolUse `tool_response.backgroundTaskId`, a
//!   background agent as `tool_response.agentId` with `isAsync`.
//!
//! Background shells run in their own session (setsid). Claude kills them when it exits on
//! SIGHUP/SIGTERM; after SIGKILL they live on, orphaned. Session crons survive `--resume`;
//! background shells and agents do not.
//!
//! Codex reports its thread id in notify (`thread-id`).
use midna_proto::{AgentCron, AgentInfo, AgentKind, BackgroundTask, PendingAgent, Subagent};
use serde_json::Value;

/// Stopped subagents kept for the header popover and their windows.
const FINISHED_KEPT: usize = 10;

fn str_at(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Fold one hook into `info`. Returns true when something worth showing changed (not for
/// status-line churn that only repeats what is known).
pub fn apply_hook(info: &mut AgentInfo, agent: AgentKind, event: &str, p: &Value, now: &str) -> bool {
    let before = material(info);
    match agent {
        AgentKind::Claude => apply_claude(info, event, p, now),
        AgentKind::Codex => {
            if event == "agent-turn-complete"
                && let Some(t) = str_at(p, "/thread-id")
            {
                info.conversation_id = Some(t);
            }
        }
    }
    material(info) != before
}

fn apply_claude(info: &mut AgentInfo, event: &str, p: &Value, now: &str) {
    let main_thread = p.get("agent_id").is_none_or(Value::is_null);
    if main_thread {
        if let Some(id) = str_at(p, "/session_id") {
            info.conversation_id = Some(id);
        }
        if let Some(t) = str_at(p, "/transcript_path") {
            info.transcript_path = Some(t);
        }
        if let Some(m) = str_at(p, "/permission_mode") {
            info.permission_mode = Some(m);
        }
    }
    match event {
        "statusline" => {
            if let Some(v) = str_at(p, "/version") {
                if info.update_available.as_deref() == Some(v.as_str()) {
                    info.update_available = None; // that version is the one running now
                }
                info.version = Some(v);
            }
            if let Some(m) = str_at(p, "/model/id") {
                info.model = Some(m);
            }
        }
        "SessionStart" => {
            // A new process (startup/resume) has nothing in flight yet; /clear and compaction
            // keep the process and its background work. A resume brings its crons back.
            let source = p.get("source").and_then(Value::as_str);
            if matches!(source, Some("startup" | "resume")) {
                info.background.clear();
                info.subagents.clear();
                info.background_at = Some(now.to_string());
            }
            if source == Some("startup") {
                info.crons.clear();
                info.finished_subagents.clear();
            }
            if let Some(m) = str_at(p, "/model") {
                info.model = Some(m);
            }
        }
        "SessionEnd" => {
            if p.get("reason").and_then(Value::as_str) != Some("clear") {
                info.subagents.clear();
            }
        }
        "UserPromptSubmit" if main_thread => {
            // "Finished this turn" starts over with each prompt the human sends.
            info.finished_subagents.clear();
        }
        "PreToolUse" if main_thread && p.get("tool_name").and_then(Value::as_str) == Some("Agent") => {
            if let Some(tool_use_id) = str_at(p, "/tool_use_id") {
                info.pending_agents.push(PendingAgent {
                    tool_use_id,
                    agent_type: str_at(p, "/tool_input/subagent_type").unwrap_or_else(|| "general-purpose".into()),
                    description: str_at(p, "/tool_input/description").unwrap_or_default(),
                    background: p.pointer("/tool_input/run_in_background").and_then(Value::as_bool) == Some(true),
                });
            }
        }
        "SubagentStart" => {
            // A woken background agent starts again under the same id; its background entry
            // still has the description.
            if let (Some(id), Some(agent_type)) = (str_at(p, "/agent_id"), str_at(p, "/agent_type"))
                && !info.subagents.iter().any(|a| a.id == id)
            {
                // SubagentStart has no tool_use_id: take the oldest Agent call of this type.
                // Parallel launches of one type may swap descriptions until an async
                // PostToolUse names the agent.
                let woken = info.background.iter().find(|t| t.id == id).map(|t| (t.description.clone(), true));
                let pending = info.pending_agents.iter().position(|a| a.agent_type == agent_type).map(|n| info.pending_agents.remove(n));
                let (description, background) = woken.or(pending.map(|a| (a.description, a.background))).unwrap_or_default();
                info.subagents.push(Subagent { id, agent_type, description, background, started_at: now.to_string(), ended_at: None });
            }
        }
        "Stop" | "SubagentStop" => {
            let stopping = str_at(p, "/agent_id").filter(|_| event == "SubagentStop");
            if let Some(id) = &stopping {
                if let Some(n) = info.subagents.iter().position(|a| &a.id == id) {
                    let mut a = info.subagents.remove(n);
                    a.ended_at = Some(now.to_string());
                    // A woken background agent finishes again: keep its latest run only.
                    info.finished_subagents.retain(|f| &f.id != id);
                    info.finished_subagents.push(a);
                    let over = info.finished_subagents.len().saturating_sub(FINISHED_KEPT);
                    info.finished_subagents.drain(..over);
                }
            } else if main_thread {
                // The main turn ended: foreground subagents are done, and background ones are
                // in background_tasks. A SubagentStop lost to a crash must not linger, nor an
                // Agent call that failed before it started one.
                info.subagents.clear();
                info.pending_agents.clear();
            }
            if let Some(tasks) = p.get("background_tasks").and_then(Value::as_array) {
                // A stopping subagent still lists itself as running in its own snapshot.
                info.background = tasks.iter().filter_map(background_task).filter(|t| stopping.as_ref() != Some(&t.id)).collect();
                info.background_at = Some(now.to_string());
            }
            if let Some(crons) = p.get("session_crons").and_then(Value::as_array) {
                info.crons = crons.iter().filter_map(cron).collect();
            }
        }
        "PostToolUseFailure" => {
            if let Some(t) = str_at(p, "/tool_use_id") {
                info.pending_agents.retain(|a| a.tool_use_id != t);
            }
        }
        "PostToolUse" => {
            if let Some(t) = str_at(p, "/tool_use_id") {
                info.pending_agents.retain(|a| a.tool_use_id != t);
            }
            let r = p.get("tool_response").unwrap_or(&Value::Null);
            // An async launch names its agent: correct a description the type match got wrong.
            if r.get("isAsync").and_then(Value::as_bool) == Some(true)
                && let Some(id) = str_at(r, "/agentId")
                && let Some(a) = info.subagents.iter_mut().find(|a| a.id == id)
            {
                a.description = str_at(r, "/description").unwrap_or_default();
                a.background = true;
            }
            // A shell sent to the background mid-turn; the next Stop confirms or drops it.
            let task = if let Some(id) = str_at(r, "/backgroundTaskId") {
                Some(BackgroundTask {
                    id,
                    kind: "shell".into(),
                    status: "running".into(),
                    description: str_at(p, "/tool_input/description").unwrap_or_default(),
                    command: str_at(p, "/tool_input/command"),
                    ..Default::default()
                })
            } else if r.get("isAsync").and_then(Value::as_bool) == Some(true)
                && let Some(id) = str_at(r, "/agentId")
            {
                Some(BackgroundTask {
                    id,
                    kind: "subagent".into(),
                    status: "running".into(),
                    description: str_at(r, "/description").unwrap_or_default(),
                    agent_type: str_at(p, "/tool_input/subagent_type"),
                    ..Default::default()
                })
            } else {
                None
            };
            if let Some(t) = task
                && !info.background.iter().any(|x| x.id == t.id)
            {
                info.background.push(t);
            }
        }
        _ => {}
    }
}

fn background_task(v: &Value) -> Option<BackgroundTask> {
    Some(BackgroundTask {
        id: str_at(v, "/id")?,
        kind: str_at(v, "/type").unwrap_or_else(|| "task".into()),
        status: str_at(v, "/status").unwrap_or_default(),
        description: str_at(v, "/description").unwrap_or_default(),
        command: str_at(v, "/command"),
        agent_type: str_at(v, "/agent_type"),
        server: str_at(v, "/server"),
        tool: str_at(v, "/tool"),
        name: str_at(v, "/name"),
    })
}

fn cron(v: &Value) -> Option<AgentCron> {
    Some(AgentCron {
        id: str_at(v, "/id")?,
        schedule: str_at(v, "/schedule").unwrap_or_default(),
        recurring: v.get("recurring").and_then(Value::as_bool).unwrap_or(false),
        prompt: str_at(v, "/prompt").unwrap_or_default(),
    })
}

/// What a change event is about: ids of in-flight work, the update, the conversation and the
/// queued restart (timestamps and the status line's model churn excluded).
fn material(i: &AgentInfo) -> Value {
    let ids = |v: Vec<&str>| v.join(",");
    serde_json::json!([
        i.conversation_id,
        i.update_available,
        ids(i.background.iter().map(|t| t.id.as_str()).collect()),
        ids(i.background.iter().map(|t| t.status.as_str()).collect()),
        ids(i.subagents.iter().map(|t| t.id.as_str()).collect()),
        ids(i.subagents.iter().map(|t| t.description.as_str()).collect()),
        ids(i.finished_subagents.iter().map(|t| t.id.as_str()).collect()),
        ids(i.crons.iter().map(|t| t.id.as_str()).collect()),
        i.restart.as_ref().map(|r| (&r.reason, &r.waiting_for)),
        i.version,
        i.permission_mode,
    ])
}

/// The command that reopens `info`'s conversation: `base` is the agent's launch command
/// without its initial prompt. Claude also keeps the model and permission mode it had.
pub fn resume_command(agent: AgentKind, base: &[String], info: &AgentInfo) -> Option<Vec<String>> {
    let id = info.conversation_id.clone()?;
    let mut v = base.to_vec();
    match agent {
        AgentKind::Claude => {
            let has = |flag: &str| base.iter().any(|a| a == flag);
            if let Some(m) = info.model.as_ref().filter(|_| !has("--model")) {
                v.extend(["--model".to_string(), m.clone()]);
            }
            // `default` is what a fresh start gets anyway; a supervised agent already pins it.
            if let Some(pm) = info.permission_mode.as_ref().filter(|m| *m != "default" && !has("--permission-mode")) {
                v.extend(["--permission-mode".to_string(), pm.clone()]);
            }
            v.extend(["--resume".to_string(), id]);
        }
        AgentKind::Codex => {
            v.insert(1.min(v.len()), "resume".into());
            v.push(id);
        }
    }
    Some(v)
}

/// The first `x.y.z` in `s` (`2.1.289 (Claude Code)` -> `2.1.289`).
pub fn parse_version(s: &str) -> Option<String> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|w| w.split('.').count() >= 3 && w.split('.').all(|p| !p.is_empty()))
        .map(str::to_string)
}

/// `a` newer than `b`, numerically per dotted part.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) > parts(b)
}

/// Plain rows from the formatter's VT export with dim text blanked: faint (SGR 2), bright
/// black (90) and grey foregrounds (256-color greys, or truecolor with r≈g≈b in the mid range)
/// become spaces. That is how TUIs draw placeholders and suggestions (Claude 2.1.289: `❯\u{a0}`
/// then `ESC[2m Try "fix typecheck errors"`), which a plain read can't tell from typed text.
/// Box drawing is kept whatever its color (Claude's input-box rules are grey `38;2;136;136;136`).
/// Rows are separated by newlines or by cursor-down (`\r ESC[nB`), as the export moves.
pub fn undim(vt: &str) -> Vec<String> {
    let mut rows = vec![String::new()];
    let (mut faint, mut grey) = (false, false);
    let mut it = vt.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\n' {
            rows.push(String::new());
            continue;
        }
        if c != '\x1b' {
            if !c.is_control() {
                let boxy = ('\u{2500}'..='\u{257f}').contains(&c);
                rows.last_mut().unwrap().push(if (faint || grey) && !boxy { ' ' } else { c });
            }
            continue;
        }
        if it.peek() != Some(&'[') {
            // OSC and other escapes: skip to BEL or ST.
            while let Some(x) = it.next() {
                if x == '\x07' || (x == '\x1b' && it.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
            continue;
        }
        it.next();
        let mut params = String::new();
        let mut fin = ' ';
        for x in it.by_ref() {
            if ('@'..='~').contains(&x) {
                fin = x;
                break;
            }
            params.push(x);
        }
        let ps: Vec<u32> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
        let n = ps.first().copied().filter(|&n| n > 0).unwrap_or(1) as usize;
        match fin {
            'B' => rows.extend(std::iter::repeat_n(String::new(), n)),
            'C' => rows.last_mut().unwrap().push_str(&" ".repeat(n)),
            'm' => {
                let mut i = 0;
                while i < ps.len() {
                    match ps[i] {
                        0 => (faint, grey) = (false, false),
                        2 => faint = true,
                        22 => faint = false,
                        39 => grey = false,
                        90 => grey = true,
                        30..=37 | 91..=97 => grey = false,
                        38 if ps.get(i + 1) == Some(&5) => {
                            let n = ps.get(i + 2).copied().unwrap_or(0);
                            grey = n == 8 || (238..=250).contains(&n);
                            i += 2;
                        }
                        38 if ps.get(i + 1) == Some(&2) => {
                            let (r, g, b) = (ps.get(i + 2).copied().unwrap_or(0), ps.get(i + 3).copied().unwrap_or(0), ps.get(i + 4).copied().unwrap_or(0));
                            let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
                            grey = hi - lo <= 16 && (70..=185).contains(&hi);
                            i += 4;
                        }
                        48 if ps.get(i + 1) == Some(&5) => i += 2,
                        48 if ps.get(i + 1) == Some(&2) => i += 4,
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => {}
        }
    }
    rows.iter().map(|r| r.trim_end().to_string()).collect()
}

/// The text in Claude's input box (between the last two horizontal rules, prompt glyph
/// stripped), or None when the screen doesn't show one. Codex: the `›` line, placeholder text
/// counts as empty only when it is dim, which a text read can't tell, so Codex returns None.
pub fn claude_input_text(screen: &[String]) -> Option<String> {
    let is_rule = |l: &String| {
        let t = l.trim();
        t.chars().count() >= 20 && t.chars().all(|c| c == '─')
    };
    let rules: Vec<usize> = screen.iter().enumerate().filter(|(_, l)| is_rule(l)).map(|(i, _)| i).collect();
    let (&b, &a) = (rules.last()?, rules.get(rules.len().checked_sub(2)?)?);
    let body: Vec<&str> = screen[a + 1..b].iter().map(|l| l.trim()).collect();
    let first = body.first()?;
    if !first.starts_with('❯') && !first.starts_with('>') {
        return None;
    }
    let text = body.iter().enumerate().map(|(i, l)| if i == 0 { l.trim_start_matches(['❯', '>']).trim() } else { l }).collect::<Vec<_>>().join("\n");
    Some(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-10-03T10:00:00Z";

    fn hook(info: &mut AgentInfo, ev: &str, p: Value) -> bool {
        apply_hook(info, AgentKind::Claude, ev, &p, NOW)
    }

    #[test]
    fn stop_snapshot_replaces_background_and_crons() {
        let mut i = AgentInfo::default();
        assert!(hook(&mut i, "SessionStart", json!({"session_id": "c1", "source": "startup", "permission_mode": "bypassPermissions"})));
        assert_eq!(i.conversation_id.as_deref(), Some("c1"));
        let stop = json!({"session_id": "c1", "background_tasks": [
            {"id": "b1", "type": "shell", "status": "running", "description": "dev server", "command": "npm run dev"},
            {"id": "a1", "type": "subagent", "status": "running", "description": "research", "agent_type": "Explore"},
        ], "session_crons": [{"id": "k1", "schedule": "30 14 3 10 *", "recurring": false, "prompt": "check CI"}]});
        assert!(hook(&mut i, "Stop", stop));
        assert_eq!(i.background.len(), 2);
        assert_eq!(i.background[0].command.as_deref(), Some("npm run dev"));
        assert_eq!(i.crons[0].schedule, "30 14 3 10 *");
        assert_eq!(i.in_flight().len(), 2, "crons survive a resume, so they don't block one");
        assert!(hook(&mut i, "Stop", json!({"session_id": "c1", "background_tasks": [], "session_crons": []})));
        assert!(i.in_flight().is_empty());
        // No snapshot fields (an older Claude): leave what is known alone.
        i.background.push(BackgroundTask { id: "x".into(), ..Default::default() });
        hook(&mut i, "Stop", json!({"session_id": "c1"}));
        assert_eq!(i.background.len(), 1);
    }

    #[test]
    fn subagents_and_live_shells() {
        let mut i = AgentInfo::default();
        hook(&mut i, "SubagentStart", json!({"session_id": "c1", "agent_id": "ag1", "agent_type": "Explore"}));
        assert_eq!(i.subagents.len(), 1);
        // Hooks from inside the subagent never replace the conversation id.
        hook(&mut i, "PreToolUse", json!({"session_id": "c1", "agent_id": "ag1", "permission_mode": "plan"}));
        assert_eq!(i.permission_mode, None);
        hook(&mut i, "SubagentStop", json!({"session_id": "c1", "agent_id": "ag1", "background_tasks": []}));
        assert!(i.subagents.is_empty());
        hook(&mut i, "PostToolUse", json!({"session_id": "c1", "tool_name": "Bash", "tool_input": {"command": "sleep 400", "run_in_background": true}, "tool_response": {"backgroundTaskId": "bsh1"}}));
        assert_eq!(i.background[0].id, "bsh1");
        assert_eq!(i.background[0].command.as_deref(), Some("sleep 400"));
        hook(&mut i, "SubagentStart", json!({"session_id": "c1", "agent_id": "ag2", "agent_type": "general-purpose"}));
        hook(&mut i, "Stop", json!({"session_id": "c1"}));
        assert!(i.subagents.is_empty(), "a main-thread Stop ends foreground subagents");
        // The prompt-suggestion agent (empty type) is not work; a stopping agent drops itself.
        hook(&mut i, "SubagentStart", json!({"session_id": "c1", "agent_id": "sug", "agent_type": ""}));
        assert!(i.subagents.is_empty());
        hook(&mut i, "SubagentStop", json!({"session_id": "c1", "agent_id": "a9", "agent_type": "general-purpose",
            "background_tasks": [{"id": "a9", "type": "subagent", "status": "running", "description": "x"}, {"id": "b2", "type": "shell", "status": "running", "description": "s", "command": "sleep 9"}]}));
        assert_eq!(i.background.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b2"]);
        hook(&mut i, "PostToolUse", json!({"session_id": "c1", "tool_name": "Agent", "tool_input": {"subagent_type": "Explore"},
            "tool_response": {"isAsync": true, "status": "async_launched", "agentId": "a10", "description": "look around"}}));
        assert_eq!((i.background[1].kind.as_str(), i.background[1].agent_type.as_deref()), ("subagent", Some("Explore")));
    }

    #[test]
    fn subagent_descriptions_come_from_the_agent_call() {
        let mut i = AgentInfo::default();
        let call = |id: &str, ty: &str, desc: &str| json!({"session_id": "c1", "tool_name": "Agent", "tool_use_id": id, "tool_input": {"description": desc, "subagent_type": ty, "prompt": "p"}});
        hook(&mut i, "PreToolUse", call("t1", "Explore", "find hooks"));
        hook(&mut i, "PreToolUse", call("t2", "general-purpose", "write tests"));
        hook(&mut i, "SubagentStart", json!({"session_id": "c1", "agent_id": "a2", "agent_type": "general-purpose"}));
        hook(&mut i, "SubagentStart", json!({"session_id": "c1", "agent_id": "a1", "agent_type": "Explore"}));
        let desc = |i: &AgentInfo| i.subagents.iter().map(|a| format!("{}={}", a.id, a.description)).collect::<Vec<_>>().join(",");
        assert_eq!(desc(&i), "a2=write tests,a1=find hooks", "matched by type, not arrival order");
        assert!(i.pending_agents.is_empty());
        // A call that fails before starting an agent leaves nothing behind.
        hook(&mut i, "PreToolUse", call("t3", "Explore", "doomed"));
        hook(&mut i, "PostToolUseFailure", json!({"session_id": "c1", "tool_name": "Agent", "tool_use_id": "t3"}));
        assert!(i.pending_agents.is_empty());
        // Two of one type swap until the async PostToolUse names each agent.
        hook(&mut i, "PreToolUse", call("t4", "Explore", "left"));
        hook(&mut i, "PreToolUse", call("t5", "Explore", "right"));
        hook(&mut i, "SubagentStart", json!({"session_id": "c1", "agent_id": "a5", "agent_type": "Explore"}));
        assert!(hook(&mut i, "PostToolUse", json!({"session_id": "c1", "tool_name": "Agent", "tool_use_id": "t5",
            "tool_response": {"isAsync": true, "agentId": "a5", "description": "right"}})));
        let a5 = i.subagents.iter().find(|a| a.id == "a5").unwrap();
        assert_eq!((a5.description.as_str(), a5.background), ("right", true));
        // Stopped ones move to finished, until the human's next prompt.
        hook(&mut i, "SubagentStop", json!({"session_id": "c1", "agent_id": "a1", "agent_type": "Explore"}));
        assert_eq!(i.finished_subagents.iter().map(|a| (a.id.as_str(), a.ended_at.is_some())).collect::<Vec<_>>(), [("a1", true)]);
        // A main-thread Stop drops calls that never started an agent.
        hook(&mut i, "Stop", json!({"session_id": "c1"}));
        assert!(i.pending_agents.is_empty());
        assert_eq!(i.finished_subagents.len(), 1, "a Stop keeps what finished");
        hook(&mut i, "UserPromptSubmit", json!({"session_id": "c1", "prompt": "next"}));
        assert!(i.finished_subagents.is_empty());
    }

    /// A real Claude 2.1.289 session (crates/midnad/tests/fixtures): a background shell, a
    /// background subagent that starts its own shell and is woken again, a session cron, the
    /// hidden prompt-suggestion agent after each turn, then SIGTERM and two resumes.
    #[test]
    fn replays_a_real_session() {
        let lines: Vec<Value> = include_str!("../tests/fixtures/claude-2.1.289-background.jsonl").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let mut i = AgentInfo::default();
        let ids = |i: &AgentInfo| i.background.iter().map(|t| t.id.clone()).collect::<Vec<_>>().join(",");
        let mut at = vec![];
        for (n, l) in lines.iter().enumerate() {
            apply_hook(&mut i, AgentKind::Claude, l["event"].as_str().unwrap(), &l["payload"], NOW);
            at.push((n, ids(&i), i.subagents.len(), i.crons.len(), i.in_flight().len()));
            if n == 10 {
                assert_eq!(i.subagents[0].description, "sleeper", "described by its Agent call (the failed worktree call has the same type)");
            }
            if n == 11 {
                assert!(i.subagents[0].background, "the async PostToolUse marks it background");
            }
        }
        let state = |n: usize| (at[n].1.as_str(), at[n].2, at[n].3);
        assert_eq!(state(5), ("bgcn3vn01", 0, 0), "background shell from the Stop snapshot");
        assert_eq!(state(6), ("bgcn3vn01", 0, 0), "the suggestion agent's SubagentStop changes nothing");
        assert_eq!(state(10).1, 1, "SubagentStart");
        assert_eq!(state(12), ("bgcn3vn01,a98dd271bfd208ddf", 0, 0), "background agent listed; the main Stop ends the live subagent");
        assert_eq!(state(15).0, "bgcn3vn01,bo2tcp0zl", "a stopping subagent drops itself; its own shell stays");
        assert_eq!(state(25), ("bgcn3vn01,bo2tcp0zl", 0, 1), "session cron");
        assert_eq!(state(28).0, "bgcn3vn01", "woken agent stopped again");
        assert_eq!(state(38).0, "bgcn3vn01");
        assert_eq!(state(41), ("", 0, 1), "resume: background gone, cron kept");
        assert_eq!(state(53).0, "bev0cz7ho");
        assert_eq!(at[48].4, 0, "nothing in flight after the resume");
        assert_eq!(i.conversation_id.as_deref(), Some(lines[1]["payload"]["session_id"].as_str().unwrap()), "resume keeps the session id");
        assert_eq!(i.version.as_deref(), Some("2.1.289"));
    }

    #[test]
    fn statusline_version_clears_a_met_update() {
        let mut i = AgentInfo::default();
        assert!(hook(&mut i, "statusline", json!({"session_id": "c1", "version": "2.1.289", "model": {"id": "claude-opus-5-5"}})));
        assert!(!hook(&mut i, "statusline", json!({"session_id": "c1", "version": "2.1.289", "model": {"id": "claude-opus-5-5"}})), "repeat is not news");
        i.update_available = Some("2.1.290".into());
        hook(&mut i, "statusline", json!({"session_id": "c1", "version": "2.1.290"}));
        assert_eq!((i.version.as_deref(), i.update_available), (Some("2.1.290"), None));
    }

    #[test]
    fn codex_thread_id() {
        let mut i = AgentInfo::default();
        assert!(apply_hook(&mut i, AgentKind::Codex, "agent-turn-complete", &json!({"type": "agent-turn-complete", "thread-id": "t-1"}), NOW));
        assert_eq!(i.conversation_id.as_deref(), Some("t-1"));
    }

    #[test]
    fn resume_commands() {
        let info = AgentInfo { conversation_id: Some("c1".into()), model: Some("claude-opus-5-5".into()), permission_mode: Some("plan".into()), ..Default::default() };
        let base: Vec<String> = ["claude", "--settings", "s.json"].map(String::from).to_vec();
        assert_eq!(
            resume_command(AgentKind::Claude, &base, &info).unwrap(),
            ["claude", "--settings", "s.json", "--model", "claude-opus-5-5", "--permission-mode", "plan", "--resume", "c1"]
        );
        let supervised: Vec<String> = ["claude", "--permission-mode", "default"].map(String::from).to_vec();
        assert_eq!(resume_command(AgentKind::Claude, &supervised, &info).unwrap(), ["claude", "--permission-mode", "default", "--model", "claude-opus-5-5", "--resume", "c1"]);
        let codex: Vec<String> = ["codex", "-c", "notify=[]"].map(String::from).to_vec();
        assert_eq!(resume_command(AgentKind::Codex, &codex, &info).unwrap(), ["codex", "resume", "-c", "notify=[]", "c1"]);
        assert!(resume_command(AgentKind::Claude, &base, &AgentInfo::default()).is_none());
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("2.1.289 (Claude Code)").as_deref(), Some("2.1.289"));
        assert_eq!(parse_version("codex-cli 0.160.0").as_deref(), Some("0.160.0"));
        assert_eq!(parse_version("nope 1.2"), None);
        assert!(newer("2.1.290", "2.1.289") && newer("2.10.0", "2.9.9") && !newer("2.1.289", "2.1.289"));
    }

    #[test]
    fn dim_text_is_not_typed_text() {
        // Bytes Claude 2.1.289 drew (100 cols, rule shortened): grey rules, then the faint
        // placeholder of a new conversation, rows joined by `\r ESC[1B`.
        let rule = "─".repeat(30);
        let screen = |input: &str| format!("\x1b[38;2;136;136;136m{rule}\r\x1b[1B\x1b[39m{input}\r\x1b[1B\x1b[22m\x1b[38;2;136;136;136m{rule}");
        let typed = |input: &str| claude_input_text(&undim(&screen(input)));
        assert_eq!(typed("❯\u{a0}\x1b[2mTry \"fix typecheck errors\"").as_deref(), Some(""), "real placeholder");
        assert_eq!(typed("❯\u{a0}").as_deref(), Some(""), "empty box");
        assert_eq!(typed("❯\u{a0}half typed").as_deref(), Some("half typed"));
        assert_eq!(typed("❯ \x1b[38;2;153;153;153mTry it\x1b[39m").as_deref(), Some(""), "grey truecolor");
        assert_eq!(typed("❯ \x1b[90mrun the tests\x1b[0m").as_deref(), Some(""), "bright-black suggestion");
        assert_eq!(typed("❯ \x1b[38;5;244mrun the tests\x1b[m").as_deref(), Some(""));
        assert_eq!(typed("❯ \x1b[38;2;215;119;87mhalf\x1b[39m typed").as_deref(), Some("half typed"), "a non-grey color is text");
        assert_eq!(typed("❯ \x1b[2mTry\x1b[0m mine").as_deref(), Some("mine"));
        assert_eq!(undim("a\x1b]8;;http://x\x07b\x1b]8;;\x1b\\c\nx\x1b[2Cy\r\x1b[2Bz"), ["abc", "x  y", "", "z"], "OSC skipped, cursor moves");
    }

    #[test]
    fn input_box() {
        let rule = "─".repeat(60);
        let screen = |input: &str| vec!["✻ Baked for 1s".to_string(), String::new(), rule.clone(), input.to_string(), rule.clone(), "  Opus · $0.10".into()];
        assert_eq!(claude_input_text(&screen("❯ ")).as_deref(), Some(""));
        assert_eq!(claude_input_text(&screen("❯ fix the flaky test")).as_deref(), Some("fix the flaky test"));
        assert_eq!(claude_input_text(&["just a shell $".to_string()]), None);
        let real: Vec<String> = include_str!("../tests/fixtures/claude-2.1.288-interrupted.screen.txt").lines().map(str::to_string).collect();
        assert_eq!(claude_input_text(&real).as_deref(), Some(""));
    }
}
