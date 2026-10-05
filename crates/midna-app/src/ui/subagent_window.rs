//! A subagent's read-only window: what it was asked, what it said, its tool calls and the
//! first line of each result, followed live from its transcript (`session.subagent_log`,
//! polled every second while it runs). Subagents run inside the agent's process, so there is
//! no terminal to attach to; this is the closest view of one. Opened from the header's
//! subagents popover (`ui/subagents.rs`); one window per subagent.
use crate::app::MainWindow;
use crate::backend::Backend;
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{Subagent, SubagentLog, SubagentLogEntry, time};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(1);
/// Prompt lines shown before "Show all".
const PROMPT_LINES: usize = 4;

/// Open subagent windows by (session, agent id).
#[derive(Default)]
struct Open(HashMap<(String, String), WindowHandle<SubagentWindow>>);
impl Global for Open {}

pub struct SubagentWindow {
    backend: Arc<dyn Backend>,
    session: String,
    /// The terminal's name, for "from api-refactor".
    from: String,
    agent_id: String,
    agent: Option<Subagent>,
    entries: Vec<SubagentLogEntry>,
    next: u64,
    running: bool,
    model: Option<String>,
    error: Option<String>,
    prompt_open: bool,
    scroll: ScrollHandle,
    main: WeakEntity<MainWindow>,
    main_window: AnyWindowHandle,
}

impl SubagentWindow {
    /// One read from where the last one ended; keeps going while the subagent runs.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let (backend, p) = (self.backend.clone(), json!({ "id": self.session, "agent": self.agent_id, "from": self.next }));
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.call("session.subagent_log", p) }).await;
            let again = this
                .update(cx, |w, cx| {
                    match res.and_then(|v| Ok(serde_json::from_value::<SubagentLog>(v)?)) {
                        Ok(log) => {
                            let at_bottom = w.at_bottom();
                            let grew = !log.entries.is_empty();
                            w.entries.extend(log.entries);
                            w.next = log.next;
                            w.running = log.running;
                            w.error = None;
                            if log.agent.is_some() {
                                w.agent = log.agent;
                            }
                            if log.model.is_some() {
                                w.model = log.model;
                            }
                            if grew && at_bottom {
                                w.scroll.scroll_to_bottom();
                            }
                        }
                        Err(e) => w.error = Some(format!("{e:#}")),
                    }
                    cx.notify();
                    w.running || w.error.is_some()
                })
                .unwrap_or(false);
            if again {
                cx.background_executor().timer(POLL).await;
                let _ = this.update(cx, |w, cx| w.poll(cx));
            }
        })
        .detach();
    }

    /// Following the end (or nothing to scroll yet).
    fn at_bottom(&self) -> bool {
        let (off, max) = (self.scroll.offset().y, self.scroll.max_offset().y);
        max <= px(0.) || -off >= max - px(24.)
    }

    /// Back to the main window it was opened from (the one that owns its terminal).
    fn show_parent(&mut self, cx: &mut Context<Self>) {
        let (main, id) = (self.main.clone(), self.session.clone());
        let _ = self.main_window.update(cx, |_, w, cx| {
            w.activate_window();
            let _ = main.update(cx, |m, cx| m.select(id, w, cx));
        });
    }

    fn title(&self) -> String {
        self.agent.as_ref().map(|a| a.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| format!("Subagent {}", self.agent_id))
    }
}

/// "1m 12s", "48s".
pub fn elapsed(from: &str, to: Option<&str>) -> String {
    let Some(a) = time::parse_rfc3339(from) else { return String::new() };
    let b = to.and_then(time::parse_rfc3339).unwrap_or_else(time::now_unix);
    let s = (b - a).max(0);
    if s < 60 { format!("{s}s") } else if s < 3600 { format!("{}m {:02}s", s / 60, s % 60) } else { format!("{}h {:02}m", s / 3600, s % 3600 / 60) }
}

impl Render for SubagentWindow {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let a = self.agent.as_ref();
        let kind = a.map(|a| a.agent_type.clone()).unwrap_or_default();
        let status = match (self.running, a) {
            (true, Some(a)) => format!("running {}", elapsed(&a.started_at, None)),
            (true, None) => "running".into(),
            (false, Some(a)) if a.ended_at.is_some() => format!("done in {}", elapsed(&a.started_at, a.ended_at.as_deref())),
            (false, _) => "done".into(),
        };
        let tools = self.entries.iter().filter(|e| e.kind == "tool").count();

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(44.))
            .pl(px(84.)) // the traffic lights
            .font_family(t.ui_font.clone())
            .pr(px(14.))
            .border_b_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(Icon::Orbit.el(14., t.work))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_weight(FontWeight::BOLD).child(self.title()))
            .child(div().flex_none().text_color(t.dim).font_family(t.mono_font.clone()).text_size(px(11.5)).child(format!("{kind} · from {}", self.from)))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .font_family(t.mono_font.clone())
                    .text_size(px(11.5))
                    .text_color(if self.running { t.work } else { t.ok })
                    .child(div().size(px(7.)).rounded_full().bg(if self.running { t.work } else { t.ok }))
                    .child(status),
            )
            .child(div().flex_none().px(px(6.)).rounded(px(4.)).border_1().border_color(t.line).text_color(t.dim).text_size(px(11.)).child("read-only"));

        let mut body = div().id("subagent-log").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).px(px(18.)).py(px(14.)).flex().flex_col().gap(px(2.));
        let mut prompts = 0;
        for (i, e) in self.entries.iter().enumerate() {
            body = body.child(match e.kind.as_str() {
                "prompt" => {
                    prompts += 1;
                    let lines: Vec<&str> = e.text.lines().collect();
                    let long = lines.len() > PROMPT_LINES;
                    let shown = if long && !self.prompt_open { lines[..PROMPT_LINES].join("\n") } else { e.text.clone() };
                    div()
                        .id(("prompt", i))
                        .my(px(6.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(t.line)
                        .bg(t.panel)
                        .font_family(t.ui_font.clone())
                        .text_size(px(13.))
                        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(if prompts == 1 { "PROMPT FROM CLAUDE" } else { "MESSAGE FROM CLAUDE" }))
                        .child(div().mt(px(4.)).whitespace_normal().child(shown))
                        .when(long, |d| {
                            d.child(
                                div()
                                    .id(("prompt-more", i))
                                    .mt(px(4.))
                                    .text_color(t.accent)
                                    .cursor_pointer()
                                    .child(if self.prompt_open { "Show less" } else { "Show all" })
                                    .on_click(cx.listener(|w, _, _, cx| {
                                        w.prompt_open = !w.prompt_open;
                                        cx.notify();
                                    })),
                            )
                        })
                        .into_any_element()
                }
                "tool" => div().mt(px(6.)).text_color(t.accent).child(format!("⏺ {}", e.text)).into_any_element(),
                "result" => div().pl(px(16.)).text_color(t.dim).child(format!("⎿ {}", e.text)).into_any_element(),
                "error" => div().pl(px(16.)).text_color(t.err).child(format!("⎿ {}", e.text)).into_any_element(),
                _ => div().mt(px(6.)).text_color(t.fg).child(format!("⏺ {}", e.text)).into_any_element(),
            });
        }
        if self.entries.is_empty() {
            body = body.child(div().text_color(t.dim).child(if self.running { "Waiting for it to start writing…" } else { "Nothing in its transcript." }));
        } else if self.running {
            body = body.child(div().mt(px(8.)).text_color(t.work).child("✻ Working…"));
        }
        if let Some(e) = &self.error {
            body = body.child(div().mt(px(8.)).text_color(t.err).child(format!("Couldn't read it: {e}")));
        }

        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(16.))
            .h(px(28.))
            .px(px(14.))
            .border_t_1()
            .border_color(t.line)
            .bg(t.panel)
            .font_family(t.ui_font.clone())
            .text_size(px(11.5))
            .text_color(t.dim)
            .child(format!("{tools} tool use{}", if tools == 1 { "" } else { "s" }))
            .children(self.model.clone())
            .child(div().flex_1())
            .child(
                div()
                    .id("show-parent")
                    .text_color(t.accent)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.fg))
                    .child("Show parent terminal")
                    .on_click(cx.listener(|w, _, _, cx| w.show_parent(cx))),
            );

        div()
            .key_context(crate::actions::CTX_MAIN)
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .bg(t.term)
            .text_color(t.fg)
            .font_family(t.mono_font.clone())
            .text_size(px(12.5))
            .line_height(px(20.))
            .child(header)
            .child(body)
            .child(footer)
    }
}

/// Open `agent`'s window, or bring it forward when it's open.
pub fn open(m: &MainWindow, session: String, agent: Subagent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let key = (session.clone(), agent.id.clone());
    if let Some(h) = cx.try_global::<Open>().and_then(|o| o.0.get(&key).copied())
        && h.update(cx, |_, w, _| w.activate_window()).is_ok()
    {
        return;
    }
    let from = m.sessions.iter().find(|s| s.id == session).map(|s| s.name.clone()).unwrap_or_else(|| session.clone());
    let (backend, main, main_window) = (m.backend.clone(), cx.entity().downgrade(), window.window_handle());
    let title = if agent.description.is_empty() { format!("{} subagent", agent.agent_type) } else { agent.description.clone() };
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(860.), px(620.)), cx))),
        titlebar: Some(TitlebarOptions { title: Some(format!("{title} · {from}").into()), appears_transparent: true, traffic_light_position: Some(point(px(14.), px(15.))) }),
        kind: WindowKind::Normal,
        app_id: Some("com.mrgnhnt.midna".into()),
        ..Default::default()
    };
    let k = key.clone();
    let opened = cx.open_window(opts, |_, cx| {
        cx.new(|cx| {
            cx.on_release(move |_: &mut SubagentWindow, cx| {
                cx.default_global::<Open>().0.remove(&k);
            })
            .detach();
            let mut w = SubagentWindow {
                backend,
                session,
                from,
                agent_id: agent.id.clone(),
                running: agent.ended_at.is_none(),
                agent: Some(agent),
                entries: vec![],
                next: 0,
                model: None,
                error: None,
                prompt_open: false,
                scroll: ScrollHandle::new(),
                main,
                main_window,
            };
            w.poll(cx);
            w
        })
    });
    match opened {
        Ok(h) => {
            cx.default_global::<Open>().0.insert(key, h);
        }
        Err(e) => eprintln!("midna-app: subagent window: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::elapsed;

    #[test]
    fn elapsed_reads_like_a_clock() {
        assert_eq!(elapsed("2026-10-05T10:00:00Z", Some("2026-10-05T10:00:48Z")), "48s");
        assert_eq!(elapsed("2026-10-05T10:00:00Z", Some("2026-10-05T10:01:12Z")), "1m 12s");
        assert_eq!(elapsed("2026-10-05T10:00:00Z", Some("2026-10-05T12:05:00Z")), "2h 05m");
        assert_eq!(elapsed("bad", None), "");
    }
}
