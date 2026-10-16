//! Pestaña de tabla: un arreglo de elementos como hoja de cálculo. Las claves de todos
//! los objetos (en orden de aparición) son las columnas y cada elemento es una fila.
//! Una celda sin esa clave se dibuja con un guion tenue y fondo distinto, para no
//! confundirla con `null` o con una cadena vacía.

use eframe::egui::{self, Align, RichText, Sense, TextStyle, Ui};
use egui_extras::{Column, TableBuilder};

use crate::common::{self, Mark, NavKeys, Out};
use crate::highlight::{JobSink, Palette};
use crate::icons::{self, Icon};
use crate::json::{self, Doc, KeyId, Kind, Layout, NONE, NodeId};
use crate::settings::Settings;

pub struct TableState {
    /// Claves (columnas) en orden de primera aparición.
    cols: Vec<KeyId>,
    /// Hay elementos que no son objetos: se muestran en la columna «(valor)».
    has_other: bool,
    /// Número que se muestra en la columna «#» de cada fila (índice en el arreglo de origen).
    labels: Vec<u32>,
    /// Fila seleccionada (id del elemento) o `NONE`.
    pub selected: NodeId,
    scroll: Option<usize>,
    /// Celda bajo el último clic derecho (para «Copiar celda»).
    ctx_cell: Option<NodeId>,
    view_h: f32,
}

impl TableState {
    /// `labels` es el número de cada fila (una por hijo de la raíz, que debe ser un arreglo).
    pub fn new(doc: &Doc, labels: Vec<u32>) -> Self {
        let mut cols: Vec<KeyId> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut has_other = false;
        for &c in &doc.node(Doc::ROOT).children {
            let n = doc.node(c);
            if n.kind == Kind::Object {
                for &f in &n.children {
                    let k = doc.node(f).key;
                    if seen.insert(k) {
                        cols.push(k);
                    }
                }
            } else {
                has_other = true;
            }
        }
        Self { cols, has_other, labels, selected: NONE, scroll: None, ctx_cell: None, view_h: 0.0 }
    }

    pub fn rows(&self, doc: &Doc) -> usize {
        doc.node(Doc::ROOT).children.len()
    }

    pub fn cols(&self) -> usize {
        self.cols.len() + usize::from(self.has_other)
    }

    /// Selecciona la fila que contiene al nodo `id` y la muestra.
    pub fn reveal(&mut self, doc: &Doc, id: NodeId) {
        let mut cur = id;
        while cur != NONE && doc.node(cur).parent != Doc::ROOT {
            cur = doc.node(cur).parent;
        }
        if cur == NONE || cur == Doc::ROOT {
            return;
        }
        self.selected = cur;
        self.scroll = Some(doc.node(cur).index as usize);
    }

    fn handle_keys(&mut self, ui: &Ui, doc: &Doc, set: &Settings, row_h: f32) {
        let k = NavKeys::read(ui);
        let items = &doc.node(Doc::ROOT).children;
        if items.is_empty() {
            return;
        }
        let page = (self.view_h / row_h).max(1.0) as isize - 1;
        if let Some(delta) = k.row_delta(page) {
            let cur = if self.selected == NONE { -1 } else { doc.node(self.selected).index as isize };
            let r = (cur + delta).clamp(0, items.len() as isize - 1) as usize;
            self.selected = items[r];
            self.scroll = Some(r);
        } else if k.copy && self.selected != NONE {
            ui.ctx().copy_text(json::to_string(doc, self.selected, Layout::Pretty(&set.indent_str())));
        }
    }
}

/// Valor de una celda para el portapapeles: cadenas sin comillas, lo demás como JSON compacto.
fn cell_text(doc: &Doc, id: NodeId) -> String {
    let n = doc.node(id);
    if n.kind == Kind::Str { doc.text_of(n).to_string() } else { json::to_string(doc, id, Layout::Compact) }
}

/// La tabla separada por tabuladores (para pegarla en una hoja de cálculo).
pub fn to_tsv(doc: &Doc, st: &TableState) -> String {
    let mut out = String::from("#");
    for &c in &st.cols {
        out.push('\t');
        out.push_str(doc.key_of_id(c));
    }
    if st.has_other {
        out.push_str("\t(valor)");
    }
    for (r, &item) in doc.node(Doc::ROOT).children.iter().enumerate() {
        out.push('\n');
        out.push_str(&st.labels.get(r).copied().unwrap_or(r as u32).to_string());
        let n = doc.node(item);
        for &c in &st.cols {
            out.push('\t');
            if n.kind == Kind::Object
                && let Some(&f) = n.children.iter().find(|&&f| doc.node(f).key == c)
            {
                out.push_str(&cell_text(doc, f).replace(['\t', '\n', '\r'], " "));
            }
        }
        if st.has_other {
            out.push('\t');
            if n.kind != Kind::Object {
                out.push_str(&cell_text(doc, item).replace(['\t', '\n', '\r'], " "));
            }
        }
    }
    out.push('\n');
    out
}

#[allow(clippy::too_many_arguments)]
pub fn ui(
    ui: &mut Ui,
    doc: &Doc,
    st: &mut TableState,
    set: &Settings,
    pal: &Palette,
    query: &str,
    keys: bool,
    mark: Option<Mark>,
) -> Option<Out> {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.fonts_mut(|f| f.row_height(&font)).round() + 6.0;
    if keys {
        st.handle_keys(ui, doc, set, row_h);
    }
    let items = &doc.node(Doc::ROOT).children;
    let marked = mark.map_or(NONE, |m| m.id);
    let mark_bg = common::mark_bg(ui);
    let mark_color = ui.visuals().hyperlink_color;
    let missing_bg = ui.visuals().faint_bg_color.gamma_multiply(0.6);
    let weak = ui.visuals().weak_text_color();

    // Ancho inicial de cada columna: el de su nombre en la cabecera más un margen.
    let head_font = TextStyle::Body.resolve(ui.style());
    let col_w: Vec<f32> = ui.fonts_mut(|f| {
        st.cols
            .iter()
            .map(|&c| doc.key_of_id(c))
            .chain(st.has_other.then_some("(valor)"))
            .map(|name| {
                let w = f.layout_no_wrap(name.to_owned(), head_font.clone(), egui::Color32::WHITE).size().x;
                (w + 16.0).clamp(60.0, 400.0)
            })
            .collect()
    });

    let mut out = None;
    let mut select = None;
    let mut ctx_cell = None;

    egui::ScrollArea::horizontal().id_salt("table_hscroll").auto_shrink(false).show(ui, |ui| {
        let mut tb = TableBuilder::new(ui)
            .id_salt("table_view")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .auto_shrink(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(56.0));
        for &w in &col_w {
            tb = tb.column(Column::initial(w).at_least(40.0).clip(true));
        }
        if let Some(r) = st.scroll.take() {
            tb = tb.scroll_to_row(r, Some(Align::Center));
        }
        let selected = st.selected;
        let cols = &st.cols;
        let labels = &st.labels;
        let has_other = st.has_other;
        let output = tb
            .header(row_h, |mut h| {
                h.col(|ui| {
                    ui.strong("#").on_hover_text("Índice del elemento en el arreglo de origen");
                });
                for &c in cols {
                    h.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(doc.key_of_id(c)).strong()).truncate())
                            .on_hover_text(doc.key_of_id(c));
                    });
                }
                if has_other {
                    h.col(|ui| {
                        ui.strong("(valor)").on_hover_text("Elementos que no son objetos");
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, items.len(), |mut row| {
                    let r = row.index();
                    let item = items[r];
                    let n = doc.node(item);
                    let is_marked = item == marked;
                    row.set_selected(item == selected);

                    let (_, resp) = row.col(|ui| {
                        if is_marked {
                            ui.painter().rect_filled(ui.max_rect(), 0.0, mark_bg);
                        }
                        let label = labels.get(r).copied().unwrap_or(r as u32).to_string();
                        if is_marked {
                            ui.label(RichText::new(icons::text(Icon::GitCompare, &label)).color(mark_color))
                                .on_hover_text("Seleccionado para comparar");
                        } else {
                            ui.label(RichText::new(label).color(weak));
                        }
                    });
                    if resp.secondary_clicked() {
                        ctx_cell = Some(None);
                    }

                    let mut cell = |row: &mut egui_extras::TableRow, target: Option<NodeId>, key: Option<&str>| {
                        let (_, resp) = row.col(|ui| {
                            if is_marked {
                                ui.painter().rect_filled(ui.max_rect(), 0.0, mark_bg);
                            }
                            match target {
                                Some(t) => {
                                    let mut s = JobSink::new(pal, font.clone(), query);
                                    json::write_value(doc, t, Layout::Compact, 200, &mut s);
                                    ui.add(egui::Label::new(s.finish()).selectable(false).truncate());
                                }
                                None => {
                                    if !is_marked {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, missing_bg);
                                    }
                                    ui.add(egui::Label::new(RichText::new("—").color(weak)).selectable(false));
                                }
                            }
                        });
                        if resp.secondary_clicked() {
                            ctx_cell = Some(target);
                        }
                        match (target, key) {
                            (Some(t), _) => {
                                resp.on_hover_ui(|ui| {
                                    let mut s = JobSink::new(pal, font.clone(), "");
                                    json::write_value(doc, t, Layout::Pretty("  "), 3000, &mut s);
                                    ui.label(s.finish());
                                });
                            }
                            (None, Some(k)) => {
                                resp.on_hover_text(format!("Este elemento no tiene la clave «{k}»"));
                            }
                            (None, None) => {}
                        }
                    };
                    for &c in cols {
                        let target = if n.kind == Kind::Object {
                            n.children.iter().copied().find(|&f| doc.node(f).key == c)
                        } else {
                            None
                        };
                        cell(&mut row, target, Some(doc.key_of_id(c)));
                    }
                    if has_other {
                        cell(&mut row, (n.kind != Kind::Object).then_some(item), None);
                    }

                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        select = Some(item);
                    }
                    resp.context_menu(|ui| {
                        if let Some(c) = st.ctx_cell {
                            if ui.button("Copiar celda").on_hover_text("Cadenas sin comillas").clicked() {
                                ui.ctx().copy_text(cell_text(doc, c));
                                ui.close();
                            }
                            ui.separator();
                        }
                        if let Some(o) = common::node_menu(ui, doc, item, set, mark) {
                            out = Some(o);
                        }
                    });
                });
            });
        st.view_h = output.inner_rect.height();
    });

    if let Some(id) = select {
        st.selected = id;
    }
    if let Some(c) = ctx_cell {
        st.ctx_cell = c;
    }
    out
}
