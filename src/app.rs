use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id, Key, KeyboardShortcut,
    Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2, pos2,
    text::{CCursor, CCursorRange},
    text_edit::TextEditState,
    vec2,
};

use crate::common::{Mark, Out};
use crate::diff::{self, Diff, DiffView};
use crate::highlight::{self, Palette};
use crate::icons::{self, Icon};
use crate::import;
use crate::json::{self, Doc, Layout, NONE, NodeId, ParseError};
use crate::settings::{InitialFold, Settings, View};
use crate::table::{self, TableState};
use crate::text::{self, TextState};
use crate::tree::{self, Fold, TreeState};
use crate::update::{self, Event, Status, Updater};

/// Por encima de este tamaño el editor de la fuente se desactiva salvo que se pida.
const SOURCE_EDIT_LIMIT: usize = 4 * 1024 * 1024;
/// Por debajo de este tamaño la fuente se vuelve a analizar al escribir.
const LIVE_PARSE_LIMIT: usize = 512 * 1024;

const SETTINGS_KEY: &str = "jsonviewer.settings";

pub struct Loaded {
    pub doc: Doc,
    pub tree: TreeState,
    pub text: TextState,
    /// Vista de tabla (solo en pestañas de tabla; se construye al mostrarla).
    pub table: Option<TableState>,
    /// En un documento filtrado: id de cada nodo en el documento completo.
    src: Vec<NodeId>,
    last_match: NodeId,
    parse_ms: f64,
}

impl Loaded {
    fn new(doc: Doc, set: &Settings, parse_ms: f64) -> Self {
        let tree = TreeState::new(&doc, set);
        Self { doc, tree, text: TextState::default(), table: None, src: Vec::new(), last_match: NONE, parse_ms }
    }
}

/// Qué es la pestaña, además de un documento.
enum Mode {
    Doc,
    /// Tabla: el documento es un arreglo con una copia de los elementos elegidos en otra pestaña.
    Table(TableMeta),
    /// Comparación de dos documentos o elementos (sin documento propio).
    Compare(Box<Compare>),
}

struct TableMeta {
    /// Índice original de cada fila en el arreglo de origen.
    orig: Vec<u32>,
    /// Se muestra la tabla (si no, la vista elegida en la barra: árbol, texto o fuente).
    show: bool,
}

/// Comparación de dos documentos o elementos (de la misma pestaña o de distintas), con
/// copia propia de cada lado. Vive en su propia pestaña, como el diff de VS Code.
pub struct Compare {
    left: Doc,
    right: Doc,
    /// Encabezado de cada lado: archivo, ruta y línea.
    labels: (String, String),
    /// Copias filtradas por claves (solo mientras el filtro está activo).
    filtered: Option<(Doc, Doc)>,
    /// Coincidencias (de ambos lados) por cada clave del filtro.
    filter_counts: Vec<usize>,
    filter_gen: u64,
    /// (ordenar claves, sangría, generación del filtro) con que se calculó `diff`.
    key: Option<(bool, String, u64)>,
    diff: Option<(Diff, DiffView)>,
}

impl Compare {
    fn new(left: Doc, right: Doc, labels: (String, String)) -> Self {
        Self { left, right, labels, filtered: None, filter_counts: Vec::new(), filter_gen: 0, key: None, diff: None }
    }

    fn docs(&self) -> (&Doc, &Doc) {
        match &self.filtered {
            Some((l, r)) => (l, r),
            None => (&self.left, &self.right),
        }
    }

    fn update_filter(&mut self, keys: &[&str], gen_: u64) {
        if self.filter_gen == gen_ {
            return;
        }
        self.filter_gen = gen_;
        self.filtered = None;
        self.filter_counts.clear();
        if keys.is_empty() {
            return;
        }
        let (l, r) = (self.left.filter_keys(keys), self.right.filter_keys(keys));
        self.filter_counts = l.counts.iter().zip(&r.counts).map(|(a, b)| a + b).collect();
        self.filtered = Some((l.doc, r.doc));
    }

    fn ensure(&mut self, set: &Settings) {
        let gen_ = if self.filtered.is_some() { self.filter_gen } else { 0 };
        let key = (set.diff_sort_keys, set.indent_str(), gen_);
        if self.key.as_ref() != Some(&key) {
            let (l, r) = self.docs();
            let d = Diff::new(l, r, &key.1, key.0);
            self.diff = Some((d, DiffView::default()));
            self.key = Some(key);
        }
    }

    fn swap(&mut self) {
        std::mem::swap(&mut self.left, &mut self.right);
        std::mem::swap(&mut self.labels.0, &mut self.labels.1);
        if let Some((l, r)) = &mut self.filtered {
            std::mem::swap(l, r);
        }
        self.key = None;
        self.diff = None;
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
}

impl CompareMark {
    /// El marcador tal como lo ven las vistas de la pestaña `t`.
    fn view<'a>(&'a self, t: &DocTab) -> Mark<'a> {
        let same = t.uid == self.uid && t.shown_gen() == (self.doc_gen, self.filter_gen);
        Mark { path: &self.path, id: if same { self.id } else { NONE } }
    }
}

pub struct DocTab {
    uid: u64,
    title: String,
    path: Option<PathBuf>,
    /// Archivo del que deriva una pestaña de tabla (para los títulos de las comparaciones).
    origin: Option<String>,
    source: String,
    /// Líneas de `source` (para el ancho de los números de línea del editor).
    source_lines: usize,
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
    mode: Mode,
}

impl DocTab {
    fn new(uid: u64, title: String, path: Option<PathBuf>, source: String, set: &Settings) -> Self {
        let mut t = Self::empty(uid, title);
        t.path = path;
        t.set_source(source);
        if !t.source.trim().is_empty() {
            t.parse(set);
        }
        t
    }

    fn empty(uid: u64, title: String) -> Self {
        Self {
            uid,
            title,
            path: None,
            origin: None,
            source: String::new(),
            source_lines: 1,
            source_dirty: false,
            loaded: None,
            error: None,
            force_edit: false,
            filtered: None,
            filter_counts: Vec::new(),
            filter_gen: 0,
            doc_gen: 0,
            mode: Mode::Doc,
        }
    }

    /// Pestaña con un documento ya analizado (la fuente es su JSON formateado).
    fn with_doc(uid: u64, title: String, doc: Doc, set: &Settings) -> Self {
        let mut t = Self::empty(uid, title);
        t.set_source(json::to_string(&doc, Doc::ROOT, Layout::Pretty(&set.indent_str())));
        t.loaded = Some(Loaded::new(doc, set, 0.0));
        t.doc_gen = 1;
        t
    }

    fn set_source(&mut self, source: String) {
        self.source_lines = source.bytes().filter(|&b| b == b'\n').count() + 1;
        self.source = source;
    }

    fn is_blank(&self) -> bool {
        self.path.is_none() && self.source.trim().is_empty() && matches!(self.mode, Mode::Doc)
    }

    fn is_compare(&self) -> bool {
        matches!(self.mode, Mode::Compare(_))
    }

    /// Se muestra la tabla de una pestaña de tabla.
    fn table_shown(&self) -> bool {
        matches!(&self.mode, Mode::Table(m) if m.show)
    }

    /// Nombre del archivo (o de la pestaña) para los títulos de las comparaciones.
    fn file_label(&self) -> &str {
        self.origin.as_deref().unwrap_or(&self.title)
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

    /// Número de cada fila de la tabla (índice en el arreglo de origen), para el documento mostrado.
    fn table_labels(&self) -> Option<Vec<u32>> {
        let Mode::Table(m) = &self.mode else { return None };
        let (l, full) = (self.shown()?, self.loaded.as_ref()?);
        let labels = l
            .doc
            .node(Doc::ROOT)
            .children
            .iter()
            .map(|&c| {
                let in_full = if l.src.is_empty() { c } else { l.src[c as usize] };
                let i = full.doc.node(in_full).index as usize;
                m.orig.get(i).copied().unwrap_or(i as u32)
            })
            .collect();
        Some(labels)
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
        v.src = f.src;
        // El camino hasta las coincidencias siempre abierto; lo encontrado según los ajustes.
        for (id, &on_path) in f.path.iter().enumerate() {
            if on_path {
                v.tree.folds[id] = Fold::Expanded;
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
    /// Elemento marcado con «Seleccionar para comparar».
    mark: Option<CompareMark>,
    updater: Updater,
}

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
            mark: None,
            updater: Updater::default(),
        };
        for f in files {
            app.open_path(f);
        }
        if app.updater.check_on_start(&cc.egui_ctx, app.set.check_updates, app.set.last_update_check) {
            app.set.last_update_check = update::timestamp();
        }
        app
    }

    fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Resultados del hilo de actualización que merecen un toast.
    fn poll_updates(&mut self) {
        let Some(ev) = self.updater.poll() else { return };
        match ev {
            Event::UpToDate => self.notify(format!("Ya tienes la última versión ({})", update::CURRENT)),
            Event::Available(v) => self.notify(format!("Versión {v} disponible (ver la barra de estado)")),
            Event::Ready(v) => self.notify(format!("Versión {v} instalada: reinicia para usarla")),
            Event::Saved(p) => self.notify(format!("Ejecutable nuevo guardado en {}", p.display())),
            Event::Error(e) => self.notify(format!("Actualizaciones: {e}")),
        }
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

    fn new_uid(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid - 1
    }

    fn push_tab(&mut self, title: String, path: Option<PathBuf>, text: String) {
        let uid = self.new_uid();
        let tab = DocTab::new(uid, title, path, text, &self.set);
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

    /// Inserta una pestaña derivada (tabla o comparación) junto a la activa y la activa.
    fn insert_tab(&mut self, tab: DocTab) {
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.active = at;
    }

    /// Abre (o recarga) el archivo y devuelve el índice de su pestaña.
    fn open_path(&mut self, path: PathBuf) -> Option<usize> {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.notify(format!("No se pudo abrir {}: {e}", path.display()));
                return None;
            }
        };
        let text = json::decode_bytes(&bytes);
        self.set.add_recent(&path.to_string_lossy());
        if let Some(i) = self.tabs.iter().position(|t| t.path.as_ref() == Some(&path)) {
            self.active = i;
            let t = &mut self.tabs[i];
            t.set_source(text);
            t.parse(&self.set);
            return Some(i);
        }
        let title = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.push_tab(title, Some(path), text);
        Some(self.active)
    }

    fn file_dialog(&self) -> rfd::FileDialog {
        let mut dlg = rfd::FileDialog::new()
            .add_filter("JSON", &["json", "jsonc", "json5", "geojson", "har", "txt", "log"])
            .add_filter("Todos los archivos", &["*"]);
        if let Some(dir) = self.tabs.get(self.active).and_then(|t| t.path.as_ref()).and_then(|p| p.parent()) {
            dlg = dlg.set_directory(dir);
        }
        dlg
    }

    fn open_dialog(&mut self) {
        for f in self.file_dialog().pick_files().unwrap_or_default() {
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

    fn set_view(&mut self, v: View) {
        self.set.view = v;
        if let Some(Mode::Table(m)) = self.tabs.get_mut(self.active).map(|t| &mut t.mode) {
            m.show = false;
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
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if let Mode::Compare(c) = &mut t.mode {
            c.update_filter(&keys, self.filter_gen);
        } else {
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

    // ---------------------------------------------------------------- tabla

    /// Abre una pestaña de tabla con `items` (elementos del arreglo `array` de la pestaña activa).
    fn open_table(&mut self, array: NodeId, items: Vec<NodeId>) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(l) = t.shown() else { return };
        if items.is_empty() {
            return;
        }
        let doc = l.doc.array_of(&items);
        // Índice de cada elemento en el arreglo original (aunque la pestaña esté filtrada).
        let orig: Vec<u32> = items
            .iter()
            .map(|&i| match (&t.filtered, &t.loaded) {
                (Some(f), Some(full)) if !f.src.is_empty() => full.doc.node(f.src[i as usize]).index,
                _ => l.doc.node(i).index,
            })
            .collect();
        let total = l.doc.node(array).children.len();
        let file = t.file_label().to_string();
        let path = l.doc.path(array);
        let title = match path.strip_prefix("$.") {
            None => format!("{file} (tabla)"),
            Some(name) if items.len() == total => format!("{file} · {name}"),
            Some(name) => format!("{file} · {name} ({} de {total})", items.len()),
        };
        let uid = self.new_uid();
        let mut tab = DocTab::with_doc(uid, title, doc, &self.set);
        tab.origin = Some(file);
        tab.mode = Mode::Table(TableMeta { orig, show: true });
        self.insert_tab(tab);
    }

    // ---------------------------------------------------------------- comparación

    /// Abre una pestaña de comparación entre dos lados ya copiados.
    fn insert_compare(&mut self, left: Doc, right: Doc, labels: (String, String), title: String) {
        let uid = self.new_uid();
        let mut tab = DocTab::empty(uid, title);
        tab.mode = Mode::Compare(Box::new(Compare::new(left, right, labels)));
        self.insert_tab(tab);
    }

    /// Compara el documento completo de dos pestañas (la primera a la izquierda).
    fn compare_tabs(&mut self, li: usize, ri: usize) {
        let (Some(lt), Some(rt)) = (self.tabs.get(li), self.tabs.get(ri)) else { return };
        let (Some(l), Some(r)) = (lt.loaded.as_ref(), rt.loaded.as_ref()) else {
            return self.notify("Una de las dos pestañas no tiene un JSON válido");
        };
        let (left, right) = (l.doc.subtree(Doc::ROOT), r.doc.subtree(Doc::ROOT));
        let labels = (lt.title.clone(), rt.title.clone());
        let title = format!("{} ↔ {}", lt.file_label(), rt.file_label());
        self.active = li;
        self.insert_compare(left, right, labels, title);
    }

    /// Elige un archivo, lo abre en una pestaña y lo compara con la pestaña `li`.
    fn compare_with_file(&mut self, li: usize) {
        let Some(path) = self.file_dialog().pick_file() else { return };
        let uid = self.tabs.get(li).map(|t| t.uid);
        let Some(ri) = self.open_path(path) else { return };
        // Abrir puede mover las pestañas: se vuelve a buscar la original por su id.
        let Some(li) = self.tabs.iter().position(|t| Some(t.uid) == uid) else { return };
        if li != ri {
            self.compare_tabs(li, ri);
        }
    }

    /// Guarda el nodo marcado con «Seleccionar para comparar».
    fn mark_for_compare(&mut self, id: NodeId) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let Some(l) = t.shown() else { return };
        let (doc_gen, filter_gen) = t.shown_gen();
        let path = l.doc.path(id);
        self.mark = Some(CompareMark { uid: t.uid, doc_gen, filter_gen, id, path: path.clone() });
        self.notify(format!("Seleccionado para comparar: {path}. Clic derecho en otro elemento → «Comparar con…»"));
    }

    /// Compara el nodo marcado con `id` de la pestaña activa en una pestaña nueva.
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
        let ct = &self.tabs[self.active];
        let (Some(ml), Some(rl)) = (mt.shown(), ct.shown()) else { return };
        if mi == self.active && m.id == id {
            return self.notify("Elige un elemento distinto del marcado");
        }
        let (left, right) = (ml.doc.subtree(m.id), rl.doc.subtree(id));
        let labels = (header_label(mt, &ml.doc, m.id), header_label(ct, &rl.doc, id));
        let title = format!("{} ↔ {}", side_label(mt, &ml.doc, m.id), side_label(ct, &rl.doc, id));
        self.insert_compare(left, right, labels, title);
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
        // En la comparación la búsqueda solo resalta.
        if tab.is_compare() {
            return;
        }
        let in_table = tab.table_shown();
        let found = match (view, tab.shown_mut()) {
            (View::Tree, Some(l)) | (_, Some(l)) if in_table || view == View::Tree => {
                let doc = &l.doc;
                let n = doc.len() as u64;
                let sel = if in_table { l.table.as_ref().map_or(NONE, |t| t.selected) } else { l.tree.selected };
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
                    match (in_table, l.table.as_mut()) {
                        (true, Some(t)) => t.reveal(doc, i),
                        _ => l.tree.reveal(doc, i),
                    }
                }
                hit.is_some()
            }
            (View::Text, Some(l)) => {
                l.text.ensure(&l.doc, &indent);
                l.text.find(&q, forward)
            }
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
            let view = [(Key::Num1, View::Tree), (Key::Num2, View::Text), (Key::Num3, View::Source)]
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
        if (diff_next || diff_prev)
            && let Some(Mode::Compare(c)) = self.tabs.get_mut(self.active).map(|t| &mut t.mode)
            && let Some((_, v)) = c.diff.as_mut()
        {
            v.step(diff_next);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::NONE, Key::F2))) {
            self.start_rename(self.active);
        }
        if let Some(v) = view {
            self.set_view(v);
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
        let mut check_updates = false;
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
            let r = icons::item_enabled(ui, !self.updater.busy(), Icon::RefreshCw, "Buscar actualizaciones…", "");
            if r.clicked() {
                check_updates = true;
            }
            r.on_hover_text(format!("Versión actual: {}", update::CURRENT));
            ui.separator();
            if icons::item(ui, Icon::LogOut, "Salir", "").clicked() {
                quit = true;
            }
        });
        if check_updates {
            self.updater.check(ui.ctx(), true);
        }
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

            // Vistas (sin efecto en una pestaña de comparación). Una pestaña de tabla
            // tiene además el botón «Tabla».
            let is_compare = self.tabs.get(self.active).is_some_and(|t| t.is_compare());
            let table_shown = self.tabs.get(self.active).is_some_and(|t| t.table_shown());
            let mut pick_view = None;
            let mut pick_table = false;
            ui.add_enabled_ui(!is_compare, |ui| {
                if let Some(Mode::Table(m)) = self.tabs.get(self.active).map(|t| &t.mode)
                    && icons::selectable(ui, m.show, Icon::Table, "Tabla: una fila por elemento").clicked()
                {
                    pick_table = true;
                }
                for (v, icon, label, key) in VIEWS {
                    let on = !is_compare && !table_shown && view == v;
                    if icons::selectable(ui, on, icon, &format!("{label} ({key})")).clicked() {
                        pick_view = Some(v);
                    }
                }
            });
            if pick_table && let Some(Mode::Table(m)) = self.tabs.get_mut(self.active).map(|t| &mut t.mode) {
                m.show = true;
            }
            if let Some(v) = pick_view {
                self.set_view(v);
            }
            toolbar_sep(ui);

            // Herramientas de la vista
            let set = &mut self.set;
            let mut msg = None;
            if let Some(tab) = self.tabs.get_mut(self.active) {
                if let Mode::Compare(c) = &mut tab.mode {
                    if icons::button(ui, Icon::ArrowLeftRight, "Intercambiar lados").clicked() {
                        c.swap();
                    }
                    toolbar_sep(ui);
                    c.ensure(set);
                    diff_tools(ui, set, c.diff.as_mut().map(|(d, v)| (&*d, v)));
                } else if table_shown {
                    if let Some(l) = tab.shown()
                        && let Some(t) = &l.table
                        && icons::button(ui, Icon::Copy, "Copiar la tabla (separada por tabuladores)").clicked()
                    {
                        ui.ctx().copy_text(table::to_tsv(&l.doc, t));
                        msg = Some("Tabla copiada");
                    }
                } else {
                    match (view, tab.shown_mut()) {
                        (View::Tree, Some(l)) => {
                            if icons::button(ui, Icon::UnfoldVertical, "Expandir todo").clicked() {
                                l.tree.set_all(Fold::Expanded);
                            }
                            if icons::button(ui, Icon::FoldVertical, "Cerrar todo").clicked() {
                                l.tree.level(&l.doc, 1, Fold::Closed);
                            }
                            if icons::button(
                                ui,
                                Icon::ListCollapse,
                                "Compactar todo: raíz abierta, cada hijo en una línea",
                            )
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
                                        tab.set_source(s);
                                        tab.parse(set);
                                    }
                                    Err(_) => msg = Some("No se puede formatear: el JSON tiene errores"),
                                }
                            }
                            if icons::button(ui, Icon::Minimize2, "Minificar: todo en una línea").clicked() {
                                match fmt(&tab.source, Layout::Minified) {
                                    Ok(s) => {
                                        tab.set_source(s);
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

    fn tab_strip(&mut self, ui: &mut Ui) {
        let mut action = None;
        let mut renamed: Option<Option<String>> = None;
        let mut new = false;
        let mut active_rect = None;
        let mut tabs_clip = Rect::NOTHING;
        let mut bottom = None;
        let mut rects = Vec::with_capacity(self.tabs.len());
        let mut dragging = None;
        // Pestañas con documento, para el submenú «Comparar con…».
        let others: Vec<(u64, String)> =
            self.tabs.iter().filter(|t| t.loaded.is_some()).map(|t| (t.uid, t.title.clone())).collect();
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
                                let (rect, act, dragged) = tab_widget(ui, t, active, &others);
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
            Some((i, TabAction::CompareWith(uid))) => {
                if let Some(ri) = self.tabs.iter().position(|t| t.uid == uid) {
                    self.compare_tabs(i, ri);
                }
            }
            Some((i, TabAction::CompareWithFile)) => self.compare_with_file(i),
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
                    let sel = if tab.table_shown() {
                        l.table.as_ref().map_or(NONE, |t| t.selected)
                    } else if view == View::Tree {
                        l.tree.selected
                    } else {
                        NONE
                    };
                    if sel != NONE {
                        let path = l.doc.path(sel);
                        let line = l.doc.node(sel).line;
                        let text = if l.doc.src_lines > 1 { format!("{path}  · línea {line}") } else { path.clone() };
                        if ui
                            .add(egui::Label::new(RichText::new(text).monospace()).sense(egui::Sense::click()))
                            .on_hover_text("Clic para copiar la ruta")
                            .clicked()
                        {
                            ui.ctx().copy_text(path);
                        }
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(tab) = self.tabs.get(self.active) {
                    match &tab.mode {
                        Mode::Compare(c) => {
                            if let Some((d, _)) = &c.diff {
                                let (l, r) = c.docs();
                                let s = format!(
                                    "{} ↔ {} nodos · +{} −{} ~{} líneas",
                                    l.len(),
                                    r.len(),
                                    d.added,
                                    d.removed,
                                    d.modified
                                );
                                ui.label(RichText::new(s).weak());
                            }
                        }
                        _ => {
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
                                if tab.table_shown() {
                                    if let Some(t) = &l.table {
                                        parts.insert(0, format!("{} filas × {} columnas", t.rows(&l.doc), t.cols()));
                                    }
                                } else {
                                    match view {
                                        View::Tree => parts.insert(1, format!("{} líneas", l.tree.line_count())),
                                        View::Text => parts.insert(1, format!("{} líneas", l.text.line_count())),
                                        View::Source => parts.insert(1, format!("{} líneas", tab.source_lines)),
                                    }
                                }
                                if full.parse_ms > 0.0 {
                                    parts.push(format!("{:.1} ms", full.parse_ms));
                                }
                            }
                            ui.label(RichText::new(parts.join(" · ")).weak());
                        }
                    }
                }
                self.update_bar(ui);
                if let Some((m, _)) = &self.toast {
                    ui.separator();
                    ui.label(RichText::new(m).color(ui.visuals().warn_fg_color));
                }
            });
        });
    }

    /// Aviso de versión nueva en la barra de estado (se dibuja de derecha a izquierda).
    fn update_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let accent = ui.visuals().hyperlink_color;
        match &self.updater.status {
            Status::Available(rel) => {
                ui.separator();
                let notes = ui.small_button("Ver notas").on_hover_text(&rel.notes_url);
                let tip = format!(
                    "Descarga {} ({}) junto al ejecutable actual y lo reemplaza; luego se pide reiniciar.\n{}",
                    rel.asset_name,
                    human_size(rel.size as usize),
                    update::sha_note(rel)
                );
                let go = ui.small_button(icons::text(Icon::Download, "Actualizar")).on_hover_text(tip);
                ui.label(RichText::new(format!("Versión {} disponible", rel.version)).color(accent));
                if notes.clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(&rel.notes_url));
                }
                if go.clicked() {
                    self.updater.download(&ctx);
                }
            }
            Status::Downloading { release, received, total } => {
                ui.separator();
                let frac = if *total > 0 { (*received as f32 / *total as f32).min(1.0) } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).desired_width(110.0).show_percentage());
                ui.label(RichText::new(format!("Descargando la versión {}…", release.version)).weak());
                ctx.request_repaint_after(Duration::from_millis(250));
            }
            Status::Ready { version } => {
                ui.separator();
                let version = version.clone();
                if self.updater.postponed {
                    if ui
                        .small_button(icons::text(Icon::RotateCw, "Reiniciar"))
                        .on_hover_text("Usar la versión nueva")
                        .clicked()
                    {
                        self.restart(&ctx);
                    }
                    ui.label(RichText::new(format!("Versión {version} lista")).weak());
                } else {
                    let later = ui
                        .small_button("Más tarde")
                        .on_hover_text("La versión nueva se usará la próxima vez que abras el programa");
                    let now = ui.small_button(icons::text(Icon::RotateCw, "Reiniciar ahora"));
                    ui.label(RichText::new(format!("Versión {version} instalada")).color(accent));
                    if later.clicked() {
                        self.updater.postponed = true;
                    }
                    if now.clicked() {
                        self.restart(&ctx);
                    }
                }
            }
            Status::Saved { version, path, reason } => {
                ui.separator();
                let dismiss = ui.small_button("Descartar aviso");
                let show = ui.small_button(icons::text(Icon::ExternalLink, "Ver archivo"));
                let tip = format!(
                    "{reason}\nEl ejecutable nuevo quedó en:\n{}\nReemplaza con él el actual cuando puedas.",
                    path.display()
                );
                ui.label(
                    RichText::new(format!("Versión {version} descargada, sin instalar"))
                        .color(ui.visuals().warn_fg_color),
                )
                .on_hover_text(&tip);
                if show.clicked() {
                    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
                }
                if dismiss.clicked() {
                    self.updater.status = Status::Unchecked;
                }
            }
            Status::Unchecked | Status::Checking | Status::UpToDate | Status::Error(_) => {}
        }
    }

    /// Lanza el ejecutable nuevo con los archivos abiertos y cierra este.
    fn restart(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = self.tabs.iter().filter_map(|t| t.path.clone()).collect();
        if let Err(e) = self.updater.restart(ctx, &files) {
            self.notify(e);
        }
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
            ui.checkbox(&mut s.line_numbers, "Números de línea (árbol, texto y fuente)");

            ui.separator();
            ui.strong("Árbol");
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
            ui.strong("Actualizaciones");
            ui.checkbox(&mut s.check_updates, "Buscar actualizaciones al iniciar").on_hover_text(
                "Consulta GitHub como mucho una vez al día y avisa en la barra de estado si hay una versión nueva",
            );
            ui.label(RichText::new(format!("Versión actual: {}", update::CURRENT)).weak());

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
            let tab_counts: Option<&[usize]> = self.tabs.get(self.active).and_then(|t| match &t.mode {
                Mode::Compare(c) => c.filtered.is_some().then_some(c.filter_counts.as_slice()),
                _ => t.filtered.is_some().then_some(t.filter_counts.as_slice()),
            });
            self.set
                .filter_keys
                .iter()
                .map(|k| {
                    let pos = active.iter().position(|a| a.eq_ignore_ascii_case(k.trim()))?;
                    tab_counts.and_then(|c| c.get(pos).copied())
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
        let keys_free = !ui.ctx().text_edit_focused();
        let query = self.search.to_ascii_lowercase();
        let active_filter = self.set.active_filter();
        let set = &self.set;
        let pal = &self.pal;
        let tab = &mut self.tabs[self.active];
        let mark = self.mark.as_ref().map(|m| m.view(tab));
        let mut filter_off = false;

        // Pestaña de comparación: aviso de filtro, lista de cambios y diff.
        if let Mode::Compare(c) = &mut tab.mode {
            if c.filtered.is_some() {
                filter_off = filter_banner(ui, &active_filter, &c.filter_counts);
            }
            c.ensure(set);
            let labels = (c.labels.0.as_str(), c.labels.1.as_str());
            let mut want_all = false;
            if let Some((d, v)) = c.diff.as_mut() {
                want_all = diff_body(ui, set, pal, &query, d, v, labels);
            }
            if want_all {
                self.set.diff_only_changes = false;
            }
            if filter_off {
                self.set.filter_on = false;
            }
            return;
        }

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
                let id = tab.source_id();
                let ctx = ui.ctx().clone();
                let mut st = TextEditState::load(&ctx, id).unwrap_or_default();
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(e.char_offset))));
                st.store(&ctx, id);
                ctx.memory_mut(|m| m.request_focus(id));
                self.set_view(View::Source);
                return;
            }
        }

        let table_shown = tab.table_shown();
        let view = if tab.loaded.is_some() { set.view } else { View::Source };
        if tab.filtered.is_some() && (table_shown || view != View::Source) {
            filter_off = filter_banner(ui, &active_filter, &tab.filter_counts);
        }

        let mut out = None;
        if table_shown {
            let labels = tab.table_labels().unwrap_or_default();
            if let Some(l) = tab.shown_mut() {
                let Loaded { doc, table, .. } = l;
                let st = table.get_or_insert_with(|| TableState::new(doc, labels));
                out = table::ui(ui, doc, st, set, pal, &query, keys_free, mark);
            }
        } else {
            match (view, tab.shown_mut()) {
                (View::Tree, Some(l)) => out = tree::ui(ui, &l.doc, &mut l.tree, set, pal, &query, keys_free, mark),
                (View::Text, Some(l)) => {
                    l.text.ensure(&l.doc, &set.indent_str());
                    text::show(ui, &mut l.text, set, pal, &query);
                }
                _ => source_ui(ui, tab, set),
            }
        }
        if filter_off {
            self.set.filter_on = false;
        }
        match out {
            Some(Out::Table { array, items }) => self.open_table(array, items),
            Some(Out::FilterKey(k)) => self.add_filter_key(k),
            Some(Out::MarkCompare(id)) => self.mark_for_compare(id),
            Some(Out::CompareWithMark(id)) => self.compare_with_mark(id),
            None => {}
        }
    }
}

/// Etiqueta corta de un lado para el título de la pestaña: «archivo.json línea 12»
/// (la ruta si el archivo es de una sola línea; solo el archivo si es la raíz).
fn side_label(tab: &DocTab, doc: &Doc, id: NodeId) -> String {
    let file = tab.file_label();
    if id == Doc::ROOT {
        return file.to_string();
    }
    if doc.src_lines > 1 {
        format!("{file} línea {}", doc.node(id).line)
    } else {
        format!("{file} {}", doc.path(id).trim_start_matches('$').trim_start_matches('.'))
    }
}

/// Encabezado completo de un lado: archivo, ruta y línea.
fn header_label(tab: &DocTab, doc: &Doc, id: NodeId) -> String {
    let mut s = format!("{} · {}", tab.file_label(), doc.path(id));
    if id != Doc::ROOT && doc.src_lines > 1 {
        s.push_str(&format!(" · línea {}", doc.node(id).line));
    }
    s
}

/// Aviso de filtro activo con sus coincidencias. Devuelve `true` si se pidió quitar el filtro.
fn filter_banner(ui: &mut Ui, keys: &[&str], counts: &[usize]) -> bool {
    let mut off = false;
    let total: usize = counts.iter().sum();
    let missing: Vec<&str> = keys.iter().zip(counts).filter(|(_, c)| **c == 0).map(|(k, _)| *k).collect();
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
                    off = true;
                }
            });
        });
    off
}

/// Opciones y navegación de una comparación.
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
/// Devuelve `true` si se pidió ver todas las líneas (apagar «Solo diferencias»).
fn diff_body(
    ui: &mut Ui,
    set: &Settings,
    pal: &Palette,
    query: &str,
    d: &Diff,
    v: &mut DiffView,
    titles: (&str, &str),
) -> bool {
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
    std::mem::take(&mut v.want_all)
}

const VIEWS: [(View, Icon, &str, &str); 3] = [
    (View::Tree, Icon::ListTree, "Árbol", "Ctrl+1"),
    (View::Text, Icon::FileJson, "Texto formateado", "Ctrl+2"),
    (View::Source, Icon::SquarePen, "Fuente: editar el texto original", "Ctrl+3"),
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
    /// Comparar esta pestaña (izquierda) con la del id dado (derecha).
    CompareWith(u64),
    /// Comparar esta pestaña con un archivo que se elige en el momento.
    CompareWithFile,
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
/// `others`: pestañas con documento, para el submenú «Comparar con…».
fn tab_widget(ui: &mut Ui, t: &DocTab, active: bool, others: &[(u64, String)]) -> (Rect, Option<TabAction>, bool) {
    let font = TextStyle::Button.resolve(ui.style());
    let mut label = match &t.mode {
        Mode::Table(_) => icons::text(Icon::Table, &t.title),
        Mode::Compare(_) => icons::text(Icon::GitCompare, &t.title),
        Mode::Doc => t.title.clone(),
    };
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

    let rename_tip = "Doble clic o F2 para cambiar el nombre";
    let hover = match (&t.mode, &t.path) {
        (Mode::Compare(c), _) => format!(
            "Comparación (copia de cada lado al momento de compararlos)\n{}\n{}",
            icons::text(Icon::ArrowLeft, &c.labels.0),
            icons::text(Icon::ArrowRight, &c.labels.1)
        ),
        (Mode::Table(_), _) => {
            format!("Tabla de elementos de {}\n{rename_tip}", t.origin.as_deref().unwrap_or("otra pestaña"))
        }
        (Mode::Doc, Some(p)) => format!("{}\n{rename_tip}", p.display()),
        (Mode::Doc, None) => format!("Sin archivo\n{rename_tip}"),
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
        if t.loaded.is_some() {
            ui.separator();
            ui.menu_button(icons::text(Icon::GitCompare, "Comparar con…"), |ui| {
                let mut any = false;
                for (uid, title) in others.iter().filter(|(uid, _)| *uid != t.uid) {
                    any = true;
                    if ui.button(title).clicked() {
                        act = Some(TabAction::CompareWith(*uid));
                        ui.close();
                    }
                }
                if !any {
                    ui.add_enabled(false, egui::Button::new("(no hay otras pestañas con JSON)"));
                }
                ui.separator();
                if ui.button(icons::text(Icon::FolderOpen, "Otro archivo…")).clicked() {
                    act = Some(TabAction::CompareWithFile);
                    ui.close();
                }
            });
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

/// Editor de la fuente con números de línea en un margen a la izquierda.
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
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let font = TextStyle::Monospace.resolve(ui.style());
            let gutter = set.line_numbers.then(|| {
                let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
                let digits = (tab.source_lines.max(1).ilog10() + 1) as f32;
                ui.allocate_exact_size(vec2(digits * char_w + 18.0, 0.0), Sense::hover()).0
            });
            let out = egui::TextEdit::multiline(&mut tab.source)
                .id(id)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(40)
                .lock_focus(true)
                .hint_text("Pega o escribe aquí el JSON…")
                .show(ui);
            if out.response.response.changed() {
                tab.source_dirty = true;
                tab.source_lines = tab.source.bytes().filter(|&b| b == b'\n').count() + 1;
            }
            if let Some(g) = gutter {
                let clip = ui.clip_rect();
                let color = ui.visuals().weak_text_color();
                let p = ui.painter();
                // Cada fila del galley es una línea (no hay ajuste de línea); una fila que no
                // sigue a un salto de línea sería la continuación de la anterior.
                let (mut n, mut starts) = (1usize, true);
                for row in &out.galley.rows {
                    let y = out.galley_pos.y + row.pos.y;
                    let h = row.row.size.y;
                    if starts && y + h >= clip.top() && y <= clip.bottom() {
                        p.text(pos2(g.right() - 10.0, y + h / 2.0), Align2::RIGHT_CENTER, n, font.clone(), color);
                    }
                    starts = row.ends_with_newline;
                    if starts {
                        n += 1;
                    }
                }
            }
        });
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
        self.poll_updates();

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
            ("Ctrl+1 … 3", "Árbol / Texto / Fuente"),
            ("F7, Mayús+F7", "Comparación: diferencia siguiente / anterior"),
            ("Ctrl+F, F3, Mayús+F3", "Buscar, siguiente, anterior"),
            ("Ctrl+Enter", "Aplicar cambios de la fuente"),
            ("", ""),
            ("↑ ↓ RePág AvPág Inicio Fin", "Moverse (árbol y tabla)"),
            ("← →", "Cerrar / expandir, ir al padre / primer hijo"),
            ("Enter, Espacio, doble clic", "Expandir/cerrar"),
            ("Ctrl+clic, Mayús+clic", "Seleccionar varios elementos (para verlos como tabla)"),
            ("C", "Compactar nodo en una línea"),
            ("H", "Compactar hijos (cada hijo en una línea)"),
            ("X", "Cerrar hijos"),
            ("E", "Expandir todo el subárbol"),
            ("A", "Auto-compactar el subárbol"),
            ("G", "Ver el arreglo (o los elementos seleccionados) como tabla"),
            ("Ctrl+C", "Copiar el valor seleccionado"),
            ("Ctrl+clic en ▶", "Compactar"),
            ("Alt+clic en ▶", "Compactar hijos"),
            ("Mayús+clic en ▶", "Expandir todo el subárbol"),
            ("Clic derecho", "Más opciones: copiar ruta, ver como tabla, filtrar por clave…"),
            (
                "Clic derecho → Seleccionar para comparar",
                "Marca un elemento; en otro, «Comparar con…» abre el diff en una pestaña",
            ),
            ("Clic derecho en pestaña → Comparar con…", "Compara dos documentos completos"),
        ] {
            ui.label(RichText::new(k).monospace());
            ui.label(d);
            ui.end_row();
        }
    });
}
