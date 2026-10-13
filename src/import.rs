//! Importar una tabla copiada de SQL Server Management Studio («Copy with Headers»): columnas separadas
//! por tabulador y una fila por línea. Se convierte en un arreglo de objetos, uno por fila.

use eframe::egui::{self, Id, Key, RichText};
use serde::{Deserialize, Serialize};

use crate::json;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportOptions {
    /// La primera línea trae los nombres de las columnas.
    pub header: bool,
    /// `NULL` se convierte en `null`.
    pub nulls: bool,
    /// Los valores con forma de número JSON se escriben sin comillas.
    pub numbers: bool,
    /// Con una sola fila se genera un objeto en lugar de un arreglo.
    pub single_object: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { header: true, nulls: true, numbers: true, single_object: false }
    }
}

pub struct Table {
    pub cols: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Filas con más o menos valores que columnas.
    fn irregular(&self) -> usize {
        self.rows.iter().filter(|r| r.len() != self.cols.len()).count()
    }
}

/// Lee la tabla. `None` si no hay nada que leer.
pub fn parse_table(text: &str, header: bool) -> Option<Table> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty()).peekable();
    let first: Vec<&str> = lines.peek()?.split('\t').collect();
    let ncols = first.len();
    let mut cols = if header {
        lines.next();
        unique_names(&first)
    } else {
        (1..=ncols).map(|i| format!("Columna{i}")).collect()
    };
    let mut rows = Vec::new();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        let mut fields: Vec<String> = line.split('\t').map(String::from).collect();
        // Un valor con saltos de línea parte la fila: se une con las siguientes mientras falten columnas.
        while fields.len() < ncols {
            let Some(next) = lines.peek() else { break };
            if fields.len() + next.split('\t').count() - 1 > ncols {
                break;
            }
            let mut parts = next.split('\t');
            if let (Some(last), Some(head)) = (fields.last_mut(), parts.next()) {
                last.push('\n');
                last.push_str(head);
            }
            fields.extend(parts.map(String::from));
            lines.next();
        }
        rows.push(fields);
    }
    let widest = rows.iter().map(Vec::len).max().unwrap_or(0);
    for i in cols.len()..widest {
        cols.push(format!("Columna{}", i + 1));
    }
    Some(Table { cols, rows })
}

/// Nombres de columna sin vacíos ni repetidos (`Code`, `Code_2`…).
fn unique_names(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for (i, n) in names.iter().enumerate() {
        let base = match n.trim() {
            "" => format!("Columna{}", i + 1),
            n => n.to_string(),
        };
        let mut name = base.clone();
        let mut k = 2;
        while out.contains(&name) {
            name = format!("{base}_{k}");
            k += 1;
        }
        out.push(name);
    }
    out
}

/// Número JSON válido: `-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`. Así `001` o `1.` quedan como texto.
fn is_number(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |i: &mut usize| {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > start
    };
    let mut i = usize::from(b.first() == Some(&b'-'));
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

/// El JSON con sangría `indent`. Los números conservan su texto (`34.800000`).
pub fn to_json(t: &Table, o: &ImportOptions, indent: &str) -> String {
    let single = o.single_object && t.rows.len() == 1;
    let pad = if single { "" } else { indent };
    let mut out = String::new();
    if !single {
        out.push('[');
    }
    for (r, row) in t.rows.iter().enumerate() {
        if !single {
            out.push_str(if r == 0 { "\n" } else { ",\n" });
            out.push_str(indent);
        }
        out.push('{');
        for (c, name) in t.cols.iter().enumerate() {
            out.push_str(if c == 0 { "\n" } else { ",\n" });
            out.push_str(pad);
            out.push_str(indent);
            json::write_escaped(name, &mut out);
            out.push_str(": ");
            match row.get(c).map(String::as_str) {
                None => out.push_str("null"),
                Some("NULL") if o.nulls => out.push_str("null"),
                Some(v) if o.numbers && is_number(v) => out.push_str(v),
                Some(v) => json::write_escaped(v, &mut out),
            }
        }
        if !t.cols.is_empty() {
            out.push('\n');
            out.push_str(pad);
        }
        out.push('}');
    }
    if !single {
        if !t.rows.is_empty() {
            out.push('\n');
        }
        out.push(']');
    }
    out.push('\n');
    out
}

/// Texto pegado que parece una tabla de SSMS y no un JSON: al menos dos columnas y filas parejas.
pub fn looks_like_table(text: &str) -> bool {
    if text.trim_start().starts_with(['{', '[', '"']) {
        return false;
    }
    parse_table(text, true).is_some_and(|t| t.cols.len() >= 2 && !t.rows.is_empty() && t.irregular() == 0)
}

// ---------------------------------------------------------------- ventana

pub struct Dialog {
    pub text: String,
    /// Convertir en el próximo cuadro (Ctrl+Enter, que se lee en la entrada global).
    pub submit: bool,
    focus: bool,
    /// (filas, columnas, filas irregulares) del último análisis; `None` si no hay texto.
    summary: Option<(usize, usize, usize)>,
    /// El texto o las opciones cambiaron y hay que volver a analizar.
    dirty: bool,
    header: bool,
}

impl Dialog {
    pub fn new(text: String) -> Self {
        Self { text, submit: false, focus: true, summary: None, dirty: true, header: true }
    }
}

pub enum Action {
    /// JSON generado y número de filas.
    Convert(String, usize),
    Close,
}

pub fn window(ctx: &egui::Context, d: &mut Dialog, o: &mut ImportOptions, indent: &str) -> Option<Action> {
    let mut action = None;
    let mut open = true;
    egui::Window::new("Importar tabla de SQL Server")
        .id(Id::new("import_window"))
        .open(&mut open)
        .default_size([820.0, 460.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label("Pega aquí el resultado de «Copy with Headers» (Ctrl+Mayús+C) de SQL Server Management Studio.");
            ui.label(
                RichText::new(
                    "Columnas separadas por tabulador, una fila por línea. Cada fila se convierte en un objeto.",
                )
                .weak(),
            );
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut o.header, "Primera fila con encabezados");
                ui.checkbox(&mut o.nulls, "NULL → null");
                ui.checkbox(&mut o.numbers, "Detectar números")
                    .on_hover_text("Los valores con forma de número se escriben sin comillas; 001 queda como texto");
                ui.checkbox(&mut o.single_object, "Una sola fila como objeto")
                    .on_hover_text("Si la tabla tiene una sola fila, genera un objeto en lugar de un arreglo");
            });
            ui.add_space(4.0);

            let h = (ui.available_height() - 34.0).max(120.0);
            egui::ScrollArea::both().id_salt("import_scroll").max_height(h).auto_shrink([false, false]).show(
                ui,
                |ui| {
                    let resp = ui.add(
                        egui::TextEdit::multiline(&mut d.text)
                            .id(Id::new("import_text"))
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(16)
                            .lock_focus(true)
                            .hint_text("Code\tName\tU_SO1_AUTORIZADO\t…\nNULL\tCMXMX2CM54\tN\t…"),
                    );
                    if d.focus {
                        resp.request_focus();
                        d.focus = false;
                    }
                    if resp.changed() {
                        d.dirty = true;
                    }
                },
            );

            if d.dirty || d.header != o.header {
                d.dirty = false;
                d.header = o.header;
                d.summary = parse_table(&d.text, o.header).map(|t| (t.rows.len(), t.cols.len(), t.irregular()));
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match d.summary {
                    Some((rows, cols, irregular)) => {
                        let f = if rows == 1 { "fila" } else { "filas" };
                        ui.label(format!("{rows} {f} × {cols} columnas"));
                        if irregular > 0 {
                            ui.label(
                                RichText::new(format!("· {irregular} con un número distinto de columnas"))
                                    .color(ui.visuals().warn_fg_color),
                            );
                        }
                    }
                    None => {
                        ui.label(RichText::new("Sin datos").weak());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancelar").clicked() {
                        action = Some(Action::Close);
                    }
                    let ok = d.summary.is_some_and(|(rows, _, _)| rows > 0);
                    let convert = ui.add_enabled(ok, egui::Button::new("Convertir a JSON")).on_hover_text("Ctrl+Enter");
                    if ui.add_enabled(!d.text.is_empty(), egui::Button::new("Limpiar")).clicked() {
                        d.text.clear();
                        d.dirty = true;
                        d.focus = true;
                    }
                    let submit = std::mem::take(&mut d.submit);
                    if let Some(t) =
                        (ok && (convert.clicked() || submit)).then(|| parse_table(&d.text, o.header)).flatten()
                    {
                        action = Some(Action::Convert(to_json(&t, o, indent), t.rows.len()));
                    }
                });
            });
        });
    if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
        action = Some(Action::Close);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Code\tName\tU_SO1_TOTALNETO\tU_SO1_FECHA\tU_SO1_HORA\tU_SO1_COMENTARIO\tU_SO1_TIPO\r\n\
        NULL\tCMXMX2CM54\t34.800000\t2026-10-08 00:00:00.000\t1218\t\t001\r\n\
        CMXMX1CM328\tCMXMX1CM328\t34.800000\t2026-10-08 00:00:00.000\t1219\tBasado en \"operación\"\t-1\r\n";

    #[test]
    fn converts_ssms_table() {
        let t = parse_table(SAMPLE, true).unwrap();
        assert_eq!((t.rows.len(), t.cols.len(), t.irregular()), (2, 7, 0));
        let s = to_json(&t, &ImportOptions::default(), "  ");
        let doc = json::parse(&s).unwrap();
        assert_eq!(
            json::to_string(&doc, 0, json::Layout::Minified),
            r#"[{"Code":null,"Name":"CMXMX2CM54","U_SO1_TOTALNETO":34.800000,"U_SO1_FECHA":"2026-10-08 00:00:00.000","U_SO1_HORA":1218,"U_SO1_COMENTARIO":"","U_SO1_TIPO":"001"},{"Code":"CMXMX1CM328","Name":"CMXMX1CM328","U_SO1_TOTALNETO":34.800000,"U_SO1_FECHA":"2026-10-08 00:00:00.000","U_SO1_HORA":1219,"U_SO1_COMENTARIO":"Basado en \"operación\"","U_SO1_TIPO":-1}]"#
        );
        assert!(looks_like_table(SAMPLE));
        assert!(!looks_like_table("{\n\t\"a\": 1\n}"));
    }

    #[test]
    fn options() {
        let t = parse_table("A\tB\nNULL\t5\n", true).unwrap();
        let o = ImportOptions { nulls: false, numbers: false, single_object: true, ..Default::default() };
        assert_eq!(to_json(&t, &o, "  "), "{\n  \"A\": \"NULL\",\n  \"B\": \"5\"\n}\n");
        let t = parse_table("x\ty\n", false).unwrap();
        assert_eq!(t.cols, ["Columna1", "Columna2"]);
        assert_eq!(t.rows, [["x", "y"]]);
    }

    #[test]
    fn multiline_values_and_names() {
        let t = parse_table("Code\tCode\t\tC\n1\tuno\ndos\t2\t3\n4\t5\t6\t7\n", true).unwrap();
        assert_eq!(t.cols, ["Code", "Code_2", "Columna3", "C"]);
        assert_eq!(t.rows, [vec!["1", "uno\ndos", "2", "3"], vec!["4", "5", "6", "7"]]);
    }

    #[test]
    fn numbers() {
        for n in ["0", "-1", "34.800000", "1e5", "2.5E-3"] {
            assert!(is_number(n), "{n}");
        }
        for s in ["", "-", "001", "1.", ".5", "1e", "12:18PM", "5.4.0", "0x1"] {
            assert!(!is_number(s), "{s}");
        }
    }
}
