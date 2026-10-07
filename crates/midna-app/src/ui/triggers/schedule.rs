//! The Triggers screen's Crons tab and its schedule editor (docs/design/Crons-D.dc.html).
//!
//! Crons are Claude's session crons (`AgentInfo.crons`). Claude owns them, so they're read-only
//! here: the detail can open the terminal, queue a request for Claude to cancel one, or copy it
//! into a local `schedule` trigger. The editor makes or changes such a trigger: once or
//! repeating, a time-of-day window, days, a start, an end (a date or a run count) and, for a new
//! one, the terminal and prompt it sends.
use super::*;
use midna_proto::cron::{self, Schedule};
use midna_proto::{AgentCron, TimeWindow, TriggerFilter, time};

// ------------------------------------------------------------------ crons

/// One of Claude's session crons and the terminal it lives in.
#[derive(Clone, Debug)]
pub struct CronRow {
    pub sid: String,
    pub term: String,
    pub cron: AgentCron,
    pub next: Option<i64>,
}

impl CronRow {
    pub fn key(&self) -> String {
        format!("{}/{}", self.sid, self.cron.id)
    }

}

/// Every terminal's crons, soonest first.
pub fn cron_rows(m: &MainWindow) -> Vec<CronRow> {
    let now = time::now_unix();
    let mut rows: Vec<CronRow> = m
        .sessions
        .iter()
        .flat_map(|s| {
            s.agent_info.iter().flat_map(|i| i.crons.iter()).map(move |c| CronRow { sid: s.id.clone(), term: s.name.clone(), cron: c.clone(), next: next_run(c, now) })
        })
        .collect();
    rows.sort_by(|a, b| (a.next.unwrap_or(i64::MAX), &a.term).cmp(&(b.next.unwrap_or(i64::MAX), &b.term)));
    rows
}

fn next_run(c: &AgentCron, now: i64) -> Option<i64> {
    cron::Cron::parse(&c.schedule).ok()?.next_after(now).filter(|t| c.expires_at().is_none_or(|e| *t <= e))
}

/// `Today 2:30 PM`, `Tomorrow 9 AM`, `Fri Oct 10 9 AM`, `Oct 6, 2027 4:17 PM`.
fn when_label(t: i64) -> String {
    let (y, _, _, h, mi, _) = time::local_parts(t);
    let now = time::now_unix();
    let today = time::local_day_start(now);
    let day = match (time::local_day_start(t) - today) / 86_400 {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ if y != time::local_parts(now).0 => format!("{}, {y}", cron::local_day(t)),
        _ => {
            let label = cron::local_label(t);
            label.rsplit_once(' ').map(|(d, _)| d.to_string()).unwrap_or(label)
        }
    };
    format!("{day} {}", cron::clock(h, mi))
}

/// `Once` (amber) or `↻ Repeats` (accent).
fn pill(t: &Theme, once: bool) -> Div {
    let c = if once { t.need } else { t.accent };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(7.))
        .h(px(18.))
        .rounded(px(9.))
        .bg(c.opacity(0.14))
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(c)
        .when(!once, |d| d.child(Icon::Restart.el(10., c)))
        .child(if once { "Once" } else { "Repeats" })
}

// ------------------------------------------------------------------ the editor

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Minutes,
    Hours,
    Daily,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ends {
    Never,
    OnDate,
    AfterRuns,
}

/// Monday first, as the day buttons show them; the cron weekday of each.
const DAY_LETTERS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
const DAY_CRON: [u32; 7] = [1, 2, 3, 4, 5, 6, 0];

pub struct ScheduleEditor {
    /// The trigger being changed; None makes a new one.
    trigger_id: Option<String>,
    /// Its filter as it was: session/project/agent/match stay; the schedule fields are replaced.
    base: TriggerFilter,
    fired: u64,
    /// "Copied from a cron in api-refactor".
    note: Option<String>,
    name: LineInput,
    once: bool,
    at: LineInput,
    unit: Unit,
    every: LineInput,
    daily_at: LineInput,
    /// A cron the form can't show (kept as typed).
    custom: bool,
    cron: LineInput,
    window: bool,
    from: LineInput,
    until: LineInput,
    days: [bool; 7],
    starts: LineInput,
    ends: Ends,
    ends_at: LineInput,
    runs: LineInput,
    /// New triggers: the terminal the prompt goes to.
    session: Option<String>,
    prompt: LineInput,
    _subs: Vec<Subscription>,
}

/// What the form adds up to.
#[derive(Debug)]
struct Built {
    name: String,
    filter: TriggerFilter,
    schedule: Schedule,
}

impl ScheduleEditor {
    fn new(cx: &mut Context<TriggersView>) -> ScheduleEditor {
        let field = |cx: &mut Context<TriggersView>, ph: &str| LineInput::new(cx, false, ph.to_string());
        let mut e = ScheduleEditor {
            trigger_id: None,
            base: TriggerFilter::default(),
            fired: 0,
            note: None,
            name: field(cx, "Name"),
            once: false,
            at: field(cx, "2026-10-06 14:30"),
            unit: Unit::Minutes,
            every: field(cx, "5"),
            daily_at: field(cx, "09:00"),
            custom: false,
            cron: field(cx, "*/5 * * * *"),
            window: false,
            from: field(cx, "13:00"),
            until: field(cx, "17:00"),
            days: [true; 7],
            starts: field(cx, "Now"),
            ends: Ends::Never,
            ends_at: field(cx, "2026-10-31"),
            runs: field(cx, "10"),
            session: None,
            prompt: field(cx, "The prompt to send"),
            _subs: vec![],
        };
        e.every.set_text("5", cx);
        e.daily_at.set_text("09:00", cx);
        e.from.set_text("13:00", cx);
        e.until.set_text("17:00", cx);
        // Retyping any field refreshes the summary.
        let fields: Vec<Entity<crate::ui::text_input::TextField>> = e.inputs().iter().map(|i| i.field.clone()).collect();
        e._subs = fields.iter().map(|f| cx.observe(f, |_, _, cx| cx.notify())).collect();
        e
    }

    fn inputs(&self) -> [&LineInput; 11] {
        [&self.name, &self.at, &self.every, &self.daily_at, &self.cron, &self.from, &self.until, &self.starts, &self.ends_at, &self.runs, &self.prompt]
    }

    /// A copy of one of Claude's crons, sent to the same terminal.
    fn from_cron(row: &CronRow, cx: &mut Context<TriggersView>) -> ScheduleEditor {
        let mut e = ScheduleEditor::new(cx);
        let name: String = row.cron.prompt.lines().next().unwrap_or("").chars().take(48).collect();
        e.name.set_text(&name, cx);
        e.prompt.set_text(&row.cron.prompt, cx);
        e.session = Some(row.sid.clone());
        e.note = Some(format!("Copied from a cron in {}", row.term));
        e.load_cron(&row.cron.schedule, cx);
        // A one-time cron without a pinned date: its next match, then done.
        if !row.cron.recurring && !e.once {
            e.ends = Ends::AfterRuns;
            e.runs.set_text("1", cx);
        }
        e
    }

    /// An existing local schedule trigger.
    fn from_trigger(tr: &TriggerItem, cx: &mut Context<TriggersView>) -> ScheduleEditor {
        let mut e = ScheduleEditor::new(cx);
        e.trigger_id = Some(tr.id.clone());
        e.base = serde_json::to_value(&tr.filter).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        e.fired = tr.fired;
        e.name.set_text(&tr.name, cx);
        e.load_cron(tr.filter.cron.as_deref().unwrap_or("*/5 * * * *"), cx);
        let f = &tr.filter;
        if let Some(w) = &f.window {
            e.window = true;
            e.from.set_text(&w.from, cx);
            e.until.set_text(&w.until, cx);
        }
        let local = |at: &Option<String>| at.as_deref().and_then(time::parse_rfc3339).map(time::format_local);
        match f.max_runs {
            Some(1) if e.once => {}
            Some(n) => {
                e.once = false;
                e.ends = Ends::AfterRuns;
                e.runs.set_text(&n.to_string(), cx);
            }
            None => e.once = false,
        }
        if !e.once {
            if let Some(s) = local(&f.starts_at) {
                e.starts.set_text(&s, cx);
            }
            if let Some(s) = local(&f.ends_at) {
                e.ends = Ends::OnDate;
                e.ends_at.set_text(&s, cx);
            }
        }
        e
    }

    /// Fill the form from a cron expression; one it can't show stays as a custom cron.
    fn load_cron(&mut self, expr: &str, cx: &mut Context<TriggersView>) {
        self.cron.set_text(expr.trim(), cx);
        let Some(r) = read_cron(expr, time::now_unix()) else {
            self.custom = true;
            return;
        };
        self.custom = false;
        self.days = r.days;
        match r.shape {
            Shape::Once(at) => {
                self.once = true;
                self.at.set_text(&time::format_local(at), cx);
            }
            Shape::Every(unit, n) => {
                self.unit = unit;
                self.every.set_text(&n.to_string(), cx);
            }
            Shape::Daily(h, m) => {
                self.unit = Unit::Daily;
                self.daily_at.set_text(&format!("{h:02}:{m:02}"), cx);
            }
        }
        if let Some((a, b)) = r.window {
            self.window = true;
            self.from.set_text(&a, cx);
            self.until.set_text(&b, cx);
        }
    }

    /// The form's values as typed.
    fn form(&self, cx: &App) -> Form {
        let text = |i: &LineInput| i.text(cx).trim().to_string();
        Form {
            new: self.trigger_id.is_none(),
            name: text(&self.name),
            once: self.once,
            at: text(&self.at),
            unit: self.unit,
            every: text(&self.every),
            daily_at: text(&self.daily_at),
            custom: self.custom,
            cron: text(&self.cron),
            window: self.window,
            from: text(&self.from),
            until: text(&self.until),
            days: self.days,
            starts: text(&self.starts),
            ends: self.ends,
            ends_at: text(&self.ends_at),
            runs: text(&self.runs),
            session: self.session.clone(),
            prompt: text(&self.prompt),
        }
    }

    /// The trigger the form describes, or what's wrong with it.
    fn build(&self, cx: &App) -> Result<Built, String> {
        build(&self.form(cx), &self.base, self.fired, time::now_unix())
    }
}

/// What a cron expression looks like in the form.
#[derive(Clone, Debug, PartialEq)]
enum Shape {
    /// A pinned date: its next occurrence.
    Once(i64),
    Every(Unit, u32),
    Daily(u32, u32),
}

#[derive(Clone, Debug, PartialEq)]
struct ReadCron {
    shape: Shape,
    days: [bool; 7],
    /// `HH:MM` from and until, from an hour range (`*/5 13-16` → 13:00–17:00).
    window: Option<(String, String)>,
}

/// Read a cron into the form's terms, or None when the form can't show it.
fn read_cron(expr: &str, now: i64) -> Option<ReadCron> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return None;
    }
    let num = |s: &str| s.parse::<u32>().ok();
    let step = |s: &str| if s == "*" { Some(1) } else { s.strip_prefix("*/").and_then(num) };
    if let (Some(mi), Some(h), Some(d), Some(mo), "*") = (num(f[0]), num(f[1]), num(f[2]), num(f[3]), f[4]) {
        let (y, ..) = time::local_parts(now);
        let mut at = time::local_unix(y, mo, d, h, mi);
        if at <= now {
            at = time::local_unix(y + 1, mo, d, h, mi);
        }
        return Some(ReadCron { shape: Shape::Once(at), days: [true; 7], window: None });
    }
    if f[2] != "*" || f[3] != "*" {
        return None;
    }
    let days = parse_days(f[4])?;
    let (shape, window) = match (f[0], f[1]) {
        (m, "*") if step(m).is_some() => (Shape::Every(Unit::Minutes, step(m)?), None),
        (m, h) if step(m).is_some() && h.contains('-') => {
            let (a, b) = h.split_once('-')?;
            let (a, b) = (num(a)?, num(b)?);
            (Shape::Every(Unit::Minutes, step(m)?), Some((format!("{a:02}:00"), format!("{:02}:00", (b + 1) % 24))))
        }
        ("0", h) if step(h).is_some() => (Shape::Every(Unit::Hours, step(h)?), None),
        (m, h) => {
            let (m, h) = (num(m)?, num(h)?);
            if m > 59 || h > 23 {
                return None;
            }
            (Shape::Daily(h, m), None)
        }
    };
    Some(ReadCron { shape, days, window })
}

/// The form's values as typed.
#[derive(Clone, Debug)]
struct Form {
    /// A new trigger (it needs a terminal and a prompt).
    new: bool,
    name: String,
    once: bool,
    at: String,
    unit: Unit,
    every: String,
    daily_at: String,
    custom: bool,
    cron: String,
    window: bool,
    from: String,
    until: String,
    days: [bool; 7],
    starts: String,
    ends: Ends,
    ends_at: String,
    runs: String,
    session: Option<String>,
    prompt: String,
}

/// The trigger filter (and name) a form adds up to, on top of `base`.
fn build(form: &Form, base: &TriggerFilter, fired: u64, now: i64) -> Result<Built, String> {
    let mut f = base.clone();
    f.window = None;
    f.starts_at = None;
    f.ends_at = None;
    f.max_runs = None;
    let dow = {
        let on: Vec<String> = (0..7).filter(|i| form.days[*i]).map(|i| DAY_CRON[i].to_string()).collect();
        match on.len() {
            0 => return Err("Pick at least one day.".into()),
            7 => "*".to_string(),
            _ => on.join(","),
        }
    };
    let cron = if form.once {
        let at = time::parse_local(&form.at).ok_or("At: use a local date and time, e.g. 2026-10-06 14:30.")?;
        if at <= now {
            return Err("At: pick a time in the future.".into());
        }
        let (_, mo, d, h, mi, _) = time::local_parts(at);
        f.starts_at = Some(time::format_unix(at));
        f.max_runs = Some(1);
        format!("{mi} {h} {d} {mo} *")
    } else {
        let cron = if form.custom {
            form.cron.clone()
        } else {
            match form.unit {
                Unit::Minutes | Unit::Hours => {
                    let (max, what) = if form.unit == Unit::Minutes { (59, "minutes") } else { (23, "hours") };
                    let n: u32 = form.every.parse().ok().filter(|n| (1..=max).contains(n)).ok_or(format!("Every: a number of {what} from 1 to {max}."))?;
                    let every = if n == 1 { "*".to_string() } else { format!("*/{n}") };
                    if form.unit == Unit::Minutes { format!("{every} * * * {dow}") } else { format!("0 {every} * * {dow}") }
                }
                Unit::Daily => {
                    let m = cron::parse_hm(&form.daily_at).map_err(|e| format!("At: {e}"))?;
                    format!("{} {} * * {dow}", m % 60, m / 60)
                }
            }
        };
        if form.window && form.unit != Unit::Daily && !form.custom {
            f.window = Some(TimeWindow { from: form.from.clone(), until: form.until.clone() });
        }
        if !form.starts.is_empty() && !form.starts.eq_ignore_ascii_case("now") {
            let s = time::parse_local(&form.starts).ok_or("Starts: use a local date and time, e.g. 2026-10-06 13:00.")?;
            f.starts_at = Some(time::format_unix(s));
        }
        match form.ends {
            Ends::Never => {}
            Ends::OnDate => {
                let e = time::parse_local(&form.ends_at).ok_or("Ends: use a local date, e.g. 2026-10-31 (or 2026-10-31 17:00).")?;
                f.ends_at = Some(time::format_unix(e));
            }
            Ends::AfterRuns => {
                let n: u64 = form.runs.parse().ok().filter(|n| *n > 0).ok_or("Ends: a number of runs, at least 1.")?;
                f.max_runs = Some(n);
            }
        }
        cron
    };
    f.cron = Some(cron);
    if form.new {
        f.session = Some(form.session.clone().ok_or("Send to: pick a terminal.")?);
        if form.prompt.is_empty() {
            return Err("Write the prompt to send.".into());
        }
    }
    // A changed run limit counts from now (the daemon resets the count).
    let fired = if f.max_runs == base.max_runs { fired } else { 0 };
    let schedule = Schedule::of(&f, fired).map_err(|e| e.replace("filter.", ""))?;
    let name = if form.name.is_empty() { form.prompt.chars().take(48).collect() } else { form.name.clone() };
    if name.is_empty() {
        return Err("Give it a name.".into());
    }
    Ok(Built { name, filter: f, schedule })
}

/// A cron day-of-week field as Monday-first switches.
fn parse_days(s: &str) -> Option<[bool; 7]> {
    if s == "*" {
        return Some([true; 7]);
    }
    let names = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let day = |x: &str| -> Option<u32> { x.parse::<u32>().ok().filter(|n| *n <= 7).map(|n| n % 7).or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(x)).map(|n| n as u32)) };
    let mut on = [false; 7];
    for part in s.split(',') {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (day(a)?, day(b)?),
            None => (day(part)?, day(part)?),
        };
        let b = if b < a { b + 7 } else { b };
        for d in a..=b {
            on[DAY_CRON.iter().position(|c| *c == d % 7)?] = true;
        }
    }
    Some(on)
}

// ------------------------------------------------------------------ the view

impl TriggersView {
    /// Open the editor: on a trigger, a copy of a cron, or blank.
    pub(super) fn open_editor(&mut self, tr: Option<&TriggerItem>, cron: Option<&CronRow>, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let mut e = match (tr, cron) {
            (Some(tr), _) => ScheduleEditor::from_trigger(tr, cx),
            (None, Some(c)) => ScheduleEditor::from_cron(c, cx),
            (None, None) => {
                let mut e = ScheduleEditor::new(cx);
                e.session = self.main.upgrade().and_then(|m| {
                    let m = m.read(cx);
                    m.selected_session().filter(|s| s.agent.is_some()).or_else(|| m.sessions.iter().find(|s| s.agent.is_some())).map(|s| s.id.clone())
                });
                e
            }
        };
        if e.trigger_id.is_none() && e.session.is_none() {
            e.session = self.agent_terminals(cx).first().map(|(id, _)| id.clone());
        }
        let focus = e.name.focus.clone();
        self.editor = Some(e);
        self.tab = Tab::Triggers;
        if let Some(window) = window {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Terminals running an agent: where a new schedule can send its prompt.
    fn agent_terminals(&self, cx: &App) -> Vec<(String, String)> {
        self.main.upgrade().map(|m| m.read(cx).sessions.iter().filter(|s| s.agent.is_some()).map(|s| (s.id.clone(), s.name.clone())).collect()).unwrap_or_default()
    }

    fn save_schedule(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.editor else { return };
        let built = match e.build(cx) {
            Ok(b) => b,
            Err(err) => return self.toast(err, cx),
        };
        let filter = serde_json::to_value(&built.filter).unwrap_or_default();
        match e.trigger_id.clone() {
            Some(id) => self.call("trigger.update", json!({ "id": id, "name": built.name, "filter": filter }), cx, |v, _, cx| {
                v.editor = None;
                v.toast("Schedule saved".into(), cx);
                v.refetch(cx);
            }),
            None => {
                let prompt = e.prompt.text(cx).trim().to_string();
                let params = json!({
                    "name": built.name,
                    "source": "local",
                    "event": "schedule",
                    "filter": filter,
                    "action": { "kind": "send_to_session", "steps": [{ "text": prompt }] },
                    "enabled": true,
                });
                self.call("trigger.add", params, cx, |v, t, cx| {
                    v.editor = None;
                    v.tab = Tab::Triggers;
                    v.selected = t.get("id").and_then(|x| x.as_str()).map(String::from);
                    v.toast("Schedule trigger added".into(), cx);
                    v.refetch(cx);
                })
            }
        }
    }

    fn cancel_cron(&mut self, row: CronRow, cx: &mut Context<Self>) {
        let text = format!("Please cancel your scheduled cron {} (CronDelete with id {}).", row.cron.id, row.cron.id);
        let term = row.term.clone();
        self.call("queue.add", json!({ "session": row.sid, "text": text }), cx, move |v, _, cx| {
            v.toast(format!("Asked Claude in {term} to cancel it; it goes in when Claude is ready."), cx);
        });
    }

    pub(super) fn render_cron_list(&self, t: &Theme, rows: &[CronRow], sel: Option<String>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut list = div().id("cron-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().pt(px(4.)).pb(px(10.));
        if rows.is_empty() {
            return list.child(
                div()
                    .px(px(16.))
                    .py(px(12.))
                    .text_size(px(12.))
                    .text_color(t.dim)
                    .child("No crons right now. When Claude schedules a prompt in one of your terminals (its CronCreate tool), it shows here."),
            );
        }
        for r in rows {
            let key = r.key();
            let on = sel.as_deref() == Some(key.as_str());
            let when = cron::describe(&r.cron.schedule);
            list = list.child(
                div()
                    .id(SharedString::from(format!("cron-{key}")))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pl(px(14.))
                    .pr(px(16.))
                    .py(px(8.))
                    .border_l_2()
                    .border_color(if on { t.accent } else { transparent_black() })
                    .when(on, |d| d.bg(t.raised))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.raised))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.sel_cron = Some(key.clone());
                        v.editor = None;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::BOLD).child(r.cron.prompt.lines().next().unwrap_or("").to_string()))
                            .child(pill(t, !r.cron.recurring)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.5))
                            .text_color(t.dim)
                            .child(div().flex_1().min_w_0().truncate().child(format!("{when} · {}", r.term)))
                            .children(r.next.map(|n| div().whitespace_nowrap().child(when_label(n)))),
                    ),
            );
        }
        list
    }

    pub(super) fn render_cron_detail(&self, t: &Theme, row: Option<&CronRow>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pane = div().id("cron-detail").flex_1().min_w_0().overflow_y_scroll().px(px(24.)).pt(px(18.)).pb(px(24.));
        let Some(r) = row.cloned() else {
            return pane.child(div().text_color(t.dim).child("Claude’s crons show here: prompts a Claude session scheduled for itself, once or on repeat."));
        };
        let c = &r.cron;
        let dt = |label: &str, last: bool| div().w(px(96.)).flex_none().px(px(14.)).py(px(11.)).when(!last, |d| d.border_b_1().border_color(t.line)).child(kit::cap(t, label));
        let dd = |last: bool| div().flex_1().min_w_0().px(px(14.)).py(px(10.)).when(!last, |d| d.border_b_1().border_color(t.line));
        let row = |a: Div, b: Div| div().flex().child(a).child(b);
        let created = c.created_at.as_deref().and_then(time::parse_rfc3339);
        let expires = match (c.recurring, c.expires_at()) {
            (false, _) => "After its one run".to_string(),
            (true, Some(e)) => format!("{} · Claude’s 7-day limit", when_label(e)),
            (true, None) => "7 days after Claude made it (Claude’s limit)".to_string(),
        };
        let (sid, row_copy, row_cancel) = (r.sid.clone(), r.clone(), r.clone());
        let main = self.main.clone();
        pane.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(pill(t, !c.recurring))
                .child(div().text_color(t.dim).child(format!("Set by Claude in {}", r.term))),
        )
        .child(div().mt(px(10.)).text_size(px(18.)).font_weight(FontWeight::BOLD).child(c.prompt.clone()))
        .child(
            div()
                .mt(px(18.))
                .flex()
                .flex_col()
                .rounded(px(10.))
                .border_1()
                .border_color(t.line)
                .bg(t.panel)
                .overflow_hidden()
                .child(row(
                    dt("When", false),
                    dd(false)
                        .flex()
                        .flex_wrap()
                        .items_baseline()
                        .gap(px(8.))
                        .child(div().font_weight(FontWeight::BOLD).child(cron::describe(&c.schedule)))
                        .child(kit::mono(t, c.schedule.clone(), 12.).text_color(t.dim)),
                ))
                .child(row(dt("Next", false), dd(false).child(r.next.map(when_label).unwrap_or_else(|| "Not again".into()))))
                .child(row(dt("Created", false), dd(false).child(created.map(when_label).unwrap_or_else(|| "Before midna saw it (e.g. it came back with a resume)".into()))))
                .child(row(dt("Expires", true), dd(true).child(expires))),
        )
        .child(div().mt(px(16.)).text_color(t.dim).child(format!(
            "Claude owns this one, so midna can’t change it. It ends when {} exits and comes back if the session is resumed. Copy it to a trigger to give it a window, a start or an end.",
            r.term
        )))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .mt(px(14.))
                .child(kit::btn(t, "cron-open", "Open terminal").on_click(move |_, window, cx| {
                    let sid = sid.clone();
                    let _ = main.update(cx, |m, cx| {
                        m.set_screen(crate::app::Screen::Terminal, window, cx);
                        m.select(sid, window, cx);
                    });
                }))
                .child(kit::btn(t, "cron-copy", "Copy to a trigger…").on_click(cx.listener(move |v, _, window, cx| v.open_editor(None, Some(&row_copy), Some(window), cx))))
                .child(kit::btn(t, "cron-cancel", "Ask Claude to cancel it").on_click(cx.listener(move |v, _, _, cx| v.cancel_cron(row_cancel.clone(), cx)))),
        )
    }

    /// One of the editor's fields: ↩ saves, esc closes the editor.
    fn sched_input(&self, t: &Theme, input: &LineInput, id: &'static str, w: Option<f32>, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let focus = input.focus.clone();
        input
            .render(t, id, window)
            .map(|d| match w {
                Some(w) => d.w(px(w)).flex_none(),
                None => d.flex_1().min_w_0(),
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.focus(window, cx))
            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, _, cx| {
                let ks = &ev.keystroke;
                if ks.modifiers.platform || ks.modifiers.control {
                    return;
                }
                match ks.key.as_str() {
                    "enter" => {
                        v.save_schedule(cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        v.editor = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                    _ => {}
                }
            }))
    }

    pub(super) fn render_editor(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let e = self.editor.as_ref().expect("editor open");
        let seg = |id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut ScheduleEditor)| {
            div()
                .id(id)
                .h(px(26.))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .when(on, |d| d.bg(t.raised).text_color(t.fg))
                .when(!on, |d| d.text_color(t.dim))
                .cursor_pointer()
                .hover(|s| s.text_color(t.fg))
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(e) = v.editor.as_mut() {
                        f(e);
                    }
                    cx.notify();
                }))
                .child(label)
        };
        let group = || div().flex().flex_none().gap(px(2.)).p(px(2.)).rounded(px(8.)).border_1().border_color(t.line);
        let label = |s: &str| div().w(px(96.)).flex_none().child(kit::cap(t, s));
        let line = |l: &str| div().flex().items_center().gap(px(12.)).min_h(px(32.)).child(label(l));
        let dim = |s: &str| div().text_color(t.dim).child(s.to_string());

        let runs = group()
            .child(seg("sched-once", "Once", e.once, cx, |e| e.once = true))
            .child(seg("sched-repeat", "Repeats", !e.once, cx, |e| e.once = false));
        let mut form = div().mt(px(16.)).flex().flex_col().gap(px(12.)).child(line("Runs").child(runs));
        if e.once {
            form = form.child(line("At").child(self.sched_input(t, &e.at, "sched-at", Some(170.), window, cx)).child(dim("local time")));
        } else if e.custom {
            form = form.child(
                line("Cron").child(self.sched_input(t, &e.cron, "sched-cron", Some(200.), window, cx)).child(dim("minute hour day month weekday")).child(
                    div().id("sched-use-form").text_color(t.accent).cursor_pointer().child("Use the form").on_click(cx.listener(|v, _, _, cx| {
                        if let Some(e) = v.editor.as_mut() {
                            e.custom = false;
                        }
                        cx.notify();
                    })),
                ),
            );
        } else {
            let units = group()
                .child(seg("sched-min", "Minutes", e.unit == Unit::Minutes, cx, |e| e.unit = Unit::Minutes))
                .child(seg("sched-hours", "Hours", e.unit == Unit::Hours, cx, |e| e.unit = Unit::Hours))
                .child(seg("sched-daily", "Daily", e.unit == Unit::Daily, cx, |e| e.unit = Unit::Daily));
            form = form.child(match e.unit {
                Unit::Daily => line("Every").child(units).child(dim("at")).child(self.sched_input(t, &e.daily_at, "sched-daily-at", Some(80.), window, cx)),
                _ => line("Every").child(self.sched_input(t, &e.every, "sched-every", Some(56.), window, cx)).child(units),
            });
            if e.unit != Unit::Daily {
                let on = e.window;
                let mut between = line("Between").child(kit::switch(t, "sched-window", on).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(e) = v.editor.as_mut() {
                        e.window = !on;
                    }
                    cx.notify();
                })));
                between = if on {
                    between.child(self.sched_input(t, &e.from, "sched-from", Some(80.), window, cx)).child(dim("and")).child(self.sched_input(t, &e.until, "sched-until", Some(80.), window, cx))
                } else {
                    between.child(dim("Any time of day"))
                };
                form = form.child(between);
            }
            let mut days = div().flex().gap(px(4.));
            for (i, letter) in DAY_LETTERS.iter().enumerate() {
                let on = e.days[i];
                days = days.child(
                    div()
                        .id(SharedString::from(format!("sched-day-{i}")))
                        .size(px(30.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .border_1()
                        .border_color(if on { t.accent } else { t.line })
                        .when(on, |d| d.bg(t.accent_soft).text_color(t.accent))
                        .when(!on, |d| d.text_color(t.dim))
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(e) = v.editor.as_mut() {
                                e.days[i] = !e.days[i];
                            }
                            cx.notify();
                        }))
                        .child(*letter),
                );
            }
            form = form.child(line("On").child(days));
            form = form.child(line("Starts").child(self.sched_input(t, &e.starts, "sched-starts", Some(170.), window, cx)).child(dim("empty = now")));
            let ends = group()
                .child(seg("sched-never", "Never", e.ends == Ends::Never, cx, |e| e.ends = Ends::Never))
                .child(seg("sched-on-date", "On a date", e.ends == Ends::OnDate, cx, |e| e.ends = Ends::OnDate))
                .child(seg("sched-after", "After runs", e.ends == Ends::AfterRuns, cx, |e| e.ends = Ends::AfterRuns));
            form = form.child(match e.ends {
                Ends::Never => line("Ends").child(ends),
                Ends::OnDate => line("Ends").child(ends).child(self.sched_input(t, &e.ends_at, "sched-ends-at", Some(170.), window, cx)),
                Ends::AfterRuns => line("Ends").child(ends).child(self.sched_input(t, &e.runs, "sched-runs", Some(56.), window, cx)).child(dim("runs")),
            });
        }
        if e.trigger_id.is_none() {
            let terms = self.agent_terminals(cx);
            let name = e.session.as_ref().and_then(|s| terms.iter().find(|(id, _)| id == s)).map(|(_, n)| n.clone()).unwrap_or_else(|| "No agent terminal open".into());
            let next = e.session.as_ref().and_then(|s| terms.iter().position(|(id, _)| id == s)).map(|i| (i + 1) % terms.len().max(1)).unwrap_or(0);
            let target = terms.get(next).map(|(id, _)| id.clone());
            form = form
                .child(
                    line("Send to").child(
                        kit::btn(t, "sched-term", format!("{name}  ▾")).on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(e) = v.editor.as_mut() {
                                e.session = target.clone().or(e.session.take());
                            }
                            cx.notify();
                        })),
                    ),
                )
                .child(line("Prompt").child(self.sched_input(t, &e.prompt, "sched-prompt", None, window, cx)));
        }

        let summary = match e.build(cx) {
            Ok(b) => {
                let expr = b.filter.cron.clone().unwrap_or_default();
                let next: Vec<String> = b.schedule.upcoming(time::now_unix(), 3).into_iter().map(when_label).collect();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().font_weight(FontWeight::BOLD).child(b.schedule.describe(&expr)))
                    .child(div().text_color(t.dim).child(if next.is_empty() { "Never runs".into() } else { format!("Next: {}", next.join(", ")) }))
                    .child(kit::mono(t, format!("cron {expr}"), 11.5).text_color(t.dim))
            }
            Err(err) => div().text_color(t.err).child(err),
        };
        let title = match (&e.trigger_id, &e.note) {
            (Some(_), _) => "Edit schedule".to_string(),
            (None, Some(n)) => n.clone(),
            (None, None) => "New schedule trigger".to_string(),
        };
        div()
            .id("sched-editor")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .px(px(24.))
            .pt(px(18.))
            .pb(px(24.))
            .child(kit::cap(t, &title))
            .child(div().mt(px(8.)).flex().child(self.sched_input(t, &e.name, "sched-name", None, window, cx).text_size(px(14.))))
            .child(form)
            .child(div().mt(px(18.)).px(px(14.)).py(px(12.)).rounded(px(10.)).border_1().border_color(t.line).bg(t.panel).child(summary))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .mt(px(16.))
                    .child(kit::btn_primary(t, "sched-save", if e.trigger_id.is_some() { "Save schedule" } else { "Add trigger" }).on_click(cx.listener(|v, _, _, cx| v.save_schedule(cx))))
                    .child(kit::btn(t, "sched-cancel", "Cancel").on_click(cx.listener(|v, _, _, cx| {
                        v.editor = None;
                        cx.notify();
                    }))),
            )
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::{Ends, Form, Shape, Unit, build, parse_days, read_cron};
    use midna_proto::{TriggerFilter, time};

    fn form() -> Form {
        Form {
            new: true,
            name: String::new(),
            once: false,
            at: String::new(),
            unit: Unit::Minutes,
            every: "5".into(),
            daily_at: "09:00".into(),
            custom: false,
            cron: String::new(),
            window: false,
            from: "13:00".into(),
            until: "17:00".into(),
            days: [true; 7],
            starts: String::new(),
            ends: Ends::Never,
            ends_at: String::new(),
            runs: String::new(),
            session: Some("s_1".into()),
            prompt: "poll the deploy".into(),
        }
    }

    #[test]
    fn reads_crons_into_the_form() {
        let now = time::local_unix(2026, 10, 6, 12, 0);
        let r = read_cron("*/5 13-16 * * 1-5", now).unwrap();
        assert_eq!(r.shape, Shape::Every(Unit::Minutes, 5));
        assert_eq!(r.window, Some(("13:00".into(), "17:00".into())));
        assert_eq!(r.days, [true, true, true, true, true, false, false]);
        assert_eq!(read_cron("0 */2 * * *", now).unwrap().shape, Shape::Every(Unit::Hours, 2));
        assert_eq!(read_cron("0 * * * *", now).unwrap().shape, Shape::Every(Unit::Hours, 1));
        assert_eq!(read_cron("57 8 * * *", now).unwrap().shape, Shape::Daily(8, 57));
        assert_eq!(read_cron("30 14 6 10 *", now).unwrap().shape, Shape::Once(time::local_unix(2026, 10, 6, 14, 30)));
        // Already past this year: next year's.
        assert_eq!(read_cron("30 9 6 10 *", now).unwrap().shape, Shape::Once(time::local_unix(2027, 10, 6, 9, 30)));
        for odd in ["5,35 * * * *", "0 9 1 * *", "@daily", "0 9 * * 1/2"] {
            assert_eq!(read_cron(odd, now), None, "{odd}");
        }
    }

    #[test]
    fn builds_filters_from_the_form() {
        let now = time::local_unix(2026, 10, 6, 12, 0);
        let mut f = form();
        f.window = true;
        f.days = [true, true, true, true, true, false, false];
        f.ends = Ends::OnDate;
        f.ends_at = "2026-10-10".into();
        let b = build(&f, &TriggerFilter::default(), 0, now).unwrap();
        assert_eq!(b.filter.cron.as_deref(), Some("*/5 * * * 1,2,3,4,5"));
        assert_eq!(b.filter.window.as_ref().map(|w| (w.from.as_str(), w.until.as_str())), Some(("13:00", "17:00")));
        assert_eq!(b.filter.ends_at.as_deref().and_then(time::parse_rfc3339), Some(time::local_unix(2026, 10, 10, 0, 0)));
        assert_eq!((b.filter.session.as_deref(), b.name.as_str()), (Some("s_1"), "poll the deploy"));
        assert_eq!(b.schedule.upcoming(now, 1), vec![time::local_unix(2026, 10, 6, 13, 0)]);
        // Once: a pinned date that runs one time.
        let mut o = form();
        o.once = true;
        o.at = "2026-10-06 14:30".into();
        let b = build(&o, &TriggerFilter::default(), 0, now).unwrap();
        assert_eq!((b.filter.cron.as_deref(), b.filter.max_runs), (Some("30 14 6 10 *"), Some(1)));
        assert_eq!(b.schedule.describe("30 14 6 10 *"), "Once, Oct 6 at 2:30 PM");
        o.at = "2026-10-06 11:00".into();
        assert!(build(&o, &TriggerFilter::default(), 0, now).unwrap_err().contains("future"));
        // Daily, after a number of runs; editing keeps the rest of the filter.
        let mut d = form();
        d.new = false;
        d.name = "Standup".into();
        d.unit = Unit::Daily;
        d.daily_at = "08:57".into();
        d.ends = Ends::AfterRuns;
        d.runs = "3".into();
        let base = TriggerFilter { project: Some("p_1".into()), cron: Some("0 9 * * *".into()), ..Default::default() };
        let b = build(&d, &base, 7, now).unwrap();
        assert_eq!((b.filter.cron.as_deref(), b.filter.max_runs, b.filter.project.as_deref(), b.filter.session.as_deref()), (Some("57 8 * * *"), Some(3), Some("p_1"), None));
        assert_eq!(b.schedule.upcoming(now, 10).len(), 3, "a new limit counts from zero");
        // What's wrong, in words.
        for (change, want) in [
            (Box::new(|f: &mut Form| f.days = [false; 7]) as Box<dyn Fn(&mut Form)>, "at least one day"),
            (Box::new(|f: &mut Form| f.every = "0".into()), "from 1 to 59"),
            (Box::new(|f: &mut Form| f.session = None), "pick a terminal"),
            (Box::new(|f: &mut Form| f.prompt = String::new()), "prompt"),
            (Box::new(|f: &mut Form| {
                f.window = true;
                f.from = "1pm".into();
            }), "window.from"),
        ] {
            let mut f = form();
            change(&mut f);
            let err = build(&f, &TriggerFilter::default(), 0, now).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
    }

    #[test]
    fn reads_cron_days() {
        assert_eq!(parse_days("*"), Some([true; 7]));
        assert_eq!(parse_days("1-5"), Some([true, true, true, true, true, false, false]));
        assert_eq!(parse_days("mon-fri"), parse_days("1-5"));
        assert_eq!(parse_days("0,6"), Some([false, false, false, false, false, true, true]));
        assert_eq!(parse_days("7"), Some([false, false, false, false, false, false, true]));
        assert_eq!(parse_days("fri-mon"), Some([true, false, false, false, true, true, true]));
        assert_eq!(parse_days("1/2"), None);
    }
}
