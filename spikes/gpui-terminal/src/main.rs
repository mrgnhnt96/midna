//! Throwaway spike: GPUI rendering a libghostty-vt terminal. Not product code.
//! MIDNA_TRANSPORT=a|braw|bsnap   MIDNA_BENCH=latency,flood:<cmd>,idle,tui:<cmd>,shot:<cmd>
mod engine;
mod transport;

use engine::*;
use gpui_kit::*;
use std::cell::{Cell as StdCell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use transport::*;

const FONT_SIZE: f32 = 14.0;
const PAD: f32 = 4.0;

#[derive(Default, Clone, Copy, Debug)]
struct Probe {
    key_ns: u64,
    write_ns: u64,
    recv_ns: u64,
    built_ns: u64,
    ui_ns: u64,
    paint_ns: u64,
}

#[derive(Default)]
struct Stats {
    frames: Vec<(u64, u64, u32)>, // (render start, paint end, cells changed rows)
    probes: Vec<Probe>,
    probe_active: bool,
    marker_ns: u64,
    marker_after: u64,
    marker_painted_ns: u64,
    engine_build_us: Vec<u32>,
}

struct Term {
    grid: Arc<Vec<RowData>>,
    cols: u16,
    rows: u16,
    cursor: Option<(u16, u16)>,
    cursor_style: u8,
    dfg: [u8; 3],
    dbg: [u8; 3],
    decckm: bool,
    bpaste: bool,
    blink_on: bool,
    last_input_ns: u64,
    mb: Arc<Mailbox>,
    tr: Rc<Transport>,
    focus: FocusHandle,
    stats: Rc<RefCell<Stats>>,
    req_size: Rc<StdCell<(u16, u16)>>,
    cell_w: f32,
    line_h: f32,
}

fn rgb3(c: [u8; 3], a: f32) -> Hsla {
    Rgba { r: c[0] as f32 / 255., g: c[1] as f32 / 255., b: c[2] as f32 / 255., a }.into()
}

fn key_bytes(ks: &Keystroke, decckm: bool) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let arrow = |c: u8| if decckm { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
    let mut v = match ks.key.as_str() {
        "enter" => vec![b'\r'],
        "backspace" => vec![0x7f],
        "tab" => if m.shift { b"\x1b[Z".to_vec() } else { vec![b'\t'] },
        "escape" => vec![0x1b],
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "space" => if m.control { vec![0] } else { vec![b' '] },
        k if m.control && k.len() == 1 => match k.as_bytes()[0] {
            c @ b'a'..=b'z' => vec![c & 0x1f],
            b'[' => vec![0x1b],
            b'\\' => vec![0x1c],
            b']' => vec![0x1d],
            b'@' | b'2' => vec![0],
            _ => return None,
        },
        k if m.alt && k.len() == 1 => k.as_bytes().to_vec(),
        k => match &ks.key_char {
            Some(s) => s.as_bytes().to_vec(),
            None if k.chars().count() == 1 => k.as_bytes().to_vec(),
            None => return None,
        },
    };
    if m.alt {
        v.insert(0, 0x1b);
    }
    Some(v)
}

impl Term {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = match std::env::var("MIDNA_TRANSPORT").as_deref() {
            Ok("braw") => Kind::BRaw,
            Ok("bsnap") => Kind::BSnap,
            _ => Kind::A,
        };
        let cmd = std::env::var("MIDNA_CMD").unwrap_or_else(|_| "/bin/zsh -f".into());
        let (wtx, wrx) = async_channel::bounded::<()>(1);
        let mb = Arc::new(Mailbox { frame: Mutex::new(None), wake: wtx });
        let tr = Rc::new(Transport::start(kind, &cmd, mb.clone()));
        eprintln!("transport={kind:?} gui_pid={} daemon_pid={} child_pid={}", std::process::id(), tr.daemon_pid, tr.child_pid);
        // font metrics
        let ts = window.text_system();
        let fid = ts.resolve_font(&font("Menlo"));
        let cell_w = f32::from(ts.advance(fid, px(FONT_SIZE), 'M').map(|s| s.width).unwrap_or(px(8.4)));
        let line_h = (FONT_SIZE * 1.2).round();
        eprintln!("cell {cell_w}x{line_h}");
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        // wake task: engine/daemon -> UI
        cx.spawn_in(window, async move |this, cx| {
            while wrx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        // cursor blink
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(530)).await;
            if this
                .update(cx, |t, cx| {
                    if crate::now_ns() - t.last_input_ns > 1_000_000_000 {
                        t.blink_on = !t.blink_on;
                    } else {
                        t.blink_on = true;
                    }
                    if t.cursor.is_some() {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        let t = Term {
            grid: Arc::new(vec![]),
            cols: 0,
            rows: 0,
            cursor: None,
            cursor_style: 0,
            dfg: [0xdd, 0xdd, 0xdd],
            dbg: [0x10, 0x10, 0x14],
            decckm: false,
            bpaste: false,
            blink_on: true,
            last_input_ns: 0,
            mb,
            tr,
            focus,
            stats: Rc::new(RefCell::new(Stats::default())),
            req_size: Rc::new(StdCell::new((0, 0))),
            cell_w,
            line_h,
        };
        if let Ok(b) = std::env::var("MIDNA_BENCH") {
            bench::start(b, window, cx);
        }
        t
    }

    fn pull(&mut self) {
        let Some(f) = self.mb.frame.lock().unwrap().take() else { return };
        if std::env::var("MIDNA_DEBUG").is_ok() { eprintln!("ui: pulled frame rows={} changed={}", f.rows, f.changed.len()); }
        let now = now_ns();
        let mut st = self.stats.borrow_mut();
        st.engine_build_us.push(f.build_us);
        if f.marker_ns > st.marker_after && st.marker_after != 0 && st.marker_ns == 0 {
            st.marker_ns = f.marker_ns;
        }
        if let Some(p) = st.probes.last_mut() {
            if p.write_ns != 0 && p.ui_ns == 0 && f.last_read_ns > p.write_ns {
                p.recv_ns = if f.first_read_ns > p.write_ns { f.first_read_ns } else { f.last_read_ns };
                p.built_ns = f.built_ns;
                p.ui_ns = now;
            }
        }
        drop(st);
        let g = Arc::make_mut(&mut self.grid);
        if f.full || f.cols != self.cols || f.rows != self.rows {
            g.clear();
            g.resize(f.rows as usize, RowData::default());
        }
        for (y, r) in f.changed {
            if (y as usize) < g.len() {
                g[y as usize] = r;
            }
        }
        self.cols = f.cols;
        self.rows = f.rows;
        self.cursor = f.cursor;
        self.cursor_style = f.cursor_style;
        self.dfg = f.default_fg;
        self.dbg = f.default_bg;
        self.decckm = f.decckm;
        self.bpaste = f.bracketed_paste;
        self.tr.want();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let t0 = now_ns();
        let ks = &ev.keystroke;
        if ks.modifiers.platform {
            match ks.key.as_str() {
                "q" | "w" => {
                    self.tr.shutdown();
                    cx.quit();
                }
                "v" => {
                    if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        let t = t.replace("\r\n", "\r").replace('\n', "\r");
                        if self.bpaste {
                            self.tr.input(format!("\x1b[200~{t}\x1b[201~").as_bytes());
                        } else {
                            self.tr.input(t.as_bytes());
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(b) = key_bytes(ks, self.decckm) {
            let probe = self.stats.borrow().probe_active;
            self.tr.input(&b);
            if probe {
                self.stats.borrow_mut().probes.push(Probe { key_ns: t0, write_ns: now_ns(), ..Default::default() });
            }
            self.last_input_ns = t0;
            self.blink_on = true;
        }
    }
}

impl Render for Term {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let r0 = now_ns();
        self.pull();
        let grid = self.grid.clone();
        let (cw, lh) = (self.cell_w, self.line_h);
        let (dfg, dbg) = (self.dfg, self.dbg);
        let cursor = if self.blink_on { self.cursor } else { None };
        let cstyle = self.cursor_style;
        let stats = self.stats.clone();
        let tr = self.tr.clone();
        let req = self.req_size.clone();
        div()
            .size_full()
            .bg(rgb3(dbg, 1.0))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| this.on_key(ev, cx)))
            .child(
                canvas(
                    move |bounds, _w, _cx| {
                        let cols = ((f32::from(bounds.size.width) - 2. * PAD) / cw).floor().max(2.) as u16;
                        let rows = ((f32::from(bounds.size.height) - 2. * PAD) / lh).floor().max(2.) as u16;
                        if req.get() != (cols, rows) {
                            req.set((cols, rows));
                            tr.resize(cols, rows, cw.round() as u32, lh as u32);
                        }
                    },
                    move |bounds, _, window, cx| {
                        if std::env::var("MIDNA_NOLAYER").is_ok() { paint_grid(&grid, bounds, cw, lh, dfg, dbg, cursor, cstyle, window, cx); } else { window.paint_layer(bounds, |window| paint_grid(&grid, bounds, cw, lh, dfg, dbg, cursor, cstyle, window, cx)); }
                        let mut st = stats.borrow_mut();
                        let end = now_ns();
                        st.frames.push((r0, end, 0));
                        if let Some(p) = st.probes.last_mut() {
                            if p.ui_ns != 0 && p.paint_ns == 0 {
                                p.paint_ns = end;
                            }
                        }
                        if st.marker_ns != 0 && st.marker_painted_ns == 0 {
                            st.marker_painted_ns = end;
                        }
                    },
                )
                .size_full(),
            )
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    grid: &[RowData], bounds: Bounds<Pixels>, cw: f32, lh: f32, dfg: [u8; 3], dbg: [u8; 3],
    cursor: Option<(u16, u16)>, cstyle: u8, window: &mut Window, cx: &mut App,
) {
    let ox = f32::from(bounds.origin.x) + PAD;
    let oy = f32::from(bounds.origin.y) + PAD;
    let fs = px(FONT_SIZE);
    let mk_font = |flags: u8| {
        let mut f = font("Menlo");
        if flags & F_BOLD != 0 {
            f.weight = FontWeight::BOLD;
        }
        if flags & F_ITALIC != 0 {
            f.style = FontStyle::Italic;
        }
        f
    };
    let mut s = String::with_capacity(512);
    let (mut t_bg, mut t_shape, mut t_paint) = (0u64, 0u64, 0u64);
    for (y, row) in grid.iter().enumerate() {
        let tb0 = now_ns();
        let py = oy + lh * y as f32;
        let cells = &row.cells;
        let n = cells.len();
        // backgrounds
        let mut x = 0;
        while x < n {
            let bg = cells[x].bg;
            if bg == dbg {
                x += 1;
                continue;
            }
            let st = x;
            while x < n && cells[x].bg == bg {
                x += 1;
            }
            window.paint_quad(fill(
                Bounds::new(point(px(ox + cw * st as f32), px(py)), size(px(cw * (x - st) as f32), px(lh))),
                rgb3(bg, 1.0),
            ));
        }
        t_bg += now_ns() - tb0;
        // text
        if *PAINT_MODE == 2 {
            // glyph cache: shape each distinct (grapheme, bold/italic) once, then paint glyphs
            // directly at cell positions every frame (no per-frame shaping).
            let ts0 = now_ns();
            for (x, c) in cells.iter().enumerate() {
                if c.flags & F_SPACER != 0 || c.ch == ' ' || c.ch == '\0' {
                    if c.flags & (F_UNDER | F_STRIKE) == 0 { continue; }
                }
                let alpha = if c.flags & F_FAINT != 0 { 0.6 } else { 1.0 };
                let color = rgb3(c.fg, alpha);
                if c.flags & (F_UNDER | F_STRIKE) != 0 {
                    let yy = if c.flags & F_UNDER != 0 { py + lh - 2. } else { py + lh / 2. };
                    window.paint_quad(fill(Bounds::new(point(px(ox + cw * x as f32), px(yy)), size(px(cw), px(1.))), color));
                    if c.ch == ' ' { continue; }
                }
                let extra = row.extras.iter().find(|(ex, _)| *ex as usize == x).map(|e| e.1.as_str());
                let key_flags = c.flags & (F_BOLD | F_ITALIC);
                let ts_l = now_ns();
                let glyphs = GLYPHS.with(|g| {
                    let mut g = g.borrow_mut();
                    let key = (c.ch, extra.map(|s| s.to_string()), key_flags);
                    if let Some(v) = g.get(&key) {
                        return v.clone();
                    }
                    let text: String = extra.map(|s| s.to_string()).unwrap_or_else(|| c.ch.to_string());
                    let run = TextRun { len: text.len(), font: mk_font(key_flags), color, background_color: None, underline: None, strikethrough: None };
                    let line = window.text_system().shape_line(SharedString::from(text), fs, &[run], None);
                    let base = (lh - f32::from(line.ascent) - f32::from(line.descent)) / 2. + f32::from(line.ascent);
                    let v: Rc<Vec<(FontId, GlyphId, f32, f32, bool)>> = Rc::new(
                        line.runs.iter().flat_map(|r| r.glyphs.iter().map(move |gl| (r.font_id, gl.id, f32::from(gl.position.x), base, gl.is_emoji))).collect(),
                    );
                    g.insert(key, v.clone());
                    v
                });
                let tg = now_ns();
                t_shape += tg - ts_l;
                for &(fid, gid, gx, base, emoji) in glyphs.iter() {
                    let o = point(px(ox + cw * x as f32 + gx), px(py + base));
                    if emoji {
                        let _ = window.paint_emoji(o, fid, gid, fs);
                    } else {
                        let _ = window.paint_glyph(o, fid, gid, fs, color);
                    }
                }
            }
            t_paint += now_ns() - ts0;
            continue;
        }
        if *PAINT_MODE == 1 {
            // one shaped line per row: ASCII cells inline with per-style runs; non-ASCII cells are
            // blanked in the line and painted individually, pinned to their column.
            s.clear();
            let mut runs: Vec<TextRun> = Vec::new();
            let mut singles: Vec<(usize, String, Cell)> = Vec::new();
            for (x, c) in cells.iter().enumerate() {
                let extra = row.extras.iter().find(|(ex, _)| *ex as usize == x);
                let ascii = c.ch.is_ascii() && c.ch != '\0' && extra.is_none() && c.flags & F_SPACER == 0;
                if !ascii && c.flags & F_SPACER == 0 && c.ch != '\0' && c.ch != ' ' {
                    singles.push((x, extra.map(|e| e.1.clone()).unwrap_or_else(|| c.ch.to_string()), *c));
                }
                let ch = if ascii { c.ch } else { ' ' };
                let alpha = if c.flags & F_FAINT != 0 { 0.6 } else { 1.0 };
                let color = rgb3(c.fg, alpha);
                let key_flags = c.flags & (F_BOLD | F_ITALIC);
                match runs.last_mut() {
                    Some(r) if ch == ' ' || (r.color == color && (r.font.weight == FontWeight::BOLD) == (key_flags & F_BOLD != 0) && (r.font.style == FontStyle::Italic) == (key_flags & F_ITALIC != 0)) => r.len += 1,
                    _ => runs.push(TextRun { len: 1, font: mk_font(key_flags), color, background_color: None, underline: None, strikethrough: None }),
                }
                s.push(ch);
                if c.flags & (F_UNDER | F_STRIKE) != 0 {
                    let yy = if c.flags & F_UNDER != 0 { py + lh - 2. } else { py + lh / 2. };
                    window.paint_quad(fill(Bounds::new(point(px(ox + cw * x as f32), px(yy)), size(px(cw), px(1.))), color));
                }
            }
            let trimmed = s.trim_end().len();
            if trimmed > 0 {
                // trim runs to trimmed length
                let mut acc = 0;
                let mut rr = Vec::with_capacity(runs.len());
                for mut r in runs {
                    if acc >= trimmed { break; }
                    r.len = r.len.min(trimmed - acc);
                    acc += r.len;
                    rr.push(r);
                }
                let ts0 = now_ns();
                let line = window.text_system().shape_line(SharedString::from(s[..trimmed].to_string()), fs, &rr, None);
                let ts1 = now_ns();
                let _ = line.paint(point(px(ox), px(py)), px(lh), TextAlign::Left, None, window, cx);
                t_shape += ts1 - ts0;
                t_paint += now_ns() - ts1;
            }
            for (x, t, c) in singles {
                let run = TextRun { len: t.len(), font: mk_font(c.flags), color: rgb3(c.fg, 1.0), background_color: None, underline: None, strikethrough: None };
                let line = window.text_system().shape_line(SharedString::from(t), fs, &[run], None);
                let _ = line.paint(point(px(ox + cw * x as f32), px(py)), px(lh), TextAlign::Left, None, window, cx);
            }
            continue;
        }
        let mut x = 0;
        while x < n {
            let c = cells[x];
            let deco = c.flags & (F_UNDER | F_STRIKE) != 0;
            if c.flags & F_SPACER != 0 || (c.ch == ' ' && !deco) || c.ch == '\0' {
                x += 1;
                continue;
            }
            let extra = row.extras.iter().find(|(ex, _)| *ex as usize == x);
            let st = x;
            s.clear();
            if c.ch.is_ascii() && extra.is_none() {
                // ASCII run with identical style: one shaped line (monospace keeps grid alignment)
                while x < n {
                    let d = cells[x];
                    if !(d.ch.is_ascii() && d.ch != '\0') || d.fg != c.fg || d.flags != c.flags || row.extras.iter().any(|(ex, _)| *ex as usize == x) {
                        break;
                    }
                    s.push(d.ch);
                    x += 1;
                }
            } else {
                // non-ASCII (box drawing, CJK, emoji, combining): one cell at a time, pinned to its column
                match extra {
                    Some((_, t)) => s.push_str(t),
                    None => s.push(c.ch),
                }
                x += 1;
            }
            let alpha = if c.flags & F_FAINT != 0 { 0.6 } else { 1.0 };
            let run = TextRun { len: s.len(), font: mk_font(c.flags), color: rgb3(c.fg, alpha), background_color: None, underline: None, strikethrough: None };
            let line = window.text_system().shape_line(SharedString::from(s.clone()), fs, &[run], None);
            let origin = point(px(ox + cw * st as f32), px(py));
            let _ = line.paint(origin, px(lh), TextAlign::Left, None, window, cx);
            let wcells = (x - st) as f32 * if c.flags & F_WIDE != 0 { 2. } else { 1. };
            if c.flags & F_UNDER != 0 {
                window.paint_quad(fill(Bounds::new(point(px(ox + cw * st as f32), px(py + lh - 2.)), size(px(cw * wcells), px(1.))), rgb3(c.fg, alpha)));
            }
            if c.flags & F_STRIKE != 0 {
                window.paint_quad(fill(Bounds::new(point(px(ox + cw * st as f32), px(py + lh / 2.)), size(px(cw * wcells), px(1.))), rgb3(c.fg, alpha)));
            }
        }
    }
    if std::env::var("MIDNA_PROF").is_ok() {
        eprintln!("paint: bg={:.2}ms shape={:.2}ms glyphs={:.2}ms", t_bg as f64 / 1e6, t_shape as f64 / 1e6, t_paint as f64 / 1e6);
    }
    if let Some((cx_, cy)) = cursor {
        let wide = grid.get(cy as usize).and_then(|r| r.cells.get(cx_ as usize)).map(|c| c.flags & F_WIDE != 0).unwrap_or(false);
        let w = if wide { 2. * cw } else { cw };
        let (x, y) = (ox + cw * cx_ as f32, oy + lh * cy as f32);
        let b = match cstyle {
            1 => Bounds::new(point(px(x), px(y)), size(px(2.), px(lh))),
            2 => Bounds::new(point(px(x), px(y + lh - 2.)), size(px(w), px(2.))),
            _ => Bounds::new(point(px(x), px(y)), size(px(w), px(lh))),
        };
        window.paint_quad(fill(b, rgb3(dfg, 0.55)));
    }
}

mod bench;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("enginebench") {
        let data = std::fs::read(&args[2]).unwrap();
        let mut e = Engine::new(110, 34, None);
        if args.len() > 3 { e.set_grapheme(false); }
        let t0 = std::time::Instant::now();
        let mut last = std::time::Instant::now();
        let mut frames = 0;
        let mut lastp = 0;
        for (i, ch) in data.chunks(65536).enumerate() {
            e.feed(ch, 0);
            if last.elapsed().as_millis() >= 8 { e.frame(); frames += 1; last = std::time::Instant::now(); }
            if i * 65536 - lastp > 4_000_000 { lastp = i*65536; eprintln!("{:.1}MB at {:?}", lastp as f64/1e6, t0.elapsed()); if t0.elapsed().as_secs() > 20 { break; } }
        }
        eprintln!("fed {} bytes in {:?}, {} frames", e.bytes, t0.elapsed(), frames);
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("daemon") {
        transport::daemon_main(&args[2], &args[3], &args[4]);
        return;
    }
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let w: f32 = std::env::var("MIDNA_W").ok().and_then(|v| v.parse().ok()).unwrap_or(900.);
    let h: f32 = std::env::var("MIDNA_H").ok().and_then(|v| v.parse().ok()).unwrap_or(600.);
    gpui_kit::application().run(move |cx| {
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("midna gpui-terminal spike".into()), ..Default::default() }),
            ..Default::default()
        };
        cx.open_window(opts, |window, cx| cx.new(|cx| Term::new(window, cx))).expect("window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

/// 0 = per-run shape_line ("cell"), 1 = one shape_line per row ("row"), 2 = glyph cache (default)
static PAINT_MODE: std::sync::LazyLock<u8> = std::sync::LazyLock::new(|| match std::env::var("MIDNA_PAINT").as_deref() {
    Ok("cell") => 0,
    Ok("row") => 1,
    _ => 2,
});
type GlyphKey = (char, Option<String>, u8);
thread_local! {
    static GLYPHS: RefCell<std::collections::HashMap<GlyphKey, Rc<Vec<(FontId, GlyphId, f32, f32, bool)>>>> = RefCell::new(Default::default());
}
