//! Streams on the Triggers screen (design/stream-health, concept C): a Streams section above
//! the triggers, a stream's detail (state, why, facts, command, what it feeds, recent events
//! and its log) with Restart / Pause / Resume and a ⋯ menu, and the footer Triggers button's
//! flag when a stream is restarting, failing or waiting for approval. Quiet otherwise: healthy
//! and paused streams never flag.
//!
//! Data: `MainWindow::streams` (`streams.list`, refetched on `stream.*` events, including
//! `stream.health`), plus `streams.events` and `events.list` for the selected stream.
use super::*;
use midna_proto::{StreamPublishRecord, StreamState, StreamStatus, StreamVerdict};

// ------------------------------------------------------------------ pure helpers

/// A stream's colour on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Work,
    Need,
    Idle,
}

impl Tone {
    pub fn color(self, t: &Theme) -> Hsla {
        match self {
            Tone::Ok => t.ok,
            Tone::Work => t.work,
            Tone::Need => t.need,
            Tone::Idle => t.dim,
        }
    }
}

pub fn tone(s: &StreamStatus) -> Tone {
    match s.state {
        StreamState::Running => Tone::Ok,
        StreamState::Starting | StreamState::Backoff => Tone::Work,
        StreamState::Failing | StreamState::AwaitingApproval => Tone::Need,
        StreamState::Paused => Tone::Idle,
    }
}

/// The detail's chip: "Running", "Restarting", "Failing", "Paused", "Waiting for approval".
pub fn chip_label(s: &StreamStatus) -> &'static str {
    match s.state {
        StreamState::Running => "Running",
        StreamState::Starting => "Starting",
        StreamState::Backoff => "Restarting",
        StreamState::Failing => "Failing",
        StreamState::Paused => "Paused",
        StreamState::AwaitingApproval => "Waiting for approval",
    }
}

/// Seconds until `at` (RFC 3339), never negative.
fn secs_until(at: Option<&str>, now: i64) -> Option<i64> {
    at.and_then(parse_rfc3339).map(|t| (t - now).max(0))
}

/// "next try in 8s", "next try in 4 min", "trying again now".
fn next_try(s: &StreamStatus, now: i64) -> String {
    match secs_until(s.retry_at.as_deref(), now) {
        Some(0) | None => "trying again now".into(),
        Some(n) if n < 60 => format!("next try in {n}s"),
        Some(n) => format!("next try in {} min", (n + 59) / 60),
    }
}

/// The list row's second line, or none for a running stream:
/// `Restarting · next try in 8s`, `Failing · invalid_auth · 7 restarts`, `Paused`,
/// `From an agent · approve to start`.
pub fn sub_line(s: &StreamStatus, now: i64) -> Option<String> {
    match s.state {
        StreamState::Running | StreamState::Starting => None,
        StreamState::Backoff => Some(format!("Restarting · {}", next_try(s, now))),
        StreamState::Failing => {
            let mut parts = vec!["Failing".to_string()];
            parts.extend(s.why.clone().filter(|w| !w.is_empty()));
            parts.push(format!("{} restart{}", s.restarts, if s.restarts == 1 { "" } else { "s" }));
            Some(parts.join(" · "))
        }
        StreamState::Paused => Some("Paused".into()),
        StreamState::AwaitingApproval => Some("From an agent · approve to start".into()),
    }
}

/// The footer Triggers button's flag: the worst stream that needs a look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flag {
    pub stream: String,
    /// "Failing", "Approve", "Restarting".
    pub word: &'static str,
    pub tone: Tone,
    /// "Stream slack: Failing · invalid_auth · 7 restarts · click to open".
    pub tip: String,
}

/// Failing first, then waiting for approval, then restarting. None while every stream is
/// healthy or paused.
pub fn flag(list: &[StreamStatus], now: i64) -> Option<Flag> {
    let order = [(StreamState::Failing, "Failing", Tone::Need), (StreamState::AwaitingApproval, "Approve", Tone::Need), (StreamState::Backoff, "Restarting", Tone::Work)];
    order.iter().find_map(|(state, word, tone)| {
        let s = list.iter().find(|s| s.state == *state)?;
        let sub = sub_line(s, now).unwrap_or_default();
        Some(Flag { stream: s.name.clone(), word, tone: *tone, tip: format!("Stream {}: {sub} · click to open", s.name) })
    })
}

/// The Streams section's count: "3 running" or "2 of 3 running".
pub fn running_text(list: &[StreamStatus]) -> String {
    let up = list.iter().filter(|s| s.state == StreamState::Running).count();
    if up == list.len() { format!("{up} running") } else { format!("{up} of {} running", list.len()) }
}

/// A short age for the list: "now", "2m", "6h", "3d".
pub fn short_age(secs: i64) -> String {
    match secs.max(0) {
        0..=44 => "now".into(),
        s @ 45..=3599 => format!("{}m", (s / 60).max(1)),
        s @ 3600..=86399 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

/// How long it has been up: "45s", "12m", "3h 12m", "2d 3h".
pub fn span(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d {}h", s / 86400, s % 86400 / 3600),
    }
}

/// `*`/`?` glob over the whole string, ignoring case (as midnad matches trigger events).
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.to_lowercase().chars().collect(), text.to_lowercase().chars().collect());
    let (mut pi, mut ti, mut star) = (0usize, 0usize, None::<(usize, usize)>);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Does a local trigger's event pattern take events from stream `name`?
/// (`stream.slack.message`, `stream.slack.*`, `stream.*`.)
pub fn feeds(event: &str, name: &str) -> bool {
    let e = event.trim().to_lowercase();
    let prefix = format!("stream.{}.", name.to_lowercase());
    e.starts_with(&prefix) || (e.contains(['*', '?']) && glob(&e, &format!("{prefix}x")))
}

/// One line of an event's payload for the events table: its `text`, `title`, `message`,
/// `summary` or `subject`, else the payload itself, cut short.
pub fn payload_text(data: &Value) -> String {
    let pick = ["text", "title", "message", "summary", "subject", "name"].iter().find_map(|k| data.get(*k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()));
    let s = match pick {
        Some(s) => s.to_string(),
        None if data.as_object().is_some_and(|o| o.is_empty()) || data.is_null() => String::new(),
        None => data.to_string(),
    };
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 160 { format!("{}…", s.chars().take(159).collect::<String>()) } else { s }
}

/// A dropped publish's verdict, in words.
pub fn verdict_words(v: StreamVerdict) -> &'static str {
    match v {
        StreamVerdict::Accepted => "Taken",
        StreamVerdict::Duplicate => "Duplicate (same id)",
        StreamVerdict::RateLimited => "Rate limited",
        StreamVerdict::TooLarge => "Too large",
        StreamVerdict::Busy => "Intake full",
        StreamVerdict::Refused => "Refused",
    }
}

/// What a taken event did: the local delivery that answered it (a firing before one its
/// cooldown held back). A delivery carries the event's seq; one from before it did is matched
/// by event name and time, a local firing of the same event within 10s after it.
pub fn fired_by<'a>(name: &str, rec: &StreamPublishRecord, deliveries: &'a [DeliveryItem]) -> Option<&'a DeliveryItem> {
    if rec.verdict != StreamVerdict::Accepted {
        return None;
    }
    let event = format!("stream.{name}.{}", rec.kind);
    let at = parse_rfc3339(&rec.at)?;
    let answers = |d: &&DeliveryItem| {
        d.trigger_id.is_some()
            && d.replay_of.is_none()
            && match (d.event_seq, rec.seq) {
                (Some(s), seq) => seq == Some(s),
                (None, _) => d.event == event && parse_rfc3339(&d.received_at).is_some_and(|got| got >= at && got - at <= 10),
            }
    };
    let held = |d: &&DeliveryItem| d.verdict == "cooldown";
    deliveries.iter().filter(answers).find(|d| !held(d)).or_else(|| deliveries.iter().filter(answers).find(held))
}

/// The stream's command line, quoted for a shell.
pub fn command_line(s: &StreamStatus) -> String {
    s.command.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")
}

fn ask_prompt(s: &StreamStatus) -> String {
    let why = s.why.as_deref().map(|w| format!(" ({w})")).unwrap_or_default();
    format!("The midna stream {} keeps failing{why}. Read its log at {}, find out why with `midna stream list`, fix it, then `midna stream restart {}`.", s.name, s.log, s.name)
}

// ------------------------------------------------------------------ view state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTab {
    Events,
    Log,
}

/// One of the selected stream's recent publishes, with its payload's line when it was taken.
#[derive(Clone, Debug)]
pub struct EventRow {
    pub rec: StreamPublishRecord,
    pub text: String,
}

/// The selected stream's recent publishes (`streams.events` + `events.list`).
pub struct StreamEvents {
    pub stream: String,
    pub rows: Vec<EventRow>,
}

impl TriggersView {
    pub(super) fn streams(&self, cx: &App) -> Vec<StreamStatus> {
        self.main.upgrade().map(|m| m.read(cx).streams.clone()).unwrap_or_default()
    }

    pub(super) fn select_stream(&mut self, name: String, cx: &mut Context<Self>) {
        if self.sel_stream.as_deref() != Some(name.as_str()) {
            self.stream_tab = StreamTab::Events;
            self.confirm_remove = None;
        }
        self.sel_stream = Some(name);
        self.tab = Tab::Triggers;
        self.editor = None;
        self.fetch_stream_events(cx);
        cx.notify();
    }

    /// The selected stream's recent publishes and, for the taken ones, their payloads.
    pub(super) fn fetch_stream_events(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.sel_stream.clone() else {
            return;
        };
        if self.events_fetching {
            return;
        }
        self.events_fetching = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(80)).await;
            let n = name.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let recs = backend.call("streams.events", json!({"stream": n, "limit": 50}));
                    let data = backend.call("events.list", json!({"limit": 200, "filter": {"kinds": [format!("stream.{n}.")]}}));
                    (recs, data)
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.events_fetching = false;
                let recs: Vec<StreamPublishRecord> = res.0.ok().and_then(|r| serde_json::from_value(r.get("events").cloned().unwrap_or_default()).ok()).unwrap_or_default();
                let by_seq: std::collections::HashMap<u64, Value> = res
                    .1
                    .ok()
                    .map(|l| parse_list::<Value>(&l))
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|e| Some((e.get("seq")?.as_u64()?, e.get("data").cloned().unwrap_or_default())))
                    .collect();
                let rows = recs
                    .into_iter()
                    .map(|rec| {
                        let text = match (rec.verdict, rec.seq.and_then(|s| by_seq.get(&s))) {
                            (StreamVerdict::Accepted, Some(d)) => payload_text(d),
                            _ => rec.detail.clone().or_else(|| rec.id.clone().map(|i| format!("id {i}"))).unwrap_or_default(),
                        };
                        EventRow { rec, text }
                    })
                    .collect();
                v.stream_events = Some(StreamEvents { stream: name.clone(), rows });
                // The selection moved while this ran: fetch the new one's.
                if v.sel_stream.as_deref() != Some(name.as_str()) {
                    v.fetch_stream_events(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stream_call(&mut self, method: &'static str, params: Value, done: String, cx: &mut Context<Self>) {
        self.stream_menu = None;
        self.call(method, params, cx, move |v, _, cx| {
            v.toast(done, cx);
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::STREAMS, cx));
            cx.notify();
        });
    }

    pub(super) fn restart_stream(&mut self, name: String, cx: &mut Context<Self>) {
        self.stream_call("streams.restart", json!({"name": name}), format!("Restarting {name}…"), cx);
    }

    pub(super) fn set_stream_enabled(&mut self, name: String, on: bool, cx: &mut Context<Self>) {
        let done = if on { format!("Resumed {name}") } else { format!("Paused {name}") };
        self.stream_call("streams.set_enabled", json!({"name": name, "enabled": on}), done, cx);
    }

    fn remove_stream(&mut self, name: String, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        if self.sel_stream.as_deref() == Some(name.as_str()) {
            self.sel_stream = None;
        }
        self.stream_call("streams.remove", json!({"name": name}), format!("Removed {name}"), cx);
    }

    // ------------------------------------------------------------------ list

    /// The Streams section: one row per stream, a dot, its name, its last event's age and,
    /// when it isn't running, why.
    pub(super) fn render_stream_rows(&self, t: &Theme, streams: &[StreamStatus], cx: &mut Context<Self>) -> Div {
        let now = kit::now_unix();
        let mut d = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(16.))
                .pt(px(12.))
                .pb(px(4.))
                .child(kit::cap(t, "Streams"))
                .child(div().text_size(px(11.5)).text_color(t.dim).child(running_text(streams))),
        );
        for s in streams {
            let on = self.sel_stream.as_deref() == Some(s.name.as_str());
            let (name, name2) = (s.name.clone(), s.name.clone());
            let tn = tone(s);
            let age = (s.state != StreamState::AwaitingApproval).then(|| s.last_event.as_ref().and_then(|e| parse_rfc3339(&e.at)).map(|at| short_age(now - at))).flatten();
            let sub = sub_line(s, now);
            let sub_color = match tn {
                Tone::Idle => t.dim,
                x => x.color(t),
            };
            let dot = if tn == Tone::Idle { div().size(px(8.)).flex_none().rounded_full().border_1().border_color(t.dim) } else { kit::dot(tn.color(t), 8.) };
            let row = div()
                .id(SharedString::from(format!("stream-{}", s.name)))
                .flex()
                .flex_col()
                .gap(px(2.))
                .pl(px(14.))
                .pr(px(16.))
                .py(px(8.))
                .cursor_pointer()
                .hover(|st| st.bg(t.raised))
                .border_l_2()
                .border_color(if on { t.accent } else { transparent_black() })
                .when(on, |d| d.bg(t.raised))
                .on_click(cx.listener(move |v, _, _, cx| v.select_stream(name.clone(), cx)))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        v.select_stream(name2.clone(), cx);
                        v.stream_menu = Some((name2.clone(), ev.position));
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(div().w(px(30.)).flex_none().flex().justify_center().child(dot))
                        .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::BOLD).when(tn == Tone::Idle, |d| d.text_color(t.dim)).child(s.name.clone()))
                        .children(age.map(|a| div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().child(a))),
                )
                .children(sub.map(|x| div().pl(px(40.)).text_size(px(12.)).text_color(sub_color).truncate().child(x)));
            d = d.child(row);
        }
        d
    }

    // ------------------------------------------------------------------ detail

    pub(super) fn render_stream_detail(&self, t: &Theme, s: &StreamStatus, cx: &mut Context<Self>) -> Stateful<Div> {
        let now = kit::now_unix();
        let tn = tone(s);
        let chip_color = match tn {
            Tone::Idle => t.dim,
            x => x.color(t),
        };
        let chip = kit::chip(t, chip_label(s)).h(px(22.)).rounded(px(11.)).font_weight(FontWeight::BOLD).text_color(chip_color).border_color(if tn == Tone::Idle { t.line } else { chip_color.opacity(0.5) });
        let name = s.name.clone();
        let approval = s.state == StreamState::AwaitingApproval;
        let confirming = self.confirm_remove.as_deref() == Some(s.name.as_str());
        let acts = if confirming {
            let (n1, n2) = (name.clone(), name.clone());
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().text_size(px(12.)).text_color(t.dim).child("Remove it? midna stops the program."))
                .child(kit::btn_danger(t, "stream-remove-yes", "Remove", 28.).on_click(cx.listener(move |v, _, _, cx| v.remove_stream(n1.clone(), cx))))
                .child(kit::btn(t, "stream-remove-no", "Keep").on_click(cx.listener(move |v, _, _, cx| {
                    let _ = &n2;
                    v.confirm_remove = None;
                    cx.notify();
                })))
        } else if approval {
            let (n1, n2) = (name.clone(), name.clone());
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::btn_primary(t, "stream-approve", "Approve and start").on_click(cx.listener(move |v, _, _, cx| {
                    v.stream_call("streams.set_enabled", json!({"name": n1, "enabled": true}), format!("Approved · {n1} starting"), cx)
                })))
                .child(kit::btn(t, "stream-decline", "Decline").on_click(cx.listener(move |v, _, _, cx| v.remove_stream(n2.clone(), cx))))
        } else {
            let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
            let paused = s.state == StreamState::Paused;
            let try_now = matches!(s.state, StreamState::Backoff | StreamState::Failing);
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(!paused, |d| {
                    d.child(icon_btn(t, "stream-restart", Icon::Restart, if try_now { "Try now" } else { "Restart" }).on_click(cx.listener(move |v, _, _, cx| v.restart_stream(n1.clone(), cx))))
                })
                .child(if paused {
                    icon_btn(t, "stream-resume", Icon::Play, "Resume").on_click(cx.listener(move |v, _, _, cx| v.set_stream_enabled(n2.clone(), true, cx)))
                } else {
                    icon_btn(t, "stream-pause", Icon::Pause, "Pause").on_click(cx.listener(move |v, _, _, cx| v.set_stream_enabled(n2.clone(), false, cx)))
                })
                .child(
                    div()
                        .id("stream-more")
                        .size(px(28.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .border_1()
                        .border_color(t.line)
                        .cursor_pointer()
                        .hover(|st| st.bg(t.raised))
                        .tooltip(crate::ui::header::tip("More"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                v.stream_menu = if v.stream_menu.is_some() { None } else { Some((n3.clone(), ev.position)) };
                                cx.notify();
                            }),
                        )
                        .child(Icon::Dots.el(14., t.dim)),
                )
        };
        let dot = if tn == Tone::Idle { div().size(px(9.)).flex_none().rounded_full().border_1().border_color(t.dim) } else { kit::dot(tn.color(t), 9.) };
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .child(dot)
            .child(div().text_size(px(18.)).font_weight(FontWeight::BOLD).child(s.name.clone()))
            .child(chip)
            .child(div().flex_1())
            .child(acts);

        // Why it isn't running.
        let feeds_names: Vec<String> = self.triggers.iter().filter(|tr| tr.is_local() && feeds(&tr.event, &s.name)).map(|tr| tr.name.clone()).collect();
        let why_box = |border: Option<Hsla>, lead: Option<String>, rest: String| {
            div()
                .mt(px(12.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(border.map(|c| c.opacity(0.55)).unwrap_or(t.line))
                .bg(t.panel)
                .text_size(px(13.))
                .line_height(px(19.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(lead.map(|l| div().w_full().font_weight(FontWeight::BOLD).child(l)))
                .child(div().w_full().child(rest))
        };
        let why = s.why.clone().filter(|w| !w.is_empty());
        let why_el = match s.state {
            StreamState::Backoff => Some(why_box(
                Some(t.work),
                why.clone().map(|w| format!("{}.", w.trim_end_matches('.'))),
                format!("Restart {} · {} (it waits 1s, then twice as long each time, up to 5 min).", s.restarts, next_try(s, now)),
            )),
            StreamState::Failing => {
                let mut rest = String::new();
                if let Some(w) = &why {
                    rest.push_str(&format!("Last error: {w}. "));
                }
                rest.push_str(&format!("midna raised a needs-you item and keeps trying ({}).", next_try(s, now)));
                if !s.secrets.is_empty() {
                    rest.push_str(&format!(" If {} was revoked, replace it, then Try now.", s.secrets.join(" or ")));
                }
                // Failing sticks until a 10-minute run, so the recent count can drop under 5.
                let lead = if s.exits_recent >= 5 { format!("Kept failing: {} exits in 10 min.", s.exits_recent) } else { "Kept failing.".to_string() };
                Some(why_box(Some(t.need), Some(lead), rest))
            }
            StreamState::Paused => Some(why_box(
                None,
                None,
                format!(
                    "Paused. Events it would have sent are not queued.{}",
                    if feeds_names.is_empty() { String::new() } else { format!(" Triggers on stream.{}.* stay on but won’t fire.", s.name) }
                ),
            )),
            StreamState::AwaitingApproval => Some(why_box(
                Some(t.need),
                Some("An agent added or changed this stream.".into()),
                "It won’t run until you approve. Check the command and the secrets it asks for.".into(),
            )),
            _ => None,
        };

        // Facts.
        let events = self.stream_events.as_ref().filter(|e| e.stream == s.name);
        let today = events.map(|e| {
            let n = e.rows.iter().filter(|r| r.rec.verdict == StreamVerdict::Accepted && kit::clock_or_day(&r.rec.at).contains(':')).count();
            if n == e.rows.len() && n >= 50 { format!("{n}+") } else { n.to_string() }
        });
        let last = s.last_event.as_ref().and_then(|e| parse_rfc3339(&e.at)).map(|at| format!("{} ago", short_age(now - at)).replace("now ago", "just now"));
        let up = (s.state == StreamState::Running).then(|| s.started_at.as_deref().and_then(parse_rfc3339).map(|at| span(now - at))).flatten();
        let fact = |label: &'static str, value: Option<String>| {
            div()
                .flex_1()
                .min_w(px(110.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(t.panel)
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(11.)).text_color(t.dim).child(label))
                .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(value.unwrap_or_else(|| "—".into())))
        };
        let facts = div()
            .mt(px(16.))
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .child(fact("Last event", if approval { None } else { last }))
            .child(fact("Events today", if approval { None } else { today }))
            .child(fact("Restarts", Some(s.restarts.to_string())))
            .child(fact("Up for", up));

        // Command, secrets, what it feeds.
        let kv = |k: &'static str, v: AnyElement| div().flex().gap(px(12.)).child(div().w(px(84.)).flex_none().text_color(t.dim).child(k)).child(div().flex_1().min_w_0().child(v));
        let cmd = command_line(s);
        let secrets = if s.secrets.is_empty() {
            div().text_color(t.dim).child("None").into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .child(kit::mono(t, s.secrets.join(", "), 12.))
                .child(div().text_color(t.dim).child("· injected by midna, never shown"))
                .into_any_element()
        };
        let feeds_el = if feeds_names.is_empty() {
            div().text_color(t.dim).child("No trigger yet").into_any_element()
        } else {
            div().child(feeds_names.join(", ")).into_any_element()
        };
        let kvs = div()
            .mt(px(16.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .text_size(px(13.))
            .child(kv("Command", kit::mono(t, cmd, 12.).into_any_element()))
            .child(kv("Secrets", secrets))
            .child(kv("Feeds", feeds_el));

        let mut detail = div()
            .id("stream-detail")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .px(px(24.))
            .pt(px(18.))
            .pb(px(24.))
            .child(header)
            .children(why_el)
            .child(facts)
            .child(kvs);
        if approval {
            return detail;
        }

        // Recent events | Log.
        let tab = |id: &'static str, label: &'static str, which: StreamTab, cx: &mut Context<Self>| {
            let on = self.stream_tab == which;
            div()
                .id(id)
                .h(px(32.))
                .flex()
                .items_center()
                .px(px(2.))
                .border_b_2()
                .border_color(if on { t.accent } else { transparent_black() })
                .text_color(if on { t.fg } else { t.dim })
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .hover(|st| st.text_color(t.fg))
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.stream_tab = which;
                    cx.notify();
                }))
                .child(label)
        };
        detail = detail.child(
            div()
                .mt(px(20.))
                .mb(px(10.))
                .flex()
                .gap(px(18.))
                .border_b_1()
                .border_color(t.line)
                .child(tab("stream-tab-events", "Recent events", StreamTab::Events, cx))
                .child(tab("stream-tab-log", "Log", StreamTab::Log, cx)),
        );
        match self.stream_tab {
            StreamTab::Events => {
                let rows = events.map(|e| e.rows.clone()).unwrap_or_default();
                if rows.is_empty() {
                    let text = if events.is_none() { "Loading…".to_string() } else { format!("No events yet. The first one {} sends shows here.", s.name) };
                    detail = detail.child(div().p(px(14.)).rounded(px(10.)).border_1().border_color(t.line).text_color(t.dim).child(text));
                } else {
                    let mut table = div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.line).overflow_hidden();
                    for r in rows.iter().take(30) {
                        let answer = fired_by(&s.name, &r.rec, &self.deliveries);
                        let held = answer.is_some_and(|d| d.verdict == "cooldown");
                        let fired = answer.and_then(|d| d.trigger_id.clone()).map(|id| self.triggers.iter().find(|tr| tr.id == id).map(|tr| tr.name.clone()).unwrap_or(id));
                        let (verdict, vcolor) = match (&fired, r.rec.verdict) {
                            (Some(n), _) if held => (format!("{n}: held back (cooldown)"), t.dim),
                            (Some(n), _) => (format!("→ {n}"), t.ok),
                            (None, StreamVerdict::Accepted) => ("No trigger fired".to_string(), t.dim),
                            (None, v) => (verdict_words(v).to_string(), t.dim),
                        };
                        table = table.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .px(px(12.))
                                .py(px(8.))
                                .border_b_1()
                                .border_color(t.line)
                                .text_size(px(12.5))
                                .child(kit::mono(t, kit::clock_or_day(&r.rec.at), 12.).w(px(70.)).flex_none().text_color(t.dim))
                                .child(kit::mono(t, r.rec.kind.clone(), 12.).w(px(110.)).flex_none().truncate())
                                .child(div().flex_1().min_w_0().truncate().child(r.text.clone()))
                                .child(div().w(px(180.)).flex_none().flex().justify_end().text_color(vcolor).when(fired.is_some(), |d| d.font_weight(FontWeight::BOLD)).truncate().child(verdict)),
                        );
                    }
                    detail = detail.child(table);
                }
                detail = detail.child(
                    div()
                        .mt(px(10.))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .child("The full list:")
                        .child(kit::mono(t, format!("midna stream events {}", s.name), 11.5)),
                );
            }
            StreamTab::Log => {
                let mut log = div().flex().flex_col().px(px(12.)).py(px(10.)).rounded(px(8.)).bg(t.term).border_1().border_color(t.line).font_family(t.mono_font.clone()).text_size(px(11.5)).line_height(px(17.));
                if s.log_tail.is_empty() {
                    log = log.child(div().text_color(t.dim).child("Nothing logged yet."));
                }
                for l in s.log_tail.iter().rev().take(40).rev() {
                    log = log.child(div().truncate().child(l.clone()));
                }
                let path = s.log.clone();
                detail = detail.child(log).child(
                    div()
                        .mt(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(kit::mono(t, s.log.clone(), 11.5).flex_1().min_w_0().truncate().text_color(t.dim))
                        .child(kit::btn(t, "stream-open-log", "Open log").on_click(cx.listener(move |v, _, _, cx| {
                            if let Err(e) = std::process::Command::new("open").arg(&path).spawn() {
                                v.toast(format!("Couldn't open the log: {e}"), cx);
                            }
                        }))),
                );
            }
        }
        detail
    }

    /// The ⋯ / right-click menu on a stream.
    pub(super) fn render_stream_menu(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (name, pos) = self.stream_menu.clone()?;
        let s = self.streams(cx).into_iter().find(|s| s.name == name)?;
        let paused = s.state == StreamState::Paused;
        let item = |id: &'static str, label: &'static str, danger: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .text_size(px(13.))
                .when(danger, |d| d.text_color(t.err))
                .cursor_pointer()
                .hover(|st| st.bg(t.accent_soft))
                .child(label)
        };
        let (n1, n2, n4, n5) = (name.clone(), name.clone(), name.clone(), name.clone());
        let main = self.main.clone();
        let prompt = ask_prompt(&s);
        let cmd = command_line(&s);
        let menu = div()
            .id("stream-menu")
            .w(px(220.))
            .p(px(5.))
            .rounded(px(9.))
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.stream_menu = None;
                cx.notify();
            }))
            .when(!paused && s.state != StreamState::AwaitingApproval, |d| d.child(item("sm-restart", "Restart", false).on_click(cx.listener(move |v, _, _, cx| v.restart_stream(n1.clone(), cx)))))
            .when(s.state != StreamState::AwaitingApproval, |d| {
                d.child(item("sm-pause", if paused { "Resume" } else { "Pause" }, false).on_click(cx.listener(move |v, _, _, cx| v.set_stream_enabled(n2.clone(), paused, cx))))
            })
            .child(item("sm-log", "Show log", false).on_click(cx.listener(|v, _, _, cx| {
                v.stream_tab = StreamTab::Log;
                v.stream_menu = None;
                cx.notify();
            })))
            .child(item("sm-ask", "Ask an agent to fix it…", false).on_click(cx.listener(move |v, _, window, cx| {
                v.stream_menu = None;
                ask(&main, prompt.clone(), window, cx);
            })))
            .child(div().my(px(4.)).h(px(1.)).bg(t.line))
            .child(item("sm-copy", "Copy command", false).on_click(cx.listener(move |v, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(cmd.clone()));
                v.stream_menu = None;
                v.toast("Copied the command".into(), cx);
            })))
            .child(item("sm-remove", "Remove stream…", true).on_click(cx.listener(move |v, _, _, cx| {
                let _ = &n4;
                v.stream_menu = None;
                v.confirm_remove = Some(n5.clone());
                v.sel_stream = Some(n5.clone());
                cx.notify();
            })));
        Some(deferred(anchored().position(pos).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }
}

/// An outlined button with an icon before its label (`.btn` with an svg).
fn icon_btn(t: &Theme, id: &'static str, icon: Icon, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(10.))
        .rounded(px(7.))
        .border_1()
        .border_color(t.line)
        .text_size(px(12.))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|st| st.bg(t.raised))
        .child(icon.el(13., t.dim))
        .child(label)
}

// ------------------------------------------------------------------ main-window glue

/// The footer's Triggers button when a stream needs a look: the Triggers screen on that stream.
pub fn open_stream(m: &mut MainWindow, name: &str, w: &mut Window, cx: &mut Context<MainWindow>) {
    m.set_screen(crate::app::Screen::Triggers, w, cx);
    let name = name.to_string();
    super::view(m, cx).update(cx, |v, cx| v.select_stream(name, cx));
}

#[cfg(test)]
mod tests {
    use super::{DeliveryItem, StreamPublishRecord, StreamState, StreamStatus, StreamVerdict, Tone, feeds, fired_by, flag, payload_text, running_text, short_age, span, sub_line};
    use serde_json::json;

    fn stream(name: &str, state: StreamState) -> StreamStatus {
        let (enabled, log) = (state != StreamState::Paused, format!("/tmp/{name}.log"));
        serde_json::from_value(json!({"name": name, "command": ["node", "listener.js"], "cwd": "/tmp", "enabled": enabled, "state": state, "restarts": 0, "log": log}))
        .unwrap()
    }

    #[test]
    fn quiet_while_healthy_or_paused() {
        let list = vec![stream("slack", StreamState::Running), stream("cal", StreamState::Paused), stream("bk", StreamState::Starting)];
        assert_eq!(flag(&list, 0), None);
        assert_eq!(flag(&[], 0), None);
    }

    #[test]
    fn flag_picks_the_worst_stream() {
        let mut bk = stream("bk", StreamState::Backoff);
        bk.retry_at = Some("1970-01-01T00:00:08Z".into());
        let list = vec![bk.clone(), stream("gh", StreamState::AwaitingApproval)];
        let f = flag(&list, 0).unwrap();
        assert_eq!((f.stream.as_str(), f.word, f.tone), ("gh", "Approve", Tone::Need));

        let mut slack = stream("slack", StreamState::Failing);
        slack.why = Some("invalid_auth".into());
        slack.restarts = 7;
        let f = flag(&[bk.clone(), stream("gh", StreamState::AwaitingApproval), slack], 0).unwrap();
        assert_eq!((f.stream.as_str(), f.word), ("slack", "Failing"));
        assert_eq!(f.tip, "Stream slack: Failing · invalid_auth · 7 restarts · click to open");

        let f = flag(&[bk, stream("cal", StreamState::Paused)], 0).unwrap();
        assert_eq!((f.word, f.tone), ("Restarting", Tone::Work));
        assert_eq!(f.tip, "Stream bk: Restarting · next try in 8s · click to open");
    }

    #[test]
    fn sub_lines() {
        assert_eq!(sub_line(&stream("a", StreamState::Running), 0), None);
        assert_eq!(sub_line(&stream("a", StreamState::Paused), 0).as_deref(), Some("Paused"));
        assert_eq!(sub_line(&stream("a", StreamState::AwaitingApproval), 0).as_deref(), Some("From an agent · approve to start"));
        let mut b = stream("a", StreamState::Backoff);
        assert_eq!(sub_line(&b, 0).as_deref(), Some("Restarting · trying again now"));
        b.retry_at = Some("1970-01-01T00:02:00Z".into());
        assert_eq!(sub_line(&b, 0).as_deref(), Some("Restarting · next try in 2 min"));
        let mut f = stream("a", StreamState::Failing);
        f.restarts = 1;
        assert_eq!(sub_line(&f, 0).as_deref(), Some("Failing · 1 restart"));
    }

    #[test]
    fn running_count() {
        let mut list = vec![stream("a", StreamState::Running), stream("b", StreamState::Running)];
        assert_eq!(running_text(&list), "2 running");
        list.push(stream("c", StreamState::Paused));
        assert_eq!(running_text(&list), "2 of 3 running");
    }

    #[test]
    fn ages_and_spans() {
        assert_eq!(short_age(10), "now");
        assert_eq!(short_age(120), "2m");
        assert_eq!(short_age(6 * 3600 + 5), "6h");
        assert_eq!(short_age(3 * 86400), "3d");
        assert_eq!(span(42), "42s");
        assert_eq!(span(3 * 3600 + 12 * 60), "3h 12m");
        assert_eq!(span(2 * 86400 + 3 * 3600), "2d 3h");
    }

    #[test]
    fn trigger_events_that_feed_a_stream() {
        assert!(feeds("stream.slack.message", "slack"));
        assert!(feeds("stream.slack.*", "slack"));
        assert!(feeds("stream.*", "slack"));
        assert!(feeds("Stream.Slack.Message", "slack"));
        assert!(!feeds("stream.slackbot.message", "slack"));
        assert!(!feeds("stream.calendar.*", "slack"));
        assert!(!feeds("session.idle", "slack"));
    }

    #[test]
    fn payload_lines() {
        assert_eq!(payload_text(&json!({"text": "failed to build\non master", "ts": "1"})), "failed to build on master");
        assert_eq!(payload_text(&json!({})), "");
        assert_eq!(payload_text(&json!({"n": 3})), r#"{"n":3}"#);
        assert!(payload_text(&json!({"text": "x".repeat(400)})).ends_with('…'));
    }

    #[test]
    fn taken_event_finds_the_trigger_it_fired() {
        let rec: StreamPublishRecord = serde_json::from_value(json!({"at": "2026-11-20T10:42:00Z", "kind": "message", "verdict": "accepted", "seq": 9})).unwrap();
        let del = |event: &str, at: &str| -> DeliveryItem { serde_json::from_value(json!({"id": "d", "event": event, "received_at": at, "trigger_id": "t_1"})).unwrap() };
        let tid = |dels: &[DeliveryItem]| fired_by("slack", &rec, dels).and_then(|d| d.trigger_id.clone());
        // Deliveries from before they carried the event's seq: by name and time.
        assert_eq!(tid(&[del("stream.slack.message", "2026-11-20T10:42:01Z")]).as_deref(), Some("t_1"));
        assert_eq!(tid(&[del("stream.slack.message", "2026-11-20T10:43:00Z")]), None);
        assert_eq!(tid(&[del("stream.slack.reaction", "2026-11-20T10:42:01Z")]), None);
        let dup = StreamPublishRecord { verdict: StreamVerdict::Duplicate, ..rec.clone() };
        assert_eq!(fired_by("slack", &dup, &[del("stream.slack.message", "2026-11-20T10:42:01Z")]).map(|d| d.id.clone()), None);
        // With the seq: exact, whenever it was received; another event's firing isn't this one's.
        let seq = |n: u64, verdict: &str, trig: &str| -> DeliveryItem {
            serde_json::from_value(json!({"id": format!("d_{trig}"), "event": "stream.slack.message", "received_at": "2026-11-20T10:42:01Z", "trigger_id": trig, "verdict": verdict, "event_seq": n})).unwrap()
        };
        assert_eq!(tid(&[seq(8, "verified", "t_8")]), None);
        assert_eq!(tid(&[seq(9, "verified", "t_2")]).as_deref(), Some("t_2"));
        // A firing wins over a trigger whose cooldown held it back; alone, the hold-back shows.
        assert_eq!(fired_by("slack", &rec, &[seq(9, "cooldown", "t_c"), seq(9, "verified", "t_f")]).map(|d| d.id.as_str()), Some("d_t_f"));
        assert_eq!(fired_by("slack", &rec, &[seq(9, "cooldown", "t_c")]).map(|d| d.verdict.as_str()), Some("cooldown"));
    }
}
