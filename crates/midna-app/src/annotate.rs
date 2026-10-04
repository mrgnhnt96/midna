//! Image annotations: attach screenshots with numbered notes to an agent's next message
//! (design: the "D · Sheet + image strip" and "Added, not sent yet" boards).
//!
//! ⌘I (`keys.add_image`), the header's image button, or ⌘V of a bare image in a terminal opens
//! the sheet for the selected terminal. Each terminal has one draft: a list of images, each
//! with notes that are a pin (click) or an area (drag), stored as fractions of the image so
//! they survive zooming. Closing the sheet keeps the draft; "Add to chat" (⌘↩) attaches it.
//!
//! Nothing is typed into the terminal when a draft is attached. It sits in the [`Outbox`]
//! (an app global, so a split or pop-out of the same terminal sees it too) and the tray under
//! the terminal offers Edit (⌘E) and Remove. The next plain ↩ in that terminal delivers it:
//! each image's path as its own paste (Claude Code turns a pasted image path into
//! `[Image #N]`), then the notes as one paste, then the ↩ itself.
//!
//! Images are normalized into `$TMPDIR/midna-images/`: PNG, JPEG, GIF and WebP are kept as
//! they are; anything else (HEIC, TIFF, …) is converted to PNG with `sips`.
use crate::app::{MainWindow, Overlay};
use crate::ui::text_input::{FieldChanged, TextField};
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

actions!(
    midna,
    [
        /// Open the image sheet for the selected terminal (`keys.add_image`).
        AddImage,
        /// Open the sheet and add the image on the clipboard (⌘V of a bare image in a terminal).
        PasteImage,
        /// ⌘E: reopen the sheet for the selected terminal's attached draft.
        EditAttachment,
    ]
);

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

    /// The pastes, in order, that deliver this (the ↩ follows them).
    pub fn pastes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.paths.iter().map(|p| p.display().to_string()).collect();
        if !self.text.is_empty() {
            v.push(format!("\n{}", self.text));
        }
        v
    }
}

/// Attached drafts by terminal id, waiting for that terminal's next ↩.
#[derive(Default)]
pub struct Outbox(pub HashMap<String, Outgoing>);

impl Global for Outbox {}

/// Take the attachment for `session`, if any (the terminal is about to send it).
pub fn take(session: &str, cx: &mut App) -> Option<Outgoing> {
    if !cx.has_global::<Outbox>() || !cx.global::<Outbox>().0.contains_key(session) {
        return None;
    }
    cx.update_global::<Outbox, _>(|o, _| o.0.remove(session))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pin,
    Area,
}

/// The sheet's state, kept on the main window.
pub struct Annotator {
    pub focus: FocusHandle,
    /// The note being edited (one shared field).
    pub field: Entity<TextField>,
    pub drafts: HashMap<String, Draft>,
    /// The terminal the open sheet is for.
    pub target: Option<String>,
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

impl Annotator {
    pub fn new(cx: &mut Context<MainWindow>) -> Annotator {
        let field = cx.new(|cx| TextField::new(cx, false, "Add a note…"));
        cx.subscribe(&field, |m, _, _: &FieldChanged, cx| field_changed(m, cx)).detach();
        cx.set_global(Outbox::default());
        // A terminal delivered an attachment: its draft is done.
        cx.observe_global::<Outbox>(|m, cx| {
            let out = cx.global::<Outbox>();
            let before = m.annot.drafts.len();
            m.annot.drafts.retain(|id, d| !d.attached || out.0.contains_key(id));
            if m.annot.drafts.len() != before {
                cx.notify();
            }
        })
        .detach();
        Annotator {
            focus: cx.focus_handle(),
            field,
            drafts: HashMap::new(),
            target: None,
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

    pub fn draft(&self) -> Option<&Draft> {
        self.drafts.get(self.target.as_ref()?)
    }

    fn draft_mut(&mut self) -> Option<&mut Draft> {
        self.drafts.get_mut(self.target.as_ref()?)
    }

    pub fn shot(&self) -> Option<&Shot> {
        self.draft()?.shots.get(self.cur)
    }

    fn shot_mut(&mut self) -> Option<&mut Shot> {
        let cur = self.cur;
        self.draft_mut()?.shots.get_mut(cur)
    }
}

// ------------------------------------------------------------------ opening and closing

pub fn open(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(id) = m.selected.clone() else {
        m.toast("Select a terminal to add an image to.", cx);
        return;
    };
    let a = &mut m.annot;
    if a.target.as_deref() != Some(&id) {
        a.cur = 0;
    }
    a.target = Some(id.clone());
    let n = a.drafts.entry(id).or_default().shots.len();
    a.cur = a.cur.min(n.saturating_sub(1));
    a.sel = None;
    a.editing = None;
    a.drag = None;
    a.zoom = None;
    if m.overlay != Overlay::Annotate {
        m.set_overlay(Overlay::Annotate, window, cx);
    }
}

pub fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    commit(m, window, cx);
    if m.overlay == Overlay::Annotate {
        m.set_overlay(Overlay::None, window, cx);
    }
}

/// ⌘↩: attach the draft to the terminal's next message.
pub fn attach(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    commit(m, window, cx);
    let Some(d) = m.annot.draft_mut() else {
        return;
    };
    if d.shots.is_empty() {
        m.toast("Add an image first.", cx);
        return;
    }
    d.attached = true;
    sync(m, cx);
    close(m, window, cx);
}

/// The tray's Remove: drop the attachment and its draft.
pub fn remove(m: &mut MainWindow, id: &str, cx: &mut Context<MainWindow>) {
    m.annot.drafts.remove(id);
    cx.update_global::<Outbox, _>(|o, _| o.0.remove(id));
    cx.notify();
}

/// Keep the outbox in step with an attached draft after an edit.
fn sync(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(id) = m.annot.target.clone() else {
        return;
    };
    let Some(d) = m.annot.drafts.get_mut(&id) else {
        return;
    };
    if !d.attached {
        return;
    }
    if d.shots.is_empty() {
        d.attached = false;
        cx.update_global::<Outbox, _>(|o, _| o.0.remove(&id));
        return;
    }
    let out = Outgoing::of(d);
    cx.update_global::<Outbox, _>(|o, _| o.0.insert(id, out));
}

// ------------------------------------------------------------------ notes

fn field_changed(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(i) = m.annot.editing else {
        return;
    };
    let text = m.annot.field.read(cx).text().to_string();
    if let Some(n) = m.annot.shot_mut().and_then(|s| s.notes.get_mut(i)) {
        n.text = text;
    }
    sync(m, cx);
    cx.notify();
}

/// Finish editing: an empty note is removed. Focus goes back to the sheet.
pub fn commit(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(i) = m.annot.editing.take() else {
        return;
    };
    let empty = m.annot.shot().and_then(|s| s.notes.get(i)).is_some_and(|n| n.text.trim().is_empty());
    if empty {
        remove_note(m, i, cx);
    }
    m.annot.focus.focus(window, cx);
    cx.notify();
}

pub fn edit(m: &mut MainWindow, i: usize, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.annot.editing == Some(i) {
        return;
    }
    commit(m, window, cx);
    let Some(text) = m.annot.shot().and_then(|s| s.notes.get(i)).map(|n| n.text.clone()) else {
        return;
    };
    m.annot.sel = Some(i);
    // Set the text before `editing`, so the change doesn't write back into the note.
    m.annot.field.update(cx, |f, cx| f.set_text(&text, cx));
    m.annot.editing = Some(i);
    let fh = m.annot.field.read(cx).focus.clone();
    fh.focus(window, cx);
    cx.notify();
}

pub fn remove_note(m: &mut MainWindow, i: usize, cx: &mut Context<MainWindow>) {
    let Some(s) = m.annot.shot_mut() else {
        return;
    };
    if i < s.notes.len() {
        s.notes.remove(i);
    }
    let a = &mut m.annot;
    a.sel = a.sel.filter(|&s| s != i).map(|s| if s > i { s - 1 } else { s });
    a.editing = a.editing.filter(|&e| e != i).map(|e| if e > i { e - 1 } else { e });
    sync(m, cx);
    cx.notify();
}

fn add_note(m: &mut MainWindow, mark: Mark, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(s) = m.annot.shot_mut() else {
        return;
    };
    s.notes.push(Note { mark, text: String::new() });
    let i = s.notes.len() - 1;
    edit(m, i, window, cx);
}

// ------------------------------------------------------------------ pointer on the image

fn frac(m: &MainWindow, p: Point<Pixels>) -> Option<(f32, f32)> {
    let b = m.annot.stage.get()?;
    let x = f32::from(p.x - b.origin.x) / f32::from(b.size.width);
    let y = f32::from(p.y - b.origin.y) / f32::from(b.size.height);
    Some((x.clamp(0., 1.), y.clamp(0., 1.)))
}

pub fn pointer_down(m: &mut MainWindow, p: Point<Pixels>, window: &mut Window, cx: &mut Context<MainWindow>) {
    commit(m, window, cx);
    if let Some(f) = frac(m, p) {
        m.annot.drag = Some((f, f));
        m.annot.sel = None;
        cx.notify();
    }
}

pub fn pointer_move(m: &mut MainWindow, p: Point<Pixels>, cx: &mut Context<MainWindow>) {
    if m.annot.drag.is_none() {
        return;
    }
    if let (Some(f), Some(d)) = (frac(m, p), m.annot.drag.as_mut()) {
        d.1 = f;
        cx.notify();
    }
}

/// A drag of at least this many screen pixels draws an area; less is a click.
const DRAG_MIN: f32 = 5.;

/// Whether the drag in progress is long enough to be an area.
pub fn drag_is_area(m: &MainWindow) -> bool {
    let (Some((a, b)), Some(st)) = (m.annot.drag, m.annot.stage.get()) else {
        return false;
    };
    let dx = (b.0 - a.0) * f32::from(st.size.width);
    let dy = (b.1 - a.1) * f32::from(st.size.height);
    dx.hypot(dy) >= DRAG_MIN
}

pub fn pointer_up(m: &mut MainWindow, p: Point<Pixels>, window: &mut Window, cx: &mut Context<MainWindow>) {
    pointer_move(m, p, cx);
    let area = drag_is_area(m);
    let Some((a, b)) = m.annot.drag.take() else {
        return;
    };
    if area {
        add_note(m, Mark::area(a, b), window, cx);
    } else if m.annot.tool == Tool::Pin {
        add_note(m, Mark::Pin { x: a.0, y: a.1 }, window, cx);
    }
    cx.notify();
}

// ------------------------------------------------------------------ images

pub fn select_image(m: &mut MainWindow, i: usize, window: &mut Window, cx: &mut Context<MainWindow>) {
    commit(m, window, cx);
    let n = m.annot.draft().map(|d| d.shots.len()).unwrap_or(0);
    if i < n {
        m.annot.cur = i;
        m.annot.sel = None;
        m.annot.zoom = None;
    }
    cx.notify();
}

pub fn remove_image(m: &mut MainWindow, i: usize, window: &mut Window, cx: &mut Context<MainWindow>) {
    commit(m, window, cx);
    let Some(d) = m.annot.draft_mut() else {
        return;
    };
    if i < d.shots.len() {
        d.shots.remove(i);
    }
    let n = d.shots.len();
    m.annot.cur = m.annot.cur.min(n.saturating_sub(1));
    m.annot.sel = None;
    sync(m, cx);
    cx.notify();
}

pub fn zoom(m: &mut MainWindow, by: Option<f32>, cx: &mut Context<MainWindow>) {
    m.annot.zoom = by.map(|f| (current_scale(m) * f).clamp(0.05, 8.));
    cx.notify();
}

/// Screen px per image px: the zoom, or the fit into the viewport (at most 2×).
pub fn current_scale(m: &MainWindow) -> f32 {
    if let Some(z) = m.annot.zoom {
        return z;
    }
    let Some(s) = m.annot.shot() else {
        return 1.;
    };
    let (vw, vh) = m.annot.viewport.get().map(|b| (f32::from(b.size.width), f32::from(b.size.height))).unwrap_or((640., 400.));
    fit_scale(s.w, s.h, vw - 32., vh - 32.)
}

pub fn fit_scale(w: u32, h: u32, vw: f32, vh: f32) -> f32 {
    if w == 0 || h == 0 {
        return 1.;
    }
    (vw / w as f32).min(vh / h as f32).clamp(0.02, 2.)
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

fn looks_like_image(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp")
}

/// Read, convert and add images to the open draft (off the UI thread).
pub fn add(m: &mut MainWindow, sources: Vec<Source>, window: &mut Window, cx: &mut Context<MainWindow>) {
    if sources.is_empty() {
        return;
    }
    let Some(target) = m.annot.target.clone() else {
        return;
    };
    m.annot.loading += sources.len();
    cx.notify();
    cx.spawn_in(window, async move |this, cx| {
        let results = cx.background_executor().spawn(async move { sources.into_iter().map(load).collect::<Vec<_>>() }).await;
        let _ = this.update_in(cx, |m, window, cx| {
            m.annot.loading = m.annot.loading.saturating_sub(results.len());
            let mut errors = vec![];
            let mut first = None;
            for r in results {
                match r {
                    Ok(s) => {
                        let d = m.annot.drafts.entry(target.clone()).or_default();
                        d.shots.push(s);
                        first.get_or_insert(d.shots.len() - 1);
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
            if let Some(i) = first.filter(|_| m.annot.target.as_deref() == Some(&target)) {
                select_image(m, i, window, cx);
                sync(m, cx);
            }
            if !errors.is_empty() {
                m.toast(format!("Couldn't add image: {}", errors.join("; ")), cx);
            }
            cx.notify();
        });
    })
    .detach();
}

pub fn pick_files(_m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
    cx.spawn_in(window, async move |this, cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let _ = this.update_in(cx, |m, window, cx| add(m, paths.into_iter().map(Source::Path).collect(), window, cx));
    })
    .detach();
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
    open(m, window, cx);
    let Some(target) = m.annot.target.clone() else {
        return;
    };
    let paths = crate::dev::var("MIDNA_DEBUG_IMAGES").unwrap_or_default();
    let d = m.annot.drafts.entry(target).or_default();
    for p in paths.split(',').filter(|p| !p.is_empty()) {
        match load(Source::Path(PathBuf::from(p))) {
            Ok(s) => d.shots.push(s),
            Err(e) => eprintln!("midna-app debug-annotate: {e}"),
        }
    }
    if let Some(s) = d.shots.first_mut() {
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
    if screen == "annotate-tray" {
        attach(m, window, cx);
    } else if let Some(n) = m.annot.shot().map(|s| s.notes.len()).filter(|&n| n > 0) {
        m.annot.sel = Some(n - 1);
    }
    cx.notify();
}

// ------------------------------------------------------------------ keys

/// Keys the sheet handles (those the note field didn't take).
pub fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let ks = &ev.keystroke;
    let md = &ks.modifiers;
    let in_field = m.annot.field.read(cx).focus.is_focused(window);
    let n_images = m.annot.draft().map(|d| d.shots.len()).unwrap_or(0);
    let handled = if md.platform {
        match ks.key.as_str() {
            "=" | "+" => {
                zoom(m, Some(1.25), cx);
                true
            }
            "-" => {
                zoom(m, Some(0.8), cx);
                true
            }
            "0" => {
                zoom(m, None, cx);
                true
            }
            "up" if m.annot.cur > 0 => {
                let i = m.annot.cur - 1;
                select_image(m, i, window, cx);
                true
            }
            "down" if m.annot.cur + 1 < n_images => {
                let i = m.annot.cur + 1;
                select_image(m, i, window, cx);
                true
            }
            "v" if !in_field => {
                let s = clipboard_sources(cx);
                if s.is_empty() {
                    m.toast("No image on the clipboard.", cx);
                }
                add(m, s, window, cx);
                true
            }
            _ => false,
        }
    } else if in_field {
        if ks.key == "enter" {
            commit(m, window, cx);
            true
        } else {
            false
        }
    } else if md.control || md.alt {
        false
    } else {
        match ks.key.as_str() {
            "p" => {
                m.annot.tool = Tool::Pin;
                true
            }
            "b" => {
                m.annot.tool = Tool::Area;
                true
            }
            "enter" => match m.annot.sel {
                Some(i) => {
                    edit(m, i, window, cx);
                    true
                }
                None => false,
            },
            "backspace" | "delete" => match m.annot.sel {
                Some(i) => {
                    remove_note(m, i, cx);
                    true
                }
                None => false,
            },
            "up" | "down" => {
                let n = m.annot.shot().map(|s| s.notes.len()).unwrap_or(0);
                if n > 0 {
                    m.annot.sel = Some(match (m.annot.sel, ks.key.as_str()) {
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

#[cfg(test)]
mod tests {
    use super::{Mark, Note, Outgoing, Source, fit_scale, load, notes_text};
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
        assert_eq!(o.pastes(), vec!["/t/a.png".to_string()]);
    }

    #[test]
    fn pastes_are_paths_then_notes() {
        let o = Outgoing { paths: vec![PathBuf::from("/t/a.png"), PathBuf::from("/t/b.png")], text: "Annotations".into(), notes: 1, thumbs: vec![] };
        assert_eq!(o.pastes(), vec!["/t/a.png".to_string(), "/t/b.png".into(), "\nAnnotations".into()]);
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
