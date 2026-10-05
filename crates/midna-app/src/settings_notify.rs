//! Settings ▸ Sounds / Sound effects / Notification images: each kind's sound (a macOS sound
//! or one you imported), its volume with a preview, and its image. Sound effects
//! (`notify::EFFECTS`, played by `crate::sounds`) use the same picker and settings. Picking "Import…" copies the file into
//! midna (`notify.import`) and uses it; the rows are ordinary `notify.sound.<kind>`,
//! `notify.volume.<kind>` and `notify.image[.<kind>]` settings, so agents can set them too.
use super::*;
use midna_proto::notify::{CATEGORIES, EFFECTS, image_key, setting_key, sound_key, volume_key};
use std::path::PathBuf;

/// One entry in a sound or image picker.
struct Pick {
    label: String,
    value: String,
    thumb: Option<PathBuf>,
    /// An imported file (offer Remove).
    imported: bool,
    /// A section heading ("Twilight", "macOS"), not a choice.
    heading: bool,
}

impl Pick {
    fn new(label: impl Into<String>, value: impl Into<String>) -> Pick {
        Pick { label: label.into(), value: value.into(), thumb: None, imported: false, heading: false }
    }

    fn heading(label: &str) -> Pick {
        Pick { heading: true, ..Pick::new(label, "") }
    }
}

const STEP: i64 = 10;

impl SettingsWindow {
    fn int(&self, key: &str) -> i64 {
        self.value(key).as_i64().unwrap_or(100)
    }

    fn text_of(&self, key: &str) -> String {
        self.value(key).as_str().unwrap_or("").to_string()
    }

    /// An imported or macOS file by name (`notify.media`).
    fn media_path(&self, kind: &str, name: &str) -> Option<PathBuf> {
        let list = self.media.get(if kind == "sound" { "sounds" } else { "images" })?.as_array()?;
        list.iter().find(|m| m["name"] == name).and_then(|m| m["path"].as_str()).map(PathBuf::from)
    }

    /// Names with their set: `twilight`, `macos`, or empty for an imported file.
    fn media_names(&self, kind: &str) -> Vec<(String, String)> {
        let list = self.media.get(if kind == "sound" { "sounds" } else { "images" }).and_then(Value::as_array).cloned().unwrap_or_default();
        list.iter()
            .filter_map(|m| {
                // A daemon from before the Twilight set marks macOS sounds only as builtin.
                let set = m["set"].as_str().unwrap_or(if m["builtin"] == true { "macos" } else { "" });
                Some((m["name"].as_str()?.to_string(), set.to_string()))
            })
            .collect()
    }

    /// Play a kind's sound the way a notification would (its volume times the master).
    fn preview(&self, cat: &str) {
        let name = self.text_of(&sound_key(cat));
        if let Some(path) = self.media_path("sound", &name) {
            let v = self.int("notify.volume") * self.int(&volume_key(cat)) / 100;
            crate::notify::play(&path.to_string_lossy(), v.clamp(0, 100) as u8);
        }
    }

    pub(super) fn notify_sound_rows(&self, t: &Theme) -> Vec<RowSpec> {
        let master = self.int("notify.volume");
        let mut rows = vec![RowSpec {
            label: "All sounds".into(),
            note: Some((
                "Every kind's volume (notifications and sound effects) is scaled by this; 0 = silent. Sounds play at your Mac's alert volume. Focus or midna's sound setting in System Settings silences the ones that come with a banner."
                    .into(),
                Hsla::default(),
            )),
            control: Control::Volume { key: "notify.volume".into() },
            cli: format!("midna settings set notify.volume {master}"),
            who: Who::Agents,
            warn: false,
        }];
        for c in CATEGORIES {
            let on = self.value(&setting_key(c.key)) != Value::Bool(false);
            let sound = self.text_of(&sound_key(c.key));
            let note = if !on {
                Some((format!("“{}” notifications are off", c.label), t.dim))
            } else if c.key == "agent" {
                Some(("Plays only when the agent asks for a sound".into(), Hsla::default()))
            } else {
                None
            };
            rows.push(RowSpec {
                label: c.label.into(),
                note,
                control: Control::Sound { cat: c.key },
                cli: format!("midna settings set {} {}", sound_key(c.key), if sound.contains(' ') { format!("\"{sound}\"") } else { sound.clone() }),
                who: Who::Agents,
                warn: false,
            });
        }
        self.silence_notes(t, &mut rows[1..], master);
        rows
    }

    /// Sounds with no banner: what you do, then small UI cues.
    pub(super) fn sound_effect_rows(&self, t: &Theme) -> Vec<RowSpec> {
        let mut rows: Vec<RowSpec> = EFFECTS
            .iter()
            .map(|e| {
                let sound = self.text_of(&sound_key(e.key));
                RowSpec {
                    label: e.label.into(),
                    note: Some((e.description.into(), Hsla::default())),
                    control: Control::Sound { cat: e.key },
                    cli: format!("midna settings set {} {}", sound_key(e.key), if sound.contains(' ') { format!("\"{sound}\"") } else { sound.clone() }),
                    who: Who::Agents,
                    warn: false,
                }
            })
            .collect();
        self.silence_notes(t, &mut rows, self.int("notify.volume"));
        rows
    }

    /// Why a sound row plays nothing: sounds are off, or All sounds is at 0.
    fn silence_notes(&self, t: &Theme, rows: &mut [RowSpec], master: i64) {
        let why = if self.value("notify.sounds") == Value::Bool(false) {
            "Silent while Play sounds is off"
        } else if master == 0 {
            "Silent while All sounds is at 0"
        } else {
            return;
        };
        for r in rows {
            r.note = Some((why.into(), t.dim));
        }
    }

    pub(super) fn notify_image_rows(&self, t: &Theme) -> Vec<RowSpec> {
        let all = self.text_of("notify.image");
        let mut rows = vec![RowSpec {
            label: "Every notification".into(),
            note: Some(("Shown beside the text. Each kind can use its own. Not shown when the app isn't running (macOS shows Script Editor's).".into(), Hsla::default())),
            control: Control::Image { key: "notify.image".into(), cat: None },
            cli: format!("midna settings set notify.image {}", if all.is_empty() { "<imported image>".to_string() } else { all }),
            who: Who::Agents,
            warn: false,
        }];
        for c in CATEGORIES {
            let key = image_key(c.key);
            let v = self.text_of(&key);
            let note = (self.value(&setting_key(c.key)) == Value::Bool(false)).then(|| (format!("“{}” notifications are off", c.label), t.dim));
            rows.push(RowSpec {
                label: c.label.into(),
                note,
                control: Control::Image { key: key.clone(), cat: Some(c.key) },
                cli: format!("midna settings set {key} {}", if v.is_empty() { "''".to_string() } else { v }),
                who: Who::Agents,
                warn: false,
            });
        }
        rows
    }

    /// − 70% + : steps of 10.
    pub(super) fn volume_stepper(&self, t: &Theme, key: &str, cat: Option<&'static str>, cx: &mut Context<Self>) -> Div {
        let v = self.int(key);
        let step = |id: &str, label: &'static str, to: i64, enabled: bool| {
            let k = key.to_string();
            div()
                .id(SharedString::from(format!("{id}-{key}")))
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_color(if enabled { t.fg } else { t.dim })
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(t.raised)))
                .on_click(cx.listener(move |s, _, _, cx| {
                    if enabled {
                        s.set(&k, json!(to), cx);
                        if let Some(c) = cat {
                            s.preview(c);
                        }
                    }
                }))
                .child(label)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(7.))
            .border_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(step("vol-down", "−", (v - STEP).max(0), v > 0))
            .child(div().w(px(38.)).text_align(TextAlign::Center).text_size(px(12.)).child(format!("{v}%")))
            .child(step("vol-up", "+", (v + STEP).min(100), v < 100))
    }

    fn picker_button(&self, t: &Theme, key: &str, label: String, width: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.picker.as_deref() == Some(key);
        let k = key.to_string();
        div()
            .id(SharedString::from(format!("pick-{key}")))
            .w(px(width))
            .flex_none()
            .h(px(26.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(7.))
            .border_1()
            .border_color(if open { t.accent } else { t.line })
            .bg(t.panel)
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |s, _, _, cx| {
                s.picker = if s.picker.as_deref() == Some(k.as_str()) { None } else { Some(k.clone()) };
                cx.notify();
            }))
            .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(label))
            .child(Icon::Chevron.el(10., t.dim))
    }

    pub(super) fn sound_control(&self, t: &Theme, cat: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let key = sound_key(cat);
        let value = self.text_of(&key);
        let silent = value == "none";
        let label = if silent { "None".to_string() } else { value.clone() };
        // Yours first, then midna's Twilight sounds, then the macOS ones.
        let names = self.media_names("sound");
        let mut picks = vec![Pick::new("None", "none")];
        picks.extend(names.iter().filter(|(_, set)| set.is_empty()).map(|(n, _)| Pick { imported: true, ..Pick::new(n, n) }));
        for (set, title) in [("twilight", "Twilight"), ("macos", "macOS")] {
            let group: Vec<Pick> = names.iter().filter(|(_, s)| s == set).map(|(n, _)| Pick::new(n, n)).collect();
            if !group.is_empty() {
                picks.push(Pick::heading(title));
                picks.extend(group);
            }
        }
        let open = self.picker.as_deref() == Some(key.as_str());
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .relative()
                    .child(self.picker_button(t, &key, label, 104., cx))
                    .when(open, |d| d.child(self.picker_menu(t, &key, "sound", &value, picks, cx))),
            )
            .child(self.volume_stepper(t, &volume_key(cat), Some(cat), cx).when(silent, |d| d.opacity(0.5)))
            .child(
                div()
                    .id(SharedString::from(format!("play-{cat}")))
                    .size(px(24.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .when(!silent, |d| d.cursor_pointer().hover(|s| s.bg(t.raised)))
                    .on_click(cx.listener(move |s, _, _, _| s.preview(cat)))
                    .child(Icon::Play.el(12., if silent { t.line } else { t.dim })),
            )
            .into_any_element()
    }

    pub(super) fn image_control(&self, t: &Theme, key: &str, cat: Option<&'static str>, cx: &mut Context<Self>) -> AnyElement {
        let value = self.text_of(key);
        // What a kind shows: its own, or the one every notification uses.
        let shown = match (value.as_str(), cat) {
            ("", Some(_)) => self.text_of("notify.image"),
            ("none", _) => String::new(),
            (v, _) => v.to_string(),
        };
        let label = match (value.as_str(), cat) {
            ("", Some(_)) if shown.is_empty() => "Same (none)".to_string(),
            ("", Some(_)) => format!("Same ({shown})"),
            ("" | "none", _) => "None".to_string(),
            (v, _) => v.to_string(),
        };
        let thumb = self.media_path("image", &shown);
        let mut picks = vec![];
        if cat.is_some() {
            picks.push(Pick::new("Same as every notification", ""));
        }
        picks.push(Pick::new("None", if cat.is_some() { "none" } else { "" }));
        picks.extend(self.media_names("image").into_iter().map(|(n, _)| Pick { thumb: self.media_path("image", &n), imported: true, ..Pick::new(&n, &n) }));
        let open = self.picker.as_deref() == Some(key);
        let current = if cat.is_none() && value == "none" { String::new() } else { value.clone() };
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .size(px(26.))
                    .flex_none()
                    .rounded(px(5.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(t.line)
                    .bg(t.panel)
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|d| match thumb {
                        Some(p) => d.child(img(p).size_full().object_fit(ObjectFit::Cover)),
                        None => d.child(Icon::Image.el(12., t.line)),
                    }),
            )
            .child(
                div()
                    .relative()
                    .child(self.picker_button(t, key, label, 150., cx))
                    .when(open, |d| d.child(self.picker_menu(t, key, "image", &current, picks, cx))),
            )
            .when_some(cat, |d, c| {
                d.child(
                    div()
                        .id(SharedString::from(format!("test-{c}")))
                        .flex_none()
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(7.))
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.raised).text_color(t.fg))
                        .on_click(cx.listener(move |s, _, _, cx| s.test_notification(c, cx)))
                        .child("Test"),
                )
            })
            .into_any_element()
    }

    fn picker_menu(&self, t: &Theme, key: &str, kind: &'static str, current: &str, picks: Vec<Pick>, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().id(SharedString::from(format!("pick-list-{key}"))).max_h(px(320.)).overflow_y_scroll().flex().flex_col();
        for (i, p) in picks.into_iter().enumerate() {
            if p.heading {
                list = list.child(
                    div().px(px(8.)).pt(px(8.)).pb(px(2.)).text_size(px(10.5)).text_color(t.dim).child(p.label.to_uppercase()),
                );
                continue;
            }
            let on = p.value == current;
            let (k, v) = (key.to_string(), p.value.clone());
            let remove = p.imported.then(|| p.value.clone());
            list = list.child(
                div()
                    .id(SharedString::from(format!("pick-{key}-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent_soft))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.picker = None;
                        s.set(&k, json!(v), cx);
                        // Hear the choice (sound effects as well as notification kinds).
                        if let Some(kind) = k.strip_prefix("notify.sound.").filter(|k| midna_proto::notify::has_sound(k)) {
                            s.preview(kind);
                        }
                    }))
                    .child(div().w(px(12.)).flex_none().when(on, |d| d.child(Icon::Check.el(11., t.accent))))
                    .when_some(p.thumb, |d, th| {
                        d.child(div().size(px(20.)).flex_none().rounded(px(4.)).overflow_hidden().child(img(th).size_full().object_fit(ObjectFit::Cover)))
                    })
                    .child(div().flex_1().min_w_0().truncate().child(p.label))
                    .when_some(remove, |d, name| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("pick-rm-{key}-{i}")))
                                .flex_none()
                                .px(px(4.))
                                .rounded(px(4.))
                                .hover(|s| s.bg(t.raised))
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    cx.stop_propagation();
                                    s.remove_media(name.clone(), cx);
                                }))
                                .child(Icon::Trash.el(11., t.dim)),
                        )
                    }),
            );
        }
        let k = key.to_string();
        let import = div()
            .id(SharedString::from(format!("pick-import-{key}")))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(5.))
            .mt(px(4.))
            .border_t_1()
            .border_color(t.line)
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(t.accent_soft))
            .on_click(cx.listener(move |s, _, _, cx| {
                s.picker = None;
                s.import_media(kind, k.clone(), cx);
            }))
            .child(div().w(px(12.)).flex_none().child(Icon::Plus.el(11., t.dim)))
            .child(if kind == "sound" { "Import sound…" } else { "Import image…" })
            .child(div().text_size(px(11.)).text_color(t.dim).child(if kind == "sound" { "aiff wav mp3 m4a caf" } else { "png jpg gif" }));
        deferred(
            anchored().offset(point(px(0.), px(30.))).snap_to_window_with_margin(px(8.)).child(
                crate::ui::sidebar::menu_box(t)
                    .id(SharedString::from(format!("pick-menu-{key}")))
                    .min_w(px(230.))
                    .p(px(4.))
                    .text_size(px(12.5))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(list)
                    .child(import),
            ),
        )
        .with_priority(2)
        .into_any_element()
    }

    /// File picker → `notify.import` (copied into midna) → used for `key`.
    fn import_media(&mut self, kind: &'static str, key: String, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import".into()) });
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let k = key.clone();
            let r = cx.background_executor().spawn(async move { backend.call("notify.import", json!({ "path": path, "use_for": [k] })) }).await;
            let _ = this.update(cx, |s, cx| {
                let file = r.as_ref().ok().and_then(|v| v["name"].as_str()).unwrap_or("").to_string();
                s.mine.push((key.clone(), Instant::now()));
                s.last = Some(Last {
                    cmd: format!("midna notify import <{kind}> --for {}", key.rsplit('.').next().filter(|_| key != "notify.image").unwrap_or("all")),
                    ok: r.is_ok(),
                    result: match &r {
                        Ok(_) => format!("✓ imported {file}"),
                        Err(e) => format!("✗ {e:#}"),
                    },
                    who: "you, from this window".into(),
                    at: Instant::now(),
                });
                s.load(cx);
                // Hear it once it's loaded.
                if let (Ok(v), Some(cat)) = (&r, key.strip_prefix("notify.sound.")) {
                    let vol = s.int("notify.volume") * s.int(&volume_key(cat)) / 100;
                    if let Some(p) = v["path"].as_str() {
                        crate::notify::play(p, vol.clamp(0, 100) as u8);
                    }
                }
            });
        })
        .detach();
    }

    fn remove_media(&mut self, name: String, cx: &mut Context<Self>) {
        self.picker = None;
        let backend = self.backend.clone();
        let n = name.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("notify.remove", json!({ "name": n })) }).await;
            let _ = this.update(cx, |s, cx| {
                let reset = r.as_ref().ok().and_then(|v| v["reset"].as_array().cloned()).unwrap_or_default();
                s.last = Some(Last {
                    cmd: format!("midna notify remove {name}"),
                    ok: r.is_ok(),
                    result: match &r {
                        Ok(_) if reset.is_empty() => "✓ removed".into(),
                        Ok(_) => format!("✓ removed; {} setting{} back to default", reset.len(), if reset.len() == 1 { "" } else { "s" }),
                        Err(e) => format!("✗ {e:#}"),
                    },
                    who: "you, from this window".into(),
                    at: Instant::now(),
                });
                s.load(cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn test_notification(&mut self, cat: &'static str, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("notify.test", json!({ "category": cat })) }).await;
            let _ = this.update(cx, |s, cx| {
                let reason = r.as_ref().ok().and_then(|v| v["reason"].as_str()).map(str::to_string);
                s.last = Some(Last {
                    cmd: format!("midna notify test {cat}"),
                    ok: r.is_ok() && reason.is_none(),
                    result: match (&r, reason) {
                        (Ok(_), None) => "✓ sent".into(),
                        (Ok(_), Some(why)) => format!("✗ not sent: {why}"),
                        (Err(e), _) => format!("✗ {e:#}"),
                    },
                    who: "you, from this window".into(),
                    at: Instant::now(),
                });
                cx.notify();
            });
        })
        .detach();
    }
}
