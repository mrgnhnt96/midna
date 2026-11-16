//! Images sent to the Mac a remote window drives (design T1948, local option C; DECISIONS.md
//! "Nearby: driving another of my Macs"). In a window on another Mac, every way an image reaches
//! a terminal (⌘V of a screenshot, ⌘⇧V, a pasted image path in an agent, a drop on the pane or
//! its sidebar row, ⌘I and the toolbar's Add image) comes here instead of the image sheet or a
//! local path, which would mean nothing there.
//!
//! The image is made ready here (`annotate::save`: upright, at most 2000 px) and a bar under
//! the terminal asks "Send shot.png to Bravo?" with its size. Nothing leaves this Mac until the
//! person clicks Send or presses ⌘↩; then `session.send_image` carries it over the drive link,
//! and that Mac saves it in its images folder and pastes its own path. Esc or Cancel drops it.
//! That Mac takes it only from the person driving (never from an agent or a script there or
//! here) and only an image under `SEND_IMAGE_MAX_BYTES`; anything bigger is turned down here
//! first. Dropped files that aren't images are refused with a short reason.
use super::TerminalView;
use crate::annotate::Source;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::SEND_IMAGE_MAX_BYTES;
use serde_json::json;
use std::path::{Path, PathBuf};

/// The images waiting for the person's yes.
pub struct Bar {
    images: Vec<Ready>,
    /// The terminal's name there ("planner"), for "its path typed into planner".
    into: String,
    sending: bool,
    error: Option<String>,
}

struct Ready {
    /// Its name here: what the bar says, and what the copy there is named after.
    name: String,
    /// The made-ready copy here (the thumbnail).
    path: PathBuf,
    bytes: Vec<u8>,
}

/// `240 KB`, `1.4 MB`.
fn size(n: usize) -> String {
    if n < 1024 * 1024 { format!("{} KB", n.div_ceil(1024)) } else { format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)) }
}

/// "Send shot.png to Bravo?" / "Send 3 images to Bravo?"
fn headline(names: &[&str], mac: &str) -> String {
    match names {
        [one] => format!("Send {one} to {mac}?"),
        many => format!("Send {} images to {mac}?", many.len()),
    }
}

/// "240 KB. Saved in a temp folder there and its path typed into planner."
fn detail(bytes: usize, count: usize, into: &str) -> String {
    let (its, path) = if count == 1 { ("its", "path") } else { ("their", "paths") };
    format!("{}. Saved in a temp folder there and {its} {path} typed into {into}.", size(bytes))
}

/// Why dropped files that aren't images weren't sent.
fn refusal(others: &[PathBuf], mac: &str) -> String {
    let what = match others {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "That file".into()),
        many => format!("{} files", many.len()),
    };
    format!("Only images can be sent to {mac}. {what} stayed on this Mac.")
}

/// Why an image wasn't offered: too big to send.
fn too_big(name: &str, bytes: usize, mac: &str) -> String {
    format!("{name} is too big to send to {mac} ({}, limit {} MB).", size(bytes), SEND_IMAGE_MAX_BYTES >> 20)
}

/// What a source is called here.
fn name_of(src: &Source) -> String {
    match src {
        Source::Bytes { name, .. } => name.clone(),
        Source::Path(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into()),
    }
}

/// Make each source ready to send: (name, the made-ready copy, its bytes), or why it can't be.
fn prepare(sources: Vec<Source>) -> Vec<Result<Ready, String>> {
    sources
        .into_iter()
        .map(|src| {
            let name = name_of(&src);
            let path = crate::annotate::save(src).map_err(|e| format!("Couldn't read {name}: {e}"))?;
            let bytes = std::fs::read(&path).map_err(|e| format!("Couldn't read {name}: {e}"))?;
            Ok(Ready { name, path, bytes })
        })
        .collect()
}

impl TerminalView {
    /// The Mac this terminal is on, when it isn't this one.
    pub fn remote_mac(&self) -> Option<String> {
        self.backend.mac().map(|m| m.name.clone())
    }

    /// Images pasted or dropped on this terminal in a remote window: made ready, then asked
    /// about in the bar. Nothing is sent from here.
    pub fn offer_images(&mut self, sources: Vec<Source>, cx: &mut Context<Self>) {
        let Some(mac) = self.remote_mac() else { return };
        if sources.is_empty() {
            return;
        }
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        cx.spawn(async move |this, cx| {
            let (ready, into) = cx
                .background_executor()
                .spawn(async move {
                    let into = backend.call("session.get", json!({ "id": id })).ok().and_then(|s| s["name"].as_str().map(str::to_string));
                    (prepare(sources), into.filter(|n| !n.is_empty()).unwrap_or_else(|| "this terminal".into()))
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                let mut said = vec![];
                let mut ok = vec![];
                for r in ready {
                    match r {
                        Ok(r) if r.bytes.len() > SEND_IMAGE_MAX_BYTES => said.push(too_big(&r.name, r.bytes.len(), &mac)),
                        Ok(r) => ok.push(r),
                        Err(e) => said.push(e),
                    }
                }
                if let Some(msg) = said.into_iter().next() {
                    t.say(msg, cx);
                }
                if ok.is_empty() {
                    return;
                }
                match t.send_bar.as_mut().filter(|b| !b.sending) {
                    Some(b) => {
                        b.images.extend(ok);
                        b.error = None;
                    }
                    None => t.send_bar = Some(Bar { images: ok, into, sending: false, error: None }),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Files dropped on this terminal in a remote window: images are offered, anything else is
    /// refused with why.
    pub fn offer_dropped(&mut self, images: Vec<PathBuf>, others: Vec<PathBuf>, cx: &mut Context<Self>) {
        if let Some(mac) = self.remote_mac().filter(|_| !others.is_empty()) {
            self.say(refusal(&others, &mac), cx);
        }
        self.offer_images(images.into_iter().map(Source::Path).collect(), cx);
    }

    /// ⌘I or Add image in a remote window: pick image files here, then offer them.
    pub fn pick_images(&mut self, cx: &mut Context<Self>) {
        let Some(mac) = self.remote_mac() else { return };
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(format!("Send to {mac}").into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let (images, others): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| crate::annotate::is_image_file(p));
            let _ = this.update(cx, |t, cx| t.offer_dropped(images, others, cx));
        })
        .detach();
    }

    /// Send: each image over the drive link, one at a time (that Mac pastes each path as it
    /// lands). The bar stays with the error if one fails, so it can be sent again or dropped.
    fn send_images(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = self.send_bar.as_mut().filter(|b| !b.sending) else { return };
        bar.sending = true;
        bar.error = None;
        let sends: Vec<(String, String)> = {
            use base64::Engine;
            bar.images.iter().map(|r| (r.name.clone(), base64::engine::general_purpose::STANDARD.encode(&r.bytes))).collect()
        };
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let mut sent = 0;
                    for (name, data) in sends {
                        backend.call("session.send_image", json!({ "id": id, "name": name, "data": data })).map_err(|e| (sent, format!("{e:#}")))?;
                        sent += 1;
                    }
                    Ok::<_, (usize, String)>(())
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                match res {
                    Ok(()) => t.send_bar = None,
                    Err((sent, e)) => {
                        if let Some(b) = t.send_bar.as_mut() {
                            b.images.drain(..sent);
                            b.sending = false;
                            b.error = Some(format!("Couldn't send it: {e}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_send(&mut self, cx: &mut Context<Self>) {
        if self.send_bar.take().is_some() {
            cx.notify();
        }
    }

    /// ⌘↩ sends, esc drops it, while the bar shows. True = the key was the bar's.
    pub(super) fn send_bar_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        if self.send_bar.is_none() {
            return false;
        }
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "enter" if m.platform && !m.alt && !m.control && !m.shift => self.send_images(cx),
            "escape" if !m.modified() => self.cancel_send(cx),
            _ => return false,
        }
        true
    }

    pub(super) fn render_send_bar(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bar = self.send_bar.as_ref()?;
        let mac = self.remote_mac()?;
        let names: Vec<&str> = bar.images.iter().map(|r| r.name.as_str()).collect();
        let bytes = bar.images.iter().map(|r| r.bytes.len()).sum();
        let thumb = bar.images.first().map(|r| r.path.clone());
        let line = match &bar.error {
            Some(e) => div().text_color(t.err).child(e.clone()),
            None => div().text_color(t.dim).child(detail(bytes, names.len(), &bar.into)),
        };
        Some(
            div()
                .id("send-image")
                .absolute()
                .left(px(12.))
                .right(px(12.))
                .bottom(px(10.))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_dashed()
                .border_color(t.accent)
                .bg(t.panel.blend(t.accent.opacity(0.07)))
                .text_size(px(13.))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.25), offset: point(px(0.), px(4.)), blur_radius: px(12.), spread_radius: px(0.), inset: false }])
                // Clicks on the bar stay off the terminal under it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(thumb.map(|p| thumbnail(&p, t)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.))
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::BOLD).text_color(t.fg).child(headline(&names, &mac)))
                        .child(line.text_size(px(12.))),
                )
                .child(btn(t, "send-image-cancel", "Cancel").on_click(cx.listener(|v, _, _, cx| v.cancel_send(cx))))
                .child(
                    btn_primary(t, "send-image-send", if bar.sending { "Sending…" } else { "Send" })
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .when(!bar.sending, |d| d.child(div().text_size(px(11.)).font_weight(FontWeight::NORMAL).opacity(0.8).child("⌘↩")))
                        .on_click(cx.listener(|v, _, _, cx| v.send_images(cx))),
                )
                .into_any_element(),
        )
    }
}

fn thumbnail(path: &Path, t: &Theme) -> Div {
    div().w(px(34.)).h(px(26.)).flex_none().rounded(px(4.)).overflow_hidden().bg(t.raised).child(img(path.to_path_buf()).size_full().object_fit(ObjectFit::Cover))
}

#[cfg(test)]
mod tests {
    use super::{Source, detail, headline, prepare, refusal, size, too_big};
    use std::path::PathBuf;

    #[test]
    fn the_bar_names_the_mac_the_size_and_where_the_path_goes() {
        assert_eq!(headline(&["shot.png"], "Bravo"), "Send shot.png to Bravo?");
        assert_eq!(headline(&["a.png", "b.png", "c.png"], "Bravo"), "Send 3 images to Bravo?");
        assert_eq!(detail(240 * 1024, 1, "planner"), "240 KB. Saved in a temp folder there and its path typed into planner.");
        assert_eq!(detail(3 * 1024 * 1024 / 2, 2, "planner"), "1.5 MB. Saved in a temp folder there and their paths typed into planner.");
        assert_eq!(size(1), "1 KB");
    }

    #[test]
    fn other_files_are_refused_with_why() {
        assert_eq!(refusal(&[PathBuf::from("/x/notes.pdf")], "Bravo"), "Only images can be sent to Bravo. notes.pdf stayed on this Mac.");
        assert_eq!(refusal(&[PathBuf::from("/x/a"), PathBuf::from("/x/b")], "Bravo"), "Only images can be sent to Bravo. 2 files stayed on this Mac.");
        assert_eq!(too_big("huge.png", 12 * 1024 * 1024, "Bravo"), "huge.png is too big to send to Bravo (12.0 MB, limit 10 MB).");
    }

    #[test]
    fn a_source_is_made_ready_or_says_why_not() {
        let dir = std::env::temp_dir().join(format!("midna-send-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = vec![];
        ::image::RgbImage::new(4, 3).write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png).unwrap();
        let shot = dir.join("My Shot.png");
        std::fs::write(&shot, &png).unwrap();
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, b"hello").unwrap();
        let out = prepare(vec![Source::Path(shot), Source::Bytes { name: "pasted image".into(), bytes: png.clone() }, Source::Path(txt)]);
        let ready = out[0].as_ref().unwrap();
        assert_eq!((ready.name.as_str(), ready.bytes.as_slice()), ("My Shot.png", png.as_slice()));
        assert_eq!(out[1].as_ref().unwrap().name, "pasted image");
        assert!(out[2].as_ref().is_err_and(|e| e.starts_with("Couldn't read notes.txt")), "{:?}", out[2].as_ref().err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bar over a terminal on another Mac: nothing goes until ⌘↩, esc drops it, and Send
    /// carries each image there with its name.
    mod bar {
        use super::super::super::TerminalView;
        use crate::annotate::Source;
        use crate::backend::{AttachRequest, Backend, BackendEvent, Mac, TermStream};
        use crate::frame::FrameSink;
        use gpui_kit::{AppContext, Entity, Keystroke, Render, TestAppContext, Window, div, prelude::*};
        use serde_json::{Value, json};
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};

        /// Another Mac, as far as the bar is concerned: names the terminal and takes images.
        struct There {
            mac: Option<Mac>,
            calls: Mutex<Vec<(String, Value)>>,
        }

        impl Backend for There {
            fn label(&self) -> &'static str {
                "test"
            }
            fn socket_path(&self) -> PathBuf {
                PathBuf::new()
            }
            fn mac(&self) -> Option<&Mac> {
                self.mac.as_ref()
            }
            fn open_mac(self: Arc<Self>, _: Mac) -> Arc<dyn Backend> {
                self
            }
            fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
                self.calls.lock().unwrap().push((method.into(), params));
                Ok(match method {
                    "session.get" => json!({ "name": "planner" }),
                    "session.send_image" => json!({ "path": "/there/images/shot.png" }),
                    _ => Value::Null,
                })
            }
            fn subscribe(&self, _: async_channel::Sender<BackendEvent>) {}
            fn attach(&self, _: AttachRequest<'_>, _: Arc<FrameSink>) -> anyhow::Result<Arc<dyn TermStream>> {
                anyhow::bail!("not here")
            }
        }

        struct Host(Entity<TerminalView>);

        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
                div()
            }
        }

        fn png() -> Vec<u8> {
            let mut png = vec![];
            ::image::RgbImage::new(4, 3).write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png).unwrap();
            png
        }

        fn open(mac: Option<&str>, cx: &mut TestAppContext) -> (Arc<There>, Entity<TerminalView>) {
            let theme = crate::theme::Theme::from_def(&midna_proto::themes::builtin("twilight").unwrap(), "ui".into(), "mono".into());
            cx.update(|cx| cx.set_global(theme));
            let there = Arc::new(There { mac: mac.map(|n| Mac { peer: "fp".into(), name: n.into() }), calls: Mutex::default() });
            let b: Arc<dyn Backend> = there.clone();
            let w = cx.add_window(|window, cx| Host(cx.new(|cx| TerminalView::new("s1".into(), b, window, cx))));
            let view = w.update(cx, |h, _, _| h.0.clone()).unwrap();
            (there, view)
        }

        fn sent(there: &There) -> Vec<Value> {
            there.calls.lock().unwrap().iter().filter(|(m, _)| m == "session.send_image").map(|(_, p)| p.clone()).collect()
        }

        fn key(view: &Entity<TerminalView>, k: &str, cx: &mut TestAppContext) -> bool {
            let ks = Keystroke::parse(k).unwrap();
            let took = view.update(cx, |v, cx| v.send_bar_key(&ks, cx));
            cx.run_until_parked();
            took
        }

        #[gpui_kit::test]
        fn nothing_goes_until_the_person_says_send(cx: &mut TestAppContext) {
            let (there, view) = open(Some("Bravo"), cx);
            view.update(cx, |v, cx| v.offer_images(vec![Source::Bytes { name: "pasted image".into(), bytes: png() }], cx));
            cx.run_until_parked();
            let into = view.read_with(cx, |v, _| v.send_bar.as_ref().map(|b| (b.images.len(), b.into.clone())));
            assert_eq!(into, Some((1, "planner".into())), "the bar asks, naming the terminal there");
            assert!(sent(&there).is_empty(), "nothing is sent by offering");
            assert!(!key(&view, "enter", cx) && !key(&view, "cmd-shift-enter", cx), "other keys go to the terminal");
            assert!(sent(&there).is_empty());

            assert!(key(&view, "cmd-enter", cx), "⌘↩ is Send");
            let got = sent(&there);
            assert_eq!(got.len(), 1);
            assert_eq!((got[0]["id"].as_str(), got[0]["name"].as_str()), (Some("s1"), Some("pasted image")));
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(got[0]["data"].as_str().unwrap()).unwrap();
            assert!(midna_proto::SEND_IMAGE_MAX_BYTES > bytes.len() && bytes.starts_with(b"\x89PNG"), "the made-ready PNG goes");
            assert!(view.read_with(cx, |v, _| v.send_bar.is_none()), "the bar goes once it's there");
        }

        #[gpui_kit::test]
        fn esc_drops_it_and_this_macs_terminals_have_no_bar(cx: &mut TestAppContext) {
            let (there, view) = open(Some("Bravo"), cx);
            view.update(cx, |v, cx| v.offer_images(vec![Source::Bytes { name: "a".into(), bytes: png() }], cx));
            cx.run_until_parked();
            assert!(key(&view, "escape", cx));
            assert!(view.read_with(cx, |v, _| v.send_bar.is_none()));
            assert!(!key(&view, "cmd-enter", cx), "no bar, no Send");
            assert!(sent(&there).is_empty());

            let (here, view) = open(None, cx);
            view.update(cx, |v, cx| v.offer_images(vec![Source::Bytes { name: "a".into(), bytes: png() }], cx));
            cx.run_until_parked();
            assert!(view.read_with(cx, |v, _| v.send_bar.is_none()), "this Mac's terminals keep the sheet and inline paths");
            assert!(here.calls.lock().unwrap().iter().all(|(m, _)| m != "session.send_image"));
        }

        #[gpui_kit::test]
        fn too_big_or_not_an_image_never_reaches_the_bar(cx: &mut TestAppContext) {
            let (there, view) = open(Some("Bravo"), cx);
            view.update(cx, |v, cx| v.offer_images(vec![Source::Bytes { name: "notes".into(), bytes: b"hello".to_vec() }], cx));
            cx.run_until_parked();
            let (bar, said) = view.read_with(cx, |v, _| (v.send_bar.is_some(), v.notice.as_ref().map(|n| n.0.clone())));
            assert!(!bar);
            assert!(said.is_some_and(|s| s.starts_with("Couldn't read notes")));
            assert!(sent(&there).is_empty());
        }
    }
}
