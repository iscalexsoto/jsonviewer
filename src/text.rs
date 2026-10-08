//! Vista de texto formateado (todo expandido, seleccionable).

use eframe::egui::{self, ScrollArea, Sense, TextStyle, Ui, pos2, vec2};

use crate::common;
use crate::highlight::{JobSink, Palette};
use crate::json::{self, Doc, Layout};

/// Longitud máxima de una línea mostrada (las líneas enormes se recortan).
const MAX_LINE: usize = 20_000;

#[derive(Default)]
pub struct TextState {
    text: String,
    lines: Vec<(usize, usize)>,
    built_indent: Option<String>,
    pub cur_line: Option<usize>,
    scroll: Option<usize>,
    offset: f32,
    view_h: f32,
}

impl TextState {
    pub fn ensure(&mut self, doc: &Doc, indent: &str) {
        if self.built_indent.as_deref() == Some(indent) {
            return;
        }
        self.text = json::to_string(doc, Doc::ROOT, Layout::Pretty(indent));
        self.lines.clear();
        let mut start = 0;
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                self.lines.push((start, i));
                start = i + 1;
            }
        }
        self.lines.push((start, self.text.len()));
        self.built_indent = Some(indent.to_string());
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Busca la siguiente línea que contenga `query` (minúsculas ASCII).
    pub fn find(&mut self, query: &str, forward: bool) -> bool {
        let n = self.lines.len();
        if n == 0 || query.is_empty() {
            return false;
        }
        let start = self.cur_line.unwrap_or(if forward { n - 1 } else { 0 });
        for step in 1..=n {
            let i = if forward { (start + step) % n } else { (start + n * 2 - step) % n };
            let (a, b) = self.lines[i];
            if self.text[a..b].to_ascii_lowercase().contains(query) {
                self.cur_line = Some(i);
                self.scroll = Some(i);
                return true;
            }
        }
        false
    }
}

pub fn show(ui: &mut Ui, st: &mut TextState, set: &crate::settings::Settings, pal: &Palette, query: &str) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let (row_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font).round() + 1.0, f.glyph_width(&font, '0')));
    let digits = (st.lines.len().max(1).ilog10() + 1) as f32;
    let num_w = if set.line_numbers { digits * char_w + 16.0 } else { 6.0 };
    let num_color = ui.visuals().weak_text_color();
    let cur_bg = ui.visuals().selection.bg_fill.gamma_multiply(0.35);

    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let mut area = ScrollArea::both().auto_shrink(false).id_salt("text_view");
    if let Some(li) = st.scroll.take() {
        if let Some(off) = common::scroll_for_row(li, row_h, true, st.offset, st.view_h) {
            area = area.vertical_scroll_offset(off);
        }
    }
    let lines = &st.lines;
    let text = &st.text;
    let cur = st.cur_line;
    let output = area.show_rows(ui, row_h, lines.len(), |ui, range| {
        for li in range {
            let (a, mut b) = lines[li];
            let mut cut = false;
            if b - a > MAX_LINE {
                b = a + MAX_LINE;
                while !text.is_char_boundary(b) {
                    b -= 1;
                }
                cut = true;
            }
            let line = &text[a..b];
            let mut s = JobSink::new(pal, font.clone(), query);
            json::lex_line(line, |r, tok| json::Sink::push(&mut s, &line[r], tok));
            if cut {
                s.colored(&format!(" … ({} caracteres más)", lines[li].1 - b), pal.faint);
            }
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(num_w, row_h), Sense::hover());
                if cur == Some(li) {
                    let full = egui::Rect::from_min_size(rect.min, vec2(ui.clip_rect().right() - rect.left(), row_h));
                    ui.painter().rect_filled(full, 0.0, cur_bg);
                }
                if set.line_numbers {
                    ui.painter().text(
                        pos2(rect.right() - 10.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        (li + 1).to_string(),
                        font.clone(),
                        num_color,
                    );
                }
                ui.add(egui::Label::new(s.finish()).extend());
            });
        }
    });
    st.offset = output.state.offset.y;
    st.view_h = output.inner_rect.height();
}
