//! Arranging Insights in place (canvas "Midna Insight Widgets", concept D): saved layouts
//! (`insights.layouts`, `insights.layout`), a grid that packs Small (1×1), Wide (2×1) and Large
//! (2×2) cards, hover controls (⋮⋮ to move, ✕ to remove, the corner to resize), the real card
//! following the pointer with a slot where it lands, dots and column guides only while moving
//! or resizing, the Layout menu and the Add gallery. Every change saves at once; ⌘Z undoes.
use super::widgets::{self, Size, COL_MIN, GAP, WIDGET_H};
use super::{InsightsView, card};
use crate::theme::Theme;
use crate::ui::charts;
use crate::ui::text_input::{FieldChanged, TextField};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::settings::{DEFAULT_INSIGHTS_LAYOUTS, format_insights_layout, parse_insights_layout};
use serde_json::{Value, json};
use std::cell::Cell;
use std::rc::Rc;

const DEFAULT_LAYOUT: &str = "My layout";

pub type Items = Vec<(String, Size)>;

enum Arrange {
    None,
    /// Moving `id`: `grab` is where the pointer holds the card, `order` the live preview.
    Drag { id: String, grab: Point<Pixels>, at: Point<Pixels>, order: Items, last: Option<String> },
    Resize { id: String, from: Size, order: Items },
}

pub struct Gallery {
    query: Entity<TextField>,
    group: Option<&'static str>,
    pick: Option<&'static str>,
    size: Size,
    at_top: bool,
}

pub struct State {
    arrange: Arrange,
    menu: bool,
    renaming: Option<Entity<TextField>>,
    gallery: Option<Gallery>,
    /// (layout name, its value) before each change.
    undo: Vec<(String, String)>,
    /// Top-left of the grid in window coordinates, measured as it paints.
    origin: Rc<Cell<Point<Pixels>>>,
    focus: FocusHandle,
    /// dev (screenshots): `MIDNA_INSIGHTS_WIDGETS=heatmap:large,latency` shows these instead.
    dev_items: Option<Items>,
}

impl State {
    pub fn from_env(cx: &mut Context<InsightsView>) -> State {
        let dev_items = crate::dev::var("MIDNA_INSIGHTS_WIDGETS").ok().map(|v| parse(&v.replace(',', " ")));
        let mut st = State { arrange: Arrange::None, menu: false, renaming: None, gallery: None, undo: vec![], origin: Rc::new(Cell::new(point(px(0.), px(0.)))), focus: cx.focus_handle(), dev_items };
        // dev (screenshots): menu | gallery | drag (the first widget, held mid-move) | resize (the first, to large)
        let first = st.dev_items.as_ref().and_then(|d| d.first().cloned());
        match crate::dev::var("MIDNA_INSIGHTS_ARRANGE").as_deref() {
            Ok("menu") => st.menu = true,
            Ok("gallery") => st.gallery = Some(new_gallery(cx, Some("parallelism"))),
            Ok("drag") if let (Some((id, _)), Some(d)) = (&first, &st.dev_items) => {
                let mut order = d.clone();
                let it = order.remove(0);
                order.insert(3.min(order.len()), it);
                st.arrange = Arrange::Drag { id: id.clone(), grab: point(px(60.), px(24.)), at: point(px(1050.), px(640.)), order, last: None }
            }
            Ok("resize") if let (Some((id, from)), Some(d)) = (&first, &st.dev_items) => {
                let mut order = d.clone();
                order[0].1 = Size::Large;
                st.arrange = Arrange::Resize { id: id.clone(), from: *from, order }
            }
            _ => {}
        }
        st
    }
}

fn parse(v: &str) -> Items {
    parse_insights_layout(v).into_iter().map(|(id, s)| (id, Size::parse(&s))).collect()
}

fn format(items: &Items) -> String {
    format_insights_layout(&items.iter().map(|(id, s)| (id.clone(), s.as_str().to_string())).collect::<Vec<_>>())
}

fn new_gallery(cx: &mut Context<InsightsView>, pick: Option<&'static str>) -> Gallery {
    let query = cx.new(|cx| TextField::new(cx, false, "Search: cost, waiting, tests…"));
    cx.subscribe(&query, |_, _, _: &FieldChanged, cx| cx.notify()).detach();
    Gallery { query, group: None, pick, size: pick.map(widgets::usual).unwrap_or(Size::Wide), at_top: true }
}

/// Where a card sits: (index into items, col, row, cols wide, rows tall).
pub type Placed = (usize, u16, u16, u16, u16);

pub fn pack(items: &Items, cols: u16) -> (Vec<Placed>, u16) {
    let mut taken: Vec<Vec<bool>> = vec![];
    let mut out = vec![];
    let mut rows = 0;
    for (i, (_, size)) in items.iter().enumerate() {
        let (w, h) = size.cells(cols);
        let mut r = 0usize;
        'find: loop {
            while taken.len() < r + h as usize {
                taken.push(vec![false; cols as usize]);
            }
            for c in 0..=(cols - w) as usize {
                let free = (r..r + h as usize).all(|rr| (c..c + w as usize).all(|cc| !taken[rr][cc]));
                if free {
                    for row in taken.iter_mut().skip(r).take(h as usize) {
                        for cell in row.iter_mut().skip(c).take(w as usize) {
                            *cell = true;
                        }
                    }
                    out.push((i, c as u16, r as u16, w, h));
                    rows = rows.max(r as u16 + h);
                    break 'find;
                }
            }
            r += 1;
        }
    }
    (out, rows)
}

impl InsightsView {
    // -------------------------------------------------------------- layouts

    fn setting(&self, key: &str, cx: &App) -> Option<Value> {
        self.main.upgrade().and_then(|m| m.read(cx).settings.get(key).cloned())
    }

    /// (name, value) of every saved layout.
    fn layouts(&self, cx: &App) -> Vec<(String, String)> {
        let rules: Vec<String> = self.setting("insights.layouts", cx).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| DEFAULT_INSIGHTS_LAYOUTS.iter().map(|s| s.to_string()).collect());
        rules.iter().filter_map(|r| r.split_once('=').map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))).collect()
    }

    fn current_name(&self, cx: &App) -> String {
        let all = self.layouts(cx);
        let want = self.setting("insights.layout", cx).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| DEFAULT_LAYOUT.into());
        if all.iter().any(|(n, _)| *n == want) { want } else { all.first().map(|(n, _)| n.clone()).unwrap_or(want) }
    }

    /// The widgets on screen, in order (the live preview while moving or resizing).
    pub(super) fn items(&self, cx: &App) -> Items {
        match &self.ar.arrange {
            Arrange::Drag { order, .. } | Arrange::Resize { order, .. } => return order.clone(),
            Arrange::None => {}
        }
        if let Some(d) = &self.ar.dev_items {
            return d.clone();
        }
        let name = self.current_name(cx);
        self.layouts(cx).into_iter().find(|(n, _)| *n == name).map(|(_, v)| parse(&v)).unwrap_or_default()
    }

    fn set(&self, key: &'static str, value: Value, cx: &mut Context<Self>) {
        if let Some(m) = self.main.upgrade() {
            m.update(cx, |m, cx| {
                m.settings.insert(key.into(), value.clone());
                m.rpc("settings.set", json!({"key": key, "value": value}), cx, |_, _, _, _| {});
                cx.notify();
            });
        }
        cx.notify();
    }

    fn write_layouts(&self, all: &[(String, String)], cx: &mut Context<Self>) {
        let rules: Vec<String> = all.iter().map(|(n, v)| format!("{n} = {v}")).collect();
        self.set("insights.layouts", json!(rules), cx);
    }

    /// Save `items` as the current layout (undoable).
    fn save_items(&mut self, items: &Items, cx: &mut Context<Self>) {
        if self.ar.dev_items.is_some() {
            self.ar.dev_items = Some(items.clone());
            cx.notify();
            return;
        }
        let name = self.current_name(cx);
        let mut all = self.layouts(cx);
        let value = format(items);
        match all.iter_mut().find(|(n, _)| *n == name) {
            Some(l) if l.1 == value => return,
            Some(l) => {
                self.ar.undo.push((name.clone(), l.1.clone()));
                l.1 = value;
            }
            None => all.push((name, value)),
        }
        self.write_layouts(&all, cx);
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        let Some((name, value)) = self.ar.undo.pop() else { return };
        let mut all = self.layouts(cx);
        if let Some(l) = all.iter_mut().find(|(n, _)| *n == name) {
            l.1 = value;
        }
        self.write_layouts(&all, cx);
        self.set("insights.layout", json!(name), cx);
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        let items: Items = self.items(cx).into_iter().filter(|(i, _)| i != id).collect();
        self.save_items(&items, cx);
    }

    // -------------------------------------------------------------- grid

    fn cols(width: f32) -> u16 {
        // 1, 2 or 4 columns, so wide cards always pair up
        if width >= 4. * COL_MIN + 3. * GAP {
            4
        } else if width >= 2. * COL_MIN + GAP {
            2
        } else {
            1
        }
    }

    fn cell_w(width: f32, cols: u16) -> f32 {
        (width - GAP * (cols as f32 - 1.)) / cols as f32
    }

    fn rect(width: f32, cols: u16, c: u16, r: u16, w: u16, h: u16) -> Bounds<Pixels> {
        let cw = Self::cell_w(width, cols);
        Bounds::new(
            point(px(c as f32 * (cw + GAP)), px(r as f32 * (WIDGET_H + GAP))),
            size(px(w as f32 * cw + (w as f32 - 1.) * GAP), px(h as f32 * WIDGET_H + (h as f32 - 1.) * GAP)),
        )
    }

    /// A fixed-height box of absolutely placed cards; `flex_none` so the scroll column
    /// doesn't shrink it to the window (which stopped the page scrolling).
    pub(super) fn grid(&mut self, t: &Theme, width: f32, cx: &mut Context<Self>) -> Div {
        let items = self.items(cx);
        let settings = self.main.upgrade().map(|m| m.read(cx).settings.clone()).unwrap_or_default();
        let pal = widgets::palette(t, &settings);
        let cols = Self::cols(width);
        let (placed, rows) = pack(&items, cols);
        let height = rows as f32 * (WIDGET_H + GAP) - GAP;
        let moving = match &self.ar.arrange {
            Arrange::Drag { id, .. } => Some(id.clone()),
            _ => None,
        };
        let resizing = match &self.ar.arrange {
            Arrange::Resize { id, from, .. } => Some((id.clone(), *from)),
            _ => None,
        };
        let origin = self.ar.origin.clone();
        let mut grid = div().relative().flex_none().w_full().h(px(height.max(WIDGET_H))).child(
            canvas(move |b, _, _| origin.set(b.origin), |_, _, _, _| {}).absolute().top_0().left_0().size_full(),
        );
        if moving.is_some() || resizing.is_some() {
            grid = grid.child(guides(t, width, cols, height));
        }
        for (i, c, r, w, h) in placed {
            let (id, size) = items[i].clone();
            let b = Self::rect(width, cols, c, r, w, h);
            let spot = div().absolute().left(b.origin.x).top(b.origin.y).w(b.size.width).h(b.size.height);
            if moving.as_deref() == Some(id.as_str()) {
                grid = grid.child(spot.rounded(px(12.)).border_2().border_dashed().border_color(t.accent).bg(t.accent_soft));
                continue;
            }
            let label = resizing.as_ref().filter(|(rid, _)| *rid == id).map(|(_, from)| (*from, size));
            grid = grid.child(spot.child(self.card(&id, size, t, &pal, label, cx)));
        }
        // the real card under the pointer
        if let Arrange::Drag { id, grab, at, order, .. } = &self.ar.arrange
            && let Some((_, size)) = order.iter().find(|(i, _)| i == id)
        {
            let (w, h) = size.cells(cols);
            let b = Self::rect(width, cols, 0, 0, w, h);
            let o = self.ar.origin.get();
            let body = self.widget(id, *size, t, &pal, cx);
            grid = grid.child(
                card(t)
                    .absolute()
                    .left(at.x - o.x - grab.x)
                    .top(at.y - o.y - grab.y)
                    .w(b.size.width)
                    .h(b.size.height)
                    .overflow_hidden()
                    .border_color(t.accent)
                    .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.55), offset: point(px(0.), px(22.)), blur_radius: px(50.), spread_radius: px(0.), inset: false }])
                    .opacity(0.96)
                    .child(body),
            );
        }
        grid
    }

    /// One card: the widget, and on hover ⋮⋮ (move), ✕ (remove) and the resize corner.
    fn card(&self, id: &str, size: Size, t: &Theme, pal: &widgets::Palette, resize_label: Option<(Size, Size)>, cx: &mut Context<Self>) -> Stateful<Div> {
        let group = SharedString::from(format!("card-{id}"));
        let body = self.widget(id, size, t, pal, cx);
        let title = widgets::spec(id).map(|s| s.title).unwrap_or("");
        let (mid, rid, did) = (id.to_string(), id.to_string(), id.to_string());
        let can_resize = widgets::sizes(id).len() > 1;
        let tool = |name: &str| {
            div().id(SharedString::from(format!("{name}-{id}"))).size(px(26.)).flex().items_center().justify_center().rounded(px(6.)).text_color(t.dim).cursor_pointer().hover(|s| s.bg(t.panel).text_color(t.fg))
        };
        let toolbar = div()
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .flex()
            .gap(px(2.))
            .p(px(3.))
            .rounded(px(8.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .opacity(0.)
            .group_hover(group.clone(), |s| s.opacity(1.))
            .child(
                tool("move")
                    .cursor_grab()
                    .tooltip(crate::ui::header::tip(format!("Drag to move {title}")))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        v.start_drag(&mid, ev.position, cx);
                    }))
                    .child("⋮⋮"),
            )
            .child(tool("remove").tooltip(crate::ui::header::tip(format!("Remove {title}"))).on_click(cx.listener(move |v, _, _, cx| v.remove(&did, cx))).child("✕"));
        let corner = can_resize.then(|| {
            div()
                .id(SharedString::from(format!("resize-{id}")))
                .absolute()
                .right(px(3.))
                .bottom(px(3.))
                .size(px(16.))
                .border_r_2()
                .border_b_2()
                .border_color(if resize_label.is_some() { t.accent } else { t.dim })
                .rounded_br(px(8.))
                .cursor(CursorStyle::ResizeUpLeftDownRight)
                .opacity(if resize_label.is_some() { 1. } else { 0. })
                .group_hover(group.clone(), |s| s.opacity(1.))
                .tooltip(crate::ui::header::tip("Drag to resize"))
                .on_mouse_down(MouseButton::Left, cx.listener(move |v, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    v.start_resize(&rid, cx);
                }))
        });
        let chip = resize_label.filter(|(a, b)| a != b).map(|(from, to)| {
            div().absolute().right(px(10.)).bottom(px(-30.)).px(px(9.)).py(px(3.)).rounded(px(6.)).bg(t.accent).text_color(t.accent_fg).text_size(px(12.)).font_weight(FontWeight::BOLD).whitespace_nowrap().child(format!("{} → {}", from.label(), to.label()))
        });
        card(t)
            .id(SharedString::from(format!("widget-{id}")))
            .group(group)
            .relative()
            .size_full()
            .when(resize_label.is_some(), |c| c.border_2().border_color(t.accent))
            .child(div().size_full().overflow_hidden().flex().flex_col().child(body))
            .child(toolbar)
            .children(corner)
            .children(chip)
    }

    // -------------------------------------------------------------- moving and resizing

    fn start_drag(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        let items = self.items(cx);
        let width = self.grid_w.get();
        let cols = Self::cols(width);
        let (placed, _) = pack(&items, cols);
        let Some(&(_, c, r, w, h)) = placed.iter().find(|p| items[p.0].0 == id) else { return };
        let b = Self::rect(width, cols, c, r, w, h);
        let o = self.ar.origin.get();
        let grab = point(at.x - o.x - b.origin.x, at.y - o.y - b.origin.y);
        self.ar.arrange = Arrange::Drag { id: id.to_string(), grab, at, order: items, last: None };
        cx.notify();
    }

    fn start_resize(&mut self, id: &str, cx: &mut Context<Self>) {
        let items = self.items(cx);
        let Some((_, from)) = items.iter().find(|(i, _)| i == id).cloned() else { return };
        self.ar.arrange = Arrange::Resize { id: id.to_string(), from, order: items };
        cx.notify();
    }

    fn pointer_moved(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let width = self.grid_w.get();
        let cols = Self::cols(width);
        let o = self.ar.origin.get();
        let local = point(at.x - o.x, at.y - o.y);
        match &mut self.ar.arrange {
            Arrange::None => {}
            Arrange::Drag { id, at: here, order, last, .. } => {
                *here = at;
                let (placed, _) = pack(order, cols);
                let over = placed.iter().find(|&&(i, c, r, w, h)| order[i].0 != *id && Self::rect(width, cols, c, r, w, h).contains(&local)).map(|p| order[p.0].0.clone());
                match over {
                    Some(target) if last.as_deref() != Some(target.as_str()) => {
                        let from = order.iter().position(|(i, _)| i == id).unwrap_or(0);
                        let to = order.iter().position(|(i, _)| *i == target).unwrap_or(from);
                        let item = order.remove(from);
                        order.insert(to, item);
                        *last = Some(target);
                    }
                    Some(_) => {}
                    None => *last = None,
                }
                cx.notify();
            }
            Arrange::Resize { id, from, order } => {
                let (placed, _) = pack(order, cols);
                let Some(&(_, c, r, _, _)) = placed.iter().find(|p| order[p.0].0 == *id) else { return };
                let corner = Self::rect(width, cols, c, r, 1, 1).origin;
                let cw = Self::cell_w(width, cols);
                let wide = f32::from(local.x - corner.x) > cw + GAP * 0.5 + cw * 0.15;
                let tall = f32::from(local.y - corner.y) > WIDGET_H + GAP * 0.5 + WIDGET_H * 0.15;
                let want = match (wide, tall) {
                    (false, _) => Size::Small,
                    (true, false) => Size::Wide,
                    (true, true) => Size::Large,
                };
                let ok = widgets::sizes(id);
                let to = if ok.contains(&want) { want } else if want == Size::Large && ok.contains(&Size::Wide) { Size::Wide } else { *from };
                if let Some(it) = order.iter_mut().find(|(i, _)| i == id)
                    && it.1 != to
                {
                    it.1 = to;
                    cx.notify();
                }
            }
        }
    }

    fn pointer_up(&mut self, cx: &mut Context<Self>) {
        let done = std::mem::replace(&mut self.ar.arrange, Arrange::None);
        match done {
            Arrange::Drag { order, .. } | Arrange::Resize { order, .. } => self.save_items(&order, cx),
            Arrange::None => return,
        }
        cx.notify();
    }

    /// The view's root listens for moves and releases while arranging, plus esc / ⌘Z.
    pub(super) fn arrange_events(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        root.track_focus(&self.ar.focus)
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                if !matches!(v.ar.arrange, Arrange::None) {
                    if ev.pressed_button != Some(MouseButton::Left) {
                        v.pointer_up(cx);
                    } else {
                        v.pointer_moved(ev.position, cx);
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, cx| v.pointer_up(cx)))
            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, _, cx| {
                let k = &ev.keystroke;
                if k.key == "escape" && (v.ar.gallery.is_some() || v.ar.menu || v.ar.renaming.is_some()) {
                    cx.stop_propagation();
                    v.ar.gallery = None;
                    v.ar.menu = false;
                    v.ar.renaming = None;
                    cx.notify();
                } else if k.key == "z" && k.modifiers.platform && !k.modifiers.shift && v.ar.renaming.is_none() && v.ar.gallery.is_none() {
                    cx.stop_propagation();
                    v.undo(cx);
                } else if k.key == "enter" && v.ar.renaming.is_some() {
                    cx.stop_propagation();
                    v.finish_rename(cx);
                }
            }))
    }

    // -------------------------------------------------------------- header buttons

    pub(super) fn layout_button(&self, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.ar.menu;
        div()
            .id("insights-layout")
            .h(px(30.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(7.))
            .border_1()
            .border_color(if open { t.accent } else { t.line })
            .when(open, |d| d.bg(t.accent_soft))
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .on_click(cx.listener(|v, _, w, cx| {
                v.ar.menu = !v.ar.menu;
                v.ar.renaming = None;
                w.focus(&v.ar.focus, cx);
                cx.notify();
            }))
            .child(div().text_color(t.dim).child("Layout"))
            .child(div().font_weight(FontWeight::BOLD).child(self.current_name(cx)))
            .child(crate::icons::Icon::Chevron.el(10., t.dim))
            .when(open, |d| d.relative().child(self.layout_menu(t, cx)))
    }

    pub(super) fn add_button(&self, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("insights-add-widget")
            .h(px(30.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(7.))
            .bg(t.accent)
            .text_color(t.accent_fg)
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .on_click(cx.listener(|v, _, w, cx| {
                let g = new_gallery(cx, None);
                let f = g.query.read(cx).focus.clone();
                w.focus(&f, cx);
                v.ar.gallery = Some(g);
                v.ar.menu = false;
                cx.notify();
            }))
            .child("+ Add widget")
    }

    // -------------------------------------------------------------- overlays

    pub(super) fn overlays(&self, t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![];
        if self.ar.gallery.is_some() {
            out.push(deferred(self.gallery(t, cx)).with_priority(2).into_any_element());
        }
        out
    }

    fn switch(&mut self, name: String, cx: &mut Context<Self>) {
        self.ar.menu = false;
        self.set("insights.layout", json!(name), cx);
    }

    fn save_as_new(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let mut all = self.layouts(cx);
        let mut n = all.len() + 1;
        let name = loop {
            let c = format!("Layout {n}");
            if !all.iter().any(|(x, _)| *x == c) {
                break c;
            }
            n += 1;
        };
        all.push((name.clone(), format(&self.items(cx))));
        self.write_layouts(&all, cx);
        self.set("insights.layout", json!(name.clone()), cx);
        self.start_rename(&name, w, cx);
    }

    fn start_rename(&mut self, name: &str, w: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| {
            let mut f = TextField::new(cx, false, "Layout name");
            f.set_text(name, cx);
            f.select_all(cx);
            f
        });
        let f = field.read(cx).focus.clone();
        w.focus(&f, cx);
        self.ar.renaming = Some(field);
        self.ar.menu = true;
        cx.notify();
    }

    fn finish_rename(&mut self, cx: &mut Context<Self>) {
        let Some(field) = self.ar.renaming.take() else { return };
        let new = field.read(cx).text().trim().replace(['=', ',', '\n'], " ").trim().to_string();
        let old = self.current_name(cx);
        let mut all = self.layouts(cx);
        if !new.is_empty() && new != old && !all.iter().any(|(n, _)| *n == new) {
            if let Some(l) = all.iter_mut().find(|(n, _)| *n == old) {
                l.0 = new.clone();
            }
            self.write_layouts(&all, cx);
            self.set("insights.layout", json!(new), cx);
        }
        self.ar.menu = false;
        cx.notify();
    }

    fn delete_current(&mut self, cx: &mut Context<Self>) {
        let name = self.current_name(cx);
        let all: Vec<(String, String)> = self.layouts(cx).into_iter().filter(|(n, _)| *n != name).collect();
        if let Some((first, _)) = all.first().cloned() {
            self.write_layouts(&all, cx);
            self.set("insights.layout", json!(first), cx);
        }
        self.ar.menu = false;
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.ar.menu = false;
        self.ar.undo.clear();
        if let Some(m) = self.main.upgrade() {
            m.update(cx, |m, cx| {
                for k in ["insights.layouts", "insights.layout"] {
                    m.settings.remove(k);
                    m.rpc("settings.reset", json!({"key": k}), cx, |_, _, _, _| {});
                }
                m.request_refresh(crate::app::refresh::SETTINGS, cx);
            });
        }
        cx.notify();
    }

    fn layout_menu(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.current_name(cx);
        let all = self.layouts(cx);
        let item = |id: SharedString| div().id(id).flex().items_center().gap(px(8.)).px(px(10.)).py(px(7.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(t.accent_soft));
        let mut list = crate::ui::sidebar::menu_box(t).occlude().w(px(290.)).text_size(px(12.5)).child(
            div().px(px(10.)).pt(px(4.)).pb(px(2.)).text_size(px(10.5)).font_weight(FontWeight::BOLD).text_color(t.dim).child("LAYOUTS"),
        );
        for (name, value) in &all {
            let on = *name == current;
            let n = parse(value).len();
            let pick = name.clone();
            let row = if on && let Some(field) = &self.ar.renaming {
                div().flex().items_center().gap(px(8.)).px(px(10.)).py(px(5.)).rounded(px(6.)).bg(t.accent_soft).child(div().w(px(14.)).text_color(t.accent).child("✓")).child(
                    div().flex_1().h(px(26.)).px(px(8.)).flex().items_center().rounded(px(5.)).bg(t.bg).border_1().border_color(t.accent).child(field.clone()),
                ).into_any_element()
            } else {
                item(SharedString::from(format!("layout-{name}")))
                    .when(on, |d| d.bg(t.accent_soft))
                    .on_click(cx.listener(move |v, _, _, cx| v.switch(pick.clone(), cx)))
                    .child(div().w(px(14.)).text_color(t.accent).child(if on { "✓" } else { "" }))
                    .child(div().flex_1().child(name.clone()))
                    .child(div().text_size(px(11.)).text_color(t.dim).child(format!("{n} widgets")))
                    .into_any_element()
            };
            list = list.child(row);
        }
        let rename_name = current.clone();
        list = list
            .child(div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(t.line))
            .child(item("layout-new".into()).pl(px(32.)).on_click(cx.listener(|v, _, w, cx| v.save_as_new(w, cx))).child("Save as new layout…"))
            .child(item("layout-rename".into()).pl(px(32.)).on_click(cx.listener(move |v, _, w, cx| v.start_rename(&rename_name, w, cx))).child(format!("Rename “{current}”…")))
            .when(all.len() > 1, |l| l.child(item("layout-delete".into()).pl(px(32.)).on_click(cx.listener(|v, _, _, cx| v.delete_current(cx))).child(format!("Delete “{current}”"))))
            .child(
                item("layout-undo".into())
                    .pl(px(32.))
                    .when(self.ar.undo.is_empty(), |d| d.text_color(t.dim))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.ar.menu = false;
                        v.undo(cx);
                    }))
                    .child(div().flex_1().child("Undo last change"))
                    .child(div().text_color(t.dim).child("⌘Z")),
            )
            .child(item("layout-reset".into()).pl(px(32.)).on_click(cx.listener(|v, _, _, cx| v.reset(cx))).child("Reset to default"));
        deferred(anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(list.mt(px(36.)))).with_priority(1)
    }

    fn gallery(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Some(g) = &self.ar.gallery else { return div().into_any_element() };
        let on_screen: Vec<String> = self.items(cx).into_iter().map(|(i, _)| i).collect();
        let settings = self.main.upgrade().map(|m| m.read(cx).settings.clone()).unwrap_or_default();
        let pal = widgets::palette(t, &settings);
        let q = g.query.read(cx).text().to_lowercase();
        let shown: Vec<&widgets::Spec> = widgets::SPECS
            .iter()
            .filter(|s| g.group.is_none_or(|gr| s.group == gr))
            .filter(|s| q.is_empty() || s.title.to_lowercase().contains(&q) || s.desc.to_lowercase().contains(&q) || s.id.contains(&q))
            .collect();
        let mut tabs = div().flex().gap(px(4.)).px(px(20.)).py(px(10.)).border_b_1().border_color(t.line);
        for (i, gr) in std::iter::once(None).chain(widgets::GROUPS.iter().map(|g| Some(*g))).enumerate() {
            let on = g.group == gr;
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("gal-tab-{i}")))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.raised).font_weight(FontWeight::BOLD))
                    .when(!on, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(g) = &mut v.ar.gallery {
                            g.group = gr;
                        }
                        cx.notify();
                    }))
                    .child(gr.unwrap_or("All")),
            );
        }
        let mut tiles = div().grid().grid_cols(2).gap(px(14.));
        for s in &shown {
            let id = s.id;
            let on = on_screen.iter().any(|x| x == id);
            let picked = g.pick == Some(id);
            let size = widgets::usual(id);
            let preview = self.widget(id, if widgets::sizes(id).contains(&Size::Small) { Size::Small } else { size }, t, &pal, cx);
            tiles = tiles.child(
                div()
                    .id(SharedString::from(format!("gal-{id}")))
                    .h(px(232.))
                    .flex()
                    .flex_col()
                    .rounded(px(10.))
                    .bg(t.raised)
                    .overflow_hidden()
                    .border_2()
                    .border_color(if picked { t.accent } else { t.raised })
                    .cursor_pointer()
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(g) = &mut v.ar.gallery {
                            g.pick = Some(id);
                            g.size = widgets::usual(id);
                        }
                        cx.notify();
                    }))
                    .child(div().h(px(160.)).flex_none().overflow_hidden().p(px(12.)).bg(t.panel).flex().flex_col().child(preview))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px(px(12.))
                            .child(div().flex_1().min_w_0().child(div().font_weight(FontWeight::BOLD).truncate().child(s.title)).child(div().text_size(px(11.5)).text_color(t.dim).truncate().child(s.desc)))
                            .child(div().text_size(px(11.)).text_color(if on { t.dim } else { t.accent }).child(if on { "On screen" } else { "" })),
                    ),
            );
        }
        if shown.is_empty() {
            tiles = tiles.child(div().col_span(2).p(px(20.)).text_color(t.dim).child("No widget matches"));
        }
        // the picked one, full size, with its size and place
        let side = g.pick.and_then(widgets::spec).map(|s| {
            let id = s.id;
            let on = on_screen.iter().any(|x| x == id);
            let mut seg = div().flex().gap(px(2.)).p(px(2.)).rounded(px(8.)).bg(t.raised);
            for sz in widgets::sizes(id) {
                let sel = sz == g.size;
                seg = seg.child(
                    div()
                        .id(SharedString::from(format!("gal-size-{}", sz.as_str())))
                        .h(px(28.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(sel, |d| d.bg(t.accent_soft).font_weight(FontWeight::BOLD))
                        .when(!sel, |d| d.text_color(t.dim))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(g) = &mut v.ar.gallery {
                                g.size = sz;
                            }
                            cx.notify();
                        }))
                        .child(sz.label()),
                );
            }
            let h = if g.size == Size::Large { 2. * WIDGET_H + GAP } else { WIDGET_H };
            let w = if g.size == Size::Small { 300. } else { 620. };
            let body = self.widget(id, g.size, t, &pal, cx);
            let place = |label: &'static str, top: bool, sel: bool| {
                div()
                    .id(SharedString::from(format!("gal-place-{top}")))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(sel, |d| d.bg(t.accent_soft).font_weight(FontWeight::BOLD))
                    .when(!sel, |d| d.text_color(t.dim))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(g) = &mut v.ar.gallery {
                            g.at_top = top;
                        }
                        cx.notify();
                    }))
                    .child(label)
            };
            div()
                .w(px(680.))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(14.))
                .p(px(20.))
                .border_l_1()
                .border_color(t.line)
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child("PREVIEW · WITH YOUR DATA"))
                .child(div().flex_1().min_h_0().overflow_hidden().child(card(t).w(px(w)).h(px(h)).overflow_hidden().child(body)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .child(div().w(px(50.)).text_color(t.dim).child("Size"))
                        .child(seg)
                        .child(div().w(px(40.)).text_color(t.dim).child("Place"))
                        .child(div().flex().gap(px(2.)).p(px(2.)).rounded(px(8.)).bg(t.raised).child(place("At the top", true, g.at_top)).child(place("At the end", false, !g.at_top))),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(10.))
                        .child(
                            div()
                                .id("gal-cancel")
                                .h(px(34.))
                                .px(px(16.))
                                .flex()
                                .items_center()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(t.line)
                                .cursor_pointer()
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.ar.gallery = None;
                                    cx.notify();
                                }))
                                .child("Cancel"),
                        )
                        .child(
                            div()
                                .id("gal-add")
                                .h(px(34.))
                                .px(px(16.))
                                .flex()
                                .items_center()
                                .rounded(px(8.))
                                .bg(t.accent)
                                .text_color(t.accent_fg)
                                .font_weight(FontWeight::BOLD)
                                .cursor_pointer()
                                .on_click(cx.listener(move |v, _, _, cx| v.add_picked(cx)))
                                .child(if on { format!("Move {} here", s.title) } else { format!("Add {}", s.title) }),
                        ),
                )
        });
        let count = on_screen.len();
        div()
            .id("gallery-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0., 0., 0., 0.55))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|v, _, _, cx| {
                v.ar.gallery = None;
                cx.notify();
            }))
            .child(
                div()
                    .id("gallery")
                    .w(relative(0.92))
                    .h(relative(0.9))
                    .max_w(px(1400.))
                    .flex()
                    .flex_col()
                    .rounded(px(14.))
                    .bg(t.panel)
                    .border_1()
                    .border_color(t.line)
                    .overflow_hidden()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .px(px(20.))
                            .py(px(14.))
                            .border_b_1()
                            .border_color(t.line)
                            .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("Add a widget"))
                            .child(div().flex_1().h(px(34.)).px(px(12.)).flex().items_center().rounded(px(8.)).bg(t.raised).child(g.query.clone()))
                            .child(div().text_size(px(12.)).text_color(t.dim).child(format!("{count} on screen · {} widgets", widgets::SPECS.len())))
                            .child(
                                div()
                                    .id("gal-close")
                                    .size(px(30.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(7.))
                                    .text_color(t.dim)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(t.raised))
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.ar.gallery = None;
                                        cx.notify();
                                    }))
                                    .child("✕"),
                            ),
                    )
                    .child(tabs)
                    .child(div().flex_1().min_h_0().flex().child(div().id("gal-tiles").flex_1().min_w_0().overflow_y_scroll().p(px(20.)).child(tiles)).children(side)),
            )
            .into_any_element()
    }

    fn add_picked(&mut self, cx: &mut Context<Self>) {
        let Some(g) = self.ar.gallery.take() else { return };
        let Some(id) = g.pick else { return };
        let mut items: Items = self.items(cx).into_iter().filter(|(i, _)| i != id).collect();
        if g.at_top {
            items.insert(0, (id.to_string(), g.size));
        } else {
            items.push((id.to_string(), g.size));
        }
        self.save_items(&items, cx);
        cx.notify();
    }
}

/// Dots every 20 px and dashed column edges, shown only while moving or resizing.
fn guides(t: &Theme, width: f32, cols: u16, height: f32) -> impl IntoElement {
    let dot = charts::alpha(t.dim, 0.28);
    let line = charts::alpha(t.accent, 0.3);
    let cw = InsightsView::cell_w(width, cols);
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height).max(height));
            let mut y = 0.;
            while y <= h {
                let mut x = 0.;
                while x <= w {
                    window.paint_quad(fill(Bounds::new(point(px(x0 + x - 1.), px(y0 + y - 1.)), size(px(2.), px(2.))), dot));
                    x += 20.;
                }
                y += 20.;
            }
            for c in 0..cols {
                for edge in [c as f32 * (cw + GAP), c as f32 * (cw + GAP) + cw] {
                    let mut yy = 0.;
                    while yy < h {
                        window.paint_quad(fill(Bounds::new(point(px(x0 + edge), px(y0 + yy)), size(px(1.), px(6.))), line));
                        yy += 12.;
                    }
                }
            }
        },
    )
    .absolute()
    .top(px(-10.))
    .left(px(-10.))
    .w(px(width + 20.))
    .h(px(height + 20.))
}

#[cfg(test)]
mod tests {
    use super::{Items, Size, format, pack, parse};

    fn it(v: &[(&str, Size)]) -> Items {
        v.iter().map(|(i, s)| (i.to_string(), *s)).collect()
    }

    #[test]
    fn packs_large_wide_and_small_without_holes() {
        let items = it(&[("a", Size::Large), ("b", Size::Small), ("c", Size::Small), ("d", Size::Wide), ("e", Size::Small)]);
        let (p, rows) = pack(&items, 4);
        assert_eq!(p.iter().map(|x| (x.1, x.2)).collect::<Vec<_>>(), vec![(0, 0), (2, 0), (3, 0), (2, 1), (0, 2)]);
        assert_eq!(rows, 3);
        // two columns: wide and large take the whole width
        let (p, rows) = pack(&items, 2);
        assert_eq!(p.iter().map(|x| (x.1, x.2, x.3)).collect::<Vec<_>>(), vec![(0, 0, 2), (0, 2, 1), (1, 2, 1), (0, 3, 2), (0, 4, 1)]);
        assert_eq!(rows, 5);
    }

    #[test]
    fn layout_values_round_trip_through_sizes() {
        let items = parse("parallelism:large heatmap approved:small");
        assert_eq!(items, it(&[("parallelism", Size::Large), ("heatmap", Size::Wide), ("approved", Size::Small)]));
        assert_eq!(format(&items), "parallelism:large heatmap approved");
    }
}
