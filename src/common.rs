//! Piezas compartidas por las vistas.

use eframe::egui::{self, Color32, Painter, Pos2, Shape, Stroke, Ui, vec2};

use crate::json::{self, Doc, Kind, Layout, NodeId};
use crate::settings::Settings;

/// Petición de una vista hacia la aplicación.
pub enum Out {
    /// Abrir la ventana de cuadrícula para un arreglo.
    Grid(NodeId),
    /// Agregar una clave al filtro.
    FilterKey(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tri {
    Open,
    Closed,
    Compact,
}

/// Dibuja el triángulo de plegado centrado en `c`.
pub fn triangle(p: &Painter, c: Pos2, size: f32, t: Tri, color: Color32) {
    let s = size * 0.26;
    let pts = match t {
        Tri::Open => vec![c + vec2(-s, -s * 0.55), c + vec2(s, -s * 0.55), c + vec2(0.0, s * 0.75)],
        Tri::Closed | Tri::Compact => vec![c + vec2(-s * 0.55, -s), c + vec2(-s * 0.55, s), c + vec2(s * 0.75, 0.0)],
    };
    if t == Tri::Compact {
        p.add(Shape::convex_polygon(pts, Color32::TRANSPARENT, Stroke::new(1.2, color)));
    } else {
        p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
    }
}

/// Opciones de copiado comunes a los menús contextuales.
pub fn node_menu(ui: &mut Ui, doc: &Doc, id: NodeId, set: &Settings) -> Option<Out> {
    let n = doc.node(id);
    let mut out = None;
    if ui.button("Copiar valor (formateado)").clicked() {
        ui.ctx().copy_text(json::to_string(doc, id, Layout::Pretty(&set.indent_str())));
        ui.close();
    }
    if n.kind.is_container() && ui.button("Copiar valor (una línea)").clicked() {
        ui.ctx().copy_text(json::to_string(doc, id, Layout::Compact));
        ui.close();
    }
    if n.kind.is_container() && ui.button("Copiar valor (minificado)").clicked() {
        ui.ctx().copy_text(json::to_string(doc, id, Layout::Minified));
        ui.close();
    }
    if n.kind == Kind::Str && ui.button("Copiar texto sin comillas").clicked() {
        ui.ctx().copy_text(doc.text_of(n).to_string());
        ui.close();
    }
    if ui.button("Copiar ruta").clicked() {
        ui.ctx().copy_text(doc.path(id));
        ui.close();
    }
    if let Some(k) = doc.key_of(n) {
        if ui.button("Copiar clave").clicked() {
            ui.ctx().copy_text(k.to_string());
            ui.close();
        }
        ui.separator();
        if ui.button("Filtrar por esta clave").clicked() {
            out = Some(Out::FilterKey(k.to_string()));
            ui.close();
        }
    }
    if n.kind == Kind::Array && !n.children.is_empty() {
        ui.separator();
        if ui.button("Ver como cuadrícula…").clicked() {
            out = Some(Out::Grid(id));
            ui.close();
        }
    }
    out
}

/// Teclas de navegación pulsadas en este cuadro.
#[derive(Default)]
pub struct NavKeys {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub enter: bool,
    pub page_up: bool,
    pub page_down: bool,
    pub home: bool,
    pub end: bool,
    pub copy: bool,
    pub letter: Option<char>,
}

impl NavKeys {
    pub fn read(ui: &Ui) -> Self {
        use egui::Key;
        ui.input(|i| {
            let plain = i.modifiers.is_none();
            let letter = [Key::C, Key::H, Key::E, Key::A, Key::X, Key::G]
                .into_iter()
                .find(|&k| plain && i.key_pressed(k))
                .map(|k| k.name().chars().next().unwrap_or(' ').to_ascii_lowercase());
            Self {
                up: i.key_pressed(Key::ArrowUp),
                down: i.key_pressed(Key::ArrowDown),
                left: i.key_pressed(Key::ArrowLeft),
                right: i.key_pressed(Key::ArrowRight),
                enter: i.key_pressed(Key::Enter) || (plain && i.key_pressed(Key::Space)),
                page_up: i.key_pressed(Key::PageUp),
                page_down: i.key_pressed(Key::PageDown),
                home: i.key_pressed(Key::Home),
                end: i.key_pressed(Key::End),
                copy: i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                letter,
            }
        })
    }

    /// Desplazamiento de fila pedido por las flechas/páginas (`None` = sin movimiento).
    pub fn row_delta(&self, page: isize) -> Option<isize> {
        if self.up {
            Some(-1)
        } else if self.down {
            Some(1)
        } else if self.page_up {
            Some(-page)
        } else if self.page_down {
            Some(page)
        } else if self.home {
            Some(isize::MIN / 2)
        } else if self.end {
            Some(isize::MAX / 2)
        } else {
            None
        }
    }
}

/// Calcula el desplazamiento vertical para mostrar la fila `row`.
pub fn scroll_for_row(row: usize, row_h: f32, center: bool, offset: f32, view_h: f32) -> Option<f32> {
    let y = row as f32 * row_h;
    if center {
        return Some((y - view_h / 2.0 + row_h / 2.0).max(0.0));
    }
    if y < offset {
        Some(y)
    } else if y + row_h > offset + view_h {
        Some((y + row_h - view_h).max(0.0))
    } else {
        None
    }
}

/// Tecla G sobre un arreglo seleccionado: abrir la cuadrícula.
pub fn grid_key(ui: &Ui, doc: &Doc, sel: NodeId) -> Option<Out> {
    if sel == crate::json::NONE || !ui.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::G)) {
        return None;
    }
    let n = doc.node(sel);
    (n.kind == Kind::Array && !n.children.is_empty()).then_some(Out::Grid(sel))
}
