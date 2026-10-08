//! Comparación de dos documentos: diff de líneas lado a lado sobre el JSON
//! formateado y lista de cambios por ruta.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, RichText, ScrollArea, Sense, Stroke, TextStyle, Ui, pos2, vec2};
use similar::{Algorithm, DiffOp, capture_diff_slices, capture_diff_slices_deadline};

use crate::common;
use crate::highlight::{JobSink, Palette};
use crate::json::{self, Doc, Kind, NONE, NodeId};

/// Las líneas más largas no se comparan carácter a carácter.
const INLINE_LIMIT: usize = 2000;
/// Tope de cambios en la lista por ruta.
const MAX_CHANGES: usize = 5000;
/// Líneas iguales que se dejan alrededor de cada diferencia al ocultar el resto.
const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Same,
    /// Línea modificada: existe en ambos lados.
    Changed,
    /// Solo a la izquierda.
    Removed,
    /// Solo a la derecha.
    Added,
    /// Separador de líneas iguales ocultas (`left` guarda cuántas).
    Fold,
}

#[derive(Clone, Copy)]
pub struct Row {
    pub kind: RowKind,
    pub left: u32,
    pub right: u32,
}

const NO_LINE: u32 = u32::MAX;

/// JSON formateado de un lado, línea por línea.
struct Side {
    lines: Vec<String>,
    /// Línea donde empieza cada nodo.
    line_of: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Changed,
    Removed,
    Added,
}

pub struct Change {
    pub kind: ChangeKind,
    pub path: String,
    pub left: NodeId,
    pub right: NodeId,
    pub summary: String,
}

pub struct Diff {
    left: Side,
    right: Side,
    /// Todas las filas alineadas.
    rows: Vec<Row>,
    /// Inicio (en `rows`) de cada bloque de diferencias.
    hunks: Vec<usize>,
    pub changes: Vec<Change>,
    pub changes_truncated: bool,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    /// El diff de líneas se cortó por tiempo y puede no ser mínimo.
    pub approximate: bool,
    /// Ancho en caracteres de la línea más larga (de ambos lados).
    max_cols: usize,
}

impl Diff {
    pub fn new(a: &Doc, b: &Doc, indent: &str, sort_keys: bool) -> Self {
        let left = render(a, indent, sort_keys);
        let right = render(b, indent, sort_keys);

        // Cada línea distinta recibe un número; así el diff compara enteros.
        let mut ids: HashMap<&str, u32> = HashMap::new();
        let [la, lb] = [&left, &right].map(|s| {
            s.lines
                .iter()
                .map(|l| {
                    let n = ids.len() as u32;
                    *ids.entry(l.as_str()).or_insert(n)
                })
                .collect::<Vec<u32>>()
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let ops = capture_diff_slices_deadline(Algorithm::Patience, &la, &lb, Some(deadline));
        let approximate = Instant::now() >= deadline;
        let max_cols = left.lines.iter().chain(&right.lines).map(|l| l.chars().count()).max().unwrap_or(0);

        let mut rows = Vec::with_capacity(la.len().max(lb.len()));
        let (mut added, mut removed, mut modified) = (0, 0, 0);
        let line = |i: usize| i as u32;
        for op in &ops {
            match *op {
                DiffOp::Equal { old_index, new_index, len } => {
                    for k in 0..len {
                        rows.push(Row { kind: RowKind::Same, left: line(old_index + k), right: line(new_index + k) });
                    }
                }
                DiffOp::Delete { old_index, old_len, .. } => {
                    removed += old_len;
                    for k in 0..old_len {
                        rows.push(Row { kind: RowKind::Removed, left: line(old_index + k), right: NO_LINE });
                    }
                }
                DiffOp::Insert { new_index, new_len, .. } => {
                    added += new_len;
                    for k in 0..new_len {
                        rows.push(Row { kind: RowKind::Added, left: NO_LINE, right: line(new_index + k) });
                    }
                }
                DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    let both = old_len.min(new_len);
                    modified += both;
                    removed += old_len - both;
                    added += new_len - both;
                    for k in 0..old_len.max(new_len) {
                        let l = if k < old_len { line(old_index + k) } else { NO_LINE };
                        let r = if k < new_len { line(new_index + k) } else { NO_LINE };
                        let kind = match (k < old_len, k < new_len) {
                            (true, true) => RowKind::Changed,
                            (true, false) => RowKind::Removed,
                            _ => RowKind::Added,
                        };
                        rows.push(Row { kind, left: l, right: r });
                    }
                }
            }
        }
        let hunks = hunk_starts(&rows);

        let mut changes = Vec::new();
        let changes_truncated = !structural(a, b, Doc::ROOT, Doc::ROOT, &mut changes);

        Self { left, right, rows, hunks, changes, changes_truncated, added, removed, modified, approximate, max_cols }
    }

    pub fn is_identical(&self) -> bool {
        self.hunks.is_empty()
    }
}

fn hunk_starts(rows: &[Row]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut prev_same = true;
    for (i, r) in rows.iter().enumerate() {
        let same = r.kind == RowKind::Same;
        if !same && prev_same {
            v.push(i);
        }
        prev_same = same;
    }
    v
}

/// Formatea el documento con una línea por elemento, recordando dónde empieza cada nodo.
fn render(doc: &Doc, indent: &str, sort_keys: bool) -> Side {
    struct Frame {
        kids: Vec<NodeId>,
        next: usize,
        close: &'static str,
        comma: bool,
    }
    let mut lines = Vec::new();
    let mut line_of = vec![0u32; doc.len()];
    let mut stack: Vec<Frame> = Vec::new();

    // Escribe la línea de `id` (con clave) y abre un marco si es contenedor no vacío.
    let mut open = |id: NodeId, comma: bool, stack: &mut Vec<Frame>, lines: &mut Vec<String>| {
        let n = doc.node(id);
        let mut s = indent.repeat(stack.len());
        if let Some(k) = doc.key_of(n) {
            json::write_escaped(k, &mut s);
            s.push_str(": ");
        }
        line_of[id as usize] = lines.len() as u32;
        if n.kind.is_container() && !n.children.is_empty() {
            let is_obj = n.kind == Kind::Object;
            s.push(if is_obj { '{' } else { '[' });
            let mut kids = n.children.clone();
            if sort_keys && is_obj {
                kids.sort_by(|&x, &y| doc.key_of(doc.node(x)).cmp(&doc.key_of(doc.node(y))));
            }
            stack.push(Frame { kids, next: 0, close: if is_obj { "}" } else { "]" }, comma });
        } else {
            json::write_scalar(doc, n, &mut s);
            if comma {
                s.push(',');
            }
        }
        lines.push(s);
    };

    if !doc.nodes.is_empty() {
        open(Doc::ROOT, false, &mut stack, &mut lines);
    }
    while let Some(top) = stack.last_mut() {
        if top.next == top.kids.len() {
            let f = stack.pop().unwrap();
            let mut s = indent.repeat(stack.len());
            s.push_str(f.close);
            if f.comma {
                s.push(',');
            }
            lines.push(s);
            continue;
        }
        let id = top.kids[top.next];
        top.next += 1;
        let comma = top.next < top.kids.len();
        open(id, comma, &mut stack, &mut lines);
    }
    Side { lines, line_of }
}

/// Compara por ruta: objetos por clave, arreglos por posición. Devuelve `false` si se llegó al tope.
fn structural(a: &Doc, b: &Doc, x: NodeId, y: NodeId, out: &mut Vec<Change>) -> bool {
    if out.len() >= MAX_CHANGES {
        return false;
    }
    let (nx, ny) = (a.node(x), b.node(y));
    match (nx.kind, ny.kind) {
        (Kind::Object, Kind::Object) => {
            let mut right: HashMap<&str, NodeId> = HashMap::new();
            for &c in ny.children.iter().rev() {
                right.insert(b.key_of(b.node(c)).unwrap_or(""), c);
            }
            for &c in &nx.children {
                let k = a.key_of(a.node(c)).unwrap_or("");
                match right.remove(k) {
                    Some(d) => {
                        if !structural(a, b, c, d, out) {
                            return false;
                        }
                    }
                    None => push_change(out, ChangeKind::Removed, a, c, NONE, b),
                }
            }
            for &d in &ny.children {
                let k = b.key_of(b.node(d)).unwrap_or("");
                if right.get(k) == Some(&d) {
                    push_change(out, ChangeKind::Added, a, NONE, d, b);
                }
            }
        }
        (Kind::Array, Kind::Array) => {
            let n = nx.children.len().min(ny.children.len());
            for i in 0..n {
                if !structural(a, b, nx.children[i], ny.children[i], out) {
                    return false;
                }
            }
            for &c in &nx.children[n..] {
                push_change(out, ChangeKind::Removed, a, c, NONE, b);
            }
            for &d in &ny.children[n..] {
                push_change(out, ChangeKind::Added, a, NONE, d, b);
            }
        }
        (kx, ky) => {
            let differ = kx != ky || (!kx.is_container() && a.text_of(nx) != b.text_of(ny));
            if differ {
                push_change(out, ChangeKind::Changed, a, x, y, b);
            }
        }
    }
    out.len() < MAX_CHANGES
}

fn push_change(out: &mut Vec<Change>, kind: ChangeKind, a: &Doc, left: NodeId, right: NodeId, b: &Doc) {
    let short = |doc: &Doc, id: NodeId| {
        let mut s = String::new();
        json::write_value(doc, id, json::Layout::Compact, 60, &mut s);
        s
    };
    let (path, summary) = match kind {
        ChangeKind::Changed => (a.path(left), format!("{} → {}", short(a, left), short(b, right))),
        ChangeKind::Removed => (a.path(left), short(a, left)),
        ChangeKind::Added => (b.path(right), short(b, right)),
    };
    out.push(Change { kind, path, left, right, summary });
}

/// Rangos de bytes distintos entre dos líneas (carácter a carácter).
fn inline_ranges(a: &str, b: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if a.len() > INLINE_LIMIT || b.len() > INLINE_LIMIT {
        return (vec![0..a.len()], vec![0..b.len()]);
    }
    let (ca, cb): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let byte_pos = |s: &str| -> Vec<usize> { s.char_indices().map(|(i, _)| i).chain([s.len()]).collect() };
    let (pa, pb) = (byte_pos(a), byte_pos(b));
    let (mut ra, mut rb) = (Vec::new(), Vec::new());
    for op in capture_diff_slices(Algorithm::Myers, &ca, &cb) {
        let (o, n) = (op.old_range(), op.new_range());
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        if !o.is_empty() {
            ra.push(pa[o.start]..pa[o.end]);
        }
        if !n.is_empty() {
            rb.push(pb[n.start]..pb[n.end]);
        }
    }
    (ra, rb)
}

// ---------------------------------------------------------------------------
// Vista

#[derive(Default)]
pub struct DiffView {
    /// Filas mostradas (con las iguales plegadas si se pidió).
    shown: Vec<Row>,
    /// Índice en `shown` de cada bloque de diferencias.
    shown_hunks: Vec<usize>,
    built_only_changes: Option<bool>,
    pub current: Option<usize>,
    scroll: Option<usize>,
    offset: f32,
    view_h: f32,
    /// Se pidió ver todas las líneas (la app apaga «Solo diferencias»).
    pub want_all: bool,
    /// Nodo a mostrar en cuanto deje de estar plegado.
    pending: Option<(NodeId, NodeId)>,
    /// Desplazamiento horizontal del texto, común a los dos lados.
    h_offset: f32,
}

impl DiffView {
    fn ensure(&mut self, d: &Diff, only_changes: bool) {
        if self.built_only_changes == Some(only_changes) {
            return;
        }
        self.built_only_changes = Some(only_changes);
        self.shown.clear();
        if !only_changes {
            self.shown = d.rows.clone();
        } else {
            // Conserva CONTEXT filas iguales alrededor de cada diferencia.
            let n = d.rows.len();
            let mut keep = vec![false; n];
            for (i, r) in d.rows.iter().enumerate() {
                if r.kind != RowKind::Same {
                    keep[i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(n)].fill(true);
                }
            }
            let mut i = 0;
            while i < n {
                if keep[i] {
                    self.shown.push(d.rows[i]);
                    i += 1;
                } else {
                    let start = i;
                    while i < n && !keep[i] {
                        i += 1;
                    }
                    self.shown.push(Row { kind: RowKind::Fold, left: (i - start) as u32, right: NO_LINE });
                }
            }
        }
        self.shown_hunks =
            hunk_starts(&self.shown).into_iter().filter(|&i| self.shown[i].kind != RowKind::Fold).collect();
        if self.current.is_some_and(|c| c >= self.shown_hunks.len()) {
            self.current = None;
        }
    }

    /// Va a la diferencia siguiente o anterior.
    pub fn step(&mut self, forward: bool) {
        let n = self.shown_hunks.len();
        if n == 0 {
            return;
        }
        let next = match self.current {
            Some(c) if forward => (c + 1) % n,
            Some(c) => (c + n - 1) % n,
            None if forward => 0,
            None => n - 1,
        };
        self.current = Some(next);
        self.scroll = Some(self.shown_hunks[next]);
    }

    /// Muestra la fila donde empieza un nodo (de la izquierda o de la derecha).
    pub fn reveal(&mut self, d: &Diff, left: NodeId, right: NodeId) {
        let (line, on_left) =
            if left != NONE { (d.left.line_of[left as usize], true) } else { (d.right.line_of[right as usize], false) };
        let row = self.shown.iter().position(|r| if on_left { r.left == line } else { r.right == line });
        match row {
            Some(r) => {
                self.scroll = Some(r);
                self.current = self.shown_hunks.iter().rposition(|&h| h <= r);
            }
            None => {
                // Está dentro de un pliegue: mostrar todo y reintentar en el próximo cuadro.
                self.want_all = true;
                self.pending = Some((left, right));
            }
        }
    }

    pub fn position_label(&self) -> String {
        match self.current {
            Some(c) => format!("{} de {}", c + 1, self.shown_hunks.len()),
            None => format!("{} diferencias", self.shown_hunks.len()),
        }
    }
}

struct Colors {
    del_line: Color32,
    del_text: Color32,
    add_line: Color32,
    add_text: Color32,
    filler: Color32,
    num: Color32,
    current: Color32,
}

impl Colors {
    fn new(dark: bool, ui: &Ui) -> Self {
        let (r, g) = if dark {
            (Color32::from_rgb(0xE0, 0x40, 0x40), Color32::from_rgb(0x40, 0xC0, 0x60))
        } else {
            (Color32::from_rgb(0xE0, 0x30, 0x30), Color32::from_rgb(0x20, 0xA0, 0x40))
        };
        Self {
            del_line: r.gamma_multiply(0.22),
            del_text: r.gamma_multiply(0.55),
            add_line: g.gamma_multiply(0.20),
            add_text: g.gamma_multiply(0.50),
            filler: ui.visuals().weak_text_color().gamma_multiply(0.08),
            num: ui.visuals().weak_text_color(),
            current: ui.visuals().hyperlink_color,
        }
    }
}

/// Dibuja la comparación lado a lado, con una regla de diferencias a la derecha.
pub fn show(
    ui: &mut Ui,
    d: &Diff,
    st: &mut DiffView,
    titles: (&str, &str),
    only_changes: bool,
    pal: &Palette,
    query: &str,
) {
    st.ensure(d, only_changes);
    if !only_changes {
        if let Some((l, r)) = st.pending.take() {
            st.reveal(d, l, r);
        }
    }
    let dark = ui.visuals().dark_mode;
    let col = Colors::new(dark, ui);
    let font = TextStyle::Monospace.resolve(ui.style());
    let (row_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font).round() + 2.0, f.glyph_width(&font, '0')));
    let digits = (d.left.lines.len().max(d.right.lines.len()).max(1).ilog10() + 1) as f32;
    let num_w = digits * char_w + 14.0;
    let ruler_w = 12.0;

    // Encabezados con el nombre de cada lado.
    let full = ui.available_rect_before_wrap();
    let half_w = ((full.width() - ruler_w) / 2.0).floor();
    let head_h = row_h + 4.0;
    let (head, _) = ui.allocate_exact_size(vec2(full.width(), head_h), Sense::hover());
    for (i, t) in [titles.0, titles.1].into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(head.left() + i as f32 * half_w, head.top()), vec2(half_w, head_h));
        ui.painter().rect_filled(r.shrink(1.0), 3.0, ui.visuals().faint_bg_color);
        ui.painter().text(
            pos2(r.left() + 8.0, r.center().y),
            egui::Align2::LEFT_CENTER,
            if i == 0 { format!("◀ {t}") } else { format!("▶ {t}") },
            TextStyle::Button.resolve(ui.style()),
            ui.visuals().strong_text_color(),
        );
    }

    let body = Rect::from_min_max(pos2(full.left(), head.bottom()), full.max);

    // Desplazamiento horizontal: común a los dos lados; los números de línea no se mueven.
    let text_w = half_w - num_w - 4.0;
    let max_h = ((d.max_cols as f32 + 2.0) * char_w - text_w).max(0.0);
    let bar_h = if max_h > 0.0 { 10.0 } else { 0.0 };
    let list_rect = Rect::from_min_max(body.min, pos2(body.left() + half_w * 2.0, body.bottom() - bar_h));
    if ui.rect_contains_pointer(list_rect) {
        st.h_offset -= ui.input(|i| i.smooth_scroll_delta().x);
    }
    st.h_offset = st.h_offset.clamp(0.0, max_h);
    let h_off = st.h_offset;
    let ruler = Rect::from_min_max(pos2(list_rect.right(), body.top()), body.max);

    let mut area = ScrollArea::vertical().auto_shrink(false).id_salt("diff_view");
    if let Some(row) = st.scroll.take() {
        if let Some(off) = common::scroll_for_row(row, row_h, true, st.offset, st.view_h) {
            area = area.vertical_scroll_offset(off);
        }
    }
    let shown = &st.shown;
    let cur_row = st.current.map(|c| st.shown_hunks[c]);
    let cur_end = cur_row.map(|s| {
        let mut e = s;
        while e < shown.len() && !matches!(shown[e].kind, RowKind::Same | RowKind::Fold) {
            e += 1;
        }
        e
    });
    let mut unfold = false;
    let output = ui
        .scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            // Que la rueda horizontal no mueva el desplazamiento vertical.
            ui.style_mut().always_scroll_the_only_direction = false;
            area.show_rows(ui, row_h, shown.len(), |ui, range| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for ri in range {
                    let row = shown[ri];
                    let (rect, resp) = ui.allocate_exact_size(vec2(half_w * 2.0, row_h), Sense::click());
                    if row.kind == RowKind::Fold {
                        ui.painter().rect_filled(rect, 0.0, col.filler);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("⋯ {} líneas iguales (doble clic para mostrar todo)", row.left),
                            font.clone(),
                            col.num,
                        );
                        if resp.double_clicked() {
                            unfold = true;
                        }
                        continue;
                    }
                    let in_current = cur_row.zip(cur_end).is_some_and(|(s, e)| ri >= s && ri < e);
                    let (l_txt, r_txt) = (
                        (row.left != NO_LINE).then(|| d.left.lines[row.left as usize].as_str()),
                        (row.right != NO_LINE).then(|| d.right.lines[row.right as usize].as_str()),
                    );
                    let (l_hi, r_hi) = match (row.kind, l_txt, r_txt) {
                        (RowKind::Changed, Some(a), Some(b)) => inline_ranges(a, b),
                        _ => (Vec::new(), Vec::new()),
                    };
                    for (side, txt, line, hi) in [(0, l_txt, row.left, l_hi), (1, r_txt, row.right, r_hi)] {
                        let half = Rect::from_min_size(
                            pos2(rect.left() + side as f32 * half_w, rect.top()),
                            vec2(half_w, row_h),
                        );
                        let (line_bg, text_bg) = match (row.kind, side) {
                            (RowKind::Same, _) => (Color32::TRANSPARENT, Color32::TRANSPARENT),
                            (_, 0) => (col.del_line, col.del_text),
                            _ => (col.add_line, col.add_text),
                        };
                        let p = ui.painter().with_clip_rect(half.intersect(ui.clip_rect()));
                        match txt {
                            None => {
                                p.rect_filled(half, 0.0, col.filler);
                            }
                            Some(text) => {
                                p.rect_filled(half, 0.0, line_bg);
                                p.text(
                                    pos2(half.left() + num_w - 8.0, half.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    (line + 1).to_string(),
                                    font.clone(),
                                    col.num,
                                );
                                let mut s = JobSink::new(pal, font.clone(), query);
                                push_highlighted(&mut s, text, &hi, text_bg, pal);
                                let galley = ui.fonts_mut(|f| f.layout_job(s.finish()));
                                let text_clip =
                                    Rect::from_x_y_ranges(half.left() + num_w - 2.0..=half.right(), half.y_range());
                                p.with_clip_rect(text_clip.intersect(p.clip_rect())).galley(
                                    pos2(half.left() + num_w - h_off, half.center().y - galley.size().y / 2.0),
                                    galley,
                                    pal.plain,
                                );
                            }
                        }
                        if in_current {
                            p.rect_filled(Rect::from_min_size(half.min, vec2(3.0, row_h)), 0.0, col.current);
                        }
                    }
                    // Separador entre los dos lados.
                    let x = rect.left() + half_w;
                    ui.painter().vline(
                        x,
                        rect.y_range(),
                        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                    );
                }
            })
        })
        .inner;
    st.offset = output.state.offset.y;
    st.view_h = output.inner_rect.height();
    if unfold {
        st.want_all = true;
    }

    // Barra horizontal bajo los dos lados.
    if max_h > 0.0 {
        let track =
            Rect::from_min_max(pos2(list_rect.left(), list_rect.bottom()), pos2(list_rect.right(), body.bottom()));
        let resp = ui.interact(track, ui.id().with("diff_hbar"), Sense::click_and_drag());
        let visible = text_w / (text_w + max_h);
        let thumb_w = (track.width() * visible).max(24.0);
        let free = track.width() - thumb_w;
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.dragged()) {
            let t = ((pos.x - track.left() - thumb_w / 2.0) / free.max(1.0)).clamp(0.0, 1.0);
            st.h_offset = t * max_h;
        }
        let x = track.left() + st.h_offset / max_h * free;
        let thumb = Rect::from_min_size(pos2(x, track.top() + 2.0), vec2(thumb_w, track.height() - 4.0));
        let w = if resp.hovered() || resp.dragged() {
            &ui.visuals().widgets.hovered
        } else {
            &ui.visuals().widgets.inactive
        };
        ui.painter().rect_filled(track, 0.0, ui.visuals().extreme_bg_color);
        ui.painter().rect_filled(thumb, 3.0, w.bg_fill);
    }

    // Regla con la posición de cada diferencia; clic para saltar.
    let resp = ui.interact(ruler, ui.id().with("diff_ruler"), Sense::click_and_drag());
    let p = ui.painter();
    p.rect_filled(ruler, 0.0, ui.visuals().extreme_bg_color);
    let total = st.shown.len().max(1) as f32;
    let y_of = |row: usize| ruler.top() + row as f32 / total * ruler.height();
    for (i, r) in st.shown.iter().enumerate() {
        let c = match r.kind {
            RowKind::Removed => col.del_text,
            RowKind::Added => col.add_text,
            RowKind::Changed => col.current.gamma_multiply(0.7),
            _ => continue,
        };
        let y = y_of(i);
        p.rect_filled(Rect::from_x_y_ranges(ruler.left() + 2.0..=ruler.right() - 2.0, y..=y + 2.0), 0.0, c);
    }
    // Zona visible.
    let vis_top = y_of((st.offset / row_h) as usize);
    let vis_h = (st.view_h / row_h) / total * ruler.height();
    p.rect_stroke(
        Rect::from_min_size(pos2(ruler.left(), vis_top), vec2(ruler.width(), vis_h.max(4.0))),
        0.0,
        Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Inside,
    );
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.dragged()) {
        let row = (((pos.y - ruler.top()) / ruler.height()) * total) as usize;
        st.scroll = Some(row.min(st.shown.len().saturating_sub(1)));
    }
}

/// Lista de cambios por ruta; clic en uno lo muestra en la comparación.
pub fn changes_list(ui: &mut Ui, d: &Diff, st: &mut DiffView) {
    let more = if d.changes_truncated { "+" } else { "" };
    ui.strong(format!("Cambios por ruta ({}{more})", d.changes.len()));
    ui.label(RichText::new("Objetos por clave, arreglos por posición.").weak().small());
    ui.add_space(4.0);
    let col = Colors::new(ui.visuals().dark_mode, ui);
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Monospace);
    ScrollArea::vertical().id_salt("diff_changes").auto_shrink(false).show_rows(
        ui,
        row_h,
        d.changes.len(),
        |ui, range| {
            for c in &d.changes[range] {
                let (sym, color, what) = match c.kind {
                    ChangeKind::Changed => ("~ ", col.current, "Modificado"),
                    ChangeKind::Removed => ("− ", col.del_text.gamma_multiply(1.8), "Solo a la izquierda"),
                    ChangeKind::Added => ("+ ", col.add_text.gamma_multiply(1.8), "Solo a la derecha"),
                };
                let mut job = egui::text::LayoutJob::default();
                let fmt = |color| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
                job.append(sym, 0.0, fmt(color));
                job.append(&c.path, 0.0, fmt(ui.visuals().text_color()));
                let resp = ui
                    .add(egui::Label::new(job).sense(Sense::click()).truncate())
                    .on_hover_text(format!("{what}\n{}\n{}", c.path, c.summary));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    st.reveal(d, c.left, c.right);
                }
            }
        },
    );
}

/// Colorea la línea y marca con fondo los rangos cambiados.
fn push_highlighted(s: &mut JobSink, line: &str, hi: &[Range<usize>], bg: Color32, pal: &Palette) {
    json::lex_line(line, |r, tok| {
        let color = pal.tok(tok);
        let mut pos = r.start;
        for h in hi.iter().filter(|h| h.start < r.end && h.end > r.start) {
            let (a, b) = (h.start.max(r.start), h.end.min(r.end));
            if a > pos {
                s.colored(&line[pos..a], color);
            }
            s.with_bg(&line[a..b], color, bg);
            pos = b;
        }
        if pos < r.end {
            s.colored(&line[pos..r.end], color);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    #[test]
    fn line_and_structural_diff() {
        let a = parse(r#"{"a":1,"b":{"c":"x","d":true},"e":[1,2]}"#).unwrap();
        let b = parse(r#"{"b":{"c":"y","d":true},"a":1,"e":[1,2,3],"f":null}"#).unwrap();
        let d = Diff::new(&a, &b, "  ", true);
        let kinds: Vec<_> = d.changes.iter().map(|c| (c.kind, c.path.as_str())).collect();
        assert!(kinds.contains(&(ChangeKind::Changed, "$.b.c")));
        assert!(kinds.contains(&(ChangeKind::Added, "$.e[2]")));
        assert!(kinds.contains(&(ChangeKind::Added, "$.f")));
        assert_eq!(d.changes.len(), 3);
        assert!(!d.is_identical());

        let same = Diff::new(&a, &parse(r#"{"e":[1,2],"b":{"d":true,"c":"x"},"a":1}"#).unwrap(), "  ", true);
        assert!(same.is_identical() && same.changes.is_empty());
    }

    #[test]
    fn inline() {
        let (a, b) = inline_ranges(r#""c": "xé","#, r#""c": "yé","#);
        assert_eq!((a, b), (vec![6..7], vec![6..7]));
    }
}
