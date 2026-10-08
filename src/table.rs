//! Vista de tabla estilo "Inspección" de Visual Studio: Nombre | Valor | Tipo.

use eframe::egui::{self, Rect, Sense, TextStyle, Ui, pos2, vec2};
use egui_extras::{Column, TableBuilder};

use crate::common::{self, NavKeys, Out, Tri};
use crate::highlight::{JobSink, Palette};
use crate::json::{self, Doc, Kind, Layout, NONE, NodeId, Sink, Tok};
use crate::settings::Settings;

pub struct TableState {
    pub open: Vec<bool>,
    rows: Vec<NodeId>,
    row_of: Vec<u32>,
    dirty: bool,
    pub selected: NodeId,
    scroll: Option<(NodeId, bool)>,
    view_h: f32,
}

impl TableState {
    pub fn new(doc: &Doc) -> Self {
        let mut open = vec![false; doc.len()];
        open[0] = true;
        Self { open, rows: Vec::new(), row_of: Vec::new(), dirty: true, selected: NONE, scroll: None, view_h: 0.0 }
    }

    pub fn set_open(&mut self, doc: &Doc, id: NodeId, open: bool, recursive: bool) {
        if recursive {
            self.open[id as usize..doc.subtree_end(id) as usize].fill(open);
        } else {
            self.open[id as usize] = open;
        }
        self.dirty = true;
    }

    pub fn collapse_all(&mut self) {
        self.open.fill(false);
        self.open[0] = true;
        self.dirty = true;
    }

    pub fn reveal(&mut self, doc: &Doc, id: NodeId) {
        let mut cur = doc.node(id).parent;
        while cur != NONE {
            self.open[cur as usize] = true;
            cur = doc.node(cur).parent;
        }
        self.dirty = true;
        self.selected = id;
        self.scroll = Some((id, true));
    }

    fn rebuild(&mut self, doc: &Doc) {
        self.rows.clear();
        self.row_of.clear();
        self.row_of.resize(doc.len(), u32::MAX);
        let mut stack = vec![Doc::ROOT];
        while let Some(id) = stack.pop() {
            self.row_of[id as usize] = self.rows.len() as u32;
            self.rows.push(id);
            if self.open[id as usize] {
                stack.extend(doc.node(id).children.iter().rev());
            }
        }
        self.dirty = false;
    }

    fn visible_row(&self, doc: &Doc, mut id: NodeId) -> Option<usize> {
        while id != NONE {
            let r = self.row_of[id as usize];
            if r != u32::MAX {
                return Some(r as usize);
            }
            id = doc.node(id).parent;
        }
        None
    }

    fn handle_keys(&mut self, ui: &Ui, doc: &Doc, set: &Settings, row_h: f32) {
        let k = NavKeys::read(ui);
        let page = (self.view_h / row_h).max(1.0) as isize - 1;
        if self.selected == NONE {
            if k.row_delta(page).is_some() {
                self.selected = Doc::ROOT;
            }
            return;
        }
        let sel = self.selected;
        let n = doc.node(sel);
        let has_kids = n.kind.is_container() && !n.children.is_empty();
        if let Some(delta) = k.row_delta(page) {
            let Some(cur) = self.visible_row(doc, sel) else { return };
            let r = (cur as isize + delta).clamp(0, self.rows.len() as isize - 1);
            self.selected = self.rows[r as usize];
            self.scroll = Some((self.selected, false));
        } else if k.left {
            if has_kids && self.open[sel as usize] {
                self.set_open(doc, sel, false, false);
            } else if n.parent != NONE {
                self.selected = n.parent;
                self.scroll = Some((n.parent, false));
            }
        } else if k.right {
            if has_kids && !self.open[sel as usize] {
                self.set_open(doc, sel, true, false);
            } else if has_kids {
                self.selected = n.children[0];
                self.scroll = Some((self.selected, false));
            }
        } else if k.enter && has_kids {
            let o = self.open[sel as usize];
            self.set_open(doc, sel, !o, false);
        } else if k.letter == Some('e') && has_kids {
            self.set_open(doc, sel, true, true);
        } else if k.copy {
            ui.ctx().copy_text(json::to_string(doc, sel, Layout::Pretty(&set.indent_str())));
        }
    }
}

fn type_text(doc: &Doc, id: NodeId) -> String {
    let n = doc.node(id);
    match n.kind {
        Kind::Object => format!("object ({})", n.children.len()),
        Kind::Array => {
            let first = n.children.first().map(|&c| doc.node(c).kind);
            let uniform = first.filter(|&k| n.children.iter().all(|&c| doc.node(c).kind == k));
            match uniform {
                Some(k) => format!("{}[{}]", k.name(), n.children.len()),
                None => format!("array[{}]", n.children.len()),
            }
        }
        k => k.name().to_string(),
    }
}

fn value_job(
    doc: &Doc,
    id: NodeId,
    set: &Settings,
    pal: &Palette,
    font: egui::FontId,
    query: &str,
) -> egui::text::LayoutJob {
    let n = doc.node(id);
    let mut s = JobSink::new(pal, font, query);
    match n.kind {
        Kind::Array => {
            s.push(&format!("Count = {}", n.children.len()), Tok::Plain);
            if set.table_preview && !n.children.is_empty() {
                s.push("   ", Tok::Plain);
                json::write_value(doc, id, Layout::Compact, 300, &mut s);
            }
        }
        Kind::Object if !set.table_preview => s.push("{…}", Tok::Punct),
        _ => {
            json::write_value(doc, id, Layout::Compact, 300, &mut s);
        }
    }
    s.finish()
}

pub fn ui(
    ui: &mut Ui,
    doc: &Doc,
    st: &mut TableState,
    set: &Settings,
    pal: &Palette,
    query: &str,
    keys: bool,
) -> Option<Out> {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts_mut(|f| f.row_height(&font)).round() + 4.0;
    let key_out = if keys {
        st.handle_keys(ui, doc, set, row_h);
        common::grid_key(ui, doc, st.selected)
    } else {
        None
    };
    if st.dirty {
        st.rebuild(doc);
    }

    let mut out = None;
    let mut toggle: Option<(NodeId, bool, bool)> = None; // (nodo, abrir, recursivo)
    let mut select = None;
    let indent = 16.0;
    let text_color = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();

    let mut table = TableBuilder::new(ui)
        .id_salt("table_view")
        .striped(true)
        .resizable(true)
        .sense(Sense::click())
        .auto_shrink(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(300.0).at_least(80.0).clip(true))
        .column(Column::initial(560.0).at_least(80.0).clip(true))
        .column(Column::remainder().at_least(60.0).clip(true));

    if let Some((node, center)) = st.scroll.take() {
        if let Some(r) = st.visible_row(doc, node) {
            table = table.scroll_to_row(r, center.then_some(egui::Align::Center));
        }
    }

    let rows = &st.rows;
    let open = &st.open;
    let selected = st.selected;
    let output = table
        .header(22.0, |mut h| {
            h.col(|ui| {
                ui.strong("Nombre");
            });
            h.col(|ui| {
                ui.strong("Valor");
            });
            h.col(|ui| {
                ui.strong("Tipo");
            });
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let id = rows[row.index()];
                let n = doc.node(id);
                let foldable = n.kind.is_container() && !n.children.is_empty();
                row.set_selected(id == selected);

                // Nombre
                row.col(|ui| {
                    let x0 = ui.cursor().left() + n.depth as f32 * indent;
                    let arrow =
                        Rect::from_min_size(pos2(x0, ui.max_rect().top()), vec2(indent, ui.max_rect().height()));
                    if foldable {
                        let resp = ui.interact(arrow, ui.id().with(("arrow", id)), Sense::click());
                        let t = if open[id as usize] { Tri::Open } else { Tri::Closed };
                        let c = if resp.hovered() { text_color } else { text_color.gamma_multiply(0.75) };
                        common::triangle(ui.painter(), arrow.center(), 18.0, t, c);
                        if resp.clicked() {
                            let shift = ui.input(|i| i.modifiers.shift);
                            toggle = Some((id, !open[id as usize], shift));
                            select = Some(id);
                        }
                    }
                    ui.add_space(n.depth as f32 * indent + indent + 2.0);
                    let mut s = JobSink::new(pal, font.clone(), query);
                    match doc.key_of(n) {
                        Some(k) => s.push(k, Tok::Key),
                        None if n.parent == NONE => s.colored("$", weak),
                        None => s.colored(&format!("[{}]", n.index), weak),
                    }
                    ui.add(egui::Label::new(s.finish()).selectable(false).truncate());
                });
                // Valor
                row.col(|ui| {
                    let job = value_job(doc, id, set, pal, font.clone(), query);
                    ui.add(egui::Label::new(job).selectable(false).truncate());
                });
                // Tipo
                row.col(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(type_text(doc, id)).color(weak))
                            .selectable(false)
                            .truncate(),
                    );
                });

                let resp = row.response();
                if resp.clicked() || resp.secondary_clicked() {
                    select = Some(id);
                }
                if foldable && resp.double_clicked() {
                    toggle = Some((id, !open[id as usize], false));
                }
                resp.context_menu(|ui| {
                    if foldable {
                        if ui.button("Expandir todo el subárbol").clicked() {
                            toggle = Some((id, true, true));
                            ui.close();
                        }
                        if ui.button("Contraer subárbol").clicked() {
                            toggle = Some((id, false, true));
                            ui.close();
                        }
                        ui.separator();
                    }
                    if let Some(o) = common::node_menu(ui, doc, id, set) {
                        out = Some(o);
                    }
                });
            });
        });
    st.view_h = output.inner_rect.height();

    if let Some(id) = select {
        st.selected = id;
    }
    if let Some((id, o, rec)) = toggle {
        st.set_open(doc, id, o, rec);
    }
    out.or(key_out)
}
