//! The queued-messages pill (Queue-E): "3 queued · next when Claude is idle" at the bottom
//! right of the pane, sitting just above the agent's input box (the second-to-last `────`
//! rule on screen) and following it as the box grows; the pane's bottom-right corner when
//! there is no input box. When a message goes in it turns green ("Sent · 2 left") with a
//! short glow; a failed one turns it red until it is retried or removed. Clicking it (or ⌘U)
//! opens the panel (`ui/queue.rs`), which the main window draws above the pill.
use super::{LINE_H, PAD_Y, TerminalView};
use crate::icons::Icon;
use crate::theme::Theme;
use crate::ui::queue::{GLOW, QueueStore, ToggleQueue, next_phrase, reduce_motion};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::QueueState;

const PILL_H: f32 = 30.;
/// Clear of the prompt rail on the right edge.
const RIGHT: f32 = 22.;

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    let n = t.chars().count();
    n >= 20 && t.chars().filter(|&c| c == '─').count() * 10 >= n * 9
}

/// The screen row of the input box's top rule: Claude Code draws the box between the last two
/// full-width rules.
pub(super) fn input_box_top(lines: &[String]) -> Option<usize> {
    let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_rule(l)).map(|(i, _)| i).collect();
    (rules.len() >= 2).then(|| rules[rules.len() - 2])
}

/// The glow at `d` (0..1) of its run: up fast, then out and fading.
fn glow(c: Hsla, d: f32) -> Vec<BoxShadow> {
    let a = if d < 0.18 { d / 0.18 } else { 1. - (d - 0.18) / 0.82 };
    vec![
        BoxShadow { color: c.opacity(0.55 * a), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(1. + 4. * d), inset: false },
        BoxShadow { color: c.opacity(0.45 * a), offset: point(px(0.), px(0.)), blur_radius: px(8. + 22. * d), spread_radius: px(2. + 6. * d), inset: false },
        drop(),
    ]
}

fn drop() -> BoxShadow {
    BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(6.)), blur_radius: px(20.), spread_radius: px(0.), inset: false }
}

impl TerminalView {
    /// `chip`: the "↓ N lines below" chip is showing in the same corner.
    pub(super) fn render_queue_pill(&self, t: &Theme, chip: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = cx.try_global::<QueueStore>()?;
        let sid = self.session_id.clone();
        let (paused, items) = store.queues.get(&sid).cloned().unwrap_or_default();
        let open = store.open.as_deref() == Some(sid.as_str());
        let flash = store.flash(&sid).cloned();
        if items.is_empty() && !open && flash.is_none() {
            store.anchors.borrow_mut().remove(&sid);
            return None;
        }

        // Above the input box when it's on screen, else the corner.
        let lines: Vec<String> = if self.ext.at_bottom() { self.grid.iter().map(|r| r.text()).collect() } else { vec![] };
        let top = input_box_top(&lines).map(|r| PAD_Y + r as f32 * LINE_H - PILL_H - 6.).filter(|y| *y > PAD_Y);
        let bottom = if chip { 46. } else { 10. };
        if let Some(b) = self.bounds.get() {
            let y = match top {
                Some(y) => b.origin.y + px(y),
                None => b.origin.y + b.size.height - px(bottom + PILL_H),
            };
            let p = point(b.origin.x + b.size.width - px(RIGHT), y);
            let moved = store.anchors.borrow_mut().insert(sid.clone(), p) != Some(p);
            if moved && open {
                // The panel is drawn by the main window from this anchor.
                window.refresh();
            }
        }

        let agent = self.agent.or(self.agent_proc).map(|a| if a == "codex" { "Codex" } else { "Claude" }).or_else(|| store.agents.get(&sid).copied());
        let failed = items.first().is_some_and(|m| m.state == QueueState::Failed);
        let dot = |c: Hsla| div().flex_none().flex().items_center().justify_center().size(px(14.)).rounded_full().bg(c.opacity(0.22)).child(div().size(px(8.)).rounded_full().bg(c));
        let (border, lead, label, label_color, rest): (Hsla, AnyElement, String, Hsla, String) = match &flash {
            Some(f) if f.ok => (t.ok, Icon::Check.el(12., t.ok).into_any_element(), "Sent".into(), t.ok, if f.left > 0 { format!("· {} left", f.left) } else { String::new() }),
            _ if failed || flash.as_ref().is_some_and(|f| !f.ok) => (t.err, Icon::Cross.el(12., t.err).into_any_element(), "Couldn't send".into(), t.err, "· open to retry".into()),
            _ if items.is_empty() => (t.accent, dot(t.accent).into_any_element(), "Nothing queued".into(), t.fg, String::new()),
            _ if paused => (t.line, dot(t.dim).into_any_element(), format!("{} queued", items.len()), t.fg, "· paused".into()),
            _ => {
                let next = items.first().map(|m| next_phrase(&m.when, agent, &store.names)).unwrap_or_default();
                (t.accent, dot(t.accent).into_any_element(), format!("{} queued", items.len()), t.fg, next)
            }
        };
        let tip = crate::ui::header::tip_keys("Queued messages", "keys.queue");
        let pill = div()
            .id("queue-pill")
            .absolute()
            .right(px(RIGHT))
            .map(|d| match top {
                Some(y) => d.top(px(y)),
                None => d.bottom(px(bottom)),
            })
            .h(px(PILL_H))
            .max_w(px(420.))
            .flex()
            .items_center()
            .gap(px(7.))
            .pl(px(10.))
            .pr(px(12.))
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(if open { t.accent_soft } else { t.raised })
            .font_family(t.ui_font.clone())
            .text_size(px(12.5))
            .text_color(t.fg)
            .whitespace_nowrap()
            .cursor_pointer()
            .tooltip(tip)
            // The terminal under it must not start a selection or take focus.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_, _, window, cx| {
                if let Some(st) = cx.try_global::<QueueStore>() {
                    st.target.replace(Some(sid.clone()));
                }
                window.dispatch_action(Box::new(ToggleQueue), cx);
            }))
            .child(lead)
            .child(div().flex_none().font_weight(FontWeight::BOLD).text_color(label_color).child(label))
            .when(!rest.is_empty(), |d| d.child(div().min_w_0().overflow_hidden().text_ellipsis().text_size(px(12.)).text_color(t.dim).child(rest)));

        if let (Some(f), Ok(d)) = (&flash, crate::dev::var("MIDNA_DEBUG_QUEUE_GLOW").map(|v| v.parse::<f32>().unwrap_or(0.3))) {
            return Some(pill.shadow(glow(if f.ok { t.ok } else { t.err }, d)).into_any_element());
        }
        Some(match flash {
            Some(f) if f.at.elapsed() < GLOW => {
                let c = if f.ok { t.ok } else { t.err };
                if reduce_motion() {
                    pill.shadow(vec![BoxShadow { color: c.opacity(0.5), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }, drop()]).into_any_element()
                } else {
                    let ease = |x: f32| 1. - (1. - x).powi(3);
                    pill.with_animation(SharedString::from(format!("queue-glow-{}", f.seq)), Animation::new(GLOW).with_easing(ease), move |el, d| el.shadow(glow(c, d))).into_any_element()
                }
            }
            _ => pill.shadow(vec![drop()]).into_any_element(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::input_box_top;

    #[test]
    fn finds_the_input_box_top_rule() {
        let rule = "─".repeat(60);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let screen = s(&["⏺ Done.", "", &rule, "❯ ", &rule, "  ⏵⏵ accept edits on"]);
        assert_eq!(input_box_top(&screen), Some(2));
        // A rule in the transcript above doesn't count; only the last two.
        let screen = s(&[&rule, "text", "", &rule, "❯ draft", "  more", &rule, ""]);
        assert_eq!(input_box_top(&screen), Some(3));
        assert_eq!(input_box_top(&s(&["$ ls", "a b c"])), None);
        assert_eq!(input_box_top(&s(&["──── short ────", "x"])), None);
    }
}
