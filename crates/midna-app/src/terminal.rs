//! Terminal pane: renders `stream.attach` frames with the per-cell glyph cache proven in
//! spikes/gpui-terminal (painted inside `window.paint_layer`).
//!
//! Input goes back over the stream as structured messages and midnad encodes them for the
//! app's current modes (libghostty's key/mouse/paste encoders): keys (`TAG_KEY`), wheel and
//! viewport scrolling (`TAG_SCROLL`), mouse buttons/motion (`TAG_MOUSE`, which is either
//! mouse reporting or the engine-owned selection), focus (`TAG_FOCUS`) and paste
//! (`TAG_PASTE`). Plain typed text (and IME commits) still goes as raw input. Copy, links
//! and find are RPCs (`session.selection`, `session.link_at`, `session.find`).
use crate::actions::*;
use crate::backend::{AttachRequest, Backend, TermStream};
use crate::frame::*;
use crate::term_edit::{self, EraseJob, KbdSel};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use std::cell::{Cell as StdCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod prompt_nav;

pub const FONT_SIZE: f32 = 12.5;
/// Design: line-height 1.6 at 12.5px.
pub const LINE_H: f32 = 20.0;
/// Design: padding 14px 20px.
const PAD_X: f32 = 20.0;
const PAD_Y: f32 = 14.0;
/// Kitty keyboard flags (see the kitty spec).
const KITTY_REPORT_EVENTS: u8 = 2;

/// `terminal.option_as_meta` (default on): option+key is sent as Meta (ESC prefix / alt
/// modifier) instead of typing the macOS character. Set by the main window from settings.
static OPTION_AS_META: AtomicBool = AtomicBool::new(true);

pub fn set_option_as_meta(on: bool) {
    OPTION_AS_META.store(on, Ordering::Relaxed);
}

type StreamSlot = Rc<RefCell<Option<Arc<dyn TermStream>>>>;

/// ⌘F find bar state.
#[derive(Clone, Debug, Default)]
struct FindBar {
    query: String,
    total: usize,
    index: usize,
    /// Bumped per request so a slow answer can't overwrite a newer one.
    seq: u64,
}

pub struct TerminalView {
    pub session_id: String,
    backend: Arc<dyn Backend>,
    sink: Arc<FrameSink>,
    stream: StreamSlot,
    status: StreamStatus,
    grid: Arc<Vec<RowData>>,
    cols: u16,
    rows: u16,
    cursor: Option<(u16, u16)>,
    cursor_style: u8,
    dfg: [u8; 3],
    dbg: [u8; 3],
    bpaste: bool,
    ext: FrameExt,
    blink_on: bool,
    last_input: Instant,
    focus: FocusHandle,
    cell_w: f32,
    req_size: Rc<StdCell<(u16, u16)>>,
    bounds: Rc<StdCell<Option<Bounds<Pixels>>>>,
    /// Mouse button held since a press inside the pane (1 left, 2 right, 3 middle).
    pressed: u8,
    /// The last drag position while the pointer is above/below the grid (autoscroll repeats it).
    drag_out: Option<MouseMsg>,
    /// Last cell a motion event was sent for (motion is deduped per cell).
    last_motion: Option<(i32, i32)>,
    /// Fractional rows of wheel/trackpad scrolling not sent yet.
    scroll_accum: f32,
    last_scroll: Instant,
    /// Focus state last reported to the daemon (mode 1004 focus events).
    reported_focus: Option<bool>,
    find: Option<FindBar>,
    /// ⌘-hover: the link under the pointer, as (row, first col, last col) on screen.
    hover_link: Option<(u16, u16, u16)>,
    /// The cell the last ⌘-hover probe was sent for.
    hover_probe: Option<(i32, i32)>,
    /// Right-click menu: where it opened, and the link under that cell (if any).
    menu: Option<(Point<Pixels>, Option<Value>)>,
    marked: Option<String>,
    /// ⇧-arrow selection in the input line (`term_edit`).
    kbd_sel: Option<KbdSel>,
    /// The terminal is an agent terminal, "claude" or "codex" (`session.get`): its input box
    /// gets text editing although the agent draws full-screen, and the left button selects
    /// (see `on_down`).
    agent: Option<&'static str>,
    /// Claude Code or Codex found running in this terminal's full-screen app (started from a
    /// shell), probed each time the alternate screen turns on.
    agent_proc: Option<&'static str>,
    /// The alternate-screen state last probed for `agent_proc`.
    alt_probed: bool,
    /// Agent terminals: the left press went to midna's selection; (cell, moved since).
    agent_press: Option<((i32, i32), bool)>,
    /// A selection being erased, and the input typed meanwhile, sent once the app has gone quiet
    /// (Claude Code can apply a typed letter in the middle of a burst of deletes). The job is
    /// None when the erase needs no check, only that wait.
    erasing: Option<(Option<EraseJob>, Vec<ClientMsg>)>,
    /// Bumped per scheduled erase check, so only the latest one runs.
    erase_gen: u64,
    font_family: SharedString,
    /// Prompt fast travel (agent terminals): the pinned bar, the rail, ⌥⌘↑ ⌥⌘↓.
    nav: prompt_nav::PromptNav,
    fps: Option<FpsMeter>,
    _tasks: Vec<Task<()>>,
}

#[derive(Clone, Debug, PartialEq)]
enum StreamStatus {
    Attaching,
    Live,
    Ended,
    Failed(String),
}

/// `MIDNA_FPS=1`: frames received and painted per second, on stderr.
struct FpsMeter {
    since: Instant,
    frames: u32,
    renders: u32,
    rows: u64,
}

fn rgb3(c: [u8; 3], a: f32) -> Hsla {
    Rgba { r: c[0] as f32 / 255., g: c[1] as f32 / 255., b: c[2] as f32 / 255., a }.into()
}

fn mods_of(m: &Modifiers) -> u8 {
    (m.shift as u8 * MOD_SHIFT) | (m.alt as u8 * MOD_ALT) | (m.control as u8 * MOD_CTRL) | (m.platform as u8 * MOD_SUPER)
}

/// Should this keystroke go to the daemon as a structured key (`Some`), or arrive as text
/// through the IME input handler (`None`)? Nearly everything is a key event, including plain
/// text keys (with their `text`): the daemon encodes them against the app's current modes,
/// so a kitty "report all keys" app gets CSI u even if this view hasn't seen the frame that
/// enabled it yet. Option characters and dead keys (option without meta) stay on the IME
/// path, as does anything typed while the IME is composing (handled by the caller).
fn key_msg(ks: &Keystroke, held: bool, option_as_meta: bool) -> Option<KeyMsg> {
    let m = &ks.modifiers;
    let printable = ks.key_char.as_ref().is_some_and(|c| !c.is_empty() && c.chars().all(|c| !c.is_control()));
    if printable && m.alt && !option_as_meta && !m.control {
        return None;
    }
    let mods = mods_of(m) & !MOD_SUPER;
    let text = ks.key_char.clone().filter(|c| c.chars().all(|c| !c.is_control())).unwrap_or_default();
    Some(KeyMsg { action: if held { KeyAction::Repeat } else { KeyAction::Press }, mods, key: ks.key.clone(), text })
}

impl TerminalView {
    pub fn new(session_id: String, backend: Arc<dyn Backend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_sized(session_id, backend, (0, 0), window, cx)
    }

    /// Like [`Self::new`], attaching at `size` (cols, rows) when known: switching terminals in
    /// the same pane reuses the pane's measured size, so the PTY isn't resized twice.
    pub fn new_sized(session_id: String, backend: Arc<dyn Backend>, size: (u16, u16), window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = cx.global::<Theme>().clone();
        let font_family = theme.mono_font.clone();
        let ts = window.text_system();
        let fid = ts.resolve_font(&font(font_family.clone()));
        let cell_w = f32::from(ts.advance(fid, px(FONT_SIZE), 'M').map(|s| s.width).unwrap_or(px(7.5)));
        let focus = cx.focus_handle();

        let (wtx, wrx) = async_channel::bounded::<()>(1);
        let sink = Arc::new(FrameSink::new(wtx));
        let mut tasks = vec![];
        // frames -> repaint
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            while wrx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        // cursor blink, scrollbar fade
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(530)).await;
                let r = this.update(cx, |t, cx| {
                    let on = t.last_input.elapsed() < Duration::from_millis(1000) || !t.blink_on;
                    let fading = t.last_scroll.elapsed() < Duration::from_millis(2200);
                    if on != t.blink_on || fading {
                        t.blink_on = on;
                        if t.cursor.is_some() || fading {
                            cx.notify();
                        }
                    }
                });
                if r.is_err() {
                    break;
                }
            }
        }));
        // selection autoscroll: repeat the last drag while the pointer is past an edge
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(60)).await;
                let r = this.update(cx, |t, _| {
                    if let (Some(m), Some(s)) = (t.drag_out, t.stream()) {
                        s.send(&ClientMsg::Mouse(m));
                    }
                });
                if r.is_err() {
                    break;
                }
            }
        }));

        // Re-attach a stream that ended (daemon restart/upgrade, a dropped socket) while the
        // session still runs. Covers the main pane, the split pane and pop-out windows alike.
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let Ok(probe) = this.update(cx, |t, _| {
                    if t.sink.is_ended() && t.status == StreamStatus::Live {
                        t.status = StreamStatus::Ended;
                    }
                    matches!(t.status, StreamStatus::Ended | StreamStatus::Failed(_)).then(|| (t.backend.clone(), t.session_id.clone()))
                }) else {
                    break;
                };
                let Some((backend, sid)) = probe else {
                    continue;
                };
                let alive = cx.background_executor().spawn(async move { backend.call("session.get", json!({ "id": sid })).ok().is_some_and(|s| !s["pid"].is_null()) }).await;
                if !alive {
                    continue;
                }
                let r = this.update_in(cx, |t, window, cx| {
                    if matches!(t.status, StreamStatus::Ended | StreamStatus::Failed(_)) {
                        if let Some(old) = t.stream.borrow_mut().take() {
                            old.close();
                        }
                        t.sink.reset();
                        t.attach(window, cx);
                    }
                });
                if r.is_err() {
                    break;
                }
            }
        }));

        let fps = std::env::var_os("MIDNA_FPS").map(|_| FpsMeter { since: Instant::now(), frames: 0, renders: 0, rows: 0 });
        let mut v = TerminalView {
            session_id,
            backend,
            sink,
            stream: Rc::new(RefCell::new(None)),
            status: StreamStatus::Attaching,
            grid: Arc::new(vec![]),
            cols: 0,
            rows: 0,
            cursor: None,
            cursor_style: 0,
            dfg: [0xdd; 3],
            dbg: [0x10; 3],
            bpaste: false,
            ext: FrameExt::default(),
            blink_on: true,
            last_input: Instant::now(),
            focus,
            cell_w,
            req_size: Rc::new(StdCell::new(size)),
            bounds: Rc::new(StdCell::new(None)),
            pressed: 0,
            drag_out: None,
            last_motion: None,
            scroll_accum: 0.,
            last_scroll: Instant::now() - Duration::from_secs(60),
            reported_focus: None,
            find: None,
            hover_link: None,
            hover_probe: None,
            kbd_sel: None,
            agent: None,
            agent_press: None,
            agent_proc: None,
            alt_probed: false,
            erasing: None,
            erase_gen: 0,
            menu: None,
            marked: None,
            font_family,
            nav: Default::default(),
            fps,
            _tasks: tasks,
        };
        v.attach(window, cx);
        v.call("session.get", json!({ "id": v.session_id }), window, cx, |t, r, window, cx| {
            t.agent = r.ok().and_then(|s| match s.get("agent").and_then(|a| a.as_str()) {
                Some("claude") => Some("claude"),
                Some("codex") => Some("codex"),
                _ => None,
            });
            if t.agent.is_some() {
                t.refresh_prompts(window, cx);
            }
        });
        v
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The pane's size in cells as last measured (0, 0 before the first layout).
    pub fn size(&self) -> (u16, u16) {
        self.req_size.get()
    }

    /// A screen has arrived (drawn or waiting in the sink), or the stream gave up.
    pub fn has_frame(&self) -> bool {
        !self.grid.is_empty() || self.sink.frame.lock().map(|f| f.is_some()).unwrap_or(false) || matches!(self.status, StreamStatus::Ended | StreamStatus::Failed(_))
    }

    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status = StreamStatus::Attaching;
        let backend = self.backend.clone();
        let sink = self.sink.clone();
        let sid = self.session_id.clone();
        // Estimate from the window until the first layout reports the real size.
        let est = window.viewport_size();
        let (mut cols, mut rows) = self.req_size.get();
        if cols == 0 {
            cols = ((f32::from(est.width) - 264. - 2. * PAD_X) / self.cell_w).floor().clamp(20., 400.) as u16;
            rows = ((f32::from(est.height) - 120. - 2. * PAD_Y) / LINE_H).floor().clamp(5., 200.) as u16;
        }
        let (cw, ch) = (self.cell_w.round() as u32, LINE_H as u32);
        let task = cx.spawn_in(window, async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.attach(AttachRequest { session: &sid, cols, rows, cell_w: cw, cell_h: ch }, sink) }).await;
            let _ = this.update(cx, |t, cx| {
                match res {
                    Ok(stream) => {
                        let (c, r) = t.req_size.get();
                        if c > 0 && (c, r) != (cols, rows) {
                            stream.resize(c, r, cw, ch);
                        }
                        *t.stream.borrow_mut() = Some(stream);
                        t.status = StreamStatus::Live;
                        t.reported_focus = None;
                    }
                    Err(e) => t.status = StreamStatus::Failed(format!("{e:#}")),
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    fn stream(&self) -> Option<Arc<dyn TermStream>> {
        self.stream.borrow().clone()
    }

    fn typed(&mut self) {
        self.last_input = Instant::now();
        self.blink_on = true;
    }

    /// Raw bytes for the PTY (text). The daemon snaps the viewport back and clears the selection.
    pub fn send(&mut self, bytes: &[u8]) {
        if let Some(s) = self.stream() {
            s.input(bytes);
        }
        self.typed();
    }

    fn send_msg(&self, m: ClientMsg) {
        if let Some(s) = self.stream() {
            s.send(&m);
        }
    }

    /// The user's input: sent now, or after the selection being erased is gone.
    fn deliver(&mut self, m: ClientMsg) {
        match self.erasing.as_mut() {
            Some((_, queued)) => queued.push(m),
            None => self.send_msg(m),
        }
        self.typed();
    }

    fn pull(&mut self, cx: &mut Context<Self>) {
        if self.sink.is_ended() && self.status == StreamStatus::Live {
            self.status = StreamStatus::Ended;
        }
        let Some(f) = self.sink.take() else { return };
        if let Some(m) = self.fps.as_mut() {
            m.frames += 1;
            m.rows += f.changed.len() as u64;
        }
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
        let moved = self.cursor != f.cursor;
        self.cursor = f.cursor;
        self.cursor_style = f.cursor_style;
        self.dfg = f.default_fg;
        self.dbg = f.default_bg;
        self.bpaste = f.bracketed_paste;
        if f.ext.scroll_offset != self.ext.scroll_offset {
            self.last_scroll = Instant::now();
        }
        self.ext = f.ext;
        if self.ext.alt_screen != self.alt_probed {
            self.alt_probed = self.ext.alt_screen;
            self.agent_proc = None;
            if self.alt_probed {
                self.probe_agent(cx);
            }
        }
        if moved && let Some(k) = self.kbd_sel.filter(|k| k.kill) {
            if self.live_cursor().is_some_and(|c| c.1 == k.anchor.1) {
                self.erase_selection(cx);
            } else {
                // The line wrapped onto another row: fall back to the shell's own ctrl-u.
                self.kbd_sel = None;
                self.send(b"\x15");
            }
        }
        if self.erasing.is_some() {
            // Check once the app has gone quiet, not between its redraws.
            self.check_erase_later(120, cx);
        }
        if let Some(s) = self.stream() {
            s.want();
        }
    }

    /// Ask the daemon whether the full-screen app just started is Claude Code or Codex.
    fn probe_agent(&mut self, cx: &mut Context<Self>) {
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("session.processes", json!({ "id": id })) }).await;
            let _ = this.update(cx, |t, cx| {
                if t.ext.alt_screen {
                    t.agent_proc = r.ok().and_then(|v| term_edit::agent_running(&v));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Claude Code or Codex runs here: an agent terminal, or one started from a shell.
    fn is_agent(&self) -> bool {
        self.agent_kind().is_some()
    }

    fn agent_kind(&self) -> Option<&'static str> {
        self.agent.or(self.agent_proc)
    }

    /// Whether the app in this terminal enabled bracketed paste (mode 2004).
    pub fn bracketed_paste(&self) -> bool {
        self.bpaste
    }

    fn scroll(&mut self, kind: ScrollKind, amount: i32) {
        self.send_msg(ClientMsg::Scroll(ScrollMsg { kind, amount, x: 0., y: 0., mods: 0 }));
        self.last_scroll = Instant::now();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        if self.find.is_some() && self.find_key(ks, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.erasing.is_some() && !m.platform {
            if let Some(k) = key_msg(ks, ev.is_held, OPTION_AS_META.load(Ordering::Relaxed)) {
                self.deliver(ClientMsg::Key(k));
                cx.stop_propagation();
            }
            return;
        }
        if self.find.is_none() && self.marked.is_none() && self.edit_key(ks, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if m.platform {
            // ⌘-shortcuts are actions, except the terminal's own scrolling and find.
            let handled = match (ks.key.as_str(), m.shift || m.alt || m.control) {
                ("up" | "home", false) => {
                    self.scroll(ScrollKind::Top, 0);
                    true
                }
                ("down" | "end", false) => {
                    self.scroll(ScrollKind::Bottom, 0);
                    true
                }
                ("pageup", false) => {
                    self.scroll(ScrollKind::Pages, -1);
                    true
                }
                ("pagedown", false) => {
                    self.scroll(ScrollKind::Pages, 1);
                    true
                }
                ("f", false) => {
                    self.open_find(cx);
                    true
                }
                _ => false,
            };
            if handled {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if m.function && ks.key.len() == 1 {
            return;
        }
        if self.marked.is_some() {
            return; // IME is composing; let it have the key
        }
        // shift-PgUp/PgDn (and shift-Home/End) scroll the scrollback outside full-screen apps.
        if m.shift && !m.control && !m.alt && !self.ext.alt_screen {
            let s = match ks.key.as_str() {
                "pageup" => Some((ScrollKind::Pages, -1)),
                "pagedown" => Some((ScrollKind::Pages, 1)),
                "home" => Some((ScrollKind::Top, 0)),
                "end" => Some((ScrollKind::Bottom, 0)),
                _ => None,
            };
            if let Some((k, n)) = s {
                self.scroll(k, n);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        if let Some(k) = key_msg(ks, ev.is_held, OPTION_AS_META.load(Ordering::Relaxed)) {
            if ks.key == "enter" && k.mods == 0 && !ev.is_held && self.marked.is_none() && self.deliver_attachment(&k, cx) {
                self.typed();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            self.deliver(ClientMsg::Key(k));
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// macOS editing keys in the input line (`term_edit`). True when the key was handled here;
    /// false sends it the usual way (after erasing a selection it replaces, for typed text).
    fn edit_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &ks.modifiers;
        if m.control {
            self.kbd_sel = None;
            return false;
        }
        let (cmd, alt, key) = (m.platform, m.alt, ks.key.as_str());
        // ⌘↑ ⌘↓: to the start or end of an agent's text (⇧ selects). A shell's line is one
        // line, so there ⇧⌘↑ ⇧⌘↓ select to its start or end and plain ⌘↑ ⌘↓ still scroll.
        if cmd && !alt && matches!(key, "up" | "down") && self.line_editing() {
            let Some(c) = self.live_cursor() else { return false };
            let line_edge = term_edit::Send::Bytes(if key == "up" { b"\x01" } else { b"\x05" });
            let moves = if self.is_agent() { term_edit::text_edge(key, &self.grid, c) } else { m.shift.then(|| vec![line_edge]) };
            let Some(moves) = moves else { return false };
            if !m.shift {
                self.kbd_sel = None;
            } else if self.kbd_range().is_none() {
                self.kbd_sel = Some(KbdSel { anchor: c, kill: false });
            }
            for mv in moves {
                self.send_move(mv);
            }
            return true;
        }
        if m.shift && !cmd && !alt && matches!(key, "up" | "down") && self.line_editing() {
            let Some(c) = self.live_cursor() else { return false };
            let Some(mv) = term_edit::vertical(key, self.is_agent(), &self.grid, c) else { return false };
            if self.kbd_range().is_none() {
                self.kbd_sel = Some(KbdSel { anchor: c, kill: false });
            }
            self.send_move(mv);
            return true;
        }
        if let Some(mv) = term_edit::movement(key, cmd, alt) {
            // In other full-screen apps (vim, less) ⇧← is theirs; ⌘ and ⌥ moves are sent
            // everywhere, as Ghostty and Terminal.app do.
            if m.shift && !self.line_editing() {
                return false;
            }
            if m.shift {
                let Some(c) = self.live_cursor() else { return false };
                if self.kbd_range().is_none() {
                    self.kbd_sel = Some(KbdSel { anchor: c, kill: false });
                }
                self.send_move(mv);
                return true;
            }
            // A plain ← → collapses a one-row selection to its edge, like a text field.
            if !cmd
                && !alt
                && let (Some((lo, hi)), Some(c)) = (self.kbd_range(), self.live_cursor())
                && lo.1 == hi.1
            {
                self.kbd_sel = None;
                let target = if key == "left" { lo.0 } else { hi.0 };
                let row = self.grid.get(c.1 as usize).cloned().unwrap_or_default();
                let n = term_edit::chars_between(&row, c.0, target);
                for _ in 0..n {
                    self.arrow(if target < c.0 { "left" } else { "right" });
                }
                return true;
            }
            self.kbd_sel = None;
            if cmd || alt {
                self.send_move(mv);
                return true;
            }
            return false;
        }
        if let Some(bytes) = term_edit::deletion(key, cmd, alt) {
            if self.erase_selection(cx) {
                return true;
            }
            match self.live_cursor() {
                Some(c) if key == "backspace" && cmd && !self.is_agent() && !self.ext.alt_screen => {
                    // Erased in `pull` once the cursor is at the line start.
                    self.kbd_sel = Some(KbdSel { anchor: c, kill: true });
                    self.send(b"\x01");
                }
                _ => self.send(bytes),
            }
            return true;
        }
        if !cmd && !alt && matches!(key, "backspace" | "delete") {
            return self.erase_selection(cx);
        }
        if key == "escape" && self.kbd_sel.take().is_some() {
            return true;
        }
        if cmd {
            return false; // ⌘C copies the selection, so it stays
        }
        let typed = ks.key_char.as_ref().is_some_and(|c| !c.is_empty() && c.chars().all(|c| !c.is_control()));
        let meta = OPTION_AS_META.load(Ordering::Relaxed);
        if typed && !alt {
            self.erase_selection(cx);
        } else if !(typed && alt && !meta) {
            // (An option character arrives through the input handler, which erases then.)
            self.kbd_sel = None;
        }
        false
    }

    /// A selection in the input line can be made and replaced: a shell prompt (not a
    /// full-screen app) or an agent's input box.
    fn line_editing(&self) -> bool {
        !self.ext.alt_screen || self.is_agent()
    }

    /// The cursor on the live screen (not while scrolled back, where rows don't line up).
    fn live_cursor(&self) -> Option<(u16, u16)> {
        self.cursor.filter(|_| self.ext.at_bottom())
    }

    /// The keyboard selection's ends in reading order.
    fn kbd_range(&self) -> Option<(term_edit::Pos, term_edit::Pos)> {
        let (k, c) = (self.kbd_sel?, self.live_cursor()?);
        Some(term_edit::ordered(k.anchor, c))
    }

    /// Continuation rows of an agent's input start past this many cells.
    fn indent(&self) -> u16 {
        if self.is_agent() { term_edit::AGENT_INDENT } else { 0 }
    }

    fn arrow(&mut self, key: &str) {
        self.send_msg(ClientMsg::Key(KeyMsg { action: KeyAction::Press, mods: 0, key: key.into(), text: String::new() }));
    }

    fn send_move(&mut self, mv: term_edit::Send) {
        match term_edit::for_agent(mv, self.agent_kind()) {
            term_edit::Send::Bytes(b) => self.send(b),
            term_edit::Send::Arrow(k) => self.arrow(k),
        }
        self.typed();
    }

    /// Erase the selection that typing replaces (`term_edit::replace_range`), if there is one.
    fn erase_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let kbd = self.kbd_sel.take();
        if !self.line_editing() || self.erasing.is_some() {
            return false;
        }
        let Some(c) = self.live_cursor() else { return false };
        let Some((s, e)) = term_edit::replace_range(kbd, &self.ext.selection, c) else { return false };
        if s.1 != e.1 {
            return self.start_erase_job(s, e, c, cx);
        }
        let Some((moves, n)) = self.grid.get(c.1 as usize).and_then(|row| term_edit::erase_plan(row, c.0, s.0, e.0)) else { return false };
        for _ in 0..moves.unsigned_abs() {
            self.arrow(if moves < 0 { "left" } else { "right" });
        }
        for _ in 0..n {
            self.arrow("backspace");
        }
        // The engine drops its selection on these keys; drop ours now so a second key typed
        // before the next frame doesn't erase it again.
        self.ext.selection.clear();
        self.settle(None, cx);
        true
    }

    /// A selection across rows: erase what it certainly holds, then check (`check_erase`).
    fn start_erase_job(&mut self, s: term_edit::Pos, e: term_edit::Pos, c: term_edit::Pos, cx: &mut Context<Self>) -> bool {
        let Some(plan) = EraseJob::plan(&self.grid, s, e, c, self.indent()) else { return false };
        let (key, n, job) = match plan {
            term_edit::Erase::Blind(key, n) => (key, n, None),
            term_edit::Erase::Checked(job, n) => (job.key, n, Some(job)),
        };
        for _ in 0..n {
            self.arrow(key);
        }
        self.ext.selection.clear();
        self.settle(job, cx);
        true
    }

    /// Hold the input typed from now on until the app has gone quiet (and `job` is done).
    fn settle(&mut self, job: Option<EraseJob>, cx: &mut Context<Self>) {
        self.erasing = Some((job, vec![]));
        self.typed();
        self.check_erase_later(250, cx);
    }

    fn check_erase_later(&mut self, ms: u64, cx: &mut Context<Self>) {
        self.erase_gen += 1;
        let generation = self.erase_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(ms)).await;
            let _ = this.update(cx, |t, cx| {
                if t.erase_gen == generation {
                    t.check_erase(cx);
                }
            });
        })
        .detach();
    }

    /// Done when the screen shows the text around the selection meeting at the cursor; else one
    /// more key, within the job's budget. Then the input typed meanwhile goes out.
    fn check_erase(&mut self, cx: &mut Context<Self>) {
        // A frame not drawn yet (the window may not be redrawing): take it in first, and judge
        // once the app has been quiet for a moment (`pull` schedules that).
        if self.sink.frame.lock().is_ok_and(|f| f.is_some()) {
            self.pull(cx);
            return;
        }
        let Some((job, _)) = self.erasing.as_mut() else { return };
        let Some(job) = job.as_mut().filter(|j| !self.cursor.is_some_and(|c| j.done(&self.grid, c))) else {
            return self.finish_erase(cx);
        };
        if job.budget > 0 {
            job.budget -= 1;
            let key = job.key;
            self.arrow(key);
            self.check_erase_later(250, cx);
            return;
        }
        self.finish_erase(cx);
    }

    fn finish_erase(&mut self, cx: &mut Context<Self>) {
        if let Some((_, queued)) = self.erasing.take() {
            for m in queued {
                self.send_msg(m);
            }
        }
        cx.notify();
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || self.find.is_some() || self.ext.kitty_flags & KITTY_REPORT_EVENTS == 0 {
            return;
        }
        // Releases only matter to apps that asked for kitty event reporting.
        if let Some(mut k) = key_msg(ks, false, OPTION_AS_META.load(Ordering::Relaxed)) {
            k.action = KeyAction::Release;
            self.send_msg(ClientMsg::Key(k));
        }
    }

    /// ↩ with an image attachment waiting (`annotate.rs`): paste each image path, then the
    /// notes, then press ↩. Spaced out so the app handles each paste (Claude Code turns a
    /// pasted image path into `[Image #N]` and reads the file as it does).
    fn deliver_attachment(&mut self, enter: &KeyMsg, cx: &mut Context<Self>) -> bool {
        let Some(stream) = self.stream.borrow().clone() else {
            return false;
        };
        let Some(out) = crate::annotate::take(&self.session_id, cx) else {
            return false;
        };
        let mut steps: Vec<ClientMsg> = out.pastes().into_iter().map(ClientMsg::Paste).collect();
        steps.push(ClientMsg::Key(enter.clone()));
        cx.spawn(async move |_, cx| {
            for (i, s) in steps.iter().enumerate() {
                if i > 0 {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                }
                stream.send(s);
            }
        })
        .detach();
        true
    }

    fn paste_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.find.as_mut() {
            f.query.push_str(text.lines().next().unwrap_or(""));
            self.run_find(true, true, window, cx);
            return;
        }
        self.erase_selection(cx);
        self.deliver(ClientMsg::Paste(text.to_string()));
    }

    fn on_paste(&mut self, _: &TermPaste, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() && crate::annotate::clipboard_is_image(cx) {
            // A bare image (a screenshot): open the image sheet with it.
            window.dispatch_action(Box::new(crate::annotate::PasteImage), cx);
            return;
        }
        if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.paste_text(&t, window, cx);
            cx.notify();
        }
    }

    /// Run a control-connection call off the UI thread and hand the answer back.
    fn call(
        &self,
        method: &'static str,
        params: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, anyhow::Result<Value>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call(method, params) }).await;
            let _ = this.update_in(cx, |t, window, cx| then(t, r, window, cx));
        })
        .detach();
    }

    fn on_copy(&mut self, _: &TermCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(window, cx);
    }

    fn copy_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ext.selection.is_empty() {
            return;
        }
        // The engine formats the selection: wide chars, wrapped lines and scrolled-off rows.
        self.call("session.selection", json!({ "id": self.session_id }), window, cx, |_, r, _, cx| {
            if let Some(text) = r.ok().and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_string)) {
                if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
                    // Dev runs leave the real pasteboard alone.
                    eprintln!("midna-app debug-term: copied {text:?}");
                    return;
                }
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        });
    }

    fn on_select_all(&mut self, _: &TermSelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.call("session.select_all", json!({ "id": self.session_id }), window, cx, |_, _, _, _| {});
    }

    // ------------------------------------------------------------------ mouse

    /// Pointer position in cell units from the grid's top-left.
    fn cell_pos(&self, p: Point<Pixels>) -> Option<(f32, f32)> {
        let b = self.bounds.get()?;
        let x = (f32::from(p.x - b.origin.x) - PAD_X) / self.cell_w;
        let y = (f32::from(p.y - b.origin.y) - PAD_Y) / LINE_H;
        Some((x, y))
    }

    /// Window position of a grid cell's center-left (dev tools).
    pub fn grid_point(&self, col: f32, row: f32) -> Option<Point<Pixels>> {
        let b = self.bounds.get()?;
        Some(point(b.origin.x + px(PAD_X + self.cell_w * col + 1.), b.origin.y + px(PAD_Y + LINE_H * (row + 0.5))))
    }

    fn button_of(b: MouseButton) -> u8 {
        match b {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 3,
            _ => 0,
        }
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.kbd_sel = None;
        let Some((x, y)) = self.cell_pos(ev.position) else {
            return;
        };
        if self.menu.take().is_some() {
            cx.notify();
        }
        if ev.modifiers.platform && ev.button == MouseButton::Left {
            self.open_link_at(x, y, window, cx);
            return;
        }
        // Right-click (or ctrl-click) opens the menu, unless the app reports the mouse
        // (shift forces the menu even then).
        let ctrl_click = ev.button == MouseButton::Left && ev.modifiers.control;
        if (ev.button == MouseButton::Right || ctrl_click) && (!self.ext.mouse_tracking || ev.modifiers.shift) {
            self.open_menu(ev.position, x, y, window, cx);
            return;
        }
        let button = Self::button_of(ev.button);
        if button == 0 || (button != 1 && !self.ext.mouse_tracking) {
            return;
        }
        self.pressed = button;
        let cell = (x.floor() as i32, y.floor() as i32);
        self.last_motion = Some(cell);
        self.agent_press = (button == 1 && self.agent_selects()).then_some((cell, false));
        let mods = mods_of(&ev.modifiers) | if self.agent_press.is_some() { MOD_SHIFT } else { 0 };
        self.send_msg(ClientMsg::Mouse(MouseMsg { action: MouseAction::Press, button, mods, x, y }));
        cx.notify();
    }

    /// Mouse move anywhere in the window (registered during paint so drags keep working
    /// outside the pane).
    fn on_move(&mut self, ev: &MouseMoveEvent, inside: bool, cx: &mut Context<Self>) {
        let Some((x, y)) = self.cell_pos(ev.position) else {
            return;
        };
        if ev.modifiers.platform && inside && self.pressed == 0 {
            self.probe_hover(x, y, cx);
        } else if self.hover_link.is_some() || self.hover_probe.is_some() {
            self.clear_hover(cx);
        }
        let cell = (x.floor() as i32, y.floor() as i32);
        let mut mods = mods_of(&ev.modifiers);
        if let Some((at, moved)) = self.agent_press.as_mut() {
            *moved |= *at != cell;
            mods |= MOD_SHIFT;
        }
        if self.pressed != 0 {
            let m = MouseMsg { action: MouseAction::Motion, button: self.pressed, mods, x, y };
            let out = y < 0. || y >= self.rows as f32;
            self.drag_out = (out && self.pressed == 1 && !self.ext.mouse_tracking).then_some(m);
            if self.last_motion != Some(cell) || self.pressed == 1 {
                self.last_motion = Some(cell);
                self.send_msg(ClientMsg::Mouse(m));
            }
        } else if inside && self.ext.mouse_tracking && self.last_motion != Some(cell) {
            // Any-event tracking (1003) wants plain motion; the encoder drops it otherwise.
            self.last_motion = Some(cell);
            self.send_msg(ClientMsg::Mouse(MouseMsg { action: MouseAction::Motion, button: 0, mods, x, y }));
        }
    }

    fn on_up(&mut self, ev: &MouseUpEvent) {
        let button = Self::button_of(ev.button);
        if self.pressed == 0 || button != self.pressed {
            return;
        }
        self.pressed = 0;
        self.drag_out = None;
        let (x, y) = self.cell_pos(ev.position).unwrap_or((0., 0.));
        let mods = mods_of(&ev.modifiers);
        match self.agent_press.take() {
            Some((_, moved)) => {
                self.send_msg(ClientMsg::Mouse(MouseMsg { action: MouseAction::Release, button, mods: mods | MOD_SHIFT, x, y }));
                // A plain click (no drag, not a double-click) still reaches the agent, which
                // moves its cursor there.
                if !moved && ev.click_count == 1 {
                    self.send_msg(ClientMsg::Mouse(MouseMsg { action: MouseAction::Press, button, mods, x, y }));
                    self.send_msg(ClientMsg::Mouse(MouseMsg { action: MouseAction::Release, button, mods, x, y }));
                }
            }
            None => self.send_msg(ClientMsg::Mouse(MouseMsg { action: MouseAction::Release, button, mods, x, y })),
        }
    }

    /// In an agent's terminal the left button makes midna's selection (as ⇧-drag does in any
    /// app that reports the mouse), so a selected word can be typed over.
    fn agent_selects(&self) -> bool {
        self.is_agent() && self.ext.mouse_tracking
    }

    fn on_wheel(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let dy = match ev.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y) / LINE_H,
            ScrollDelta::Lines(l) => l.y * 3.,
        };
        // Positive delta = content moves down = toward history (negative rows).
        self.scroll_accum -= dy;
        let rows = self.scroll_accum.trunc();
        if rows == 0. {
            return;
        }
        self.scroll_accum -= rows;
        let (x, y) = self.cell_pos(ev.position).unwrap_or((0., 0.));
        self.send_msg(ClientMsg::Scroll(ScrollMsg { kind: ScrollKind::Wheel, amount: rows as i32, x, y, mods: mods_of(&ev.modifiers) }));
        if !self.ext.mouse_tracking && !self.ext.alt_screen {
            self.last_scroll = Instant::now();
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// ⌘-click: open an OSC 8 hyperlink, a URL, or `path:line` under the pointer.
    fn open_link_at(&mut self, x: f32, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        if x < 0. || y < 0. {
            return;
        }
        let params = json!({ "id": self.session_id, "col": x as u16, "row": y as u16 });
        self.call("session.link_at", params, window, cx, |t, r, _, cx| {
            if let Ok(v) = r {
                t.open_link(&v, cx);
            }
        });
    }

    /// Open a `session.link_at` result: URLs in the browser, files in the editor.
    fn open_link(&self, v: &Value, cx: &mut Context<Self>) {
        let target = v.get("target").and_then(Value::as_str).unwrap_or("").to_string();
        if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
            // Dev runs never launch browsers or editors.
            eprintln!("midna-app debug-term: link {v}");
            return;
        }
        match v.get("kind").and_then(Value::as_str) {
            Some("url") => cx.open_url(&target),
            Some("file") => {
                let line = v.get("line").and_then(Value::as_u64).map(|n| n as u32);
                open_file(self.backend.clone(), self.session_id.clone(), target, line);
            }
            _ => {}
        }
    }

    /// ⌘ held over the grid: ask the daemon what link is under the pointer (once per cell)
    /// and underline it.
    fn probe_hover(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let cell = (x.floor() as i32, y.floor() as i32);
        if self.hover_probe == Some(cell) {
            return;
        }
        self.hover_probe = Some(cell);
        if cell.0 < 0 || cell.1 < 0 || cell.1 >= self.rows as i32 || cell.0 >= self.cols as i32 {
            self.set_hover(None, cx);
            return;
        }
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        let params = json!({ "id": id, "col": cell.0, "row": cell.1 });
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("session.link_at", params) }).await;
            let _ = this.update(cx, |t, cx| {
                if t.hover_probe != Some(cell) {
                    return; // the pointer moved on
                }
                let span = r.ok().filter(|v| v.get("kind").and_then(Value::as_str).is_some_and(|k| k != "none")).and_then(|v| {
                    let row = t.grid.get(cell.1 as usize)?;
                    let chars: Vec<char> = row.cells.iter().map(|c| c.ch).collect();
                    let target = v.get("target").and_then(Value::as_str).filter(|_| v.get("kind").and_then(Value::as_str) == Some("url"));
                    link_span(&chars, cell.0 as usize, target).map(|(a, b)| (cell.1 as u16, a as u16, b as u16))
                });
                t.set_hover(span, cx);
            });
        })
        .detach();
    }

    fn set_hover(&mut self, span: Option<(u16, u16, u16)>, cx: &mut Context<Self>) {
        if self.hover_link != span {
            self.hover_link = span;
            cx.notify();
        }
    }

    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_probe = None;
        self.set_hover(None, cx);
    }

    // ------------------------------------------------------------------ context menu

    fn open_menu(&mut self, pos: Point<Pixels>, x: f32, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = Some((pos, None));
        cx.notify();
        if x < 0. || y < 0. {
            return;
        }
        let params = json!({ "id": self.session_id, "col": x as u16, "row": y as u16 });
        self.call("session.link_at", params, window, cx, move |t, r, _, cx| {
            let link = r.ok().filter(|v| v.get("kind").and_then(Value::as_str).is_some_and(|k| k != "none"));
            if let Some((p, l)) = t.menu.as_mut()
                && *p == pos
            {
                *l = link;
                cx.notify();
            }
        });
    }

    fn menu_action(&mut self, what: &str, window: &mut Window, cx: &mut Context<Self>) {
        let link = self.menu.take().and_then(|(_, l)| l);
        match what {
            "copy" => self.copy_selection(window, cx),
            "paste" => {
                if let Some(t) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.paste_text(&t, window, cx);
                }
            }
            "open" => {
                if let Some(v) = link {
                    self.open_link(&v, cx);
                }
            }
            "select_all" => self.call("session.select_all", json!({ "id": self.session_id }), window, cx, |_, _, _, _| {}),
            "clear" => self.call("session.clear", json!({ "id": self.session_id }), window, cx, |_, _, _, _| {}),
            _ => {}
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn render_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pos, link) = self.menu.clone()?;
        let has_sel = !self.ext.selection.is_empty();
        let link_label = match link.as_ref().and_then(|v| v.get("kind")).and_then(Value::as_str) {
            Some("file") => "Open file",
            _ => "Open link",
        };
        let item = |id: &'static str, label: &'static str, keys: &'static str, enabled: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(18.))
                .h(px(24.))
                .px(px(10.))
                .rounded(px(5.))
                .text_size(px(12.5))
                .text_color(if enabled { theme.fg } else { theme.dim.opacity(0.6) })
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|s| s.bg(theme.accent).text_color(theme.accent_fg)).on_click(cx.listener(move |t, _, window, cx| t.menu_action(id, window, cx)))
                })
                .child(div().flex_1().child(label))
                .child(div().text_color(theme.dim).text_size(px(11.5)).child(keys))
        };
        let sep = || div().h(px(1.)).my(px(4.)).mx(px(6.)).bg(theme.line);
        let menu = div()
            .id("term-menu")
            .occlude()
            .min_w(px(190.))
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
            .on_mouse_down_out(cx.listener(|t, _, _, cx| {
                t.menu = None;
                cx.notify();
            }))
            .child(item("copy", "Copy", "⌘C", has_sel, cx))
            .child(item("paste", "Paste", "⌘V", true, cx))
            .child(sep())
            .child(item("open", link_label, "⌘-click", link.is_some(), cx))
            .child(sep())
            .child(item("select_all", "Select All", "⌘A", true, cx))
            .child(item("clear", "Clear", "", true, cx));
        Some(deferred(anchored().position(pos).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }

    // ------------------------------------------------------------------ find

    fn open_find(&mut self, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.find = Some(FindBar::default());
        }
        cx.notify();
    }

    fn close_find(&mut self) {
        if self.find.take().is_some() {
            let (backend, id) = (self.backend.clone(), self.session_id.clone());
            std::thread::spawn(move || backend.call("session.find", json!({ "id": id, "query": "" })));
        }
    }

    /// Keys while the find bar is open. Returns true when the key was used.
    fn find_key(&mut self, ks: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = &ks.modifiers;
        let used = match ks.key.as_str() {
            "escape" => {
                self.close_find();
                true
            }
            // Enter goes back in time (older matches), shift-Enter forward.
            "enter" => {
                self.run_find(!m.shift, false, window, cx);
                true
            }
            "g" if m.platform => {
                self.run_find(!m.shift, false, window, cx);
                true
            }
            "backspace" => {
                if let Some(f) = self.find.as_mut() {
                    if m.alt || m.platform {
                        f.query.clear();
                    } else {
                        f.query.pop();
                    }
                }
                self.run_find(true, true, window, cx);
                true
            }
            "f" if m.platform => true,
            "v" if m.platform => false, // TermPaste appends to the query
            _ if m.platform => false,
            _ => {
                // Text keys type into the query; other keys are swallowed while finding.
                if let Some(t) = ks.key_char.as_ref().filter(|t| !m.control && !t.is_empty() && t.chars().all(|c| !c.is_control())) {
                    if let Some(f) = self.find.as_mut() {
                        f.query.push_str(t);
                    }
                    self.run_find(true, true, window, cx);
                }
                true
            }
        };
        if used {
            cx.notify();
        }
        used
    }

    fn run_find(&mut self, backwards: bool, restart: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        f.seq += 1;
        let (seq, query) = (f.seq, f.query.clone());
        let id = self.session_id.clone();
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    if restart {
                        // A changed query starts again from the newest match.
                        let _ = backend.call("session.find", json!({ "id": id, "query": "" }));
                    }
                    backend.call("session.find", json!({ "id": id, "query": query, "backwards": backwards }))
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if let (Ok(v), Some(f)) = (r, t.find.as_mut())
                    && f.seq == seq
                {
                    f.total = v.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
                    f.index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let b = self.bounds.get()?;
        let (cx_, cy) = self.cursor.unwrap_or((0, 0));
        Some(Bounds::new(point(b.origin.x + px(PAD_X + self.cell_w * cx_ as f32), b.origin.y + px(PAD_Y + LINE_H * cy as f32)), size(px(self.cell_w), px(LINE_H))))
    }

    /// Report focus changes (window activation included) to apps using mode 1004.
    fn report_focus(&mut self, focused: bool) {
        if self.reported_focus != Some(focused) && self.stream().is_some() {
            self.reported_focus = Some(focused);
            self.send_msg(ClientMsg::Focus(focused));
        }
    }
}

/// Open `path` at `line`: `$VISUAL`/`$EDITOR` (from the user's login shell) in a new terminal
/// next to this one, or the default app (`open`) when no editor is set.
pub(crate) fn open_file(backend: Arc<dyn Backend>, session: String, path: String, line: Option<u32>) {
    std::thread::spawn(move || {
        let editor = login_editor();
        let Some(editor) = editor else {
            let _ = std::process::Command::new("/usr/bin/open").arg(&path).status();
            return;
        };
        let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        let bin = editor.split_whitespace().next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
        let cmd = match (bin.as_str(), line) {
            ("code" | "cursor" | "zed" | "subl", Some(l)) => {
                format!("{editor} -g {}", q(&format!("{path}:{l}")))
            }
            (_, Some(l)) => format!("{editor} +{l} {}", q(&path)),
            (_, None) => format!("{editor} {}", q(&path)),
        };
        let s = backend.call("session.get", json!({ "id": session })).unwrap_or(Value::Null);
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut p = json!({ "kind": "shell", "name": name, "command": [cmd] });
        for k in ["project_id", "cwd"] {
            if let Some(v) = s.get(k).filter(|v| v.is_string()) {
                p[k] = v.clone();
            }
        }
        if let Ok(opened) = backend.call("session.open", p)
            && let Some(id) = opened.get("id").and_then(Value::as_str)
        {
            let _ = backend.call("session.focus", json!({ "id": id }));
        }
    });
}

/// `$VISUAL` / `$EDITOR` as the user's login shell sees them (a GUI app's environment usually
/// lacks them). Cached for the app's lifetime.
fn login_editor() -> Option<String> {
    static EDITOR: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    EDITOR
        .get_or_init(|| {
            let from_env = std::env::var("VISUAL").ok().or_else(|| std::env::var("EDITOR").ok()).filter(|s| !s.trim().is_empty());
            if from_env.is_some() {
                return from_env;
            }
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let out = std::process::Command::new(shell).args(["-l", "-c", "printf %s \"${VISUAL:-$EDITOR}\""]).stdin(std::process::Stdio::null()).output().ok()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        })
        .clone()
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(&mut self, _r: Range<usize>, _adj: &mut Option<Range<usize>>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _ignore: bool, _w: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _w: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _r: Option<Range<usize>>, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        if let Some(f) = self.find.as_mut() {
            f.query.push_str(text);
            self.run_find(true, true, window, cx);
        } else if !text.is_empty() {
            self.erase_selection(cx);
            self.deliver(ClientMsg::Input(text.as_bytes().to_vec()));
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, _r: Option<Range<usize>>, new_text: &str, _sel: Option<Range<usize>>, _w: &mut Window, cx: &mut Context<Self>) {
        self.marked = if new_text.is_empty() { None } else { Some(new_text.to_string()) };
        cx.notify();
    }

    fn bounds_for_range(&mut self, _r: Range<usize>, _eb: Bounds<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        self.cursor_bounds()
    }

    fn character_index_for_point(&mut self, _p: Point<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        None
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = item.text() {
            self.paste_text(&t, window, cx);
            cx.notify();
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Colors the painter needs, resolved from the theme each frame.
#[derive(Clone, Copy)]
struct Palette {
    fg: Hsla,
    cursor: Hsla,
    selection: Hsla,
}

/// Scrollbar thumb geometry and visibility, from the frame's scroll state.
#[derive(Clone, Copy)]
struct ScrollBar {
    total: u64,
    offset: u64,
    len: u64,
    alpha: f32,
    color: Hsla,
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.pull(cx);
        if let Some(m) = self.fps.as_mut() {
            m.renders += 1;
            if m.since.elapsed() >= Duration::from_secs(1) {
                let s = m.since.elapsed().as_secs_f32();
                eprintln!("midna-app fps {}: {:.0} frames/s, {:.0} renders/s, {:.0} rows/s", self.session_id, m.frames as f32 / s, m.renders as f32 / s, m.rows as f32 / s);
                *m = FpsMeter { since: Instant::now(), frames: 0, renders: 0, rows: 0 };
            }
        }
        let theme = cx.global::<Theme>().clone();
        if theme.mono_font != self.font_family {
            self.font_family = theme.mono_font.clone();
            GLYPHS.with(|g| g.borrow_mut().clear());
        }
        let focused = self.focus.is_focused(window);
        // Dev runs (MIDNA_DEBUG_TERM) don't activate the window; count it as active.
        let active = window.is_window_active() || crate::dev::var_os("MIDNA_DEBUG_TERM").is_some();
        self.report_focus(focused && active);
        let pal = Palette { fg: theme.fg, cursor: theme.fg.opacity(0.55), selection: theme.accent.opacity(0.32) };
        let term_bg = theme.term;
        let dim = theme.dim;
        let grid = self.grid.clone();
        let cw = self.cell_w;
        let (dfg, dbg) = (self.dfg, self.dbg);
        let at_bottom = self.ext.at_bottom();
        // The cursor belongs to the live screen; hide it while scrolled back.
        let cursor = if (self.blink_on || !focused) && at_bottom { self.cursor } else { None };
        let cstyle = if focused { self.cursor_style } else { 3 };
        let mut selection = self.ext.selection.clone();
        if let Some((s, e)) = self.kbd_range() {
            selection.extend(term_edit::spans(&self.grid, s, e, self.indent()));
        }
        let hover_link = self.hover_link;
        let link_color = theme.accent;
        let menu = self.render_menu(&theme, cx);
        self.update_nav(window, cx);
        let nav = self.render_nav(&theme, cx);
        let marked = self.marked.clone();
        let req = self.req_size.clone();
        let stream = self.stream.clone();
        let bounds_cell = self.bounds.clone();
        let family = self.font_family.clone();
        let entity = cx.entity();
        let focus = self.focus.clone();
        let since_scroll = self.last_scroll.elapsed().as_secs_f32();
        let sb_alpha = if !at_bottom { 0.45 } else { (0.45 * (1. - (since_scroll - 1.2) / 0.8)).clamp(0., 0.45) };
        let scrollbar = (self.ext.scroll_total > self.ext.scroll_len && sb_alpha > 0.).then_some(ScrollBar {
            total: self.ext.scroll_total,
            offset: self.ext.scroll_offset,
            len: self.ext.scroll_len,
            alpha: sb_alpha,
            color: theme.fg,
        });
        let below = self.ext.scroll_total.saturating_sub(self.ext.scroll_offset + self.ext.scroll_len);

        let overlay = match &self.status {
            StreamStatus::Attaching => Some(("Attaching…".to_string(), dim)),
            StreamStatus::Ended => Some(("Stream ended. The session closed or midnad went away.".to_string(), dim)),
            StreamStatus::Failed(e) => Some((format!("Could not attach: {e}"), theme.err)),
            StreamStatus::Live => None,
        };
        let chip = |d: Div| d.px(px(10.)).py(px(4.)).rounded(px(6.)).bg(theme.panel).border_1().border_color(theme.line).text_size(px(11.5));

        div()
            .id("terminal")
            .key_context(CTX_TERM)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(term_bg)
            .cursor(if self.hover_link.is_some() {
                CursorStyle::PointingHand
            } else if self.ext.mouse_tracking {
                CursorStyle::Arrow
            } else {
                CursorStyle::IBeam
            })
            .on_modifiers_changed(cx.listener(|t, ev: &ModifiersChangedEvent, _, cx| {
                if !ev.modifiers.platform {
                    t.clear_hover(cx);
                }
            }))
            .on_key_down(cx.listener(Self::on_key))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_prev_prompt))
            .on_action(cx.listener(Self::on_next_prompt))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_down))
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            .child(
                canvas(
                    move |bounds, _w, _cx| {
                        bounds_cell.set(Some(bounds));
                        let cols = ((f32::from(bounds.size.width) - 2. * PAD_X) / cw).floor().max(2.) as u16;
                        let rows = ((f32::from(bounds.size.height) - 2. * PAD_Y) / LINE_H).floor().max(2.) as u16;
                        if req.get() != (cols, rows) {
                            req.set((cols, rows));
                            if let Some(s) = stream.borrow().as_ref() {
                                s.resize(cols, rows, cw.round() as u32, LINE_H as u32);
                            }
                        }
                    },
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                        // Moves and releases anywhere in the window, so drags can leave the pane.
                        let e = entity.clone();
                        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _w, cx| {
                            if phase == DispatchPhase::Bubble {
                                let inside = bounds.contains(&ev.position);
                                e.update(cx, |t, cx| t.on_move(ev, inside, cx));
                            }
                        });
                        let e = entity.clone();
                        window.on_mouse_event(move |ev: &MouseUpEvent, phase, _w, cx| {
                            if phase == DispatchPhase::Bubble {
                                e.update(cx, |t, _| t.on_up(ev));
                            }
                        });
                        window.paint_layer(bounds, |window| {
                            paint_grid(&grid, bounds, cw, &family, dfg, dbg, pal, cursor, cstyle, &selection, window, cx);
                            if let (Some(m), Some((x, y))) = (&marked, cursor.or(Some((0, 0)))) {
                                paint_marked(m, bounds, cw, &family, pal, x, y, window, cx);
                            }
                            if let Some(sb) = scrollbar {
                                paint_scrollbar(sb, bounds, window);
                            }
                            if let Some((row, a, b)) = hover_link {
                                let x0 = f32::from(bounds.origin.x) + PAD_X + cw * a as f32;
                                let y0 = f32::from(bounds.origin.y) + PAD_Y + LINE_H * (row as f32 + 1.) - 3.;
                                window.paint_quad(fill(Bounds::new(point(px(x0), px(y0)), size(px(cw * (b - a + 1) as f32), px(1.))), link_color));
                            }
                        });
                    },
                )
                .size_full(),
            )
            .when(!at_bottom && below > 0 && overlay.is_none(), |d| {
                d.child(chip(div().absolute().bottom(px(10.)).right(px(18.))).text_color(dim).child(format!("↓ {below} line{} below · ⌘↓", if below == 1 { "" } else { "s" })))
            })
            .when_some(self.find.clone(), |d, f| {
                let count = if f.query.is_empty() {
                    "type to find".to_string()
                } else if f.total == 0 {
                    "no matches".to_string()
                } else {
                    format!("{}/{}", f.index, f.total)
                };
                d.child(
                    chip(div().absolute().top(px(8.)).right(px(18.)))
                        .flex()
                        .gap(px(10.))
                        .items_center()
                        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.25), offset: point(px(0.), px(4.)), blur_radius: px(12.), spread_radius: px(0.), inset: false }])
                        .child(div().text_color(dim).child("Find"))
                        .child(div().min_w(px(140.)).font_family(self.font_family.clone()).text_color(theme.fg).child(format!("{}▏", f.query)))
                        .child(div().text_color(if f.total == 0 && !f.query.is_empty() { theme.err } else { dim }).child(count))
                        .child(div().text_color(dim).child("↩ older · ⇧↩ newer · esc")),
                )
            })
            .when_some(overlay, |d, (msg, color)| d.child(chip(div().absolute().bottom(px(12.)).right(px(16.))).text_color(color).child(msg)))
            .children(nav)
            .children(menu)
    }
}

/// Columns `(first, last)` of the link around `col` on one screen row: the whitespace-
/// delimited word, narrowed to the URL `target` when it appears there, else trimmed of the
/// brackets, quotes and trailing punctuation the daemon's detector ignores.
pub fn link_span(chars: &[char], col: usize, target: Option<&str>) -> Option<(usize, usize)> {
    let ws = |c: char| c.is_whitespace() || c == '\0';
    if col >= chars.len() || ws(chars[col]) {
        return None;
    }
    let mut a = col;
    while a > 0 && !ws(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < chars.len() && !ws(chars[b + 1]) {
        b += 1;
    }
    if let Some(t) = target {
        let word: Vec<char> = chars[a..=b].to_vec();
        let tc: Vec<char> = t.chars().collect();
        if !tc.is_empty() && tc.len() <= word.len() {
            for i in 0..=word.len() - tc.len() {
                if word[i..i + tc.len()] == tc[..] && a + i <= col && col < a + i + tc.len() {
                    return Some((a + i, a + i + tc.len() - 1));
                }
            }
        }
    }
    let lead = |c: char| matches!(c, '(' | '[' | '<' | '"' | '\'' | '`');
    let trail = |c: char| matches!(c, ')' | ']' | '>' | '"' | '\'' | '`' | ',' | ';' | '.' | ':');
    while a < b && lead(chars[a]) {
        a += 1;
    }
    while b > a && trail(chars[b]) {
        b -= 1;
    }
    (a <= col && col <= b).then_some((a, b))
}

fn paint_scrollbar(sb: ScrollBar, bounds: Bounds<Pixels>, window: &mut Window) {
    let h = f32::from(bounds.size.height) - 2. * PAD_Y;
    let total = sb.total.max(1) as f32;
    let thumb_h = (h * sb.len as f32 / total).max(18.);
    let top = (h - thumb_h) * (sb.offset as f32 / (sb.total - sb.len).max(1) as f32);
    let x = f32::from(bounds.origin.x + bounds.size.width) - 9.;
    let y = f32::from(bounds.origin.y) + PAD_Y + top.clamp(0., h - thumb_h);
    window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(4.), px(thumb_h))), sb.color.opacity(sb.alpha)).corner_radii(px(2.)));
}

#[allow(clippy::too_many_arguments)]
fn paint_marked(m: &str, bounds: Bounds<Pixels>, cw: f32, family: &SharedString, pal: Palette, x: u16, y: u16, window: &mut Window, cx: &mut App) {
    let ox = f32::from(bounds.origin.x) + PAD_X + cw * x as f32;
    let oy = f32::from(bounds.origin.y) + PAD_Y + LINE_H * y as f32;
    let run = TextRun { len: m.len(), font: font(family.clone()), color: pal.fg, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from(m.to_string()), px(FONT_SIZE), &[run], None);
    let w = f32::from(line.width).max(cw);
    window.paint_quad(fill(Bounds::new(point(px(ox), px(oy)), size(px(w), px(LINE_H))), pal.selection));
    window.paint_quad(fill(Bounds::new(point(px(ox), px(oy + LINE_H - 2.)), size(px(w), px(1.))), pal.fg));
    let _ = line.paint(point(px(ox), px(oy)), px(LINE_H), TextAlign::Left, None, window, cx);
}

type GlyphKey = (char, Option<String>, u8);
type GlyphRun = Rc<Vec<(FontId, GlyphId, f32, f32, bool)>>;
thread_local! {
    static GLYPHS: RefCell<HashMap<GlyphKey, GlyphRun>> = RefCell::new(HashMap::new());
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    grid: &[RowData],
    bounds: Bounds<Pixels>,
    cw: f32,
    family: &SharedString,
    dfg: [u8; 3],
    dbg: [u8; 3],
    pal: Palette,
    cursor: Option<(u16, u16)>,
    cstyle: u8,
    selection: &[(u16, u16, u16)],
    window: &mut Window,
    _cx: &mut App,
) {
    let lh = LINE_H;
    let ox = f32::from(bounds.origin.x) + PAD_X;
    let oy = f32::from(bounds.origin.y) + PAD_Y;
    let fs = px(FONT_SIZE);
    let mk_font = |flags: u8| {
        let mut f = font(family.clone());
        if flags & F_BOLD != 0 {
            f.weight = FontWeight::BOLD;
        }
        if flags & F_ITALIC != 0 {
            f.style = FontStyle::Italic;
        }
        f
    };
    // Default fg/bg from the engine map to the theme so light/dark both read right.
    let fg_of = |c: [u8; 3], alpha: f32| {
        if c == dfg { pal.fg.opacity(alpha) } else { rgb3(c, alpha) }
    };
    for (y, row) in grid.iter().enumerate() {
        let py = oy + lh * y as f32;
        let cells = &row.cells;
        let n = cells.len();
        // backgrounds (default bg is the pane's own color)
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
            let color = if bg == dfg { pal.fg } else { rgb3(bg, 1.0) };
            window.paint_quad(fill(Bounds::new(point(px(ox + cw * st as f32), px(py)), size(px(cw * (x - st) as f32), px(lh))), color));
        }
        // selection (engine-owned; a range ending on a wide char covers its spacer too)
        for &(sy, x0, x1) in selection.iter().filter(|s| s.0 as usize == y) {
            let _ = sy;
            let wide = cells.get(x1 as usize).is_some_and(|c| c.flags & F_WIDE != 0);
            let x1 = x1 as f32 + if wide { 2. } else { 1. };
            let x0 = x0 as f32;
            if x1 > x0 {
                window.paint_quad(fill(Bounds::new(point(px(ox + cw * x0), px(py)), size(px(cw * (x1 - x0)), px(lh))), pal.selection));
            }
        }
        // text: shape each distinct (grapheme, bold/italic) once, then paint glyphs at cells
        for (x, c) in cells.iter().enumerate() {
            if (c.flags & F_SPACER != 0 || c.ch == ' ' || c.ch == '\0') && c.flags & (F_UNDER | F_STRIKE) == 0 {
                continue;
            }
            let alpha = if c.flags & F_FAINT != 0 { 0.6 } else { 1.0 };
            let color = fg_of(c.fg, alpha);
            if c.flags & (F_UNDER | F_STRIKE) != 0 {
                let yy = if c.flags & F_UNDER != 0 { py + lh - 3. } else { py + lh / 2. };
                window.paint_quad(fill(Bounds::new(point(px(ox + cw * x as f32), px(yy)), size(px(cw), px(1.))), color));
                if c.ch == ' ' || c.ch == '\0' {
                    continue;
                }
            }
            let extra = row.extras.iter().find(|(ex, _)| *ex as usize == x).map(|e| e.1.as_str());
            let key_flags = c.flags & (F_BOLD | F_ITALIC);
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
                let v: GlyphRun = Rc::new(line.runs.iter().flat_map(|r| r.glyphs.iter().map(move |gl| (r.font_id, gl.id, f32::from(gl.position.x), base, gl.is_emoji))).collect());
                g.insert(key, v.clone());
                v
            });
            for &(fid, gid, gx, base, emoji) in glyphs.iter() {
                let o = point(px(ox + cw * x as f32 + gx), px(py + base));
                if emoji {
                    let _ = window.paint_emoji(o, fid, gid, fs);
                } else {
                    let _ = window.paint_glyph(o, fid, gid, fs, color);
                }
            }
        }
    }
    if let Some((cx_, cy)) = cursor {
        let wide = grid.get(cy as usize).and_then(|r| r.cells.get(cx_ as usize)).map(|c| c.flags & F_WIDE != 0).unwrap_or(false);
        let w = if wide { 2. * cw } else { cw };
        let (x, y) = (ox + cw * cx_ as f32, oy + lh * cy as f32);
        match cstyle {
            1 => window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(2.), px(lh))), pal.cursor)),
            2 => window.paint_quad(fill(Bounds::new(point(px(x), px(y + lh - 2.)), size(px(w), px(2.))), pal.cursor)),
            3 => window.paint_quad(outline(Bounds::new(point(px(x), px(y)), size(px(w), px(lh))), pal.cursor, BorderStyle::Solid)),
            _ => window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(w), px(lh))), pal.cursor)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{key_msg, link_span};

    #[test]
    fn link_spans() {
        let row: Vec<char> = "see (https://midna.dev/a) and src/main.rs:12: ok".chars().collect();
        // The URL itself, not the parens around it.
        assert_eq!(link_span(&row, 10, Some("https://midna.dev/a")), Some((5, 23)));
        // A path keeps :line, loses the trailing colon.
        let i = 30;
        assert_eq!(link_span(&row, i, None), Some((30, 43)));
        assert_eq!(link_span(&row, 3, None), None); // on a space
    }

    use crate::frame::{KeyAction, MOD_ALT, MOD_CTRL, MOD_SHIFT};
    use gpui_kit::Keystroke;

    fn ks(s: &str, key_char: Option<&str>) -> Keystroke {
        let mut k = Keystroke::parse(s).unwrap();
        k.key_char = key_char.map(str::to_string);
        k
    }

    #[test]
    fn routing() {
        // Text keys are key events too, carrying their text.
        let k = key_msg(&ks("a", Some("a")), false, true).unwrap();
        assert_eq!((k.key.as_str(), k.text.as_str(), k.mods), ("a", "a", 0));
        let k = key_msg(&ks("shift-a", Some("A")), false, true).unwrap();
        assert_eq!((k.text.as_str(), k.mods), ("A", MOD_SHIFT));
        // Ctrl and option-as-meta combos, named keys.
        assert_eq!(key_msg(&ks("ctrl-c", None), false, true).unwrap().mods, MOD_CTRL);
        assert_eq!(key_msg(&ks("alt-b", Some("∫")), false, true).unwrap().mods, MOD_ALT);
        // Option without meta types macOS characters (and dead keys) through the IME.
        assert!(key_msg(&ks("alt-b", Some("∫")), false, false).is_none());
        assert_eq!(key_msg(&ks("alt-left", None), false, false).unwrap().mods, MOD_ALT);
        let k = key_msg(&ks("shift-tab", Some("\t")), true, true).unwrap();
        assert_eq!((k.key.as_str(), k.mods, k.action), ("tab", MOD_SHIFT, KeyAction::Repeat));
        assert_eq!(key_msg(&ks("enter", Some("\n")), false, true).unwrap().text, "");
        assert_eq!(key_msg(&ks("escape", None), false, true).unwrap().key, "escape");
        assert_eq!(key_msg(&ks("cmd-c", None), false, true).unwrap().mods, 0, "super never reaches the app");
    }
}
