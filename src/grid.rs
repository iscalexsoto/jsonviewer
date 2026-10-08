//! Ventana de cuadrícula: un arreglo de objetos como hoja de cálculo.

use eframe::egui::{self, Sense, TextStyle};
use egui_extras::{Column, TableBuilder};

use crate::highlight::{JobSink, Palette};
use crate::json::{self, Doc, KeyId, Kind, Layout, NodeId};

pub struct Grid {
    pub node: NodeId,
    pub open: bool,
    cols: Vec<KeyId>,
    /// Hay elementos que no son objetos (columna "(valor)").
    has_other: bool,
    title: String,
}

impl Grid {
    pub fn new(doc: &Doc, node: NodeId) -> Self {
        let mut cols: Vec<KeyId> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut has_other = false;
        for &c in &doc.node(node).children {
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
        Self { node, open: true, cols, has_other, title: format!("Cuadrícula — {}", doc.path(node)) }
    }
}

/// Muestra la ventana. Devuelve el nodo pulsado (para mostrarlo en la vista principal).
pub fn window(ctx: &egui::Context, doc: &Doc, g: &mut Grid, pal: &Palette, query: &str) -> Option<NodeId> {
    let mut clicked = None;
    let mut open = g.open;
    egui::Window::new(g.title.as_str())
        .id(egui::Id::new("grid_window"))
        .open(&mut open)
        .default_size([900.0, 420.0])
        .resizable(true)
        .show(ctx, |ui| {
            let font = TextStyle::Monospace.resolve(ui.style());
            let row_h = ui.fonts_mut(|f| f.row_height(&font)).round() + 4.0;
            let items = &doc.node(g.node).children;
            let ncols = g.cols.len() + g.has_other as usize;
            egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
                let mut tb = TableBuilder::new(ui)
                    .id_salt(("grid", g.node))
                    .striped(true)
                    .resizable(true)
                    .sense(Sense::click())
                    .auto_shrink([false, false])
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(48.0));
                for _ in 0..ncols {
                    tb = tb.column(Column::initial(150.0).at_least(40.0).clip(true));
                }
                tb.header(22.0, |mut h| {
                    h.col(|ui| {
                        ui.strong("#");
                    });
                    for c in &g.cols {
                        h.col(|ui| {
                            ui.strong(doc.key_of_id(*c));
                        });
                    }
                    if g.has_other {
                        h.col(|ui| {
                            ui.strong("(valor)");
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, items.len(), |mut row| {
                        let item = items[row.index()];
                        let n = doc.node(item);
                        let (_, r) = row.col(|ui| {
                            ui.label(egui::RichText::new(n.index.to_string()).weak());
                        });
                        if r.clicked() {
                            clicked = Some(item);
                        }
                        let mut cell = |row: &mut egui_extras::TableRow, target: Option<NodeId>| {
                            let (_, r) = row.col(|ui| {
                                if let Some(t) = target {
                                    let mut s = JobSink::new(pal, font.clone(), query);
                                    json::write_value(doc, t, Layout::Compact, 200, &mut s);
                                    ui.add(egui::Label::new(s.finish()).selectable(false).truncate());
                                }
                            });
                            if r.clicked() {
                                clicked = Some(target.unwrap_or(item));
                            }
                            if let Some(t) = target {
                                r.on_hover_ui(|ui| {
                                    let mut s = JobSink::new(pal, font.clone(), "");
                                    json::write_value(doc, t, Layout::Pretty("  "), 3000, &mut s);
                                    ui.label(s.finish());
                                });
                            }
                        };
                        for c in &g.cols {
                            let target = if n.kind == Kind::Object {
                                n.children.iter().copied().find(|&f| doc.node(f).key == *c)
                            } else {
                                None
                            };
                            cell(&mut row, target);
                        }
                        if g.has_other {
                            cell(&mut row, (n.kind != Kind::Object).then_some(item));
                        }
                    });
                });
            });
        });
    g.open = open;
    clicked
}
