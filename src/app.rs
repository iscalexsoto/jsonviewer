use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id, Key, KeyboardShortcut, Modifiers,
    Rect, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2, pos2,
    text::{CCursor, CCursorRange},
    text_edit::TextEditState,
    vec2,
};

use crate::common::{Mark, Out};
use crate::diff::{self, Diff, DiffView};
use crate::grid::{self, Grid};
use crate::highlight::{self, Palette};
use crate::icons::{self, Icon};
use crate::import;
use crate::json::{self, Doc, Layout, NONE, NodeId, ParseError};
use crate::settings::{InitialFold, Settings, View};
use crate::table::{self, TableState};
use crate::text::{self, TextState};
use crate::tree::{self, Fold, TreeState};

/// Por encima de este tamaño el editor de la fuente se desactiva salvo que se pida.
const SOURCE_EDIT_LIMIT: usize = 4 * 1024 * 1024;
/// Por debajo de este tamaño la fuente se vuelve a analizar al escribir.
const LIVE_PARSE_LIMIT: usize = 512 * 1024;

const SETTINGS_KEY: &str = "jsonviewer.settings";

pub struct Loaded {
    pub doc: Doc,
    pub tree: TreeState,
    pub table: TableState,
    pub text: TextState,
    pub grid: Option<Grid>,
    last_match: NodeId,
    parse_ms: f64,
}

impl Loaded {
    fn new(doc: Doc, set: &Settings, parse_ms: f64) -> Self {
        let tree = TreeState::new(&doc, set);
        let table = TableState::new(&doc);
        Self { doc, tree, table, text: TextState::default(), grid: None, last_match: NONE, parse_ms }
    }
}

pub struct DocTab {
    uid: u64,
    title: String,
    path: Option<PathBuf>,
    source: String,
    /// La fuente se editó y no se ha vuelto a analizar.
    source_dirty: bool,
    loaded: Option<Loaded>,
    error: Option<ParseError>,
    force_edit: bool,
    /// Vista filtrada por claves (solo mientras el filtro está activo).
    filtered: Option<Loaded>,
    /// Coincidencias por cada clave de `Settings::active_filter`.
    filter_counts: Vec<usize>,
    /// Generación del filtro con la que se calculó `filtered` (0 = pendiente).
    filter_gen: u64,
    /// Aumenta cada vez que el documento se vuelve a analizar.
    doc_gen: u64,
    /// Pestaña temporal de comparación entre dos elementos (sin documento propio).
    compare: Option<ElemCompare>,
}

/// Comparación de dos elementos (de la misma pestaña o de distintas), con copia
/// propia de cada subárbol. Vive en una pestaña temporal, como el diff de VS Code.
pub struct ElemCompare {
    left: Doc,
    right: Doc,
    /// Ruta de cada lado, precedida del nombre de la pestaña si no es la misma.
    titles: (String, String),
    /// (ordenar claves, sangría) con que se calculó `diff`.
    key: Option<(bool, String)>,
    diff: Option<(Diff, DiffView)>,
}

impl ElemCompare {
    fn ensure(&mut self, set: &Settings) {
        let key = (set.diff_sort_keys, set.indent_str());
        if self.key.as_ref() != Some(&key) {
            self.diff = Some((Diff::new(&self.left, &self.right, &key.1, key.0), DiffView::default()));
            self.key = Some(key);
        }
    }

    fn swap(&mut self) {
        std::mem::swap(&mut self.left, &mut self.right);
        std::mem::swap(&mut self.titles.0, &mut self.titles.1);
        self.key = None;
        self.diff = None;
    }

    /// Título corto para la pestaña: `Partidas[3] ↔ Partidas[7]` (último tramo de
    /// cada ruta; la ruta completa si los dos tramos fueran iguales).
    fn tab_title(&self) -> String {
        let (a, b) = (self.titles.0.as_str(), self.titles.1.as_str());
        fn last(t: &str) -> &str {
            match t.rfind('.') {
                Some(i) => &t[i + 1..],
                None => t.strip_prefix('$').unwrap_or(t),
            }
        }
        let (sa, sb) = (last(a), last(b));
        if sa != sb && !sa.is_empty() && !sb.is_empty() {
            format!("{sa} ↔ {sb}")
        } else {
            let full = |t: &str| t.strip_prefix("$.").unwrap_or(t).to_string();
            format!("{} ↔ {}", full(a), full(b))
        }
    }
}

/// Nodo marcado con «Seleccionar para comparar».
struct CompareMark {
    /// Pestaña y versión del documento (y del filtro) donde se marcó.
    uid: u64,
    doc_gen: u64,
    filter_gen: u64,
    id: NodeId,
    path: String,
    tab_title: String,
}

impl CompareMark {
    /// El marcador tal como lo ven las vistas de la pestaña `t`.
    fn view<'a>(&'a self, t: &DocTab) -> Mark<'a> {
        let same = t.uid == self.uid && t.shown_gen() == (self.doc_gen, self.filter_gen);
        Mark { path: &self.path, id: if same { self.id } else { NONE } }
    }
}

impl DocTab {
    fn new(uid: u64, title: String, path: Option<PathBuf>, source: String, set: &Settings) -> Self {
        let mut t = Self {
            uid,
            title,
            path,
            source,
            source_dirty: false,
            loaded: None,
            error: None,
            force_edit: false,
            filtered: None,
            filter_counts: Vec::new(),
            filter_gen: 0,
            doc_gen: 0,
            compare: None,
        };
        if !t.source.trim().is_empty() {
            t.parse(set);
        }
        t
    }

    fn is_blank(&self) -> bool {
        self.path.is_none() && self.source.trim().is_empty() && self.compare.is_none()
    }

    /// Versión del documento mostrado: cambia al reanalizar o al cambiar el filtro.
    fn shown_gen(&self) -> (u64, u64) {
        (self.doc_gen, if self.filtered.is_some() { self.filter_gen } else { 0 })
    }

    /// Lo que muestran las vistas: el documento filtrado si hay filtro, si no el completo.
    fn shown(&self) -> Option<&Loaded> {
        self.filtered.as_ref().or(self.loaded.as_ref())
    }

    fn shown_mut(&mut self) -> Option<&mut Loaded> {
        if self.filtered.is_some() { self.filtered.as_mut() } else { self.loaded.as_mut() }
    }

    fn update_filter(&mut self, keys: &[&str], gen_: u64, set: &Settings) {
        if self.filter_gen == gen_ {
            return;
        }
        self.filter_gen = gen_;
        self.filtered = None;
        self.filter_counts.clear();
        let Some(l) = &self.loaded else { return };
        if keys.is_empty() {
            return;
        }
        let f = l.doc.filter_keys(keys);
        let mut v = Loaded::new(f.doc, set, 0.0);
        // El camino hasta las coincidencias siempre abierto; lo encontrado según los ajustes.
        for (id, &on_path) in f.path.iter().enumerate() {
            if on_path {
                v.tree.folds[id] = Fold::Expanded;
                v.table.open[id] = true;
            }
        }
        self.filtered = Some(v);
        self.filter_counts = f.counts;
    }

    /// Nombre sugerido al guardar, a partir del título de la pestaña.
    fn file_name(&self) -> String {
        let name: String = self
            .title
            .trim()
            .chars()
            .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
            .collect();
        let name = if name.is_empty() { "documento".into() } else { name };
        if std::path::Path::new(&name).extension().is_some() { name } else { format!("{name}.json") }
    }

    fn source_id(&self) -> Id {
        Id::new(("source_edit", self.uid))
    }

    fn parse(&mut self, set: &Settings) {
        self.source_dirty = false;
        if self.source.trim().is_empty() {
            self.loaded = None;
            self.error = None;
            self.doc_gen += 1;
            self.filtered = None;
            return;
        }
        let t0 = Instant::now();
        match json::parse(&self.source) {
            Ok(doc) => {
                let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
                self.loaded = Some(Loaded::new(doc, set, parse_ms));
                self.error = None;
                self.filtered = None;
                self.filter_gen = 0;
                self.doc_gen += 1;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

pub struct App {
    set: Settings,
    tabs: Vec<DocTab>,
    active: usize,
    next_uid: u64,
    paste_n: usize,
    table_n: usize,
    /// Ventana «Importar tabla de SQL Server».
    import: Option<import::Dialog>,
    search: String,
    focus_search: bool,
    /// ((pestaña, generación del filtro), búsqueda, coincidencias)
    count_cache: Option<((u64, u64), String, usize)>,
    pal: Palette,
    style_key: Option<(bool, u32, u32)>,
    toast: Option<(String, Instant)>,
    show_help: bool,
    rename: Option<Rename>,
    /// Claves activas del filtro en el último cuadro, para detectar cambios.
    filter_sig: String,
    filter_gen: u64,
    /// Pestaña con la que se compara la activa.
    compare_with: Option<u64>,
    diff: Option<(DiffKey, Diff, DiffView)>,
    /// Elemento marcado con «Seleccionar para comparar».
    mark: Option<CompareMark>,
}

/// (uid, versión del documento, generación del filtro) de cada lado, orden de claves, sangría.
type DiffKey = ((u64, u64, u64), (u64, u64, u64), bool, String);

/// Pestaña cuyo nombre se está editando.
struct Rename {
    uid: u64,
    text: String,
    /// Pedir el foco (y seleccionar todo) en el próximo cuadro.
    focus: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        let set: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        setup_fonts(&cc.egui_ctx);
        let mut app = Self {
            pal: if set.dark { Palette::dark() } else { Palette::light() },
            set,
            tabs: Vec::new(),
            active: 0,
            next_uid: 1,
            paste_n: 0,
            table_n: 0,
            import: None,
            search: String::new(),
            focus_search: false,
            count_cache: None,
            style_key: None,
            toast: None,
            show_help: false,
            rename: None,
            filter_sig: String::new(),
            filter_gen: 1,
            compare_with: None,
            diff: None,
            mark: None,
        };
        for f in files {
            app.open_path(f);
        }
        app
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn apply_style(&mut self, ctx: &egui::Context) {
        let key = (self.set.dark, self.set.font_size.to_bits(), self.set.ui_scale.to_bits());
        if self.style_key == Some(key) {
            return;
        }
        let first = self.style_key.is_none();
        self.style_key = Some(key);
        ctx.set_theme(if self.set.dark { egui::Theme::Dark } else { egui::Theme::Light });
        let size = self.set.font_size;
        ctx.all_styles_mut(|s| {
            s.text_styles.insert(TextStyle::Monospace, FontId::monospace(size));
            s.text_styles.insert(TextStyle::Body, FontId::proportional(size));
            s.text_styles.insert(TextStyle::Button, FontId::proportional(size));
            s.spacing.button_padding = egui::vec2(6.0, 2.0);
        });
        if !first || (self.set.ui_scale - 1.0).abs() > f32::EPSILON {
            ctx.set_zoom_factor(self.set.ui_scale);
        }
        self.pal = if self.set.dark { Palette::dark() } else { Palette::light() };
    }

    // ---------------------------------------------------------------- documentos

    fn push_tab(&mut self, title: String, path: Option<PathBuf>, text: String) {
        let tab = DocTab::new(self.next_uid, title, path, text, &self.set);
        self.next_uid += 1;
        if let Some(e) = &tab.error {
            self.toast = Some((format!("JSON inválido: {e}"), Instant::now()));
        }
        match self.tabs.get(self.active) {
            Some(t) if t.is_blank() => self.tabs[self.active] = tab,
            _ => {
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return self.notify(format!("No se pudo abrir {}: {e}", path.display())),
        };
        let text = json::decode_bytes(&bytes);
        self.set.add_recent(&path.to_string_lossy());
        if let Some(i) = self.tabs.iter().position(|t| t.path.as_ref() == Some(&path)) {
            self.active = i;
            let t = &mut self.tabs[i];
            t.source = text;
            t.parse(&self.set);
            return;
        }
        let title = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.push_tab(title, Some(path), text);
    }

    fn open_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new()
            .add_filter("JSON", &["json", "jsonc", "json5", "geojson", "har", "txt", "log"])
            .add_filter("Todos los archivos", &["*"]);
        if let Some(dir) = self.tabs.get(self.active).and_then(|t| t.path.as_ref()).and_then(|p| p.parent()) {
            dlg = dlg.set_directory(dir);
        }
        for f in dlg.pick_files().unwrap_or_default() {
            self.open_path(f);
        }
    }

    fn paste_new(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        // Una tabla copiada de SQL Server abre la ventana de importación.
        if import::looks_like_table(&text) {
            return self.open_import(text);
        }
        self.paste_n += 1;
        self.push_tab(format!("Pegado {}", self.paste_n), None, text);
    }

    fn open_import(&mut self, text: String) {
        self.import = Some(import::Dialog::new(text));
    }

    fn import_window(&mut self, ctx: &egui::Context) {
        let Some(d) = self.import.as_mut() else { return };
        let indent = self.set.indent_str();
        match import::window(ctx, d, &mut self.set.import, &indent) {
            Some(import::Action::Convert(json, rows)) => {
                self.import = None;
                self.table_n += 1;
                self.push_tab(format!("Tabla {}", self.table_n), None, json);
                self.notify(format!("Tabla importada: {rows} {}", if rows == 1 { "fila" } else { "filas" }));
            }
            Some(import::Action::Close) => self.import = None,
            None => {}
        }
    }

    fn new_blank(&mut self) {
        if self.tabs.get(self.active).is_some_and(|t| t.is_blank()) {
            return;
        }
        self.push_tab("Nuevo".into(), None, String::new());
    }

    fn close_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            } else if i < self.active {
                self.active -= 1;
            }
        }
    }

    fn reload(&mut self) {
        if let Some(p) = self.tabs.get(self.active).and_then(|t| t.path.clone()) {
            self.open_path(p);
            self.notify("Archivo recargado");
        }
    }

    /// Guarda `content` proponiendo como nombre el de la pestaña activa.
    fn save_as(&mut self, content: String) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let mut dlg = rfd::FileDialog::new()
            .set_file_name(tab.file_name())
            .add_filter("JSON", &["json"])
            .add_filter("Todos los archivos", &["*"]);
        if let Some(dir) = tab.path.as_ref().and_then(|p| p.parent()) {
            dlg = dlg.set_directory(dir);
        }
        if let Some(path) = dlg.save_file() {
            match std::fs::write(&path, content) {
                Ok(()) => self.notify(format!("Guardado en {}", path.display())),
                Err(e) => self.notify(format!("Error al guardar: {e}")),
            }
        }
    }

    fn start_rename(&mut self, i: usize) {
        if let Some(t) = self.tabs.get(i) {
            self.active = i;
            self.rename = Some(Rename { uid: t.uid, text: t.title.clone(), focus: true });
        }
    }

    // ---------------------------------------------------------------- filtro

    /// Recalcula la vista filtrada de la pestaña activa si cambió el filtro o el documento.
    fn sync_filter(&mut self) {
        let keys = self.set.active_filter();
        let sig = keys.join("\u{1}");
        if sig != self.filter_sig {
            self.filter_sig = sig;
            self.filter_gen += 1;
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.update_filter(&keys, self.filter_gen, &self.set);
        }
    }

    fn add_filter_key(&mut self, key: String) {
        let keys = &mut self.set.filter_keys;
        keys.retain(|k| !k.trim().is_empty());
        if !keys.iter().any(|k| k.trim().eq_ignore_ascii_case(&key)) {
            keys.push(key);
        }
        // No cambia «Aplicar filtro»: si está apagado, la clave solo queda en la lista.
        self.set.show_filter = true;
    }

    // ---------------------------------------------------------------- comparación

    /// Índice de la pestaña con la que se compara la activa. Si no hay una válida,
    /// elige la anterior (o la siguiente).
    fn compare_target(&mut self) -> Option<usize> {
        let usable = |i: usize, t: &DocTab| i != self.active && t.compare.is_none();
        let found = self.compare_with.and_then(|uid| self.tabs.iter().position(|t| t.uid == uid));
        let i = match found.filter(|&i| usable(i, &self.tabs[i])) {
            Some(i) => i,
            None => {
                // La anterior si sirve; si no, la más cercana que tenga documento.
                let prev = self.active.checked_sub(1).filter(|&i| usable(i, &self.tabs[i]));
                let i = prev.or_else(|| self.tabs.iter().enumerate().position(|(i, t)| usable(i, t)))?;
                self.compare_with = Some(self.tabs[i].uid);
                i
            }
        };
        Some(i)
    }

    /// Recalcula la comparación si cambió alguno de los documentos o las opciones.
    fn ensure_diff(&mut self) -> Option<usize> {
        let ri = self.compare_target()?;
        let keys = self.set.active_filter();
        self.tabs[ri].update_filter(&keys, self.filter_gen, &self.set);
        let (l, r) = (&self.tabs[self.active], &self.tabs[ri]);
        let (ld, rd) = (l.shown()?, r.shown()?);
        let ver = |t: &DocTab| (t.uid, t.doc_gen, if t.filtered.is_some() { t.filter_gen } else { 0 });
        let key = (ver(l), ver(r), self.set.diff_sort_keys, self.set.indent_str());
        if self.diff.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let d = Diff::new(&ld.doc, &rd.doc, &key.3, key.2);
            self.diff = Some((key, d, DiffView::default()));
        }
        Some(ri)
    }

    fn compare_toolbar(&mut self, ui: &mut Ui) {
        let others: Vec<(u64, String)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|&(i, t)| i != self.active && t.compare.is_none())
            .map(|(_, t)| (t.uid, t.title.clone()))
            .collect();
        let Some(ri) = self.ensure_diff().or_else(|| self.compare_target()) else {
            ui.label(RichText::new("Abre otro JSON en una pestaña para comparar").weak());
            return;
        };
        let current = self.tabs[ri].title.clone();
        egui::ComboBox::from_id_salt("compare_with")
            .selected_text(current)
            .width(150.0)
            .show_ui(ui, |ui| {
                for (uid, title) in &others {
                    ui.selectable_value(&mut self.compare_with, Some(*uid), title);
                }
            })
            .response
            .on_hover_text("Pestaña con la que se compara la activa (a la derecha)");
        if icons::button(ui, Icon::ArrowLeftRight, "Intercambiar lados").clicked() {
            self.compare_with = Some(self.tabs[self.active].uid);
            self.active = ri;
        }
        toolbar_sep(ui);
        let diff = self.diff.as_mut().map(|(_, d, v)| (&*d, v));
        diff_tools(ui, &mut self.set, diff);
    }

    /// Barra de la pestaña temporal de comparación de elementos.
    fn elem_compare_toolbar(&mut self, ui: &mut Ui) {
        let Some(c) = self.tabs.get_mut(self.active).and_then(|t| t.compare.as_mut()) else { return };
        if icons::button(ui, Icon::ArrowLeftRight, "Intercambiar lados").clicked() {
            c.swap();
        }
        toolbar_sep(ui);
        c.ensure(&self.set);
        diff_tools(ui, &mut self.set, c.diff.as_mut().map(|(d, v)| (&*d, v)));
    }

    fn compare_ui(&mut self, ui: &mut Ui) {
        let Some(ri) = self.ensure_diff() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                if self.tabs.iter().filter(|t| t.compare.is_none()).count() < 2 {
                    ui.label("Para comparar necesitas al menos dos pestañas.");
                    ui.label(RichText::new("Abre o pega otro JSON (Ctrl+O, Ctrl+V) y vuelve a esta vista.").weak());
                } else {
                    ui.label("Una de las dos pestañas no tiene un JSON válido.");
                }
            });
            return;
        };
        let query = self.search.to_ascii_lowercase();
        let titles = (self.tabs[self.active].title.clone(), self.tabs[ri].title.clone());
        let Some((_, d, v)) = self.diff.as_mut() else { return };
        diff_body(ui, &mut self.set, &self.pal, &query, d, v, (&titles.0, &titles.1));
    }

    /// Contenido de la pestaña temporal de comparación de elementos.
    fn elem_compare_ui(&mut self, ui: &mut Ui) {
        let query = self.search.to_ascii_lowercase();
        let Some(c) = self.tabs.get_mut(self.active).and_then(|t| t.compare.as_mut()) else { return };
        c.ensure(&self.set);
        let titles = (c.titles.0.as_str(), c.titles.1.as_str());
        let Some((d, v)) = c.diff.as_mut() else { return };
        diff_body(ui, &mut self.set, &self.pal, &query, d, v, titles);
    }

    /// La pestaña activa es una comparación de elementos.
    fn on_elem_compare(&self) -> bool {
        self.tabs.get(self.active).is_some_and(|t| t.compare.is_some())
    }

    /// Guarda el nodo marcado con «Seleccionar para comparar».
    fn mark_for_compare(&mut self, id: NodeId) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(l) = t.shown() else { return };
        let (doc_gen, filter_gen) = t.shown_gen();
        let path = l.doc.path(id);
        self.mark =
            Some(CompareMark { uid: t.uid, doc_gen, filter_gen, id, path: path.clone(), tab_title: t.title.clone() });
        self.notify(format!("Seleccionado para comparar: {path}. Clic derecho en otro elemento → «Comparar con…»"));
    }

    /// Compara el nodo marcado con `id` de la pestaña activa en una pestaña temporal.
    fn compare_with_mark(&mut self, id: NodeId) {
        let Some(m) = self.mark.as_ref() else { return };
        let Some(mi) = self.tabs.iter().position(|t| t.uid == m.uid) else {
            self.mark = None;
            return self.notify("La pestaña del elemento marcado ya se cerró");
        };
        let mt = &self.tabs[mi];
        if mt.shown_gen() != (m.doc_gen, m.filter_gen) {
            self.mark = None;
            return self.notify("El documento cambió: vuelve a marcar el elemento a comparar");
        }
        let (Some(ml), Some(rl)) = (mt.shown(), self.tabs[self.active].shown()) else { return };
        if mi == self.active && m.id == id {
            return self.notify("Elige un elemento distinto del marcado");
        }
        let left = ml.doc.subtree(m.id);
        let right = rl.doc.subtree(id);
        let right_path = rl.doc.path(id);
        let titles = if mi == self.active {
            (m.path.clone(), right_path)
        } else {
            (format!("{} · {}", m.tab_title, m.path), format!("{} · {}", self.tabs[self.active].title, right_path))
        };
        let c = ElemCompare { left, right, titles, key: None, diff: None };
        let title = c.tab_title();
        // Una sola pestaña temporal: la siguiente comparación la reutiliza (como VS Code).
        match self.tabs.iter().position(|t| t.compare.is_some()) {
            Some(i) => {
                let t = &mut self.tabs[i];
                t.compare = Some(c);
                t.title = title;
                self.active = i;
            }
            None => {
                let mut t = DocTab::new(self.next_uid, title, None, String::new(), &self.set);
                self.next_uid += 1;
                t.compare = Some(c);
                let at = (self.active + 1).min(self.tabs.len());
                self.tabs.insert(at, t);
                self.active = at;
            }
        }
    }

    fn current_view(&self) -> View {
        match self.tabs.get(self.active) {
            Some(t) if t.loaded.is_some() => self.set.view,
            _ => View::Source,
        }
    }

    // ---------------------------------------------------------------- búsqueda

    fn find(&mut self, ctx: &egui::Context, forward: bool) {
        let q = self.search.to_ascii_lowercase();
        if q.is_empty() {
            return;
        }
        let view = self.current_view();
        let indent = self.set.indent_str();
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let found = match (if tab.compare.is_some() { View::Compare } else { view }, tab.shown_mut()) {
            (View::Tree | View::Table, Some(l)) => {
                let doc = &l.doc;
                let n = doc.len() as u64;
                let sel = if view == View::Tree { l.tree.selected } else { l.table.selected };
                let start = if l.last_match != NONE && sel != NONE && doc.is_ancestor(sel, l.last_match) {
                    l.last_match
                } else if sel != NONE {
                    sel
                } else if forward {
                    (n - 1) as NodeId
                } else {
                    0
                };
                let hit = (1..=n)
                    .map(|step| {
                        let s = start as u64;
                        (if forward { (s + step) % n } else { (s + n - step % n) % n }) as NodeId
                    })
                    .find(|&i| highlight::node_matches(doc, doc.node(i), &q));
                if let Some(i) = hit {
                    l.last_match = i;
                    if view == View::Tree {
                        l.tree.reveal(doc, i);
                    } else {
                        l.table.reveal(doc, i);
                    }
                }
                hit.is_some()
            }
            (View::Text, Some(l)) => {
                l.text.ensure(&l.doc, &indent);
                l.text.find(&q, forward)
            }
            // En la comparación la búsqueda solo resalta.
            (View::Compare, _) => true,
            _ => find_in_source(ctx, tab, &q, forward),
        };
        if !found {
            self.notify(format!("Sin coincidencias para «{}»", self.search));
        }
    }

    fn match_count(&mut self) -> Option<usize> {
        let tab = self.tabs.get(self.active)?;
        let l = tab.shown()?;
        if self.search.is_empty() {
            return None;
        }
        let key = (tab.uid, if tab.filtered.is_some() { tab.filter_gen } else { 0 });
        if let Some((k, q, c)) = &self.count_cache {
            if *k == key && *q == self.search {
                return Some(*c);
            }
        }
        let q = self.search.to_ascii_lowercase();
        let c = l.doc.nodes.iter().filter(|n| highlight::node_matches(&l.doc, n, &q)).count();
        self.count_cache = Some((key, self.search.clone(), c));
        Some(c)
    }

    // ---------------------------------------------------------------- entrada global

    fn handle_input(&mut self, ctx: &egui::Context) {
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let (open, new, close, find, next, prev, reload, apply, next_tab, view) = ctx.input_mut(|i| {
            let view = [
                (Key::Num1, View::Tree),
                (Key::Num2, View::Table),
                (Key::Num3, View::Text),
                (Key::Num4, View::Source),
                (Key::Num5, View::Compare),
            ]
            .into_iter()
            .find(|&(k, _)| i.consume_shortcut(&sc(Modifiers::COMMAND, k)))
            .map(|(_, v)| v);
            (
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::O)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::N)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::W)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::F)),
                i.consume_shortcut(&sc(Modifiers::NONE, Key::F3)),
                i.consume_shortcut(&sc(Modifiers::SHIFT, Key::F3)),
                i.consume_shortcut(&sc(Modifiers::NONE, Key::F5)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Enter)),
                i.consume_shortcut(&sc(Modifiers::CTRL, Key::Tab)),
                view,
            )
        });
        if open {
            self.open_dialog();
        }
        if new {
            self.new_blank();
        }
        if close {
            self.close_tab(self.active);
        }
        if find {
            self.focus_search = true;
        }
        if next {
            self.find(ctx, true);
        }
        if prev {
            self.find(ctx, false);
        }
        if reload {
            self.reload();
        }
        if apply {
            if let Some(d) = self.import.as_mut() {
                d.submit = true;
            } else {
                let set = self.set.clone();
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.parse(&set);
                }
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::COMMAND, Key::I))) && self.import.is_none() {
            self.open_import(String::new());
        }
        if next_tab && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        let (diff_next, diff_prev) = ctx.input_mut(|i| {
            (i.consume_shortcut(&sc(Modifiers::NONE, Key::F7)), i.consume_shortcut(&sc(Modifiers::SHIFT, Key::F7)))
        });
        if diff_next || diff_prev {
            let view = if self.on_elem_compare() {
                self.tabs[self.active].compare.as_mut().and_then(|c| c.diff.as_mut()).map(|(_, v)| v)
            } else if self.current_view() == View::Compare {
                self.diff.as_mut().map(|(_, _, v)| v)
            } else {
                None
            };
            if let Some(v) = view {
                v.step(diff_next);
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::NONE, Key::F2))) {
            self.start_rename(self.active);
        }
        if let Some(v) = view {
            self.set.view = v;
        }

        // Pegar fuera de un campo de texto = documento nuevo.
        if !ctx.text_edit_focused() {
            let pasted = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Paste(s) => Some(s.clone()),
                    _ => None,
                })
            });
            if let Some(text) = pasted {
                self.paste_new(text);
            }
        }

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        for p in dropped {
            self.open_path(p);
        }
    }

    // ---------------------------------------------------------------- paneles

    /// Menú con lo que no cabe en la barra: recientes, tema, atajos, salir.
    fn overflow_menu(&mut self, ui: &mut Ui) {
        let mut pick: Option<PathBuf> = None;
        let mut clear_recent = false;
        let mut close = false;
        let mut help = false;
        let mut quit = false;
        icons::menu(ui, Icon::Menu, "Menú", |ui| {
            ui.menu_button(icons::text(Icon::History, "Recientes"), |ui| {
                if self.set.recent.is_empty() {
                    ui.label(RichText::new("(vacío)").weak());
                }
                for p in &self.set.recent {
                    if ui.button(p).clicked() {
                        pick = Some(PathBuf::from(p));
                        ui.close();
                    }
                }
                if !self.set.recent.is_empty() {
                    ui.separator();
                    if icons::item(ui, Icon::Trash2, "Limpiar lista", "").clicked() {
                        clear_recent = true;
                    }
                }
            });
            ui.separator();
            if icons::item_enabled(ui, !self.tabs.is_empty(), Icon::X, "Cerrar pestaña", "Ctrl+W").clicked() {
                close = true;
            }
            ui.separator();
            let (icon, label) = if self.set.dark { (Icon::Sun, "Tema claro") } else { (Icon::Moon, "Tema oscuro") };
            if icons::item(ui, icon, label, "").clicked() {
                self.set.dark = !self.set.dark;
            }
            if icons::item(ui, Icon::Keyboard, "Atajos de teclado", "").clicked() {
                help = true;
            }
            ui.separator();
            if icons::item(ui, Icon::LogOut, "Salir", "").clicked() {
                quit = true;
            }
        });
        if let Some(p) = pick {
            self.open_path(p);
        }
        if clear_recent {
            self.set.recent.clear();
        }
        if close {
            self.close_tab(self.active);
        }
        if help {
            self.show_help = true;
        }
        if quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn save_current(&mut self, pretty: bool) {
        let indent = self.set.indent_str();
        let Some(l) = self.tabs.get(self.active).and_then(|t| t.shown()) else { return };
        let layout = if pretty { Layout::Pretty(&indent) } else { Layout::Minified };
        let content = json::to_string(&l.doc, Doc::ROOT, layout);
        self.save_as(content);
    }

    /// Una sola fila: archivo · vistas · herramientas de la vista · paneles y menú.
    fn toolbar(&mut self, ui: &mut Ui) {
        let view = self.current_view();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;

            // Archivo
            if icons::button(ui, Icon::FolderOpen, "Abrir archivo… (Ctrl+O)").clicked() {
                self.open_dialog();
            }
            if icons::button(ui, Icon::FilePlus, "Nuevo documento para escribir (Ctrl+N)").clicked() {
                self.new_blank();
                self.set.view = View::Source;
            }
            if icons::button(ui, Icon::ClipboardPaste, "Pegar como documento nuevo (Ctrl+V)").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            if icons::button(ui, Icon::Database, "Importar tabla de SQL Server… (Ctrl+I)").clicked() {
                self.open_import(String::new());
            }
            let has_path = self.tabs.get(self.active).is_some_and(|t| t.path.is_some());
            if icons::enabled_button(ui, has_path, Icon::RefreshCw, "Recargar archivo (F5)").clicked() {
                self.reload();
            }
            let loaded = self.tabs.get(self.active).is_some_and(|t| t.loaded.is_some());
            let mut save_pretty = None;
            if loaded {
                icons::menu(ui, Icon::Save, "Guardar como…", |ui| {
                    if icons::item(ui, Icon::Braces, "Formateado…", "").clicked() {
                        save_pretty = Some(true);
                    }
                    if icons::item(ui, Icon::Minimize2, "Minificado…", "").clicked() {
                        save_pretty = Some(false);
                    }
                });
            } else {
                icons::enabled_button(ui, false, Icon::Save, "Guardar como…");
            }
            if let Some(pretty) = save_pretty {
                self.save_current(pretty);
            }
            toolbar_sep(ui);

            // Vistas (sin efecto en una pestaña de comparación de elementos)
            let elem_compare = self.on_elem_compare();
            ui.add_enabled_ui(!elem_compare, |ui| {
                for (v, icon, label, key) in VIEWS {
                    let on = !elem_compare && view == v;
                    if icons::selectable(ui, on, icon, &format!("{label} ({key})")).clicked() {
                        self.set.view = v;
                    }
                }
            });
            toolbar_sep(ui);

            // Herramientas de la vista
            let set = &mut self.set;
            let mut msg = None;
            if elem_compare {
                self.elem_compare_toolbar(ui);
            } else if view == View::Compare {
                self.compare_toolbar(ui);
            } else if let Some(tab) = self.tabs.get_mut(self.active) {
                match (view, tab.shown_mut()) {
                    (View::Tree, Some(l)) => {
                        if icons::button(ui, Icon::UnfoldVertical, "Expandir todo").clicked() {
                            l.tree.set_all(Fold::Expanded);
                        }
                        if icons::button(ui, Icon::FoldVertical, "Cerrar todo").clicked() {
                            l.tree.level(&l.doc, 1, Fold::Closed);
                        }
                        if icons::button(ui, Icon::ListCollapse, "Compactar todo: raíz abierta, cada hijo en una línea")
                            .clicked()
                        {
                            l.tree.level(&l.doc, 1, Fold::Compact);
                        }
                        let auto_tip = format!(
                            "Auto: compacta en una línea lo que quepa en {} columnas (se ajusta en Ajustes)",
                            set.auto_width
                        );
                        if icons::button(ui, Icon::WandSparkles, &auto_tip).clicked() {
                            l.tree.auto(&l.doc, Doc::ROOT, set);
                        }
                        icons::menu(ui, Icon::Layers, "Expandir hasta un nivel", |ui| {
                            for lv in 1..=5 {
                                if ui.button(format!("Nivel {lv}")).clicked() {
                                    let rest = if set.level_compact { Fold::Compact } else { Fold::Closed };
                                    l.tree.level(&l.doc, lv, rest);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            ui.checkbox(&mut set.level_compact, "Resto compacto").on_hover_text(
                                "Lo que queda fuera del nivel se compacta en una línea en vez de cerrarse",
                            );
                        });
                    }
                    (View::Table, Some(l)) => {
                        if icons::button(ui, Icon::UnfoldVertical, "Expandir todo").clicked() {
                            l.table.set_open(&l.doc, Doc::ROOT, true, true);
                        }
                        if icons::button(ui, Icon::FoldVertical, "Contraer todo").clicked() {
                            l.table.collapse_all();
                        }
                    }
                    (View::Text, Some(l)) => {
                        l.text.ensure(&l.doc, &set.indent_str());
                        if icons::button(ui, Icon::Copy, "Copiar todo el texto").clicked() {
                            ui.ctx().copy_text(l.text.text().to_string());
                            msg = Some("Texto copiado");
                        }
                    }
                    _ => {
                        if icons::enabled_button(ui, tab.source_dirty, Icon::Check, "Aplicar cambios (Ctrl+Enter)")
                            .clicked()
                        {
                            tab.parse(set);
                        }
                        let fmt = |src: &str, layout: Layout| {
                            json::parse(src).map(|d| json::to_string(&d, Doc::ROOT, layout))
                        };
                        let format_tip = "Formatear: reescribe la fuente con sangría (quita comentarios)";
                        if icons::button(ui, Icon::IndentIncrease, format_tip).clicked() {
                            match fmt(&tab.source, Layout::Pretty(&set.indent_str())) {
                                Ok(s) => {
                                    tab.source = s;
                                    tab.parse(set);
                                }
                                Err(_) => msg = Some("No se puede formatear: el JSON tiene errores"),
                            }
                        }
                        if icons::button(ui, Icon::Minimize2, "Minificar: todo en una línea").clicked() {
                            match fmt(&tab.source, Layout::Minified) {
                                Ok(s) => {
                                    tab.source = s;
                                    tab.parse(set);
                                }
                                Err(_) => msg = Some("No se puede minificar: el JSON tiene errores"),
                            }
                        }
                        if icons::button(ui, Icon::Copy, "Copiar la fuente").clicked() {
                            ui.ctx().copy_text(tab.source.clone());
                            msg = Some("Fuente copiada");
                        }
                    }
                }
            }
            if let Some(m) = msg {
                self.notify(m);
            }

            // Paneles y menú
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.overflow_menu(ui);
                icons::toggle(ui, &mut self.set.show_settings, Icon::Settings2, "Ajustes");
                icons::toggle(
                    ui,
                    &mut self.set.show_filter,
                    Icon::Filter,
                    "Filtro por claves: mostrar solo ciertas claves y el camino hasta ellas",
                );
            });
        });
    }

    fn search_box(&mut self, ui: &mut Ui) {
        {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mut go = None;
                if icons::button(ui, Icon::ChevronDown, "Siguiente coincidencia (F3 / Enter)").clicked() {
                    go = Some(true);
                }
                if icons::button(ui, Icon::ChevronUp, "Coincidencia anterior (Mayús+F3 / Mayús+Enter)").clicked() {
                    go = Some(false);
                }
                if let Some(c) = self.match_count() {
                    ui.label(RichText::new(c.to_string()).weak()).on_hover_text(if c == 1 {
                        "1 coincidencia"
                    } else {
                        "Coincidencias"
                    });
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .id(Id::new("search_box"))
                        .hint_text(icons::text(Icon::Search, "Buscar clave o valor (Ctrl+F)"))
                        .desired_width(220.0),
                );
                if self.focus_search {
                    // Seleccionar todo para sobrescribir la búsqueda anterior.
                    let mut st = TextEditState::load(ui.ctx(), resp.id).unwrap_or_default();
                    let n = self.search.chars().count();
                    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
                    st.store(ui.ctx(), resp.id);
                    resp.request_focus();
                    self.focus_search = false;
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    go = Some(!ui.input(|i| i.modifiers.shift));
                    resp.request_focus();
                }
                if let Some(fwd) = go {
                    let ctx = ui.ctx().clone();
                    self.find(&ctx, fwd);
                }
            });
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui) {
        let mut action = None;
        let mut renamed: Option<Option<String>> = None;
        let mut new = false;
        let mut active_rect = None;
        let mut tabs_clip = Rect::NOTHING;
        let mut bottom = None;
        let mut rects = Vec::with_capacity(self.tabs.len());
        let mut dragging = None;
        ui.horizontal(|ui| {
            let tabs_w = (ui.available_width() - 340.0).max(120.0);
            egui::ScrollArea::horizontal().id_salt("tabs").max_width(tabs_w).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    tabs_clip = ui.clip_rect();
                    for (i, t) in self.tabs.iter().enumerate() {
                        let active = i == self.active;
                        let rect = match self.rename.as_mut().filter(|r| r.uid == t.uid) {
                            Some(r) => {
                                let (rect, done) = rename_tab(ui, r);
                                if done.is_some() {
                                    renamed = done;
                                }
                                rect
                            }
                            None => {
                                let (rect, act, dragged) = tab_widget(ui, t, active);
                                if dragged {
                                    dragging = Some(i);
                                }
                                if let Some(a) = act {
                                    action = Some((i, a));
                                }
                                rect
                            }
                        };
                        if active {
                            active_rect = Some(rect);
                        }
                        bottom = Some(rect.bottom());
                        rects.push(rect);
                    }
                    ui.add_space(4.0);
                    if icons::button(ui, Icon::Plus, "Nuevo documento (Ctrl+N)").clicked() {
                        new = true;
                    }
                });
            });
            self.search_box(ui);
        });

        // Línea base de la barra, interrumpida bajo la pestaña activa para que se una con el contenido.
        if let Some(y) = bottom {
            let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
            let y = y - stroke.width / 2.0;
            let full = ui.clip_rect().x_range();
            let p = ui.painter();
            match active_rect.map(|r| r.intersect(tabs_clip)).filter(|r| r.width() > 0.0) {
                Some(r) => {
                    p.hline(full.min..=r.left(), y, stroke);
                    p.hline(r.right()..=full.max, y, stroke);
                }
                None => {
                    p.hline(full, y, stroke);
                }
            }
        }

        if let Some(title) = renamed {
            let uid = self.rename.take().map(|r| r.uid);
            if let (Some(title), Some(t)) = (title, self.tabs.iter_mut().find(|t| Some(t.uid) == uid)) {
                t.title = title;
            }
        }
        if new {
            self.new_blank();
            self.set.view = View::Source;
        }
        match action {
            Some((i, TabAction::Select)) => self.active = i,
            Some((i, TabAction::Close)) => {
                self.close_tab(i);
                return;
            }
            Some((i, TabAction::CloseOthers)) => {
                let keep = self.tabs.swap_remove(i);
                self.tabs = vec![keep];
                self.active = 0;
                return;
            }
            Some((i, TabAction::Rename)) => self.start_rename(i),
            Some((i, TabAction::CompareWithActive)) => {
                self.compare_with = Some(self.tabs[i].uid);
                self.set.view = View::Compare;
            }
            Some((i, TabAction::CopyPath)) => {
                if let Some(p) = &self.tabs[i].path {
                    ui.ctx().copy_text(p.display().to_string());
                }
            }
            None => {}
        }

        // Arrastre: la pestaña cambia de lugar cuando el puntero pasa el centro de otra.
        let pointer = ui.ctx().pointer_interact_pos();
        if let (Some(i), Some(pos)) = (dragging, pointer) {
            if rects.len() != self.tabs.len() {
                return;
            }
            let target = if pos.x > rects[i].right() {
                (i + 1..rects.len()).rev().find(|&j| pos.x > rects[j].center().x)
            } else if pos.x < rects[i].left() {
                (0..i).find(|&j| pos.x < rects[j].center().x)
            } else {
                None
            };
            if let Some(j) = target {
                let active_uid = self.tabs.get(self.active).map(|t| t.uid);
                let t = self.tabs.remove(i);
                self.tabs.insert(j, t);
                if let Some(k) = self.tabs.iter().position(|t| Some(t.uid) == active_uid) {
                    self.active = k;
                }
                ui.ctx().request_repaint();
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.toast = None;
        }
        let view = self.current_view();
        ui.horizontal(|ui| {
            if let Some(tab) = self.tabs.get(self.active) {
                if let Some(l) = tab.shown() {
                    let sel = match view {
                        View::Tree => l.tree.selected,
                        View::Table => l.table.selected,
                        _ => NONE,
                    };
                    if sel != NONE {
                        let path = l.doc.path(sel);
                        if ui
                            .add(egui::Label::new(RichText::new(&path).monospace()).sense(egui::Sense::click()))
                            .on_hover_text("Clic para copiar la ruta")
                            .clicked()
                        {
                            ui.ctx().copy_text(path);
                        }
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(c) = self.tabs.get(self.active).and_then(|t| t.compare.as_ref()) {
                    if let Some((d, _)) = &c.diff {
                        let s = format!(
                            "{} ↔ {} nodos · +{} −{} ~{} líneas",
                            c.left.len(),
                            c.right.len(),
                            d.added,
                            d.removed,
                            d.modified
                        );
                        ui.label(RichText::new(s).weak());
                    }
                } else if let Some(tab) = self.tabs.get(self.active) {
                    let mut parts = vec![human_size(tab.source.len())];
                    if let (Some(l), Some(full)) = (tab.shown(), &tab.loaded) {
                        parts.insert(
                            0,
                            if tab.filtered.is_some() {
                                format!("{} de {} nodos (filtrado)", l.doc.len(), full.doc.len())
                            } else {
                                format!("{} nodos", l.doc.len())
                            },
                        );
                        match view {
                            View::Tree => parts.insert(1, format!("{} líneas", l.tree.line_count())),
                            View::Text => parts.insert(1, format!("{} líneas", l.text.line_count())),
                            _ => {}
                        }
                        parts.push(format!("{:.1} ms", full.parse_ms));
                    }
                    ui.label(RichText::new(parts.join(" · ")).weak());
                }
                if let Some((m, _)) = &self.toast {
                    ui.separator();
                    ui.label(RichText::new(m).color(ui.visuals().warn_fg_color));
                }
            });
        });
    }

    fn settings_panel(&mut self, ui: &mut Ui) {
        let s = &mut self.set;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(icons::text(Icon::Settings2, "Ajustes"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::X, "Ocultar panel").clicked() {
                        s.show_settings = false;
                    }
                });
            });
            ui.add_space(4.0);
            egui::Grid::new("settings_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Tema");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.dark, true, icons::text(Icon::Moon, "Oscuro"));
                    ui.selectable_value(&mut s.dark, false, icons::text(Icon::Sun, "Claro"));
                });
                ui.end_row();
                ui.label("Tamaño de letra");
                ui.add(egui::Slider::new(&mut s.font_size, 9.0..=24.0).step_by(0.5));
                ui.end_row();
                ui.label("Escala");
                ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0).step_by(0.05));
                ui.end_row();
                ui.label("Sangría");
                indent_picker(ui, &mut s.indent);
                ui.end_row();
            });

            ui.separator();
            ui.strong("Árbol");
            ui.checkbox(&mut s.line_numbers, "Números de línea");
            ui.checkbox(&mut s.show_guides, "Guías de sangría");
            ui.checkbox(&mut s.show_counts, "Mostrar cantidad en nodos cerrados");
            ui.checkbox(&mut s.show_indices, "Mostrar índices de arreglo");
            ui.checkbox(&mut s.show_commas, "Mostrar comas");
            ui.checkbox(&mut s.quote_keys, "Comillas en claves");
            ui.horizontal(|ui| {
                ui.label("Máx. caracteres por línea compacta");
                ui.add(egui::DragValue::new(&mut s.compact_limit).range(100..=100_000));
            });

            ui.separator();
            ui.strong("Al abrir un documento");
            ui.radio_value(&mut s.initial, InitialFold::Auto, "Auto: compactar lo que quepa en una línea");
            ui.radio_value(&mut s.initial, InitialFold::Level, "Expandir hasta un nivel, cerrar el resto");
            ui.radio_value(&mut s.initial, InitialFold::LevelCompact, "Expandir hasta un nivel, compactar el resto");
            ui.radio_value(&mut s.initial, InitialFold::ExpandAll, "Expandir todo");
            egui::Grid::new("initial_grid").num_columns(2).show(ui, |ui| {
                ui.label("Ancho (Auto)");
                ui.add(egui::DragValue::new(&mut s.auto_width).range(20..=400).suffix(" col"));
                ui.end_row();
                ui.label("Nivel");
                ui.add(egui::DragValue::new(&mut s.initial_level).range(0..=50));
                ui.end_row();
            });

            ui.separator();
            ui.strong("Tabla");
            ui.checkbox(&mut s.table_preview, "Vista previa de objetos y arreglos");

            ui.separator();
            if ui.button(icons::text(Icon::RotateCcw, "Restablecer valores predeterminados")).clicked() {
                let recent = std::mem::take(&mut s.recent);
                let filter_keys = std::mem::take(&mut s.filter_keys);
                *s = Settings { recent, filter_keys, show_settings: true, ..Default::default() };
            }
        });
    }

    fn filter_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading(icons::text(Icon::Filter, "Filtro por claves"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::X, "Ocultar panel").clicked() {
                    self.set.show_filter = false;
                }
            });
        });
        ui.label(
            RichText::new("Muestra solo estas claves y el camino desde la raíz hasta ellas. No distingue mayúsculas.")
                .weak(),
        );
        ui.add_space(4.0);
        ui.checkbox(&mut self.set.filter_on, "Aplicar filtro");
        ui.add_space(4.0);

        // Coincidencias por fila, según la posición de la clave en el filtro activo.
        let counts: Vec<Option<usize>> = {
            let active = self.set.active_filter();
            let tab = self.tabs.get(self.active).filter(|t| t.filtered.is_some());
            self.set
                .filter_keys
                .iter()
                .map(|k| {
                    let pos = active.iter().position(|a| a.eq_ignore_ascii_case(k.trim()))?;
                    tab.and_then(|t| t.filter_counts.get(pos).copied())
                })
                .collect()
        };

        let keys = &mut self.set.filter_keys;
        if keys.last().is_none_or(|k| !k.trim().is_empty()) {
            keys.push(String::new());
        }
        let mut remove = None;
        let n = keys.len();
        let warn = ui.visuals().warn_fg_color;
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(ui.available_height() - 40.0).show(
            ui,
            |ui| {
                for (i, k) in keys.iter_mut().enumerate() {
                    let last = i + 1 == n;
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(k)
                            .id(Id::new(("filter_key", i)))
                            .hint_text(if last { "Agregar clave…" } else { "" })
                            .font(TextStyle::Monospace)
                            .desired_width(ui.available_width() - 52.0);
                        let resp = ui.add(edit);
                        if resp.lost_focus() && !last && k.trim().is_empty() {
                            remove = Some(i);
                        }
                        if !last {
                            match counts.get(i).copied().flatten() {
                                Some(0) => ui.label(RichText::new("0").color(warn)).on_hover_text("Sin coincidencias"),
                                Some(c) => ui.label(RichText::new(c.to_string()).weak()).on_hover_text("Coincidencias"),
                                None => ui.label(""),
                            };
                            if icons::small(ui, Icon::X, "Quitar esta clave").clicked() {
                                remove = Some(i);
                            }
                        }
                    });
                }
            },
        );
        if let Some(i) = remove {
            keys.remove(i);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(n > 1, egui::Button::new(icons::text(Icon::Trash2, "Quitar todas"))).clicked() {
                self.set.filter_keys.clear();
            }
        });
        ui.label(RichText::new("Tip: clic derecho en un nodo → «Filtrar por esta clave».").weak().small());
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() {
            match welcome(ui) {
                Some(Welcome::Open) => self.open_dialog(),
                Some(Welcome::Import) => self.open_import(String::new()),
                None => {}
            }
            return;
        }
        let view = self.current_view();
        if self.on_elem_compare() {
            self.elem_compare_ui(ui);
            return;
        }
        if view == View::Compare {
            self.compare_ui(ui);
            return;
        }
        let keys = !ui.ctx().text_edit_focused();
        let query = self.search.to_ascii_lowercase();
        let set = &self.set;
        let pal = &self.pal;
        let tab = &mut self.tabs[self.active];
        let mark = self.mark.as_ref().map(|m| m.view(tab));

        if let Some(e) = tab.error.clone() {
            let mut goto = false;
            egui::Frame::new()
                .fill(ui.visuals().error_fg_color.gamma_multiply(0.15))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icons::text(Icon::TriangleAlert, &e.to_string()))
                                .color(ui.visuals().error_fg_color),
                        );
                        if tab.loaded.is_some() {
                            ui.label(RichText::new("· se muestra la última versión válida").weak());
                        }
                        if ui.small_button(icons::text(Icon::Crosshair, "Ir al error")).clicked() {
                            goto = true;
                        }
                    });
                });
            if goto {
                self.set.view = View::Source;
                let id = tab.source_id();
                let ctx = ui.ctx().clone();
                let mut st = TextEditState::load(&ctx, id).unwrap_or_default();
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(e.char_offset))));
                st.store(&ctx, id);
                ctx.memory_mut(|m| m.request_focus(id));
                return;
            }
        }

        let mut filter_off = false;
        if tab.filtered.is_some() && view != View::Source {
            let keys = set.active_filter();
            let total: usize = tab.filter_counts.iter().sum();
            let missing: Vec<&str> =
                keys.iter().zip(&tab.filter_counts).filter(|(_, c)| **c == 0).map(|(k, _)| *k).collect();
            egui::Frame::new()
                .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.25))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let what = if total == 1 { "coincidencia" } else { "coincidencias" };
                        ui.label(icons::text(Icon::Filter, &format!("Filtro por claves: {total} {what}")));
                        if !missing.is_empty() {
                            ui.label(
                                RichText::new(format!("· sin coincidencias: {}", missing.join(", ")))
                                    .color(ui.visuals().warn_fg_color),
                            );
                        }
                        if ui.small_button(icons::text(Icon::FilterX, "Quitar filtro")).clicked() {
                            filter_off = true;
                        }
                    });
                });
        }

        let mut out = None;
        let mut reveal = None;
        match (view, tab.shown_mut()) {
            (View::Tree, Some(l)) => out = tree::ui(ui, &l.doc, &mut l.tree, set, pal, &query, keys, mark),
            (View::Table, Some(l)) => out = table::ui(ui, &l.doc, &mut l.table, set, pal, &query, keys, mark),
            (View::Text, Some(l)) => {
                l.text.ensure(&l.doc, &set.indent_str());
                text::show(ui, &mut l.text, set, pal, &query);
            }
            _ => source_ui(ui, tab, set),
        }
        if let Some(l) = tab.shown_mut() {
            if let Some(Out::Grid(id)) = out {
                l.grid = Some(Grid::new(&l.doc, id));
            }
            if let Some(g) = l.grid.as_mut() {
                reveal = grid::window(ui.ctx(), &l.doc, g, pal, &query);
                if !g.open {
                    l.grid = None;
                }
            }
            if let Some(id) = reveal {
                match view {
                    View::Table => l.table.reveal(&l.doc, id),
                    _ => l.tree.reveal(&l.doc, id),
                }
            }
        }
        if reveal.is_some() && !matches!(view, View::Tree | View::Table) {
            self.set.view = View::Tree;
        }
        if filter_off {
            self.set.filter_on = false;
        }
        match out {
            Some(Out::FilterKey(k)) => self.add_filter_key(k),
            Some(Out::MarkCompare(id)) => self.mark_for_compare(id),
            Some(Out::CompareWithMark(id)) => self.compare_with_mark(id),
            _ => {}
        }
    }
}

/// Opciones y navegación de una comparación (compartidas por la vista Comparar y la
/// pestaña de comparación de elementos).
fn diff_tools(ui: &mut Ui, set: &mut Settings, diff: Option<(&Diff, &mut DiffView)>) {
    icons::toggle(ui, &mut set.diff_sort_keys, Icon::ArrowDownAZ, "Ignorar orden de claves");
    icons::toggle(
        ui,
        &mut set.diff_only_changes,
        Icon::Diff,
        "Solo diferencias: pliega las líneas iguales lejos de un cambio",
    );
    icons::toggle(ui, &mut set.diff_show_list, Icon::List, "Lista de cambios por ruta");
    toolbar_sep(ui);
    if let Some((d, v)) = diff {
        if icons::button(ui, Icon::ChevronUp, "Diferencia anterior (Mayús+F7)").clicked() {
            v.step(false);
        }
        if icons::button(ui, Icon::ChevronDown, "Diferencia siguiente (F7)").clicked() {
            v.step(true);
        }
        if d.is_identical() {
            ui.label(RichText::new("Sin diferencias").color(ui.visuals().hyperlink_color));
        } else {
            ui.label(v.position_label());
            ui.label(RichText::new(format!("+{} −{} ~{} líneas", d.added, d.removed, d.modified)).weak())
                .on_hover_text("Añadidas, eliminadas y modificadas");
        }
    }
}

/// Cuerpo de una comparación: aviso, lista de cambios y diff lado a lado.
fn diff_body(
    ui: &mut Ui,
    set: &mut Settings,
    pal: &Palette,
    query: &str,
    d: &Diff,
    v: &mut DiffView,
    titles: (&str, &str),
) {
    if d.approximate {
        ui.label(
            RichText::new("Documentos muy distintos: la alineación de líneas es aproximada.")
                .color(ui.visuals().warn_fg_color),
        );
    }
    if set.diff_show_list && !d.changes.is_empty() {
        egui::Panel::right("diff_changes").default_size(320.0).show(ui, |ui| diff::changes_list(ui, d, v));
    }
    diff::show(ui, d, v, titles, set.diff_only_changes, pal, query);
    if std::mem::take(&mut v.want_all) {
        set.diff_only_changes = false;
    }
}

const VIEWS: [(View, Icon, &str, &str); 5] = [
    (View::Tree, Icon::ListTree, "Árbol", "Ctrl+1"),
    (View::Table, Icon::Table, "Tabla", "Ctrl+2"),
    (View::Text, Icon::FileJson, "Texto formateado", "Ctrl+3"),
    (View::Source, Icon::SquarePen, "Fuente: editar el texto original", "Ctrl+4"),
    (View::Compare, Icon::GitCompare, "Comparar dos pestañas", "Ctrl+5"),
];

/// Separador vertical con un poco de aire a los lados.
fn toolbar_sep(ui: &mut Ui) {
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}

fn indent_picker(ui: &mut Ui, indent: &mut usize) {
    ui.horizontal(|ui| {
        ui.selectable_value(indent, 2, "2");
        ui.selectable_value(indent, 4, "4");
        ui.selectable_value(indent, 0, "Tab");
    });
}

enum TabAction {
    Select,
    Close,
    CloseOthers,
    Rename,
    CopyPath,
    CompareWithActive,
}

const TAB_PAD: f32 = 10.0;
const TAB_CLOSE: f32 = 16.0;

fn tab_height(ui: &Ui) -> f32 {
    ui.spacing().interact_size.y + 8.0
}

/// Fondo de una pestaña: la activa lleva borde, acento arriba y se abre por abajo.
fn paint_tab_bg(ui: &Ui, rect: Rect, active: bool, hovered: bool) {
    let vis = ui.visuals();
    let p = ui.painter();
    let radius = CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 };
    if active {
        let border = vis.widgets.noninteractive.bg_stroke;
        p.rect_filled(rect, radius, vis.panel_fill);
        p.rect_stroke(rect, radius, border, StrokeKind::Inside);
        let gap = Rect::from_x_y_ranges(
            rect.left() + border.width..=rect.right() - border.width,
            rect.bottom() - border.width - 0.5..=rect.bottom(),
        );
        p.rect_filled(gap, 0.0, vis.panel_fill);
        p.rect_filled(Rect::from_min_size(rect.min, vec2(rect.width(), 2.0)), radius, vis.hyperlink_color);
    } else {
        let t = if hovered { 0.2 } else { 0.5 };
        let r = Rect::from_min_max(pos2(rect.left(), rect.top() + 3.0), rect.max);
        p.rect_filled(r, radius, vis.panel_fill.lerp_to_gamma(vis.extreme_bg_color, t));
    }
}

/// Dibuja una pestaña. Devuelve su rectángulo, la acción pedida y si se está arrastrando.
fn tab_widget(ui: &mut Ui, t: &DocTab, active: bool) -> (Rect, Option<TabAction>, bool) {
    let font = TextStyle::Button.resolve(ui.style());
    let mut label = t.title.clone();
    if t.error.is_some() {
        label.push(' ');
        label.push(Icon::TriangleAlert.unicode());
    }
    let galley = ui.painter().layout_no_wrap(label, font.clone(), Color32::PLACEHOLDER);
    // Centro vertical de las mayúsculas dentro de la fila: la caja de línea deja espacio
    // para acentos y descendentes, así que centrarla no centra las letras ni las alinea con la X.
    let cap_mid = ui.painter().layout_no_wrap("H".into(), font, Color32::PLACEHOLDER).mesh_bounds.center().y;
    let size = vec2(TAB_PAD + galley.size().x + 6.0 + TAB_CLOSE + 5.0, tab_height(ui));
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    // Id estable por pestaña: así el arrastre sigue aunque cambie de posición.
    let resp = ui.interact(rect, Id::new(("tab", t.uid)), Sense::click_and_drag());
    let y_off = if active { 0.0 } else { 1.5 };
    let close_rect = Rect::from_center_size(
        pos2(rect.right() - 5.0 - TAB_CLOSE / 2.0, rect.center().y + y_off),
        Vec2::splat(TAB_CLOSE),
    );
    let close = ui.interact(close_rect, Id::new(("tab_close", t.uid)), Sense::click());
    let hovered = resp.hovered() || close.hovered();

    paint_tab_bg(ui, rect, active, hovered);
    let vis = ui.visuals();
    let color = if t.error.is_some() {
        vis.warn_fg_color
    } else if active {
        vis.strong_text_color()
    } else if hovered {
        vis.text_color()
    } else {
        vis.weak_text_color()
    };
    let p = ui.painter();
    p.galley(pos2(rect.left() + TAB_PAD, rect.center().y - cap_mid + y_off), galley, color);

    // Cierre: solo el glifo, sin fondo de botón. Con cambios sin aplicar se ve un punto.
    let c = close_rect.center();
    if close.hovered() || hovered || (active && !t.source_dirty) {
        let col = if close.hovered() { vis.strong_text_color() } else { vis.weak_text_color() };
        let st = Stroke::new(if close.hovered() { 1.7 } else { 1.2 }, col);
        let s = 3.5;
        p.line_segment([c + vec2(-s, -s), c + vec2(s, s)], st);
        p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], st);
    } else if t.source_dirty {
        p.circle_filled(c, 3.5, color);
    }

    let hover = match (&t.compare, &t.path) {
        (Some(c), _) => format!(
            "Comparación de elementos (pestaña temporal: la siguiente comparación la reutiliza)\n{}\n{}",
            icons::text(Icon::ArrowLeft, &c.titles.0),
            icons::text(Icon::ArrowRight, &c.titles.1)
        ),
        (None, Some(p)) => format!("{}\nDoble clic o F2 para cambiar el nombre", p.display()),
        (None, None) => "Sin archivo\nDoble clic o F2 para cambiar el nombre".to_string(),
    };
    let close = close.on_hover_text("Cerrar (Ctrl+W)");
    let resp = resp.on_hover_text(hover);
    let mut act = None;
    if close.clicked() || resp.middle_clicked() {
        act = Some(TabAction::Close);
    } else if resp.double_clicked() {
        act = Some(TabAction::Rename);
    } else if resp.clicked() || resp.drag_started() {
        act = Some(TabAction::Select);
    }
    let dragged = resp.dragged();
    if dragged {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    resp.context_menu(|ui| {
        if ui.add(egui::Button::new("Cambiar nombre…").shortcut_text("F2")).clicked() {
            act = Some(TabAction::Rename);
        }
        if t.path.is_some() && ui.button("Copiar ruta del archivo").clicked() {
            act = Some(TabAction::CopyPath);
        }
        if !active
            && t.compare.is_none()
            && ui.add(egui::Button::new("Comparar con la pestaña activa").shortcut_text("Ctrl+5")).clicked()
        {
            act = Some(TabAction::CompareWithActive);
        }
        ui.separator();
        if ui.add(egui::Button::new("Cerrar").shortcut_text("Ctrl+W")).clicked() {
            act = Some(TabAction::Close);
        }
        if ui.button("Cerrar las demás").clicked() {
            act = Some(TabAction::CloseOthers);
        }
    });
    (rect, act, dragged)
}

/// Pestaña en modo de edición del nombre. Devuelve `Some` al terminar:
/// `Some(Some(nombre))` para aplicar, `Some(None)` si se canceló.
fn rename_tab(ui: &mut Ui, r: &mut Rename) -> (Rect, Option<Option<String>>) {
    let font = TextStyle::Button.resolve(ui.style());
    let text_w = ui.painter().layout_no_wrap(r.text.clone(), font.clone(), Color32::PLACEHOLDER).size().x;
    let rect = Rect::from_min_size(ui.cursor().min, vec2((text_w + 2.0 * TAB_PAD + 24.0).max(140.0), tab_height(ui)));
    paint_tab_bg(ui, rect, true, false);
    let id = Id::new(("tab_rename", r.uid));
    let resp = ui.put(
        rect.shrink2(vec2(5.0, 5.0)),
        egui::TextEdit::singleline(&mut r.text).id(id).font(font).margin(vec2(4.0, 1.0)),
    );
    let mut done = None;
    if r.focus {
        let mut st = TextEditState::load(ui.ctx(), id).unwrap_or_default();
        let n = r.text.chars().count();
        st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
        st.store(ui.ctx(), id);
        resp.request_focus();
        r.focus = false;
    } else if !resp.has_focus() {
        let cancel = ui.input(|i| i.key_pressed(Key::Escape));
        let name = r.text.trim();
        done = Some((!cancel && !name.is_empty()).then(|| name.to_string()));
    }
    (rect, done)
}

fn human_size(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

enum Welcome {
    Open,
    Import,
}

/// Pantalla inicial. Devuelve el botón pulsado.
fn welcome(ui: &mut Ui) -> Option<Welcome> {
    let mut open = None;
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        ui.heading("Visor JSON");
        ui.add_space(8.0);
        ui.label("Abre un archivo (Ctrl+O), arrástralo aquí o pega un JSON (Ctrl+V).");
        ui.label(RichText::new("Se aceptan comentarios // y /* */ y comas finales.").weak());
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let w = 450.0;
            ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
            if ui.button(icons::text(Icon::FolderOpen, "Abrir archivo…")).on_hover_text("Ctrl+O").clicked() {
                open = Some(Welcome::Open);
            }
            if ui.button(icons::text(Icon::ClipboardPaste, "Pegar del portapapeles")).on_hover_text("Ctrl+V").clicked()
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            if ui.button(icons::text(Icon::Database, "Importar tabla de SQL Server…")).on_hover_text("Ctrl+I").clicked()
            {
                open = Some(Welcome::Import);
            }
        });
    });
    open
}

fn source_ui(ui: &mut Ui, tab: &mut DocTab, set: &Settings) {
    if tab.source.len() > SOURCE_EDIT_LIMIT && !tab.force_edit {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(format!(
                "El documento es grande ({}); el editor está desactivado para mantener la fluidez.",
                human_size(tab.source.len())
            ));
            if ui.button("Editar de todos modos").clicked() {
                tab.force_edit = true;
            }
        });
        return;
    }
    let id = tab.source_id();
    egui::ScrollArea::both().auto_shrink(false).id_salt(("source_scroll", tab.uid)).show(ui, |ui| {
        let resp = ui.add(
            egui::TextEdit::multiline(&mut tab.source)
                .id(id)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(40)
                .lock_focus(true)
                .hint_text("Pega o escribe aquí el JSON…"),
        );
        if resp.changed() {
            tab.source_dirty = true;
        }
    });
    if tab.source_dirty && tab.source.len() <= LIVE_PARSE_LIMIT {
        tab.parse(set);
    }
}

fn find_in_source(ctx: &egui::Context, tab: &mut DocTab, q: &str, forward: bool) -> bool {
    let id = tab.source_id();
    let mut st = TextEditState::load(ctx, id).unwrap_or_default();
    let (sel_a, sel_b) = st
        .cursor
        .char_range()
        .map(|r| {
            let (a, b) = (r.primary.index.0, r.secondary.index.0);
            (a.min(b), a.max(b))
        })
        .unwrap_or((0, 0));
    let lower = tab.source.to_ascii_lowercase();
    let byte_of = |c: usize| lower.char_indices().nth(c).map_or(lower.len(), |(b, _)| b);
    let hit = if forward {
        let from = byte_of(sel_b);
        lower[from..].find(q).map(|i| i + from).or_else(|| lower.find(q))
    } else {
        let to = byte_of(sel_a);
        lower[..to].rfind(q).or_else(|| lower.rfind(q))
    };
    let Some(b) = hit else { return false };
    let start = lower[..b].chars().count();
    let end = start + lower[b..b + q.len()].chars().count();
    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(start), CCursor::new(end))));
    st.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
    true
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let candidates: [(&str, &str, FontFamily, bool); 2] = [
        ("consolas", "consola.ttf", FontFamily::Monospace, true),
        ("segoeui", "segoeui.ttf", FontFamily::Proportional, true),
    ];
    for (name, file, family, first) in candidates {
        let path = PathBuf::from(&windir).join("Fonts").join(file);
        if let Ok(bytes) = std::fs::read(&path) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
            let list = fonts.families.entry(family).or_default();
            if first {
                list.insert(0, name.into());
            } else {
                list.push(name.into());
            }
        }
    }
    // Segoe UI como respaldo de la monoespaciada (acentos, símbolos).
    if fonts.font_data.contains_key("segoeui") {
        fonts.families.entry(FontFamily::Monospace).or_default().push("segoeui".into());
    }
    icons::install(&mut fonts);
    ctx.set_fonts(fonts);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_style(&ctx);
        self.handle_input(&ctx);

        self.sync_filter();
        let top_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin { left: 8, right: 8, top: 2, bottom: 0 });
        egui::Panel::top("top").show_separator_line(false).frame(top_frame).show(ui, |ui| {
            self.toolbar(ui);
            ui.add_space(2.0);
            self.tab_strip(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.set.show_settings {
            egui::Panel::right("settings").default_size(320.0).show(ui, |ui| self.settings_panel(ui));
        }
        if self.set.show_filter {
            egui::Panel::left("filter").default_size(280.0).show(ui, |ui| self.filter_panel(ui));
        }
        self.sync_filter();
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));

        self.import_window(&ctx);
        if self.show_help {
            egui::Window::new("Atajos de teclado").open(&mut self.show_help).resizable(false).show(&ctx, help_ui);
        }

        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let rect = ctx.content_rect();
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("drop")));
            p.rect_filled(rect, 0.0, Color32::from_black_alpha(160));
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Suelta para abrir",
                FontId::proportional(28.0),
                Color32::WHITE,
            );
        }
        if self.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.set);
    }
}

fn help_ui(ui: &mut Ui) {
    egui::Grid::new("help").num_columns(2).striped(true).show(ui, |ui| {
        for (k, d) in [
            ("Ctrl+O", "Abrir archivo(s)"),
            ("Ctrl+V", "Pegar JSON como documento nuevo"),
            ("Ctrl+N", "Documento nuevo para escribir"),
            ("Ctrl+I", "Importar tabla de SQL Server (Copy with Headers)"),
            ("Ctrl+W / clic medio", "Cerrar pestaña"),
            ("Ctrl+Tab", "Siguiente pestaña"),
            ("F2 / doble clic en pestaña", "Cambiar el nombre de la pestaña (se usa al guardar)"),
            ("F5", "Recargar archivo"),
            ("Ctrl+1 … 5", "Árbol / Tabla / Texto / Fuente / Comparar"),
            ("F7, Mayús+F7", "Comparar: diferencia siguiente / anterior"),
            ("Ctrl+F, F3, Mayús+F3", "Buscar, siguiente, anterior"),
            ("Ctrl+Enter", "Aplicar cambios de la fuente"),
            ("", ""),
            ("↑ ↓ RePág AvPág Inicio Fin", "Moverse (árbol y tabla)"),
            ("← →", "Cerrar / expandir, ir al padre / primer hijo"),
            ("Enter, Espacio, doble clic", "Expandir/cerrar"),
            ("C", "Compactar nodo en una línea"),
            ("H", "Compactar hijos (cada hijo en una línea)"),
            ("X", "Cerrar hijos"),
            ("E", "Expandir todo el subárbol"),
            ("A", "Auto-compactar el subárbol"),
            ("G", "Ver arreglo como cuadrícula"),
            ("Ctrl+C", "Copiar el valor seleccionado"),
            ("Ctrl+clic en ▶", "Compactar"),
            ("Alt+clic en ▶", "Compactar hijos"),
            ("Mayús+clic en ▶", "Expandir todo el subárbol"),
            ("Clic derecho", "Más opciones: copiar ruta, cuadrícula, filtrar por clave…"),
            (
                "Clic derecho → Seleccionar para comparar",
                "Marca un elemento; en otro, «Comparar con…» abre el diff en una pestaña",
            ),
        ] {
            ui.label(RichText::new(k).monospace());
            ui.label(d);
            ui.end_row();
        }
    });
}
