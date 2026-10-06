//! Image annotations: attach screenshots with numbered notes to an agent's next message
//! (design: the "D · Sheet + image strip" and "Added, not sent yet" boards).
//!
//! ⌘I (`keys.add_image`), the header's image button, ⌘V of a bare image in a terminal, or
//! image files dropped on a terminal open the sheet for that terminal. Dragging image files
//! over a pane focuses it, and holding them over a sidebar row selects that terminal. Each
//! terminal has one draft: a list of images, each with notes that are a pin (click) or an area
//! (drag), stored as fractions of the image so they survive zooming. Closing the sheet keeps
//! the draft; "Add to chat" (⌘↩) attaches it.
//!
//! The sheet is its own view (`AnnotateView`), so any window can host one: the main window and
//! each pop-out do. Drafts live in the [`Drafts`] global, so a terminal's draft follows it
//! between windows.
//!
//! Nothing is typed into the terminal when a draft is attached. It sits in the [`Outbox`]
//! (an app global, so a split or pop-out of the same terminal sees it too) and the tray under
//! the terminal offers Edit (⌘E) and Remove. The next plain ↩ in that terminal delivers it:
//! each image's path as its own paste (Claude Code turns a pasted image path into
//! `[Image #N]`), then a newline (Ctrl+J), then the notes as one paste, then the ↩ itself.
//!
//! Images are normalized into `$TMPDIR/midna-images/`: PNG, JPEG, GIF and WebP are kept as
//! they are; anything else (HEIC, TIFF, …) is converted to PNG with `sips`.
use crate::app::{MainWindow, Overlay};
use crate::ui::text_input::{FieldChanged, TextField};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::frame::ClientMsg;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

actions!(
    midna,
    [
        /// Open the image sheet for the focused terminal (`keys.add_image`).
        AddImage,
        /// Open the sheet and add the image on the clipboard (⌘V of a bare image in a terminal).
        PasteImage,
        /// ⌘E: reopen the sheet for the focused terminal's attached draft.
        EditAttachment,
    ]
);

/// Image files dropped on a terminal: open its sheet and add them.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = midna, no_json)]
pub struct DropImages {
    pub session: String,
    pub paths: Vec<PathBuf>,
}

/// Where a note points, as fractions (0..1) of the image's width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    Pin { x: f32, y: f32 },
    Area { x0: f32, y0: f32, x1: f32, y1: f32 },
}

impl Mark {
    /// An area from two drag corners, in any order.
    pub fn area(a: (f32, f32), b: (f32, f32)) -> Mark {
        Mark::Area { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1) }
    }

    /// Where the note's number badge sits.
    pub fn anchor(&self) -> (f32, f32) {
        match *self {
            Mark::Pin { x, y } => (x, y),
            Mark::Area { x0, y0, .. } => (x0, y0),
        }
    }

    /// `[x=12, y=15]`, or `[x=34-54, y=69-78]` for an area: percentages from the top-left.
    pub fn label(&self) -> String {
        match *self {
            Mark::Pin { x, y } => format!("[x={}, y={}]", pct(x), pct(y)),
            Mark::Area { x0, y0, x1, y1 } => format!("[x={}-{}, y={}-{}]", pct(x0), pct(x1), pct(y0), pct(y1)),
        }
    }
}

fn pct(v: f32) -> i32 {
    (v.clamp(0., 1.) * 100.).round() as i32
}

#[derive(Clone, Debug)]
pub struct Note {
    pub mark: Mark,
    pub text: String,
}

#[derive(Clone)]
pub struct Shot {
    pub name: String,
    /// The normalized file that gets pasted.
    pub path: PathBuf,
    pub image: Arc<Image>,
    pub w: u32,
    pub h: u32,
    pub notes: Vec<Note>,
}

impl Shot {
    fn written(&self) -> usize {
        self.notes.iter().filter(|n| !n.text.trim().is_empty()).count()
    }
}

#[derive(Clone, Default)]
pub struct Draft {
    pub shots: Vec<Shot>,
    /// "Add to chat" was pressed: the draft is in the [`Outbox`].
    pub attached: bool,
}

impl Draft {
    pub fn note_count(&self) -> usize {
        self.shots.iter().map(Shot::written).sum()
    }
}

/// The notes as they are sent. Notes without text are left out; no written notes, no text.
/// One image uses Saggar's wording; several are numbered in the order their paths are pasted.
pub fn notes_text(shots: &[(&str, &[Note])]) -> String {
    let written = |notes: &[Note]| notes.iter().filter(|n| !n.text.trim().is_empty()).cloned().collect::<Vec<_>>();
    if shots.iter().all(|(_, n)| written(n).is_empty()) {
        return String::new();
    }
    let list = |notes: &[Note]| notes.iter().enumerate().map(|(i, n)| format!("{}. {} {}", i + 1, n.mark.label(), n.text.trim())).collect::<Vec<_>>().join("\n");
    if let [(_, notes)] = shots {
        return format!("Annotations (coordinates are percentages from the image's top-left):\n{}", list(&written(notes)));
    }
    let mut out = String::from("Annotations (coordinates are percentages from each image's top-left; images are in the order attached):");
    for (i, (name, notes)) in shots.iter().enumerate() {
        let w = written(notes);
        if !w.is_empty() {
            out.push_str(&format!("\n\nImage {} ({name}):\n{}", i + 1, list(&w)));
        }
    }
    out
}

/// What an attached draft becomes on the next ↩.
#[derive(Clone)]
pub struct Outgoing {
    pub paths: Vec<PathBuf>,
    pub text: String,
    pub notes: usize,
    pub thumbs: Vec<Arc<Image>>,
}

impl Outgoing {
    fn of(d: &Draft) -> Outgoing {
        let shots: Vec<(&str, &[Note])> = d.shots.iter().map(|s| (s.name.as_str(), s.notes.as_slice())).collect();
        Outgoing { paths: d.shots.iter().map(|s| s.path.clone()).collect(), text: notes_text(&shots), notes: d.note_count(), thumbs: d.shots.iter().map(|s| s.image.clone()).collect() }
    }

    /// The steps, in order, that deliver this (the ↩ follows them). The notes start on their
    /// own line: a newline inside a paste doesn't break the line in Claude Code's prompt, so
    /// it goes in as Ctrl+J, which does.
    pub fn steps(&self) -> Vec<ClientMsg> {
        let mut v: Vec<ClientMsg> = self.paths.iter().map(|p| ClientMsg::Paste(p.display().to_string())).collect();
        if !self.text.is_empty() {
            if !v.is_empty() {
                v.push(ClientMsg::Input(b"\n".to_vec()));
            }
            v.push(ClientMsg::Paste(self.text.clone()));
        }
        v
    }
}

/// Attached drafts by terminal id, waiting for that terminal's next ↩.
#[derive(Default)]
pub struct Outbox(pub HashMap<String, Outgoing>);

impl Global for Outbox {}

/// Each terminal's draft, shared by every window's sheet.
#[derive(Default)]
pub struct Drafts(pub HashMap<String, Draft>);

impl Global for Drafts {}

pub fn init(cx: &mut App) {
    cx.set_global(Outbox::default());
    cx.set_global(Drafts::default());
}

/// Take the attachment for `session`, if any (the terminal is about to send it). Its draft is
/// done with it.
pub fn take(session: &str, cx: &mut App) -> Option<Outgoing> {
    if !cx.has_global::<Outbox>() || !cx.global::<Outbox>().0.contains_key(session) {
        return None;
    }
    if cx.global::<Drafts>().0.get(session).is_some_and(|d| d.attached) {
        cx.update_global::<Drafts, _>(|d, _| d.0.remove(session));
    }
    cx.update_global::<Outbox, _>(|o, _| o.0.remove(session))
}

/// Whether `session` has an attachment waiting for its next ↩.
pub fn is_attached(session: &str, cx: &App) -> bool {
    cx.try_global::<Outbox>().is_some_and(|o| o.0.contains_key(session))
}

/// The tray's Remove: drop the attachment and its draft.
pub fn remove(id: &str, cx: &mut App) {
    cx.update_global::<Drafts, _>(|d, _| d.0.remove(id));
    cx.update_global::<Outbox, _>(|o, _| o.0.remove(id));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pin,
    Area,
}

/// What the sheet tells its window: it closed (refocus the terminal), or something to say.
pub enum AnnotateEvent {
    Closed,
    Toast(String),
}

/// The sheet. Each window that shows terminals hosts one.
pub struct AnnotateView {
    pub focus: FocusHandle,
    /// The note being edited (one shared field).
    pub field: Entity<TextField>,
    /// The terminal the sheet is for.
    pub target: Option<String>,
    open: bool,
    /// Index of the shown image.
    pub cur: usize,
    pub sel: Option<usize>,
    pub editing: Option<usize>,
    pub tool: Tool,
    /// None = fit. Otherwise screen px per image px.
    pub zoom: Option<f32>,
    /// Drag in progress on the image: start and current point, as fractions.
    pub drag: Option<((f32, f32), (f32, f32))>,
    pub show_text: bool,
    /// Images being read or converted.
    pub loading: usize,
    /// The image's bounds and the viewport's, from the last paint.
    pub stage: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl EventEmitter<AnnotateEvent> for AnnotateView {}

impl AnnotateView {
    pub fn new(cx: &mut Context<Self>) -> AnnotateView {
        let field = cx.new(|cx| {
            let mut f = TextField::new(cx, false, "Add a note…");
            f.wrap = true;
            f
        });
        cx.subscribe(&field, |v, _, _: &FieldChanged, cx| v.field_changed(cx)).detach();
        // Another window's sheet, or a terminal sending the attachment, changed a draft.
        cx.observe_global::<Drafts>(|_, cx| cx.notify()).detach();
        AnnotateView {
            focus: cx.focus_handle(),
            field,
            target: None,
            open: false,
            cur: 0,
            sel: None,
            editing: None,
            tool: Tool::Pin,
            zoom: None,
            drag: None,
            show_text: false,
            loading: 0,
            stage: Rc::new(Cell::new(None)),
            viewport: Rc::new(Cell::new(None)),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn draft<'a>(&self, cx: &'a App) -> Option<&'a Draft> {
        cx.try_global::<Drafts>()?.0.get(self.target.as_ref()?)
    }

    pub fn shot<'a>(&self, cx: &'a App) -> Option<&'a Shot> {
        self.draft(cx)?.shots.get(self.cur)
    }

    fn update_draft<R>(&self, cx: &mut App, f: impl FnOnce(&mut Draft) -> R) -> Option<R> {
        let id = self.target.clone()?;
        cx.update_global::<Drafts, _>(|d, _| d.0.get_mut(&id).map(f))
    }

    fn update_shot<R>(&self, cx: &mut App, f: impl FnOnce(&mut Shot) -> R) -> Option<R> {
        let cur = self.cur;
        self.update_draft(cx, |d| d.shots.get_mut(cur).map(f)).flatten()
    }

    fn toast(&mut self, msg: impl Into<String>, cx: &mut Context<Self>) {
        cx.emit(AnnotateEvent::Toast(msg.into()));
    }

    // -------------------------------------------------------------- opening and closing

    /// Show the sheet for `session` (its draft, or a new one).
    pub fn open(&mut self, session: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_deref() != Some(&session) {
            self.cur = 0;
        }
        self.target = Some(session.clone());
        let n = cx.update_global::<Drafts, _>(|d, _| d.0.entry(session).or_default().shots.len());
        self.cur = self.cur.min(n.saturating_sub(1));
        self.sel = None;
        self.editing = None;
        self.drag = None;
        self.zoom = None;
        self.open = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Esc, Close, the scrim: keep the draft and tell the window.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.hide(window, cx);
            cx.emit(AnnotateEvent::Closed);
        }
    }

    /// The window moved on (another overlay): put the sheet away without telling it.
    pub fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
        self.open = false;
        cx.notify();
    }

    /// ⌘↩: attach the draft to the terminal's next message.
    pub fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
        if self.draft(cx).is_none_or(|d| d.shots.is_empty()) {
            self.toast("Add an image first.", cx);
            return;
        }
        self.update_draft(cx, |d| d.attached = true);
        self.sync(cx);
        crate::sounds::play("image_added");
        self.close(window, cx);
    }

    /// Keep the outbox in step with an attached draft after an edit.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.target.clone() else {
            return;
        };
        let out = cx.update_global::<Drafts, _>(|drafts, _| {
            let d = drafts.0.get_mut(&id).filter(|d| d.attached)?;
            if d.shots.is_empty() {
                d.attached = false;
                return Some(None);
            }
            Some(Some(Outgoing::of(d)))
        });
        match out {
            Some(Some(out)) => cx.update_global::<Outbox, _>(|o, _| o.0.insert(id, out)),
            Some(None) => cx.update_global::<Outbox, _>(|o, _| o.0.remove(&id)),
            None => None,
        };
    }

    // -------------------------------------------------------------- notes

    fn field_changed(&mut self, cx: &mut Context<Self>) {
        let Some(i) = self.editing else {
            return;
        };
        let text = self.field.read(cx).text().to_string();
        self.update_shot(cx, |s| {
            if let Some(n) = s.notes.get_mut(i) {
                n.text = text;
            }
        });
        self.sync(cx);
        cx.notify();
    }

    /// Finish editing: an empty note is removed. Focus goes back to the sheet.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.editing.take() else {
            return;
        };
        let empty = self.shot(cx).and_then(|s| s.notes.get(i)).is_some_and(|n| n.text.trim().is_empty());
        if empty {
            self.remove_note(i, cx);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub fn edit(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing == Some(i) {
            return;
        }
        self.commit(window, cx);
        let Some(text) = self.shot(cx).and_then(|s| s.notes.get(i)).map(|n| n.text.clone()) else {
            return;
        };
        self.sel = Some(i);
        // Set the text before `editing`, so the change doesn't write back into the note.
        self.field.update(cx, |f, cx| f.set_text(&text, cx));
        self.editing = Some(i);
        let fh = self.field.read(cx).focus.clone();
        fh.focus(window, cx);
        cx.notify();
    }

    pub fn remove_note(&mut self, i: usize, cx: &mut Context<Self>) {
        let removed = self.update_shot(cx, |s| {
            if i < s.notes.len() {
                s.notes.remove(i);
            }
        });
        if removed.is_none() {
            return;
        }
        self.sel = self.sel.filter(|&s| s != i).map(|s| if s > i { s - 1 } else { s });
        self.editing = self.editing.filter(|&e| e != i).map(|e| if e > i { e - 1 } else { e });
        self.sync(cx);
        cx.notify();
    }

    fn add_note(&mut self, mark: Mark, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.update_shot(cx, |s| {
            s.notes.push(Note { mark, text: String::new() });
            s.notes.len() - 1
        }) else {
            return;
        };
        self.edit(i, window, cx);
    }

    // -------------------------------------------------------------- pointer on the image

    fn frac(&self, p: Point<Pixels>) -> Option<(f32, f32)> {
        let b = self.stage.get()?;
        let x = f32::from(p.x - b.origin.x) / f32::from(b.size.width);
        let y = f32::from(p.y - b.origin.y) / f32::from(b.size.height);
        Some((x.clamp(0., 1.), y.clamp(0., 1.)))
    }

    pub fn pointer_down(&mut self, p: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
        if let Some(f) = self.frac(p) {
            self.drag = Some((f, f));
            self.sel = None;
            cx.notify();
        }
    }

    pub fn pointer_move(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        if let (Some(f), Some(d)) = (self.frac(p), self.drag.as_mut()) {
            d.1 = f;
            cx.notify();
        }
    }

    /// Whether the drag in progress is long enough to be an area.
    pub fn drag_is_area(&self) -> bool {
        let (Some((a, b)), Some(st)) = (self.drag, self.stage.get()) else {
            return false;
        };
        let dx = (b.0 - a.0) * f32::from(st.size.width);
        let dy = (b.1 - a.1) * f32::from(st.size.height);
        dx.hypot(dy) >= DRAG_MIN
    }

    pub fn pointer_up(&mut self, p: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.pointer_move(p, cx);
        let area = self.drag_is_area();
        let Some((a, b)) = self.drag.take() else {
            return;
        };
        if area {
            self.add_note(Mark::area(a, b), window, cx);
        } else if self.tool == Tool::Pin {
            self.add_note(Mark::Pin { x: a.0, y: a.1 }, window, cx);
        }
        cx.notify();
    }

    // -------------------------------------------------------------- images

    pub fn select_image(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
        let n = self.draft(cx).map(|d| d.shots.len()).unwrap_or(0);
        if i < n {
            self.cur = i;
            self.sel = None;
            self.zoom = None;
        }
        cx.notify();
    }

    pub fn remove_image(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(window, cx);
        let Some(n) = self.update_draft(cx, |d| {
            if i < d.shots.len() {
                d.shots.remove(i);
            }
            d.shots.len()
        }) else {
            return;
        };
        self.cur = self.cur.min(n.saturating_sub(1));
        self.sel = None;
        self.sync(cx);
        cx.notify();
    }

    pub fn zoom(&mut self, by: Option<f32>, cx: &mut Context<Self>) {
        self.zoom = by.map(|f| (self.current_scale(cx) * f).clamp(0.05, 8.));
        cx.notify();
    }

    /// Screen px per image px: the zoom, or the fit into the viewport (at most 2×).
    pub fn current_scale(&self, cx: &App) -> f32 {
        if let Some(z) = self.zoom {
            return z;
        }
        let Some(s) = self.shot(cx) else {
            return 1.;
        };
        let (vw, vh) = self.viewport.get().map(|b| (f32::from(b.size.width), f32::from(b.size.height))).unwrap_or((640., 400.));
        fit_scale(s.w, s.h, vw - 32., vh - 32.)
    }

    /// ⌘V in the sheet, or its Paste button.
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = clipboard_sources(cx);
        if s.is_empty() {
            self.toast("No image on the clipboard.", cx);
        }
        self.add(s, window, cx);
    }

    /// Read, convert and add images to the open draft (off the UI thread).
    pub fn add(&mut self, sources: Vec<Source>, window: &mut Window, cx: &mut Context<Self>) {
        if sources.is_empty() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        self.loading += sources.len();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let results = cx.background_executor().spawn(async move { sources.into_iter().map(load).collect::<Vec<_>>() }).await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.loading = v.loading.saturating_sub(results.len());
                let mut errors = vec![];
                let mut shots = vec![];
                for r in results {
                    match r {
                        Ok(s) => shots.push(s),
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                if !shots.is_empty() {
                    let first = cx.update_global::<Drafts, _>(|d, _| {
                        let d = d.0.entry(target.clone()).or_default();
                        d.shots.extend(shots);
                        d.shots.len() - 1
                    });
                    if v.target.as_deref() == Some(&target) {
                        v.select_image(first, window, cx);
                        v.sync(cx);
                    }
                }
                if !errors.is_empty() {
                    v.toast(format!("Couldn't add image: {}", errors.join("; ")), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = this.update_in(cx, |v, window, cx| v.add(paths.into_iter().map(Source::Path).collect(), window, cx));
        })
        .detach();
    }

    // -------------------------------------------------------------- keys

    /// Keys the sheet handles (those the note field didn't take).
    pub fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let md = &ks.modifiers;
        let in_field = self.field.read(cx).focus.is_focused(window);
        let n_images = self.draft(cx).map(|d| d.shots.len()).unwrap_or(0);
        let handled = if md.platform {
            match ks.key.as_str() {
                "=" | "+" => {
                    self.zoom(Some(1.25), cx);
                    true
                }
                "-" => {
                    self.zoom(Some(0.8), cx);
                    true
                }
                "0" => {
                    self.zoom(None, cx);
                    true
                }
                "up" if self.cur > 0 => {
                    self.select_image(self.cur - 1, window, cx);
                    true
                }
                "down" if self.cur + 1 < n_images => {
                    self.select_image(self.cur + 1, window, cx);
                    true
                }
                "v" if !in_field => {
                    self.paste(window, cx);
                    true
                }
                _ => false,
            }
        } else if in_field {
            if ks.key == "enter" {
                self.commit(window, cx);
                true
            } else {
                false
            }
        } else if md.control || md.alt {
            false
        } else {
            match ks.key.as_str() {
                "p" => {
                    self.tool = Tool::Pin;
                    true
                }
                "b" => {
                    self.tool = Tool::Area;
                    true
                }
                "enter" => match self.sel {
                    Some(i) => {
                        self.edit(i, window, cx);
                        true
                    }
                    None => false,
                },
                "backspace" | "delete" => match self.sel {
                    Some(i) => {
                        self.remove_note(i, cx);
                        true
                    }
                    None => false,
                },
                "up" | "down" => {
                    let n = self.shot(cx).map(|s| s.notes.len()).unwrap_or(0);
                    if n > 0 {
                        self.sel = Some(match (self.sel, ks.key.as_str()) {
                            (None, "up") => n - 1,
                            (None, _) => 0,
                            (Some(i), "up") => i.saturating_sub(1),
                            (Some(i), _) => (i + 1).min(n - 1),
                        });
                    }
                    true
                }
                _ => false,
            }
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

/// A drag of at least this many screen pixels draws an area; less is a click.
const DRAG_MIN: f32 = 5.;

pub fn fit_scale(w: u32, h: u32, vw: f32, vh: f32) -> f32 {
    if w == 0 || h == 0 {
        return 1.;
    }
    (vw / w as f32).min(vh / h as f32).clamp(0.02, 2.)
}

// ------------------------------------------------------------------ the main window's sheet

pub fn new_for_main(window: &Window, cx: &mut Context<MainWindow>) -> Entity<AnnotateView> {
    let view = cx.new(AnnotateView::new);
    cx.subscribe_in(&view, window, |m: &mut MainWindow, _, ev: &AnnotateEvent, window, cx| match ev {
        AnnotateEvent::Closed if m.overlay == Overlay::Annotate => m.set_overlay(Overlay::None, window, cx),
        AnnotateEvent::Closed => m.focus_terminal(window, cx),
        AnnotateEvent::Toast(msg) => m.toast(msg.clone(), cx),
    })
    .detach();
    // A terminal sent its attachment, or a sheet changed it: the trays follow.
    cx.observe_global::<Outbox>(|_, cx| cx.notify()).detach();
    view
}

/// The terminal image actions are for: the focused pane's (the split's, when it has focus),
/// otherwise the sidebar selection.
pub fn pane_session(m: &MainWindow, window: &Window, cx: &App) -> Option<String> {
    if let Some(s) = &m.split
        && s.view.read(cx).focus_handle().contains_focused(window, cx)
    {
        return Some(s.session_id(cx));
    }
    m.selected.clone()
}

/// Open the main window's sheet for `session` (the focused pane's when None).
pub fn open(m: &mut MainWindow, session: Option<String>, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    let Some(id) = session.or_else(|| pane_session(m, window, cx)) else {
        m.toast("Select a terminal to add an image to.", cx);
        return false;
    };
    // A popped-out terminal's sheet opens in its own window.
    if crate::ui::popout::is_popped(&id, cx) {
        return false;
    }
    m.annot.update(cx, |v, cx| v.open(id, window, cx));
    if m.overlay != Overlay::Annotate {
        m.set_overlay(Overlay::Annotate, window, cx);
    }
    true
}

/// Image files dropped on `session` (a pane or a sidebar row): open its sheet with them.
pub fn drop_images(m: &mut MainWindow, session: String, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<MainWindow>) {
    if crate::ui::popout::is_popped(&session, cx) {
        crate::ui::popout::drop_images(&session, paths, cx);
        return;
    }
    if open(m, Some(session), window, cx) {
        m.annot.update(cx, |v, cx| v.add(paths.into_iter().map(Source::Path).collect(), window, cx));
    }
}

// ------------------------------------------------------------------ sidebar rows

/// How long image files hover over a sidebar row before its terminal is selected.
const SPRING: std::time::Duration = std::time::Duration::from_millis(450);

/// The sidebar row image files are hovering over (spring-loaded selection).
#[derive(Default)]
struct Spring(Option<String>);

impl Global for Spring {}

/// A sidebar row as a target for image files: held over it for a moment, they select its
/// terminal; dropped on it, they open its sheet. Rows another main window shows are left alone.
pub fn row_drop_target(row: Stateful<Div>, m: &MainWindow, session: &str, t: &crate::theme::Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    if m.windows.borrow().owned(session) && !m.shows(session) {
        return row;
    }
    let (hover_id, drop_id) = (session.to_string(), session.to_string());
    let tint = t.accent.opacity(0.14);
    row.drag_over::<ExternalPaths>(move |s, paths, _, _| if image_paths(paths).is_empty() { s } else { s.bg(tint) })
        .on_drag_move(cx.listener(move |m, ev: &DragMoveEvent<ExternalPaths>, w, cx| spring(m, &hover_id, ev, w, cx)))
        .on_drop(cx.listener(move |m, paths: &ExternalPaths, w, cx| {
            let paths = image_paths(paths);
            if !paths.is_empty() {
                if !crate::ui::popout::is_popped(&drop_id, cx) {
                    m.select_only(drop_id.clone(), w, cx);
                }
                drop_images(m, drop_id.clone(), paths, w, cx);
            }
        }))
}

/// Image files moving over `session`'s row: select it once they've stayed there for `SPRING`.
fn spring(m: &mut MainWindow, session: &str, ev: &DragMoveEvent<ExternalPaths>, window: &mut Window, cx: &mut Context<MainWindow>) {
    let bounds = ev.bounds;
    // Drag moves reach every row; only the one under the cursor counts, and not under a sheet.
    if !bounds.contains(&ev.event.position) || m.overlay != Overlay::None || m.selected.as_deref() == Some(session) || crate::ui::popout::is_popped(session, cx) {
        return;
    }
    if cx.try_global::<Spring>().and_then(|s| s.0.as_deref()) == Some(session) || image_paths(ev.drag(cx)).is_empty() {
        return;
    }
    cx.set_global(Spring(Some(session.to_string())));
    let id = session.to_string();
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(SPRING).await;
        let _ = this.update_in(cx, |m, window, cx| {
            if cx.try_global::<Spring>().and_then(|s| s.0.as_deref()) != Some(id.as_str()) {
                return;
            }
            cx.set_global(Spring(None));
            if cx.has_active_drag() && bounds.contains(&window.mouse_position()) && m.selected.as_deref() != Some(id.as_str()) {
                m.select_only(id, window, cx);
            }
        });
    })
    .detach();
}

/// The image paths among dropped files.
pub fn image_paths(paths: &ExternalPaths) -> Vec<PathBuf> {
    paths.paths().iter().filter(|p| looks_like_image(p)).cloned().collect()
}

/// Where an image comes from.
pub enum Source {
    Bytes { name: String, bytes: Vec<u8> },
    Path(PathBuf),
}

/// Images on the clipboard: image data, or image files copied in Finder.
pub fn clipboard_sources(cx: &App) -> Vec<Source> {
    let Some(item) = cx.read_from_clipboard() else {
        return vec![];
    };
    let mut out = vec![];
    for e in item.entries() {
        match e {
            ClipboardEntry::Image(i) if !i.bytes.is_empty() => out.push(Source::Bytes { name: "pasted image".into(), bytes: i.bytes.clone() }),
            ClipboardEntry::ExternalPaths(p) => out.extend(p.0.iter().filter(|p| looks_like_image(p)).cloned().map(Source::Path)),
            _ => {}
        }
    }
    out
}

/// Whether the clipboard holds an image and no text (so ⌘V in a terminal means "add image").
pub fn clipboard_is_image(cx: &App) -> bool {
    let Some(item) = cx.read_from_clipboard() else {
        return false;
    };
    item.text().is_none() && item.entries().iter().any(|e| matches!(e, ClipboardEntry::Image(_)))
}

pub fn looks_like_image(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp")
}

fn images_dir() -> std::io::Result<PathBuf> {
    let d = std::env::temp_dir().join("midna-images");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// FNV-1a, for content-addressed file names.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

fn load(src: Source) -> anyhow::Result<Shot> {
    let (name, bytes) = match src {
        Source::Bytes { name, bytes } => (name, bytes),
        Source::Path(p) => {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
            let bytes = std::fs::read(&p).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
            (name, bytes)
        }
    };
    let dir = images_dir()?;
    let stem = format!("{:016x}", hash(&bytes));
    let kept = match ::image::guess_format(&bytes) {
        Ok(::image::ImageFormat::Png) => Some((ImageFormat::Png, "png")),
        Ok(::image::ImageFormat::Jpeg) => Some((ImageFormat::Jpeg, "jpg")),
        Ok(::image::ImageFormat::Gif) => Some((ImageFormat::Gif, "gif")),
        Ok(::image::ImageFormat::WebP) => Some((ImageFormat::Webp, "webp")),
        _ => None,
    };
    let (format, path, bytes) = match kept {
        Some((f, ext)) => {
            let path = dir.join(format!("{stem}.{ext}"));
            std::fs::write(&path, &bytes)?;
            (f, path, bytes)
        }
        None => {
            // HEIC, TIFF, …: let macOS convert it.
            let src = dir.join(format!("{stem}.src"));
            let path = dir.join(format!("{stem}.png"));
            std::fs::write(&src, &bytes)?;
            let out = std::process::Command::new("/usr/bin/sips").args(["-s", "format", "png"]).arg(&src).arg("--out").arg(&path).output();
            let _ = std::fs::remove_file(&src);
            match out {
                Ok(o) if o.status.success() => {}
                _ => anyhow::bail!("{name} is not an image macOS can read"),
            }
            (ImageFormat::Png, path.clone(), std::fs::read(&path)?)
        }
    };
    let (w, h) = ::image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?.into_dimensions().map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
    Ok(Shot { name, path, image: Arc::new(Image::from_bytes(format, bytes)), w, h, notes: vec![] })
}

/// Drop converted images older than a day (called at startup).
pub fn sweep() {
    let Ok(dir) = images_dir() else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        if e.metadata().and_then(|md| md.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > day) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Dev (`MIDNA_DEBUG_SCREEN=annotate|annotate-tray`): open the sheet on the images in
/// `MIDNA_DEBUG_IMAGES` (comma-separated paths) with the notes in `MIDNA_DEBUG_NOTES`
/// (`;`-separated `x,y,text` pins or `x0,y0,x1,y1,text` areas, fractions, on the first image).
/// `annotate-tray` attaches the draft instead, to show the tray.
pub fn debug(m: &mut MainWindow, screen: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if !open(m, None, window, cx) {
        return;
    }
    let Some(target) = m.annot.read(cx).target.clone() else {
        return;
    };
    let mut shots = vec![];
    for p in crate::dev::var("MIDNA_DEBUG_IMAGES").unwrap_or_default().split(',').filter(|p| !p.is_empty()) {
        match load(Source::Path(PathBuf::from(p))) {
            Ok(s) => shots.push(s),
            Err(e) => eprintln!("midna-app debug-annotate: {e}"),
        }
    }
    if let Some(s) = shots.first_mut() {
        for spec in crate::dev::var("MIDNA_DEBUG_NOTES").unwrap_or_default().split(';').filter(|n| !n.is_empty()) {
            let parts: Vec<&str> = spec.splitn(5, ',').collect();
            let num = |i: usize| parts.get(i).and_then(|v| v.trim().parse::<f32>().ok());
            let note = match (parts.len(), num(0), num(1), num(2), num(3)) {
                (5, Some(x0), Some(y0), Some(x1), Some(y1)) => Note { mark: Mark::area((x0, y0), (x1, y1)), text: parts[4].into() },
                (n, Some(x), Some(y), ..) if n >= 3 => Note { mark: Mark::Pin { x, y }, text: parts[2..].join(",") },
                _ => continue,
            };
            s.notes.push(note);
        }
    }
    let notes = shots.first().map(|s| s.notes.len()).unwrap_or(0);
    cx.update_global::<Drafts, _>(|d, _| d.0.entry(target).or_default().shots.extend(shots));
    m.annot.update(cx, |v, cx| {
        if screen == "annotate-tray" {
            v.attach(window, cx);
        } else if notes > 0 {
            v.sel = Some(notes - 1);
        }
        cx.notify();
    });
}

#[cfg(test)]
mod tests {
    use super::{ClientMsg, Mark, Note, Outgoing, Source, fit_scale, load, notes_text};
    use ::core::prelude::v1::test;
    use std::path::PathBuf;

    fn note(mark: Mark, text: &str) -> Note {
        Note { mark, text: text.into() }
    }

    #[test]
    fn labels_are_rounded_percentages() {
        assert_eq!(Mark::Pin { x: 0.123, y: 0.149 }.label(), "[x=12, y=15]");
        assert_eq!(Mark::area((0.54, 0.78), (0.34, 0.69)).label(), "[x=34-54, y=69-78]");
        assert_eq!(Mark::Pin { x: -0.1, y: 1.4 }.label(), "[x=0, y=100]");
    }

    #[test]
    fn one_image_uses_saggars_wording_and_skips_empty_notes() {
        let notes = vec![note(Mark::Pin { x: 0.12, y: 0.15 }, " This is an annotation "), note(Mark::Pin { x: 0.5, y: 0.5 }, "  "), note(Mark::Pin { x: 0.82, y: 0.46 }, "And that")];
        assert_eq!(
            notes_text(&[("a.png", &notes)]),
            "Annotations (coordinates are percentages from the image's top-left):\n1. [x=12, y=15] This is an annotation\n2. [x=82, y=46] And that"
        );
    }

    #[test]
    fn several_images_are_numbered_in_paste_order() {
        let a = vec![note(Mark::Pin { x: 0.06, y: 0.06 }, "Logo")];
        let b: Vec<Note> = vec![];
        let c = vec![note(Mark::area((0.1, 0.2), (0.3, 0.4)), "Warning")];
        let t = notes_text(&[("signin.png", &a), ("empty.png", &b), ("log.png", &c)]);
        assert!(t.starts_with("Annotations (coordinates are percentages from each image's top-left"));
        assert!(t.contains("\n\nImage 1 (signin.png):\n1. [x=6, y=6] Logo"));
        assert!(!t.contains("Image 2"));
        assert!(t.contains("\n\nImage 3 (log.png):\n1. [x=10-30, y=20-40] Warning"));
    }

    #[test]
    fn no_written_notes_means_no_text() {
        let a = vec![note(Mark::Pin { x: 0.1, y: 0.1 }, "")];
        assert_eq!(notes_text(&[("a.png", &a)]), "");
        let o = Outgoing { paths: vec![PathBuf::from("/t/a.png")], text: String::new(), notes: 0, thumbs: vec![] };
        assert_eq!(o.steps(), vec![ClientMsg::Paste("/t/a.png".into())]);
    }

    #[test]
    fn steps_are_paths_then_a_newline_then_notes() {
        let o = Outgoing { paths: vec![PathBuf::from("/t/a.png"), PathBuf::from("/t/b.png")], text: "Annotations".into(), notes: 1, thumbs: vec![] };
        let want = vec![
            ClientMsg::Paste("/t/a.png".into()),
            ClientMsg::Paste("/t/b.png".into()),
            ClientMsg::Input(b"\n".to_vec()),
            ClientMsg::Paste("Annotations".into()),
        ];
        assert_eq!(o.steps(), want);
    }

    #[test]
    fn fit_never_upscales_past_two() {
        assert_eq!(fit_scale(100, 50, 1000., 1000.), 2.);
        assert!((fit_scale(2880, 1800, 576., 360.) - 0.2).abs() < 1e-6);
        assert_eq!(fit_scale(0, 10, 100., 100.), 1.);
    }

    #[test]
    fn converts_and_measures_images() {
        // A 3x2 PNG made by the image crate.
        let mut png = vec![];
        ::image::RgbImage::new(3, 2).write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png).unwrap();
        let s = load(Source::Bytes { name: "x".into(), bytes: png }).unwrap();
        assert_eq!((s.w, s.h), (3, 2));
        assert!(s.path.exists() && s.path.extension().unwrap() == "png");
        assert!(load(Source::Bytes { name: "junk".into(), bytes: b"not an image".to_vec() }).is_err());
    }
}
