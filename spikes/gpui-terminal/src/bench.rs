//! In-process benchmark driver. Synthetic input targets only this window (dispatch_keystroke).
use crate::engine::{now_ns, proc_usage};
use crate::Term;
use gpui_kit::*;
use std::time::Duration;

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn hz() -> f64 {
    std::env::var("MIDNA_HZ").ok().and_then(|v| v.parse().ok()).unwrap_or(120.0)
}

fn unescape(s: &str) -> Vec<u8> {
    s.replace("\\e", "\x1b").replace("\\r", "\r").replace("\\n", "\n").replace("\\t", "\t").replace("\\c", ",").replace("\\f", "\x06").replace("\\b", "\x02").into_bytes()
}

fn window_id(window: &Window) -> isize {
    use objc2::msg_send;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).unwrap().as_raw() else { return 0 };
    unsafe {
        let view = h.ns_view.as_ptr() as *mut objc2::runtime::AnyObject;
        let w: *mut objc2::runtime::AnyObject = msg_send![&*view, window];
        msg_send![&*w, windowNumber]
    }
}

pub fn start(spec: String, window: &mut Window, cx: &mut Context<Term>) {
    let label = std::env::var("MIDNA_LABEL").unwrap_or_else(|_| std::env::var("MIDNA_TRANSPORT").unwrap_or("a".into()));
    cx.spawn_in(window, async move |this, cx| {
        let ex = cx.background_executor().clone();
        let sleep = move |ms: u64| ex.timer(Duration::from_millis(ms));
        sleep(1500).await;
        let pids = this.update(cx, |t, _| (t.tr.daemon_pid, t.tr.child_pid)).unwrap();
        let me = std::process::id() as i32;
        for step in spec.split(',') {
            let (name, arg) = step.split_once(':').unwrap_or((step, ""));
            match name {
                "wait" => sleep(arg.parse().unwrap()).await,
                "type" => {
                    let _ = this.update(cx, |t, _| t.tr.input(&unescape(arg)));
                }
                "key" => {
                    let _ = cx.update(|w, cx| w.dispatch_keystroke(Keystroke::parse(arg).unwrap(), cx));
                }
                "size" => {
                    let (w, h) = arg.split_once('x').unwrap();
                    let (w, h): (f32, f32) = (w.parse().unwrap(), h.parse().unwrap());
                    let _ = cx.update(|win, _| win.resize(size(px(w), px(h))));
                }
                "shot" => {
                    let id = cx.update(|w, _| window_id(w)).unwrap();
                    let dir = std::env::var("MIDNA_SHOTS").unwrap_or("/tmp".into());
                    let path = format!("{dir}/{label}-{arg}.png");
                    let _ = std::process::Command::new("screencapture").args(["-x", "-o", "-l", &id.to_string(), &path]).status();
                    println!("SHOT {path}");
                }
                "latency" => {
                    let n: usize = arg.parse().unwrap_or(150);
                    let _ = this.update(cx, |t, _| {
                        let mut s = t.stats.borrow_mut();
                        s.probes.clear();
                        s.probe_active = true;
                    });
                    let mut done = 0;
                    let mut i = 0u64;
                    while done < n {
                        let _ = cx.update(|w, cx| w.dispatch_keystroke(Keystroke::parse("x").unwrap(), cx));
                        sleep(45 + (i * 7) % 23).await;
                        i += 1;
                        done += 1;
                        if done % 40 == 0 {
                            let _ = this.update(cx, |t, _| t.stats.borrow_mut().probe_active = false);
                            let _ = cx.update(|w, cx| w.dispatch_keystroke(Keystroke::parse("ctrl-u").unwrap(), cx));
                            sleep(120).await;
                            let _ = this.update(cx, |t, _| t.stats.borrow_mut().probe_active = true);
                        }
                    }
                    sleep(200).await;
                    let _ = this.update(cx, |t, _| {
                        let mut s = t.stats.borrow_mut();
                        s.probe_active = false;
                        let ok: Vec<_> = s.probes.iter().filter(|p| p.paint_ns != 0).copied().collect();
                        let ms = |f: &dyn Fn(&crate::Probe) -> u64| -> Vec<f64> { ok.iter().map(|p| f(p) as f64 / 1e6).collect() };
                        let mut cols = vec![
                            ("key->write", ms(&|p| p.write_ns - p.key_ns)),
                            ("write->echo_read", ms(&|p| p.recv_ns.saturating_sub(p.write_ns))),
                            ("echo_read->frame_built", ms(&|p| p.built_ns.saturating_sub(p.recv_ns))),
                            ("frame_built->ui_pull", ms(&|p| p.ui_ns.saturating_sub(p.built_ns))),
                            ("ui_pull->painted", ms(&|p| p.paint_ns - p.ui_ns)),
                            ("TOTAL key->painted", ms(&|p| p.paint_ns - p.key_ns)),
                        ];
                        println!("LATENCY [{label}] samples={}/{}", ok.len(), s.probes.len());
                        for (k, v) in cols.iter_mut() {
                            println!("  {k:24} p50={:7.3}ms p95={:7.3}ms max={:7.3}ms", pct(v, 0.5), pct(v, 0.95), pct(v, 1.0));
                        }
                    });
                    let _ = this.update(cx, |t, _| t.tr.input(b"\x15"));
                    sleep(200).await;
                }
                "flood" | "tui" | "idle" => {
                    let secs: u64 = if name == "flood" { 180 } else { arg.split('|').nth(1).and_then(|v| v.parse().ok()).unwrap_or(8) };
                    let cmd = arg.split('|').next().unwrap_or("");
                    let _ = this.update(cx, |t, _| {
                        let mut s = t.stats.borrow_mut();
                        s.marker_ns = 0;
                        s.marker_painted_ns = 0;
                        s.marker_after = now_ns();
                    });
                    if name == "tui" {
                        let _ = this.update(cx, |t, _| t.tr.input(&unescape(&format!("{cmd}\r"))));
                        sleep(1500).await;
                    }
                    let (f0, b0) = this.update(cx, |t, _| { let s = t.stats.borrow(); (s.frames.len(), s.engine_build_us.len()) }).unwrap();
                    let (g0, _) = proc_usage(me);
                    let (d0, _) = if pids.0 > 0 { proc_usage(pids.0) } else { (0, 0) };
                    let t0 = now_ns();
                    if name == "flood" {
                        let line = format!("{cmd}; echo __MIDNA_\"\"DONE__\r");
                        let _ = this.update(cx, |t, _| t.tr.input(line.as_bytes()));
                    }
                    let mut peak_g = 0u64;
                    let mut peak_d = 0u64;
                    loop {
                        sleep(50).await;
                        peak_g = peak_g.max(proc_usage(me).1);
                        if pids.0 > 0 {
                            peak_d = peak_d.max(proc_usage(pids.0).1);
                        }
                        // tui: drive full-screen redraw keys if given
                        if name == "tui" {
                            if let Some(k) = arg.split('|').nth(2) {
                                for _ in 0..3 {
                                    let _ = this.update(cx, |t, _| t.tr.input(&unescape(k)));
                                    sleep(16).await;
                                }
                            }
                        }
                        let el = (now_ns() - t0) as f64 / 1e9;
                        let done = this.update(cx, |t, _| t.stats.borrow().marker_painted_ns != 0).unwrap();
                        if (name == "flood" && done) || el > secs as f64 {
                            break;
                        }
                    }
                    let t1 = now_ns();
                    let (g1, _) = proc_usage(me);
                    let (d1, _) = if pids.0 > 0 { proc_usage(pids.0) } else { (0, 0) };
                    let wall = (t1 - t0) as f64 / 1e9;
                    let _ = this.update(cx, |t, _| {
                        let s = t.stats.borrow();
                        let fr = &s.frames[f0..];
                        let mut dur: Vec<f64> = fr.iter().map(|f| (f.1 - f.0) as f64 / 1e6).collect();
                        let ends: Vec<u64> = fr.iter().map(|f| f.1).collect();
                        let period = 1e9 / hz();
                        let mut gaps: Vec<f64> = ends.windows(2).map(|w| (w[1] - w[0]) as f64 / 1e6).collect();
                        let dropped: u64 = ends.windows(2).map(|w| (((w[1] - w[0]) as f64 / period).round() as u64).saturating_sub(1)).sum();
                        let mut eb: Vec<f64> = s.engine_build_us[b0..].iter().map(|v| *v as f64 / 1e3).collect();
                        let end_ns = if name == "flood" && s.marker_painted_ns != 0 { s.marker_painted_ns } else { t1 };
                        let span = (end_ns - t0) as f64 / 1e9;
                        println!("{} [{label}] {cmd}", name.to_uppercase());
                        if name == "flood" {
                            println!("  engine_done={:.3}s painted_done={:.3}s", (s.marker_ns.saturating_sub(t0)) as f64 / 1e9, span);
                        }
                        println!("  frames={} fps={:.1} vsync_misses(@{}Hz, while frames flowing)={} gap_p50={:.2}ms gap_p95={:.2}ms gap_max={:.1}ms",
                            fr.len(), fr.len() as f64 / span, hz(), dropped, pct(&mut gaps, 0.5), pct(&mut gaps, 0.95), pct(&mut gaps, 1.0));
                        println!("  ui_frame_cpu(render+paint) p50={:.2}ms p95={:.2}ms max={:.2}ms | engine_frame_build p50={:.2}ms p95={:.2}ms max={:.2}ms",
                            pct(&mut dur, 0.5), pct(&mut dur, 0.95), pct(&mut dur, 1.0), pct(&mut eb, 0.5), pct(&mut eb, 0.95), pct(&mut eb, 1.0));
                        println!("  cpu: gui={:.1}% daemon={:.1}% (of one core, over {:.1}s) rss: gui={:.0}MB daemon={:.0}MB (peak sampled)",
                            (g1 - g0) as f64 / 1e9 / wall * 100., (d1 - d0) as f64 / 1e9 / wall * 100., wall, peak_g as f64 / 1e6, peak_d as f64 / 1e6);
                    });
                    if name == "tui" {
                        let _ = this.update(cx, |t, _| t.tr.input(b"\x1b:qa!\rq"));
                        sleep(500).await;
                        let _ = this.update(cx, |t, _| t.tr.input(b"\x15clear\r"));
                    }
                    sleep(500).await;
                }
                _ => println!("unknown step {step}"),
            }
        }
        if std::env::var("MIDNA_STAY").is_err() {
            sleep(300).await;
            let _ = this.update(cx, |t, cx| {
                t.tr.shutdown();
                cx.quit();
            });
        }
    })
    .detach();
}
