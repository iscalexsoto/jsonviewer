use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum View {
    Tree,
    Table,
    Text,
    Source,
    Compare,
}

/// Cómo se pliega el árbol al abrir un documento.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum InitialFold {
    /// Compacta en una línea todo lo que quepa en `auto_width` columnas.
    Auto,
    /// Expande hasta `initial_level` niveles; lo demás queda cerrado.
    Level,
    /// Expande hasta `initial_level` niveles; lo demás queda compacto.
    LevelCompact,
    ExpandAll,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    pub font_size: f32,
    pub ui_scale: f32,
    pub indent: usize,
    pub initial: InitialFold,
    pub auto_width: usize,
    pub initial_level: usize,
    pub show_guides: bool,
    pub show_counts: bool,
    pub show_indices: bool,
    pub show_commas: bool,
    pub quote_keys: bool,
    pub line_numbers: bool,
    pub compact_limit: usize,
    pub table_preview: bool,
    /// Botones de nivel de la barra: el resto queda compacto en lugar de cerrado.
    pub level_compact: bool,
    pub view: View,
    pub recent: Vec<String>,
    pub show_settings: bool,
    pub show_filter: bool,
    pub filter_on: bool,
    /// Claves del filtro; la última fila vacía sirve para escribir una nueva.
    pub filter_keys: Vec<String>,
    /// Comparación: ordenar las claves antes de comparar (ignora el orden).
    pub diff_sort_keys: bool,
    /// Comparación: plegar las líneas iguales lejos de una diferencia.
    pub diff_only_changes: bool,
    /// Comparación: mostrar la lista de cambios por ruta.
    pub diff_show_list: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: true,
            font_size: 13.5,
            ui_scale: 1.0,
            indent: 2,
            initial: InitialFold::Auto,
            auto_width: 160,
            initial_level: 2,
            show_guides: true,
            show_counts: true,
            show_indices: false,
            show_commas: true,
            quote_keys: true,
            line_numbers: true,
            compact_limit: 4000,
            table_preview: true,
            level_compact: false,
            view: View::Tree,
            recent: Vec::new(),
            show_settings: false,
            show_filter: false,
            filter_on: true,
            filter_keys: Vec::new(),
            diff_sort_keys: true,
            diff_only_changes: false,
            diff_show_list: true,
        }
    }
}

impl Settings {
    pub fn indent_str(&self) -> String {
        if self.indent == 0 { "\t".into() } else { " ".repeat(self.indent) }
    }

    /// Claves del filtro en uso, sin vacías ni repetidas (vacío si está apagado).
    pub fn active_filter(&self) -> Vec<&str> {
        let mut v: Vec<&str> = Vec::new();
        if self.filter_on {
            for k in self.filter_keys.iter().map(|k| k.trim()) {
                if !k.is_empty() && !v.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                    v.push(k);
                }
            }
        }
        v
    }

    pub fn add_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_string());
        self.recent.truncate(12);
    }
}
