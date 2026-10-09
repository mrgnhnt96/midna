//! Tooltips with room for long text (design B + C on the "Midna Tooltips" canvas): a card
//! of fixed width that wraps, `code` in mono, and for a setting its first sentence, its
//! "value: what it does" clauses as a list, the rest dimmed, and its key in a footer.
//! Short tips keep the one-line look (header.rs `Tip`).
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

/// The card's width. GPUI lays a tooltip out at its smallest size, so it needs one to wrap.
const WIDTH: f32 = 340.;
/// Longer than this (in chars) and a plain tip becomes a card.
pub const LONG: usize = 60;

/// A description cut up for the card.
#[derive(Debug, PartialEq)]
pub struct Parts<'a> {
    pub lead: &'a str,
    /// (value, what it does), in the order the text gives them.
    pub values: Vec<(&'a str, &'a str)>,
    pub rest: String,
}

/// Split a description into its first sentence, its "value: …" clauses (each starting a
/// sentence; two or more of `values`) and the rest.
pub fn parts<'a>(text: &'a str, values: &[&'a str]) -> Parts<'a> {
    let mut marks: Vec<(usize, &str)> = vec![];
    for v in values.iter().filter(|v| !v.is_empty()) {
        let tag = format!("{v}: ");
        let mut from = 0;
        while let Some(i) = text[from..].find(&tag) {
            let at = from + i;
            if at == 0 || text[..at].ends_with(". ") {
                marks.push((at, v));
                break;
            }
            from = at + tag.len();
        }
    }
    marks.sort_by_key(|m| m.0);
    if marks.len() < 2 {
        let all = sentences(text);
        return Parts { lead: all[0].trim(), values: vec![], rest: all[1..].join(" ") };
    }
    let mut list = vec![];
    let mut rest = String::new();
    for (n, &(at, v)) in marks.iter().enumerate() {
        let start = at + v.len() + 2;
        match marks.get(n + 1) {
            Some(&(next, _)) => list.push((v, text[start..next].trim())),
            None => {
                // the last value has its own sentence; anything after it is the rest
                let tail = sentences(&text[start..]);
                list.push((v, tail[0].trim()));
                rest = tail[1..].join(" ");
            }
        }
    }
    Parts { lead: text[..marks[0].0].trim(), values: list, rest }
}

/// A text's sentences, split at ". " (not after "e.g.", "i.e.", "etc.").
pub fn sentences(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let (mut start, mut from) = (0, 0);
    while let Some(i) = text[from..].find(". ") {
        let end = from + i + 1;
        if !["e.g.", "i.e.", "etc."].iter().any(|a| text[..end].ends_with(a)) {
            out.push(&text[start..end]);
            start = end + 1;
        }
        from = end;
    }
    out.push(&text[start..]);
    out
}

/// A description's trailing " (default: …)" (Settings adds it to changed settings), split off.
fn split_default(text: &str) -> (&str, Option<&str>) {
    match text.rfind(" (default: ") {
        Some(i) if text.ends_with(')') => (&text[..i], Some(&text[i + 11..text.len() - 1])),
        _ => (text, None),
    }
}

/// The card. `footer` (a setting's key) also gets the description's default, if it has one.
pub fn card(t: &Theme, text: &str, values: &[SharedString], footer: &str) -> Div {
    let (text, default) = if footer.is_empty() { (text, None) } else { split_default(text) };
    let values: Vec<&str> = values.iter().map(|v| v.as_ref()).collect();
    // a plain tip is one paragraph; a setting's is cut up
    let p = if footer.is_empty() { Parts { lead: text, values: vec![], rest: String::new() } } else { parts(text, &values) };
    let footer = match default {
        Some(d) => format!("{footer} · default {d}"),
        None => footer.to_string(),
    };
    div()
        .w(px(WIDTH))
        .flex()
        .flex_col()
        .rounded(px(8.))
        .bg(t.raised)
        .border_1()
        .border_color(t.line)
        .text_color(t.fg)
        .font_family(t.ui_font.clone())
        .text_size(px(11.5))
        .line_height(px(17.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(7.))
                .px(px(11.))
                .pt(px(8.))
                .pb(px(9.))
                .when(!p.lead.is_empty(), |d| d.child(prose(t, p.lead, t.fg)))
                .when(!p.values.is_empty(), |d| {
                    d.child(div().flex().flex_col().gap(px(4.)).children(p.values.iter().map(|(v, what)| {
                        div()
                            .flex()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(72.))
                                    .font_family(t.mono_font.clone())
                                    .text_size(px(10.5))
                                    .text_color(t.accent)
                                    .child(SharedString::from(v.to_string())),
                            )
                            .child(div().flex_1().min_w_0().child(prose(t, &capitalized(what), t.fg)))
                    })))
                })
                .when(!p.rest.is_empty(), |d| d.child(prose(t, &p.rest, t.dim))),
        )
        .when(!footer.is_empty(), |d| {
            d.child(
                div()
                    .px(px(11.))
                    .py(px(4.))
                    .border_t_1()
                    .border_color(t.line)
                    .font_family(t.mono_font.clone())
                    .text_size(px(10.5))
                    .text_color(t.dim)
                    .child(footer),
            )
        })
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Text with its `backticked` spans in mono, tinted with the accent.
fn prose(t: &Theme, text: &str, color: Hsla) -> StyledText {
    let run = |len: usize, family: &SharedString, color: Hsla| TextRun {
        len,
        font: font(family.clone()),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let pieces: Vec<&str> = text.split('`').collect();
    // an unpaired backtick means the text isn't marked up; show it as typed
    if pieces.len() % 2 == 0 {
        return StyledText::new(SharedString::from(text.to_string())).with_runs(vec![run(text.len(), &t.ui_font, color)]);
    }
    let out: String = pieces.concat();
    let runs = pieces
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_empty())
        .map(|(i, p)| if i % 2 == 1 { run(p.len(), &t.mono_font, t.accent) } else { run(p.len(), &t.ui_font, color) })
        .collect();
    StyledText::new(SharedString::from(out)).with_runs(runs)
}

#[cfg(test)]
mod tests {
    use super::{Parts, parts, sentences, split_default};

    #[test]
    fn values_become_a_list() {
        let text = "Restart the terminal. ask: offer Restart (no needs-you item; Not now hides it). when_idle: queue it. \
                    A queued restart waits. off: do nothing. Changing it answers prompts.";
        let p = parts(text, &["ask", "when_idle", "off"]);
        assert_eq!(p.lead, "Restart the terminal.");
        assert_eq!(
            p.values,
            vec![("ask", "offer Restart (no needs-you item; Not now hides it)."), ("when_idle", "queue it. A queued restart waits."), ("off", "do nothing.")]
        );
        assert_eq!(p.rest, "Changing it answers prompts.");
    }

    #[test]
    fn values_mid_sentence_are_prose() {
        // "prompt" appears, but not as "prompt: " at a sentence start
        let text = "Name terminals. agent: the summary, falling back to prompt: then context. off: keep names.";
        let p = parts(text, &["agent", "prompt", "off"]);
        assert_eq!(p.values.iter().map(|v| v.0).collect::<Vec<_>>(), vec!["agent", "off"]);
        assert_eq!(p.values[0].1, "the summary, falling back to prompt: then context.");
    }

    #[test]
    fn one_value_is_not_a_list() {
        let p = parts("Run it under midna. Subcommands run as typed. Off: everything runs as typed.", &["On", "Off"]);
        assert_eq!(p, Parts { lead: "Run it under midna.", values: vec![], rest: "Subcommands run as typed. Off: everything runs as typed.".into() });
    }

    #[test]
    fn sentences_skip_abbreviations() {
        assert_eq!(sentences("Use a key, e.g. ⌘K. Then go."), vec!["Use a key, e.g. ⌘K.", "Then go."]);
    }

    #[test]
    fn default_splits_off() {
        assert_eq!(split_default("Which channel. (default: stable)"), ("Which channel.", Some("stable")));
        assert_eq!(split_default("Which channel (beta)."), ("Which channel (beta).", None));
    }
}
