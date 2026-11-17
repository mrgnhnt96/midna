//! Images for the Mac a remote window drives (DECISIONS.md "Nearby: driving another of my
//! Macs"). In a window on another Mac, images work as they would on that Mac: ⌘V of a
//! screenshot, a dropped image, ⌘I and Add image open the image sheet here, and ⌘⇧V (or any
//! terminal that isn't an agent) pastes it inline. A path here would mean nothing there, so
//! when the attachment is delivered (↩, or the composer's send) or pasted inline, each image is
//! carried over the drive link with `session.send_image`; that Mac saves it in its images folder
//! and pastes its own path, waiting for Claude's `[Image #N]`, before the notes and ↩ follow.
//! That Mac takes it only from the person driving (never from an agent or a script there or
//! here) and only an image under `SEND_IMAGE_MAX_BYTES`; anything bigger is turned down here
//! first. Dropped files that aren't images stay here, with a short reason.
use super::TerminalView;
use crate::annotate::Source;
use crate::backend::Backend;
use gpui_kit::*;
use midna_proto::SEND_IMAGE_MAX_BYTES;
use serde_json::json;
use std::path::{Path, PathBuf};

/// `240 KB`, `1.4 MB`.
fn size(n: usize) -> String {
    if n < 1024 * 1024 { format!("{} KB", n.div_ceil(1024)) } else { format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)) }
}

/// Why dropped files that aren't images weren't sent.
pub(super) fn refusal(others: &[PathBuf], mac: &str) -> String {
    let what = match others {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "That file".into()),
        many => format!("{} files", many.len()),
    };
    format!("Only images can be sent to {mac}. {what} stayed on this Mac.")
}

/// Why an image wasn't sent: too big.
fn too_big(name: &str, bytes: usize, mac: &str) -> String {
    format!("{name} is too big to send to {mac} ({}, limit {} MB).", size(bytes), SEND_IMAGE_MAX_BYTES >> 20)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into())
}

/// Send each made-ready image (`annotate::save`) to terminal `id` on the other Mac, one at a
/// time, in order; that Mac pastes each path as it lands. Blocks: run it off the UI thread. Err
/// says why, and nothing after the one that failed is sent.
pub fn send_there(backend: &dyn Backend, id: &str, paths: &[PathBuf], mac: &str) -> Result<(), String> {
    use base64::Engine;
    for p in paths {
        let name = file_name(p);
        let bytes = std::fs::read(p).map_err(|e| format!("Couldn't read {name}: {e}"))?;
        if bytes.len() > SEND_IMAGE_MAX_BYTES {
            return Err(too_big(&name, bytes.len(), mac));
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
        backend.call("session.send_image", json!({ "id": id, "name": name, "data": data })).map_err(|e| format!("Couldn't send {name} to {mac}: {e:#}"))?;
    }
    Ok(())
}

impl TerminalView {
    /// The Mac this terminal is on, when it isn't this one.
    pub fn remote_mac(&self) -> Option<String> {
        self.backend.mac().map(|m| m.name.clone())
    }

    /// Images pasted inline on another Mac's terminal: made ready here, then sent there, where
    /// each path is pasted.
    pub fn send_images(&mut self, sources: Vec<Source>, cx: &mut Context<Self>) {
        let Some(mac) = self.remote_mac() else { return };
        if sources.is_empty() {
            return;
        }
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let mut paths = vec![];
                    for src in sources {
                        let name = match &src {
                            Source::Bytes { name, .. } => name.clone(),
                            Source::Path(p) => file_name(p),
                        };
                        paths.push(crate::annotate::save(src).map_err(|e| format!("Couldn't read {name}: {e}"))?);
                    }
                    send_there(&*backend, &id, &paths, &mac)
                })
                .await;
            if let Err(e) = res {
                let _ = this.update(cx, |t, cx| t.say(e, cx));
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{refusal, size, too_big};
    use std::path::PathBuf;

    #[test]
    fn sizes_and_refusals_say_why() {
        assert_eq!(size(1), "1 KB");
        assert_eq!(refusal(&[PathBuf::from("/x/notes.pdf")], "Bravo"), "Only images can be sent to Bravo. notes.pdf stayed on this Mac.");
        assert_eq!(refusal(&[PathBuf::from("/x/a"), PathBuf::from("/x/b")], "Bravo"), "Only images can be sent to Bravo. 2 files stayed on this Mac.");
        assert_eq!(too_big("huge.png", 12 * 1024 * 1024, "Bravo"), "huge.png is too big to send to Bravo (12.0 MB, limit 10 MB).");
    }

    /// A terminal on another Mac: images go straight there, with nothing to confirm.
    mod there {
        use super::super::super::TerminalView;
        use crate::annotate::Source;
        use crate::backend::{AttachRequest, Backend, BackendEvent, Mac, TermStream};
        use crate::frame::FrameSink;
        use gpui_kit::{AppContext, Entity, Render, TestAppContext, Window, div, prelude::*};
        use serde_json::{Value, json};
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};

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

        fn there(mac: Option<&str>) -> Arc<There> {
            Arc::new(There { mac: mac.map(|n| Mac { peer: "fp".into(), name: n.into() }), calls: Mutex::default() })
        }

        fn open(there: Arc<There>, cx: &mut TestAppContext) -> Entity<TerminalView> {
            let theme = crate::theme::Theme::from_def(&midna_proto::themes::builtin("twilight").unwrap(), "ui".into(), "mono".into());
            cx.update(|cx| cx.set_global(theme));
            let b: Arc<dyn Backend> = there;
            let w = cx.add_window(|window, cx| Host(cx.new(|cx| TerminalView::new("s1".into(), b, window, cx))));
            w.update(cx, |h, _, _| h.0.clone()).unwrap()
        }

        fn sent(there: &There) -> Vec<Value> {
            there.calls.lock().unwrap().iter().filter(|(m, _)| m == "session.send_image").map(|(_, p)| p.clone()).collect()
        }

        #[gpui_kit::test]
        fn a_pasted_image_goes_there_without_asking(cx: &mut TestAppContext) {
            let t = there(Some("Bravo"));
            let view = open(t.clone(), cx);
            view.update(cx, |v, cx| v.send_images(vec![Source::Bytes { name: "pasted image".into(), bytes: png() }], cx));
            cx.run_until_parked();
            let got = sent(&t);
            assert_eq!(got.len(), 1);
            assert_eq!(got[0]["id"].as_str(), Some("s1"));
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(got[0]["data"].as_str().unwrap()).unwrap();
            assert!(bytes.starts_with(b"\x89PNG"), "the made-ready PNG goes");
        }

        #[gpui_kit::test]
        fn not_an_image_says_why_and_this_macs_terminals_send_nothing(cx: &mut TestAppContext) {
            let t = there(Some("Bravo"));
            let view = open(t.clone(), cx);
            view.update(cx, |v, cx| v.send_images(vec![Source::Bytes { name: "notes".into(), bytes: b"hello".to_vec() }], cx));
            cx.run_until_parked();
            assert!(sent(&t).is_empty());
            let said = view.read_with(cx, |v, _| v.notice.as_ref().map(|n| n.0.clone()));
            assert!(said.is_some_and(|s| s.starts_with("Couldn't read notes")));

            let here = there(None);
            let view = open(here.clone(), cx);
            view.update(cx, |v, cx| v.send_images(vec![Source::Bytes { name: "a".into(), bytes: png() }], cx));
            cx.run_until_parked();
            assert!(sent(&here).is_empty());
        }

        #[test]
        fn send_there_goes_in_order_and_stops_at_one_too_big() {
            let dir = std::env::temp_dir().join(format!("midna-send-there-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let (a, big, b) = (dir.join("a.png"), dir.join("big.png"), dir.join("b.png"));
            std::fs::write(&a, png()).unwrap();
            std::fs::write(&big, vec![0u8; midna_proto::SEND_IMAGE_MAX_BYTES + 1]).unwrap();
            std::fs::write(&b, png()).unwrap();
            let t = there(Some("Bravo"));
            let err = super::super::send_there(&*t, "s1", &[a, big, b], "Bravo").unwrap_err();
            assert!(err.starts_with("big.png is too big to send to Bravo"), "{err}");
            let names: Vec<_> = sent(&t).iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
            assert_eq!(names, ["a.png"], "nothing after the one that failed");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
