//! Settings: a folder list for the path-list settings (`projects.roots`, `agents.trust_folders`).
//! One row per entry, shown as typed (`~/Development`, `~/work/client-*`) with a remove button,
//! then Add, which opens a folder picker. Every change saves the whole list at once.
use super::*;

/// `path` with the home folder written as `~`, the way the CLI and the catalog show paths.
fn tilde(path: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some("") if !home.is_empty() => "~".into(),
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The list with `picked` added at the end, skipping ones already in it.
fn with_added(list: &[String], picked: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = list.to_vec();
    for p in picked {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

impl SettingsWindow {
    fn folders(&self, key: &str) -> Vec<String> {
        self.value(key).as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    }

    /// The folder picker; what's picked goes at the end of `key`'s list.
    fn add_folders(&mut self, key: String, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Add".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let home = std::env::var("HOME").unwrap_or_default();
            let picked: Vec<String> = paths.iter().map(|p| tilde(&p.to_string_lossy(), &home)).collect();
            let _ = this.update(cx, |s, cx| {
                let list = s.folders(&key);
                let next = with_added(&list, picked);
                if next != list {
                    s.set(&key, json!(next), cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn folders_control(&self, t: &Theme, key: String, list: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div().flex().flex_col().gap(px(6.)).w_full();
        if list.is_empty() {
            col = col.child(div().text_size(px(12.)).text_color(t.dim).child("No folders yet"));
        }
        for (n, path) in list.iter().enumerate() {
            let (k, rest) = (key.clone(), list.iter().filter(|p| *p != path).cloned().collect::<Vec<_>>());
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .child(Icon::Project.el(12., t.dim))
                    .child(div().flex_1().min_w_0().truncate().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.fg).child(path.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("folder-x-{key}-{n}")))
                            .flex_none()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.line))
                            .tooltip(crate::ui::header::tip("Remove"))
                            .on_click(cx.listener(move |s, _, _, cx| s.set(&k, json!(rest), cx)))
                            .child(Icon::Cross.el(10., t.dim)),
                    ),
            );
        }
        let k = key.clone();
        col.child(
            div().flex().child(
                div()
                    .id(SharedString::from(format!("folder-add-{key}")))
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .text_size(px(12.))
                    .text_color(t.fg)
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.dim))
                    .on_click(cx.listener(move |s, _, _, cx| s.add_folders(k.clone(), cx)))
                    .child(Icon::Plus.el(11., t.dim))
                    .child("Add folder…"),
            ),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{tilde, with_added};

    #[test]
    fn picked_folders_read_like_typed_ones() {
        assert_eq!(tilde("/Users/me/Development", "/Users/me"), "~/Development");
        assert_eq!(tilde("/Users/me", "/Users/me/"), "~");
        assert_eq!(tilde("/Users/meg/x", "/Users/me"), "/Users/meg/x");
        assert_eq!(tilde("/opt/src", ""), "/opt/src");
        let list = vec!["~/a".to_string(), "~/work/client-*".to_string()];
        assert_eq!(with_added(&list, ["~/a".into(), "~/b".into(), "~/b".into()]), ["~/a", "~/work/client-*", "~/b"]);
    }
}
