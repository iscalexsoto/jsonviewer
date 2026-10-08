//! Colores y construcción de texto coloreado.

use eframe::egui::{Color32, FontId, TextFormat, text::LayoutJob};

use crate::json::{Sink, Tok};

#[derive(Clone)]
pub struct Palette {
    pub key: Color32,
    pub string: Color32,
    pub number: Color32,
    pub boolean: Color32,
    pub null: Color32,
    pub punct: Color32,
    pub plain: Color32,
    pub faint: Color32,
    pub guide: Color32,
    pub find_bg: Color32,
    pub fold_bg: Color32,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            key: Color32::from_rgb(0x9C, 0xDC, 0xFE),
            string: Color32::from_rgb(0xCE, 0x91, 0x78),
            number: Color32::from_rgb(0xB5, 0xCE, 0xA8),
            boolean: Color32::from_rgb(0x56, 0x9C, 0xD6),
            null: Color32::from_rgb(0xC5, 0x86, 0xC0),
            punct: Color32::from_rgb(0xD4, 0xD4, 0xD4),
            plain: Color32::from_rgb(0xD4, 0xD4, 0xD4),
            faint: Color32::from_rgb(0x6A, 0x99, 0x55),
            guide: Color32::from_rgba_unmultiplied(0x80, 0x80, 0x80, 0x40),
            find_bg: Color32::from_rgba_unmultiplied(0xE2, 0xA0, 0x20, 0x70),
            fold_bg: Color32::from_rgba_unmultiplied(0x80, 0x80, 0x80, 0x45),
        }
    }

    pub fn light() -> Self {
        Self {
            key: Color32::from_rgb(0x04, 0x51, 0xA5),
            string: Color32::from_rgb(0xA3, 0x15, 0x15),
            number: Color32::from_rgb(0x09, 0x86, 0x58),
            boolean: Color32::from_rgb(0x00, 0x00, 0xFF),
            null: Color32::from_rgb(0xAF, 0x00, 0xDB),
            punct: Color32::from_rgb(0x30, 0x30, 0x30),
            plain: Color32::from_rgb(0x30, 0x30, 0x30),
            faint: Color32::from_rgb(0x00, 0x80, 0x00),
            guide: Color32::from_rgba_unmultiplied(0x60, 0x60, 0x60, 0x40),
            find_bg: Color32::from_rgba_unmultiplied(0xF0, 0xC0, 0x30, 0x90),
            fold_bg: Color32::from_rgba_unmultiplied(0x90, 0x90, 0x90, 0x40),
        }
    }

    pub fn tok(&self, t: Tok) -> Color32 {
        match t {
            Tok::Key => self.key,
            Tok::Str => self.string,
            Tok::Num => self.number,
            Tok::Bool => self.boolean,
            Tok::Null => self.null,
            Tok::Punct => self.punct,
            Tok::Plain => self.plain,
        }
    }
}

/// Acumula texto coloreado en un `LayoutJob`, resaltando las coincidencias de búsqueda.
pub struct JobSink<'a> {
    pub job: LayoutJob,
    pal: &'a Palette,
    font: FontId,
    /// Búsqueda en minúsculas ASCII; vacía = sin resaltado.
    query: &'a str,
    len: usize,
}

impl<'a> JobSink<'a> {
    pub fn new(pal: &'a Palette, font: FontId, query: &'a str) -> Self {
        Self { job: LayoutJob::default(), pal, font, query, len: 0 }
    }

    pub fn colored(&mut self, s: &str, color: Color32) {
        self.with_bg(s, color, Color32::TRANSPARENT);
    }

    pub fn with_bg(&mut self, s: &str, color: Color32, bg: Color32) {
        if s.is_empty() {
            return;
        }
        self.len += s.len();
        let fmt = |bg| TextFormat { font_id: self.font.clone(), color, background: bg, ..Default::default() };
        if self.query.is_empty() || s.len() < self.query.len() {
            self.job.append(s, 0.0, fmt(bg));
            return;
        }
        let lower = s.to_ascii_lowercase();
        let mut last = 0;
        for (i, m) in lower.match_indices(self.query) {
            if i > last {
                self.job.append(&s[last..i], 0.0, fmt(bg));
            }
            self.job.append(&s[i..i + m.len()], 0.0, fmt(self.pal.find_bg));
            last = i + m.len();
        }
        if last < s.len() {
            self.job.append(&s[last..], 0.0, fmt(bg));
        }
    }

    pub fn finish(self) -> LayoutJob {
        self.job
    }
}

impl Sink for JobSink<'_> {
    fn push(&mut self, s: &str, tok: Tok) {
        let c = self.pal.tok(tok);
        self.colored(s, c);
    }
    fn len(&self) -> usize {
        self.len
    }
}

/// Coincidencia de búsqueda en clave o valor de un nodo (sin distinguir mayúsculas ASCII).
pub fn node_matches(doc: &crate::json::Doc, n: &crate::json::Node, query_lower: &str) -> bool {
    let has = |s: &str| s.len() >= query_lower.len() && s.to_ascii_lowercase().contains(query_lower);
    doc.key_of(n).is_some_and(has) || (!n.kind.is_container() && has(doc.text_of(n)))
}
