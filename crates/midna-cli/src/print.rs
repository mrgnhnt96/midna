//! Human-readable output (default). `--json` prints the raw results instead.
use serde_json::Value;

fn s(v: &Value, k: &str) -> String {
    plain(v.get(k).unwrap_or(&Value::Null))
}

/// A JSON value as plain text (strings unquoted, null empty).
pub fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.iter().all(Value::is_string) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
        v => v.to_string(),
    }
}

pub fn table(rows: Vec<Vec<String>>) {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    // A row's last cell isn't padded, so it doesn't widen its column (a note row can be long).
    let widths: Vec<usize> = (0..n).map(|i| rows.iter().filter(|r| i + 1 < r.len()).map(|r| r[i].chars().count()).max().unwrap_or(0)).collect();
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(c);
            } else {
                line.push_str(&format!("{c:<w$}  ", w = widths[i]));
            }
        }
        println!("{}", line.trim_end());
    }
}

pub fn kv(v: &Value) {
    if let Some(o) = v.as_object() {
        let w = o.keys().map(String::len).max().unwrap_or(0);
        for (k, val) in o {
            println!("{k:<w$}  {}", plain(val));
        }
    }
}

/// A terminal's status for one line: the built-in state, plus the label a local trigger put
/// on it (`needs_you · Prompt blocked`).
pub fn status_text(x: &Value) -> String {
    let state = plain(&x["status"]["state"]);
    match x["custom_status"]["label"].as_str() {
        Some(label) => format!("{state} · {label}"),
        None => state,
    }
}

pub fn sessions(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no terminals");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "STATUS".into(), "KIND".into(), "NAME".into(), "PROJECT".into(), "TITLE".into()]];
    for x in list {
        let mut kind = match x.get("agent") {
            Some(a) if !a.is_null() => plain(a),
            _ => s(&x, "kind"),
        };
        if x["background"].as_bool() == Some(true) {
            kind.push_str(" (bg)");
        }
        rows.push(vec![s(&x, "id"), status_text(&x), kind, s(&x, "name"), s(&x, "project_id"), s(&x, "title")]);
    }
    table(rows);
}

/// `midna get <id>`: one terminal, the fields worth a glance (`--json` has everything).
pub fn session(x: &Value) {
    let mut rows: Vec<(&str, String)> = vec![
        ("id", s(x, "id")),
        ("name", s(x, "name")),
        ("status", status_text(x)),
        ("reason", plain(&x["status"]["reason"])),
        ("since", plain(&x["status"]["since"])),
        ("kind", s(x, "kind")),
        ("agent", plain(&x["agent"])),
        ("project", s(x, "project_id")),
        ("cwd", s(x, "cwd")),
        ("command", x["command"].as_array().map(|c| c.iter().map(plain).collect::<Vec<_>>().join(" ")).unwrap_or_default()),
        ("pid", plain(&x["pid"])),
        ("title", s(x, "title")),
        ("branch", plain(&x["git"]["branch"])),
    ];
    let info = &x["agent_info"];
    if info.is_object() {
        rows.push(("conversation", plain(&info["conversation_id"])));
        rows.push(("version", plain(&info["version"])));
        let n = |k: &str| info[k].as_array().map_or(0, Vec::len);
        rows.push(("background", n("background").to_string()));
        rows.push(("subagents", n("subagents").to_string()));
    }
    let w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in rows.into_iter().filter(|(_, v)| !v.is_empty() && v != "null") {
        println!("{k:<w$}  {v}");
    }
}

/// `midna needs get|wait <id>`: where one item stands.
pub fn needs_you_state(v: &Value) {
    let state = s(v, "state");
    match state.as_str() {
        "open" => println!("{} open: {}", s(v, "id"), plain(&v["item"]["title"])),
        _ => {
            let how = v["resolution"]["kind"].as_str().unwrap_or("");
            let why = v["resolution"]["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
            let by = v["resolved_by"]["kind"].as_str().map(|b| format!(" by {b}")).unwrap_or_default();
            println!("{} {state}: {how}{why}{by}", s(v, "id"));
        }
    }
    if let Some(r) = v.get("result") {
        println!("the call returned: {r}");
    }
    if let Some(e) = v.get("error") {
        println!("the call failed: {}", e["message"].as_str().unwrap_or(""));
    }
}

pub fn needs_you(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("nothing needs you");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "KIND".into(), "SESSION".into(), "TITLE".into()]];
    for x in list {
        rows.push(vec![s(&x, "id"), s(&x, "kind"), s(&x, "session_id"), s(&x, "title")]);
    }
    table(rows);
}

fn scope(v: &Value) -> String {
    match (v["kind"].as_str(), v.get("id")) {
        (Some(k), Some(id)) => format!("{k}:{}", plain(id)),
        (Some(k), None) => k.into(),
        _ => String::new(),
    }
}

pub fn rules(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no rules");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "EFFECT".into(), "KIND".into(), "PATTERN".into(), "SCOPE".into(), "FIRED".into(), "NOTE".into()]];
    for x in list {
        let mut note = vec![];
        if let Some(e) = x["expires_at"].as_str() {
            note.push(format!("expires {e}"));
        }
        if x.get("removal_request").is_some_and(|r| !r.is_null()) {
            note.push("removal requested".into());
        }
        rows.push(vec![
            s(&x, "id"),
            s(&x, "effect"),
            plain(&x["matcher"]["kind"]),
            plain(&x["matcher"]["pattern"]),
            scope(&x["scope"]),
            s(&x, "fired"),
            note.join(", "),
        ]);
    }
    table(rows);
}

pub fn check(v: &Value) {
    let rule = v.get("rule").filter(|r| !r.is_null()).map(|r| format!(" (rule {})", plain(&r["id"]))).unwrap_or_else(|| format!(" ({})", s(v, "source")));
    println!("{}{rule}", s(v, "decision"));
    for t in v["trace"].as_array().into_iter().flatten() {
        println!("  {} {:<10} {}", if t["matched"].as_bool() == Some(true) { "✓" } else { "·" }, s(t, "rule_id"), s(t, "reason"));
    }
}

pub fn settings(v: &Value) {
    let mut rows = vec![vec!["KEY".into(), "VALUE".into(), "DEFAULT".into(), "".into()]];
    for x in v.as_array().into_iter().flatten() {
        let flag = if x["human_only"].as_bool() == Some(true) { "human only" } else { "" };
        rows.push(vec![s(x, "key"), s(x, "value"), s(x, "default"), flag.into()]);
    }
    table(rows);
}

fn dur(secs: i64) -> String {
    let a = secs.abs();
    let t = if a >= 3600 { format!("{}h{:02}m", a / 3600, a % 3600 / 60) } else { format!("{}m", a / 60) };
    if secs < 0 { format!("-{t}") } else { t }
}

fn totals_row(label: String, t: &Value) -> Vec<String> {
    let i = |k: &str| t[k].as_i64().unwrap_or(0);
    vec![
        label,
        i("turns").to_string(),
        i("messages").to_string(),
        format!("${:.2}", t["spend_usd"].as_f64().unwrap_or(0.0)),
        dur(i("working_secs")),
        dur(i("waiting_secs")),
        i("approvals").to_string(),
        i("triggers_fired").to_string(),
    ]
}

pub fn insights(v: &Value) {
    println!("{} ({} → {})", s(v, "range"), s(v, "from"), s(v, "to"));
    let mut rows = vec![["", "TURNS", "MSGS", "SPEND", "WORKING", "WAITING", "APPROVALS", "TRIGGERS"].map(String::from).to_vec()];
    rows.push(totals_row("total".into(), &v["totals"]));
    let d = &v["vs_previous"];
    rows.push(totals_row("vs prev".into(), d));
    for r in v["rows"].as_array().into_iter().flatten() {
        rows.push(totals_row(format!("  {}", s(r, "label")), &r["totals"]));
    }
    table(rows);
}

/// `insights detail`: one line per widget.
pub fn insights_detail(v: &Value) {
    let i = |v: &Value, k: &str| v[k].as_i64().unwrap_or(0);
    println!("{} → {}", s(v, "from"), s(v, "to"));
    // " at <when>" / " (<session>)" only when there is one
    let at = |x: &Value, k: &str| x[k].as_str().filter(|t| !t.is_empty()).map(|t| format!(" at {t}")).unwrap_or_default();
    let none = |list: String| if list.is_empty() { "none".to_string() } else { list };
    let c = &v["concurrency"];
    if i(c, "peak") == 0 {
        println!("agents at once  none working");
    } else {
        println!("agents at once  peak {}{}, avg {} while working, {} with 2+", i(c, "peak"), at(c, "peak_at"), plain(&c["avg_while_working"]), dur(i(c, "multi_secs")));
    }
    let t = &v["turns"];
    let bins: Vec<String> = t["bins"].as_array().into_iter().flatten().map(plain).collect();
    let longest_in = t["longest_session"].as_str().map(|sid| format!(" ({sid})")).unwrap_or_default();
    println!("turns           <1m/5m/15m/30m/1h/1h+ {}, median {}, longest {}{longest_in}", bins.join("/"), dur(i(t, "median_secs")), dur(i(t, "longest_secs")));
    let waits = v["waits"].as_array().cloned().unwrap_or_default();
    println!("waits           {} answered, {} total", waits.len(), dur(waits.iter().map(|w| i(w, "secs")).sum()));
    let counts = |k: &str, f: &dyn Fn(&Value) -> String| none(v[k].as_array().into_iter().flatten().take(5).map(|x| format!("{} {}", s(x, "label"), f(&x["value"]))).collect::<Vec<_>>().join(", "));
    println!("approved        {}", counts("approved", &plain));
    let (denied, stopped) = v["corrections"].as_array().into_iter().flatten().fold((0, 0), |(d, st), x| (d + i(x, "denied"), st + i(x, "stopped")));
    println!("corrections     {denied} denied, {stopped} stopped");
    println!("projects        {}", counts("projects", &|x| dur(x.as_f64().unwrap_or(0.0) as i64)));
    println!("models          {}", counts("models", &|x| format!("${:.2}", x.as_f64().unwrap_or(0.0))));
    let b = &v["bests"];
    if i(b, "peak_agents") == 0 && i(b, "longest_turn_secs") == 0 {
        println!("records         none yet");
    } else {
        println!(
            "records         busiest day {} ({}), longest turn {}{}, peak {} agents{}",
            s(b, "busiest_day"), dur(i(b, "busiest_day_secs")), dur(i(b, "longest_turn_secs")), at(b, "longest_turn_at"), i(b, "peak_agents"), at(b, "peak_at")
        );
    }
    let mut rows = vec![["TERMINAL", "WORKING", "BLOCKED", "IDLE"].map(String::from).to_vec()];
    for r in v["agent_time"].as_array().into_iter().flatten() {
        rows.push(vec![s(r, "label"), dur(i(r, "working_secs")), dur(i(r, "blocked_secs")), dur(i(r, "idle_secs"))]);
    }
    if rows.len() > 1 {
        table(rows);
    }
}

pub fn event(e: &Value) {
    let who = match (e["actor"]["kind"].as_str(), e["actor"]["session"].as_str()) {
        (Some(k), Some(sid)) => format!("{k}:{sid}"),
        (Some(k), None) => k.into(),
        _ => String::new(),
    };
    let target = e["session_id"].as_str().or(e["project_id"].as_str()).unwrap_or("");
    let mut data = e["data"].to_string();
    if data.chars().count() > 160 {
        data = format!("{}…", data.chars().take(160).collect::<String>());
    }
    println!("{:>6} {} {:<24} {:<14} {:<9} {}", s(e, "seq"), s(e, "at"), s(e, "kind"), who, target, data);
}

/// `session.prompts`: one line per prompt, ▸ on the one the agent's view is in.
pub fn prompts(v: &Value) {
    let list = v["prompts"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no prompts");
        return;
    }
    let here = v["here"].as_u64();
    for p in &list {
        let n = p["n"].as_u64().unwrap_or(0);
        let mark = if Some(n) == here { "▸" } else { " " };
        let time = s(p, "at").get(11..16).unwrap_or("").to_string();
        let text: String = s(p, "text").lines().next().unwrap_or("").chars().take(100).collect();
        let gone = if p["on_screen"].as_bool() == Some(false) { "  (before /clear)" } else { "" };
        println!("{mark} {n:>3}  {time}  {text}{gone}");
    }
    println!("{}", if v["scrolled"].as_bool() == Some(true) { "(scrolled back)" } else { "(live)" });
}

/// `in 2h 5m` / `5m ago` for an RFC 3339 time.
fn relative(ts: &str) -> String {
    let Some(t) = midna_proto::time::parse_rfc3339(ts) else { return ts.to_string() };
    let d = t - midna_proto::time::now_unix();
    let span = |s: i64| match s {
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}d {}h", s / 86400, s % 86400 / 3600),
    };
    if d >= 0 { format!("in {}", span(d)) } else { format!("{} ago", span(-d)) }
}

/// `midna usage`.
pub fn keep_awake(v: &Value) {
    println!("{}", s(v, "line"));
    let st = &v["settings"];
    let mode = match st["mode"].as_str() {
        Some("always") => "always during hours".to_string(),
        _ => format!("only with work (lingers {} min)", st["linger_mins"]),
    };
    println!("  {:<8} {}{}", "enabled", st["enabled"], if st["enabled"] == true { "" } else { "  (midna keep-awake on)" });
    println!("  {:<8} {}", "hours", s(v, "schedule"));
    println!("  {:<8} {mode}", "mode");
    if let Some(t) = v["today"].as_object() {
        println!("  {:<8} {}", "today", t.get("line").and_then(Value::as_str).unwrap_or_default());
    }
    // 12-hour, like the app: `Fri Oct 9 9 AM`.
    let at12 = |t: i64| {
        let (_, _, _, h, mi, _) = midna_proto::time::local_parts(t);
        let day = midna_proto::cron::local_label(t);
        format!("{} {}", day.rsplit_once(' ').map_or(day.as_str(), |(d, _)| d), midna_proto::cron::clock(h, mi))
    };
    let next = |k: &str| v[k].as_str().and_then(midna_proto::time::parse_rfc3339).map(at12);
    if let Some(at) = next("next_off") {
        println!("  {:<8} hours end {at}", "next");
    } else if let Some(at) = next("next_on") {
        println!("  {:<8} hours start {at}", "next");
    }
    let min = st["min_battery"].as_i64().unwrap_or_default();
    let limit = if min > 0 { format!(" (stops below {min}% on battery)") } else { String::new() };
    match v["battery"].as_object() {
        Some(b) => {
            let power = if b.get("on_ac") == Some(&Value::Bool(true)) { "on power" } else { "on battery" };
            println!("  {:<8} {}% {power}{limit}", "battery", b.get("percent").and_then(Value::as_i64).unwrap_or_default());
        }
        None => println!("  {:<8} none{limit}", "battery"),
    }
    if let Some(since) = v["held_since"].as_str().and_then(midna_proto::time::parse_rfc3339) {
        println!("  {:<8} holding since {}", "held", at12(since));
    }
    if let Some(line) = v["wake"]["line"].as_str() {
        println!("  {:<8} {line}", "wake");
    }
}

pub fn usage(v: &Value) {
    let Some(c) = v.get("claude").filter(|c| c.is_object()) else {
        println!("no usage limits reported yet (Claude's status line reports them on Pro/Max plans; setting agents.claude.statusline)");
        return;
    };
    println!("claude plan usage, observed {} in {}", relative(&s(c, "observed_at")), s(c, "session"));
    for (key, label) in [("five_hour", "5-hour"), ("seven_day", "weekly")] {
        let w = &c[key];
        if !w.is_object() {
            continue;
        }
        let resets = w["resets_at"].as_str().map(|r| format!("  resets {r} ({})", relative(r))).unwrap_or_default();
        let stale = if w["expired"] == true { "  (reset since: out of date)" } else { "" };
        let used = w["used_percentage"].as_f64().unwrap_or(0.0);
        let used = if used.fract() == 0.0 { format!("{used:.0}") } else { format!("{used:.1}") };
        println!("  {label:<7} {used:>5}%{resets}{stale}");
    }
    if c["limited"] == true {
        let until = c["limited_until"].as_str().map(|u| format!(" until {u} ({})", relative(u))).unwrap_or_default();
        println!("  LIMITED{until}");
    }
}

/// `midna system`.
pub fn system_load(v: &Value) {
    let state = if v["overloaded"].as_bool() == Some(true) {
        format!("overloaded (busy {} min)", v["busy_for_secs"].as_u64().unwrap_or(0) / 60)
    } else if v["busy"].as_bool() == Some(true) {
        "busy".into()
    } else {
        "calm".into()
    };
    println!(
        "load {:.2} {:.2} {:.2} on {} cores: {}% per core, {state} (busy at {}%, setting system.busy_load)",
        v["load1"].as_f64().unwrap_or(0.),
        v["load5"].as_f64().unwrap_or(0.),
        v["load15"].as_f64().unwrap_or(0.),
        v["cpus"],
        v["load_percent"],
        v["busy_at"]
    );
    let top = v["top"].as_array().cloned().unwrap_or_default();
    if top.is_empty() {
        println!("no terminal is using much CPU");
        return;
    }
    println!("{:<12} {:>6} {:>6}  {}", "TERMINAL", "CPU%", "PROCS", "NAME / BUSIEST");
    for t in top {
        let busiest = t["busiest"].as_array().map(|b| b.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let paused = if t["paused"].as_bool() == Some(true) { " [paused]" } else { "" };
        let slowed = if t["backgrounded"].as_bool() == Some(true) { " [slowed]" } else { "" };
        println!("{:<12} {:>6} {:>6}  {}{paused}{slowed}  ({busiest})", s(&t, "session_id"), t["cpu_percent"], t["processes"], s(&t, "name"));
    }
    println!("pause one with `midna system pause <terminal>`, stop what it started with `midna system stop <terminal>`");
}

/// `midna worktrees`.
pub fn worktrees(v: &Value) {
    let hours = v["auto_clean_hours"].as_u64().unwrap_or(0);
    let list = v["worktrees"].as_array().cloned().unwrap_or_default();
    let repos = v["repos"].as_array().map(|r| r.len()).unwrap_or(0);
    let rule = if hours == 0 { "auto-clean off (worktrees.auto_clean_hours = 0)".to_string() } else { format!("removed after {hours}h idle") };
    println!("{} worktree(s) in {repos} watched repo(s); {rule}", list.len());
    for w in list {
        let verdict = if w["removable"].as_bool() == Some(true) { "removable now".to_string() } else { format!("kept: {}", s(&w, "keep_because")) };
        println!("{}  [{}]  idle {}h  {verdict}", s(&w, "path"), s(&w, "branch"), w["idle_hours"]);
    }
}

/// `midna worktrees clean`.
pub fn worktrees_clean(v: &Value) {
    let dry = v["dry_run"].as_bool() == Some(true);
    let removed = v["removed"].as_array().cloned().unwrap_or_default();
    if removed.is_empty() {
        println!("nothing to remove (`midna worktrees` says why each is kept)");
    }
    for w in removed {
        println!("{} {}  [{}, branch kept]", if dry { "would remove" } else { "removed" }, s(&w, "path"), s(&w, "branch"));
    }
    for f in v["failed"].as_array().cloned().unwrap_or_default() {
        println!("kept {}: {}", s(&f, "path"), s(&f, "error"));
    }
}

/// `midna cleanup items`.
pub fn cleanup_items(v: &Value) {
    use midna_proto::cleanup as cl;
    let items: Vec<String> = v["items"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let (on, own) = cl::split_items(&items);
    let enabled = v["enabled"].as_bool() != Some(false);
    println!("cleanup after a closed terminal: {}", if enabled { "on" } else { "off (`midna cleanup enable`)" });
    for b in cl::BUILTINS {
        println!("  [{}] {:<14} {}", if on.contains(b) { "x" } else { " " }, b, cl::builtin_label(b).unwrap_or(""));
    }
    if own.is_empty() {
        println!("your own items: none (`midna cleanup add \"…\"`)");
    } else {
        println!("your own items:");
        for (i, item) in own.iter().enumerate() {
            println!("  {}. {item}", i + 1);
        }
    }
    let keep: Vec<&str> = v["keep"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    println!("never touched: {}", if keep.is_empty() { "the default branch only".to_string() } else { format!("{} (and the default branch)", keep.join(", ")) });
}

fn cleanup_line(r: &Value) -> String {
    let state = match s(r, "state").as_str() {
        "running" => "running".to_string(),
        "failed" => "failed".to_string(),
        _ => format!("{} removed, {} kept", r["removed"].as_array().map_or(0, Vec::len), r["kept"].as_array().map_or(0, Vec::len)),
    };
    let dry = if r["dry_run"].as_bool() == Some(true) { " (dry run)" } else { "" };
    format!("{}  {}  {:<28} {state}{dry}", s(r, "id"), s(r, "started_at"), format!("{} ({})", s(r, "session_name"), s(r, "session_id")))
}

/// `midna cleanup runs`.
pub fn cleanup_runs(v: &Value) {
    let runs = v["runs"].as_array().cloned().unwrap_or_default();
    if runs.is_empty() {
        println!("no cleanups yet");
    }
    for r in runs {
        println!("{}", cleanup_line(&r));
    }
}

/// `midna cleanup show <id>` and `run`.
pub fn cleanup_run(r: &Value) {
    println!("{}", cleanup_line(r));
    if !s(r, "repo").is_empty() {
        println!("repo {}  model {}", s(r, "repo"), s(r, "model"));
    }
    let list = |k: &str| r[k].as_array().cloned().unwrap_or_default();
    for t in list("targets") {
        println!("  target  {} {}", s(&t, "kind"), s(&t, "name"));
    }
    for i in list("items") {
        println!("  item    {}", plain(&i));
    }
    let dry = r["dry_run"].as_bool() == Some(true);
    for x in list("removed") {
        println!("{} {}", if dry { "would remove" } else { "removed" }, plain(&x));
    }
    for x in list("kept") {
        println!("kept    {}", plain(&x));
    }
    for l in s(r, "summary").lines().filter(|l| l.starts_with("DENIED: ")) {
        println!("denied  {}", &l[8..]);
    }
    if let Some(c) = r["cost_usd"].as_f64() {
        println!("cost    ${c:.4}");
    }
    if !s(r, "error").is_empty() {
        println!("error   {}", s(r, "error"));
    }
}

/// `midna cleanup preview`.
pub fn cleanup_plan(p: &Value) {
    match p["would_run"].as_bool() {
        Some(true) => println!("closing {} would clean up with {}:", s(p, "session_id"), s(p, "model")),
        _ => println!("closing {} would clean up nothing: {}", s(p, "session_id"), s(p, "reason")),
    }
    for t in p["targets"].as_array().cloned().unwrap_or_default() {
        println!("  target  {} {}", s(&t, "kind"), s(&t, "name"));
    }
    for i in p["items"].as_array().cloned().unwrap_or_default() {
        println!("  item    {}", plain(&i));
    }
    for k in p["skipped"].as_array().cloned().unwrap_or_default() {
        println!("  left    {}", plain(&k));
    }
}
