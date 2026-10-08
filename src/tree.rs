//! Vista de árbol: JSON formateado donde cada nodo puede estar
//! expandido, cerrado o compacto (en una sola línea).

use eframe::egui::{self, Rect, ScrollArea, Sense, Stroke, TextStyle, Ui, pos2, vec2};

use crate::common::{self, NavKeys, Out, Tri};
use crate::highlight::{JobSink, Palette};
use crate::json::{self, Doc, Kind, Layout, NONE, NodeId, Tok};
use crate::settings::{InitialFold, Settings};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fold {
    Expanded,
    Closed,
    Compact,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Leaf,
    Open,
    Close,
    Inline,
    Closed,
}

#[derive(Clone, Copy)]
struct Line {
    node: NodeId,
    kind: LineKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Toggle,
    Expand,
    Close,
    Compact,
    CompactChildren,
    CloseChildren,
    ExpandAll,
    Auto,
    ApplySimilar,
}

pub struct TreeState {
    pub folds: Vec<Fold>,
    lines: Vec<Line>,
    line_of: Vec<u32>,
    dirty: bool,
    pub selected: NodeId,
    /// (nodo, centrar)
    scroll: Option<(NodeId, bool)>,
    offset: f32,
    view_h: f32,
}

impl TreeState {
    pub fn new(doc: &Doc, set: &Settings) -> Self {
        let mut st = Self {
            folds: vec![Fold::Expanded; doc.len()],
            lines: Vec::new(),
            line_of: Vec::new(),
            dirty: true,
            selected: NONE,
            scroll: None,
            offset: 0.0,
            view_h: 0.0,
        };
        st.apply_initial(doc, set);
        st
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn apply_initial(&mut self, doc: &Doc, set: &Settings) {
        match set.initial {
            InitialFold::Auto => self.auto(doc, Doc::ROOT, set),
            InitialFold::Level => self.level(doc, set.initial_level, Fold::Closed),
            InitialFold::LevelCompact => self.level(doc, set.initial_level, Fold::Compact),
            InitialFold::ExpandAll => self.folds.fill(Fold::Expanded),
        }
        self.dirty = true;
    }

    /// Expande hasta `level` niveles y deja el resto en `rest`.
    pub fn level(&mut self, doc: &Doc, level: usize, rest: Fold) {
        for (f, n) in self.folds.iter_mut().zip(&doc.nodes) {
            *f = if (n.depth as usize) < level { Fold::Expanded } else { rest };
        }
        self.dirty = true;
    }

    pub fn set_all(&mut self, fold: Fold) {
        self.folds.fill(fold);
        self.folds[0] = if fold == Fold::Compact { Fold::Expanded } else { fold };
        self.dirty = true;
    }

    /// Compacta cada nodo del subárbol cuya forma de una línea quepa en `auto_width`.
    pub fn auto(&mut self, doc: &Doc, from: NodeId, set: &Settings) {
        let indent = if set.indent == 0 { 4 } else { set.indent };
        for id in from..doc.subtree_end(from) {
            let n = doc.node(id);
            if !n.kind.is_container() || n.children.is_empty() {
                continue;
            }
            let prefix = n.depth as usize * indent + doc.key_of(n).map_or(0, |k| k.len() + 4) + 1;
            let fits = set.auto_width > prefix && json::compact_len(doc, id, set.auto_width - prefix).is_some();
            self.folds[id as usize] = if fits { Fold::Compact } else { Fold::Expanded };
        }
        self.dirty = true;
    }

    pub fn apply(&mut self, doc: &Doc, id: NodeId, action: Action, set: &Settings) {
        let n = doc.node(id);
        if !n.kind.is_container() || n.children.is_empty() {
            return;
        }
        let i = id as usize;
        let f = &mut self.folds;
        match action {
            Action::Toggle => f[i] = if f[i] == Fold::Expanded { Fold::Closed } else { Fold::Expanded },
            Action::Expand => f[i] = Fold::Expanded,
            Action::Close => f[i] = Fold::Closed,
            Action::Compact => f[i] = Fold::Compact,
            Action::CompactChildren | Action::CloseChildren => {
                let child = if action == Action::CompactChildren { Fold::Compact } else { Fold::Closed };
                f[i] = Fold::Expanded;
                for &c in &n.children {
                    f[c as usize] = child;
                }
            }
            Action::ExpandAll => f[i..doc.subtree_end(id) as usize].fill(Fold::Expanded),
            Action::Auto => {
                self.auto(doc, id, set);
                self.folds[i] = Fold::Expanded;
            }
            Action::ApplySimilar => self.apply_similar(doc, id),
        }
        self.dirty = true;
    }

    /// Copia el estado del nodo (y de sus hijos directos) a los nodos con la misma ruta
    /// ignorando índices de arreglo.
    fn apply_similar(&mut self, doc: &Doc, id: NodeId) {
        let n = doc.node(id);
        let fold = self.folds[id as usize];
        let containers: Vec<NodeId> = n.children.iter().copied().filter(|&c| doc.node(c).kind.is_container()).collect();
        let uniform = containers
            .first()
            .map(|&c| self.folds[c as usize])
            .filter(|&f0| containers.iter().all(|&c| self.folds[c as usize] == f0));
        for j in 0..doc.len() as NodeId {
            let m = doc.node(j);
            if j == id || m.kind != n.kind || m.depth != n.depth || !doc.same_shape_path(id, j) {
                continue;
            }
            self.folds[j as usize] = fold;
            for &c in &m.children {
                let cn = doc.node(c);
                if !cn.kind.is_container() {
                    continue;
                }
                let src = if n.kind == Kind::Object {
                    n.children.iter().find(|&&s| doc.node(s).key == cn.key).map(|&s| self.folds[s as usize])
                } else {
                    uniform
                };
                if let Some(f) = src {
                    self.folds[c as usize] = f;
                }
            }
        }
    }

    /// Abre los ancestros cerrados y selecciona el nodo (o el ancestro compacto que lo contiene).
    pub fn reveal(&mut self, doc: &Doc, id: NodeId) {
        let mut chain = Vec::new();
        let mut cur = doc.node(id).parent;
        while cur != NONE {
            chain.push(cur);
            cur = doc.node(cur).parent;
        }
        let mut target = id;
        for &a in chain.iter().rev() {
            match self.folds[a as usize] {
                Fold::Closed => {
                    self.folds[a as usize] = Fold::Expanded;
                    self.dirty = true;
                }
                Fold::Compact => {
                    target = a;
                    break;
                }
                Fold::Expanded => {}
            }
        }
        self.selected = target;
        self.scroll = Some((target, true));
    }

    fn rebuild(&mut self, doc: &Doc) {
        self.lines.clear();
        self.line_of.clear();
        self.line_of.resize(doc.len(), u32::MAX);
        let mut stack: Vec<(NodeId, bool)> = vec![(Doc::ROOT, false)];
        while let Some((id, closing)) = stack.pop() {
            if closing {
                self.lines.push(Line { node: id, kind: LineKind::Close });
                continue;
            }
            let n = doc.node(id);
            let kind = if !n.kind.is_container() {
                LineKind::Leaf
            } else if n.children.is_empty() || self.folds[id as usize] == Fold::Compact {
                LineKind::Inline
            } else if self.folds[id as usize] == Fold::Closed {
                LineKind::Closed
            } else {
                LineKind::Open
            };
            self.line_of[id as usize] = self.lines.len() as u32;
            self.lines.push(Line { node: id, kind });
            if kind == LineKind::Open {
                stack.push((id, true));
                stack.extend(n.children.iter().rev().map(|&c| (c, false)));
            }
        }
        self.dirty = false;
    }

    /// Línea visible del nodo o de su ancestro visible más cercano.
    fn visible_line(&self, doc: &Doc, mut id: NodeId) -> Option<usize> {
        while id != NONE {
            let l = self.line_of[id as usize];
            if l != u32::MAX {
                return Some(l as usize);
            }
            id = doc.node(id).parent;
        }
        None
    }

    fn handle_keys(&mut self, ui: &Ui, doc: &Doc, set: &Settings) {
        let k = NavKeys::read(ui);
        let page = (self.view_h / ui.text_style_height(&TextStyle::Monospace)).max(1.0) as isize - 1;
        if self.selected == NONE {
            if k.row_delta(page).is_some() {
                self.selected = Doc::ROOT;
                self.scroll = Some((Doc::ROOT, false));
            }
            return;
        }
        let sel = self.selected;
        let n = doc.node(sel);
        let fold = self.folds[sel as usize];
        let has_kids = n.kind.is_container() && !n.children.is_empty();
        let mut action = None;
        if let Some(delta) = k.row_delta(page) {
            let Some(cur) = self.visible_line(doc, sel) else { return };
            let last = self.lines.len() as isize - 1;
            let mut li = (cur as isize + delta).clamp(0, last);
            // Saltar las líneas de cierre.
            let step = if delta < 0 { -1 } else { 1 };
            while self.lines[li as usize].kind == LineKind::Close {
                if (li + step) < 0 || (li + step) > last {
                    break;
                }
                li += step;
            }
            if self.lines[li as usize].kind == LineKind::Close {
                li = cur as isize;
            }
            self.selected = self.lines[li as usize].node;
            self.scroll = Some((self.selected, false));
        } else if k.left {
            if has_kids && fold != Fold::Closed {
                action = Some(Action::Close);
            } else if n.parent != NONE {
                self.selected = n.parent;
                self.scroll = Some((n.parent, false));
            }
        } else if k.right {
            if has_kids && fold != Fold::Expanded {
                action = Some(Action::Expand);
            } else if has_kids {
                self.selected = n.children[0];
                self.scroll = Some((self.selected, false));
            }
        } else if k.enter {
            action = Some(Action::Toggle);
        } else if k.copy {
            ui.ctx().copy_text(json::to_string(doc, sel, Layout::Pretty(&set.indent_str())));
        } else if let Some(c) = k.letter {
            action = match c {
                'c' => Some(Action::Compact),
                'h' => Some(Action::CompactChildren),
                'e' => Some(Action::ExpandAll),
                'a' => Some(Action::Auto),
                'x' => Some(Action::CloseChildren),
                _ => None,
            };
        }
        if let Some(a) = action {
            self.apply(doc, sel, a, set);
            self.scroll = Some((sel, false));
        }
    }
}

fn line_job(
    doc: &Doc,
    line: Line,
    set: &Settings,
    pal: &Palette,
    font: egui::FontId,
    query: &str,
) -> egui::text::LayoutJob {
    let n = doc.node(line.node);
    let mut s = JobSink::new(pal, font, query);
    let is_obj = n.kind == Kind::Object;
    let last = n.parent == NONE || n.index as usize + 1 == doc.node(n.parent).children.len();
    if line.kind != LineKind::Close {
        if set.show_indices && !n.has_key() && n.parent != NONE {
            s.colored(&format!("{}: ", n.index), pal.faint);
        }
        if let Some(k) = doc.key_of(n) {
            json::write_key(k, set.quote_keys, &mut s);
            json::Sink::push(&mut s, ": ", Tok::Punct);
        }
    }
    let (open, close) = if is_obj { ("{", "}") } else { ("[", "]") };
    let push = |s: &mut JobSink, t: &str| json::Sink::push(s, t, Tok::Punct);
    match line.kind {
        LineKind::Leaf | LineKind::Inline => {
            json::write_value(doc, line.node, Layout::Compact, set.compact_limit, &mut s);
        }
        LineKind::Open => push(&mut s, open),
        LineKind::Close => push(&mut s, close),
        LineKind::Closed => {
            push(&mut s, open);
            s.with_bg(" … ", pal.punct, pal.fold_bg);
            push(&mut s, close);
        }
    }
    if set.show_commas && !last && line.kind != LineKind::Open {
        push(&mut s, ",");
    }
    if set.show_counts && line.kind == LineKind::Closed {
        let c = n.children.len();
        let what = match (is_obj, c == 1) {
            (true, true) => "propiedad",
            (true, false) => "propiedades",
            (false, true) => "elemento",
            (false, false) => "elementos",
        };
        s.colored(&format!("  // {c} {what}"), pal.faint);
    }
    s.finish()
}

fn context_menu(ui: &mut Ui, doc: &Doc, id: NodeId, set: &Settings, act: &mut Option<(NodeId, Action)>) -> Option<Out> {
    let n = doc.node(id);
    if n.kind.is_container() && !n.children.is_empty() {
        let mut item = |ui: &mut Ui, label: &str, shortcut: &str, a: Action| {
            if ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked() {
                *act = Some((id, a));
                ui.close();
            }
        };
        item(ui, "Expandir", "→", Action::Expand);
        item(ui, "Cerrar", "←", Action::Close);
        item(ui, "Compactar (una línea)", "C", Action::Compact);
        ui.separator();
        item(ui, "Compactar hijos", "H", Action::CompactChildren);
        item(ui, "Cerrar hijos", "X", Action::CloseChildren);
        item(ui, "Expandir todo el subárbol", "E", Action::ExpandAll);
        item(ui, "Auto-compactar subárbol", "A", Action::Auto);
        ui.separator();
        item(ui, "Aplicar este estado a nodos similares", "", Action::ApplySimilar);
        ui.separator();
    }
    common::node_menu(ui, doc, id, set)
}

pub fn ui(
    ui: &mut Ui,
    doc: &Doc,
    st: &mut TreeState,
    set: &Settings,
    pal: &Palette,
    query: &str,
    keys: bool,
) -> Option<Out> {
    let key_out = if keys {
        st.handle_keys(ui, doc, set);
        common::grid_key(ui, doc, st.selected)
    } else {
        None
    };
    if st.dirty {
        st.rebuild(doc);
    }

    let mut out = None;
    let mut act: Option<(NodeId, Action)> = None;
    let mut select: Option<NodeId> = None;

    let font = TextStyle::Monospace.resolve(ui.style());
    let (row_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font).round() + 2.0, f.glyph_width(&font, '0')));
    let indent_px = char_w * if set.indent == 0 { 4 } else { set.indent } as f32;
    let digits = (st.lines.len().max(1).ilog10() + 1) as f32;
    let num_w = if set.line_numbers { digits * char_w + 14.0 } else { 4.0 };
    let arrow_w = row_h;
    let text_x0 = num_w + arrow_w + 2.0;

    let sel_bg = ui.visuals().selection.bg_fill.gamma_multiply(0.45);
    let hover_bg = ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.5);
    let num_color = ui.visuals().weak_text_color();
    let arrow_color = ui.visuals().text_color();

    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let mut area = ScrollArea::both().auto_shrink(false).id_salt("tree_view");
    if let Some((node, center)) = st.scroll.take() {
        if let Some(li) = st.visible_line(doc, node) {
            if let Some(off) = common::scroll_for_row(li, row_h, center, st.offset, st.view_h) {
                area = area.vertical_scroll_offset(off);
            }
        }
    }

    let lines = &st.lines;
    let selected = st.selected;
    let output = area.show_rows(ui, row_h, lines.len(), |ui, range| {
        for li in range {
            let line = lines[li];
            let n = doc.node(line.node);
            let text_x = text_x0 + n.depth as f32 * indent_px;
            let galley = ui.painter().layout_job(line_job(doc, line, set, pal, font.clone(), query));
            let width = (text_x + galley.size().x + 24.0).max(ui.available_width());
            let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let p = ui.painter();
            if line.node == selected {
                p.rect_filled(rect, 0.0, sel_bg);
            } else if resp.hovered() {
                p.rect_filled(rect, 0.0, hover_bg);
            }
            if set.line_numbers {
                p.text(
                    pos2(rect.left() + num_w - 8.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    (li + 1).to_string(),
                    font.clone(),
                    num_color,
                );
            }
            if set.show_guides {
                for l in 0..n.depth {
                    let x = (rect.left() + text_x0 + l as f32 * indent_px + char_w * 0.5).round() + 0.5;
                    p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, pal.guide));
                }
            }
            let foldable = n.kind.is_container() && !n.children.is_empty() && line.kind != LineKind::Close;
            let arrow_rect = Rect::from_min_size(pos2(rect.left() + num_w, rect.top()), vec2(arrow_w, row_h));
            if foldable {
                let t = match line.kind {
                    LineKind::Open => Tri::Open,
                    LineKind::Inline => Tri::Compact,
                    _ => Tri::Closed,
                };
                let c = if resp.hovered() { arrow_color } else { arrow_color.gamma_multiply(0.75) };
                common::triangle(p, arrow_rect.center(), row_h, t, c);
            }
            p.galley(pos2(rect.left() + text_x, rect.center().y - galley.size().y / 2.0), galley, pal.plain);

            if resp.clicked() || resp.secondary_clicked() {
                select = Some(line.node);
            }
            let on_arrow = resp.interact_pointer_pos().is_some_and(|pp| pp.x < arrow_rect.right());
            if foldable && resp.clicked() && on_arrow {
                let m = ui.input(|i| i.modifiers);
                let a = if m.shift {
                    Action::ExpandAll
                } else if m.command {
                    Action::Compact
                } else if m.alt {
                    Action::CompactChildren
                } else {
                    Action::Toggle
                };
                act = Some((line.node, a));
            } else if foldable && resp.double_clicked() {
                act = Some((line.node, Action::Toggle));
            }
            if foldable && on_arrow {
                resp.clone().on_hover_text_at_pointer(
                    "Clic: expandir/cerrar · Ctrl+clic: compactar · Alt+clic: compactar hijos · Mayús+clic: expandir todo",
                );
            }
            resp.context_menu(|ui| {
                if let Some(o) = context_menu(ui, doc, line.node, set, &mut act) {
                    out = Some(o);
                }
            });
        }
    });
    st.offset = output.state.offset.y;
    st.view_h = output.inner_rect.height();

    if let Some(id) = select {
        st.selected = id;
    }
    if let Some((id, a)) = act {
        st.apply(doc, id, a, set);
        st.selected = id;
    }
    out.or(key_out)
}
