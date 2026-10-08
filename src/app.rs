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

use crate::common::Out;
use crate::diff::{self, Diff, DiffView};
use crate::grid::{self, Grid};
use crate::highlight::{self, Palette};
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
        };
        if !t.source.trim().is_empty() {
            t.parse(set);
        }
        t
    }

    fn is_blank(&self) -> bool {
        self.path.is_none() && self.source.trim().is_empty()
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
        self.paste_n += 1;
        self.push_tab(format!("Pegado {}", self.paste_n), None, text);
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
        let found = self.compare_with.and_then(|uid| self.tabs.iter().position(|t| t.uid == uid));
        let i = match found.filter(|&i| i != self.active) {
            Some(i) => i,
            None => {
                if self.tabs.len() < 2 {
                    return None;
                }
                let i = if self.active > 0 { self.active - 1 } else { 1 };
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
            .filter(|&(i, _)| i != self.active)
            .map(|(_, t)| (t.uid, t.title.clone()))
            .collect();
        let Some(ri) = self.ensure_diff().or_else(|| self.compare_target()) else {
            ui.label(RichText::new("Abre otro JSON en una pestaña para comparar").weak());
            return;
        };
        ui.label("Comparar con:");
        let current = self.tabs[ri].title.clone();
        egui::ComboBox::from_id_salt("compare_with").selected_text(current).width(160.0).show_ui(ui, |ui| {
            for (uid, title) in &others {
                ui.selectable_value(&mut self.compare_with, Some(*uid), title);
            }
        });
        if ui.button("⇄").on_hover_text("Intercambiar lados").clicked() {
            self.compare_with = Some(self.tabs[self.active].uid);
            self.active = ri;
        }
        ui.separator();
        ui.checkbox(&mut self.set.diff_sort_keys, "Ignorar orden de claves");
        ui.checkbox(&mut self.set.diff_only_changes, "Solo diferencias")
            .on_hover_text("Pliega las líneas iguales lejos de un cambio");
        ui.checkbox(&mut self.set.diff_show_list, "Lista de cambios");
        ui.separator();
        if let Some((_, d, v)) = self.diff.as_mut() {
            if ui.button("▲").on_hover_text("Diferencia anterior (Mayús+F7)").clicked() {
                v.step(false);
            }
            if ui.button("▼").on_hover_text("Diferencia siguiente (F7)").clicked() {
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

    fn compare_ui(&mut self, ui: &mut Ui) {
        let Some(ri) = self.ensure_diff() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                if self.tabs.len() < 2 {
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
        if d.approximate {
            ui.label(
                RichText::new("Documentos muy distintos: la alineación de líneas es aproximada.")
                    .color(ui.visuals().warn_fg_color),
            );
        }
        if self.set.diff_show_list && !d.changes.is_empty() {
            egui::Panel::right("diff_changes").default_size(320.0).show(ui, |ui| diff::changes_list(ui, d, v));
        }
        diff::show(ui, d, v, (&titles.0, &titles.1), self.set.diff_only_changes, &self.pal, &query);
        if std::mem::take(&mut v.want_all) {
            self.set.diff_only_changes = false;
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
        let found = match (view, tab.shown_mut()) {
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
            let set = self.set.clone();
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.parse(&set);
            }
        }
        if next_tab && !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        let (diff_next, diff_prev) = ctx.input_mut(|i| {
            (i.consume_shortcut(&sc(Modifiers::NONE, Key::F7)), i.consume_shortcut(&sc(Modifiers::SHIFT, Key::F7)))
        });
        if (diff_next || diff_prev) && self.current_view() == View::Compare {
            if let Some((_, _, v)) = self.diff.as_mut() {
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

    fn menu_bar(&mut self, ui: &mut Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Archivo", |ui| {
                if ui.add(egui::Button::new("Abrir…").shortcut_text("Ctrl+O")).clicked() {
                    ui.close();
                    self.open_dialog();
                }
                if ui.add(egui::Button::new("Nuevo (pegar/escribir)").shortcut_text("Ctrl+N")).clicked() {
                    ui.close();
                    self.new_blank();
                    self.set.view = View::Source;
                }
                if ui.add(egui::Button::new("Pegar como documento nuevo").shortcut_text("Ctrl+V")).clicked() {
                    ui.close();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                }
                let has_path = self.tabs.get(self.active).is_some_and(|t| t.path.is_some());
                if ui.add_enabled(has_path, egui::Button::new("Recargar").shortcut_text("F5")).clicked() {
                    ui.close();
                    self.reload();
                }
                ui.menu_button("Recientes", |ui| {
                    if self.set.recent.is_empty() {
                        ui.label(RichText::new("(vacío)").weak());
                    }
                    let mut pick = None;
                    for p in &self.set.recent {
                        if ui.button(p).clicked() {
                            pick = Some(PathBuf::from(p));
                            ui.close();
                        }
                    }
                    if !self.set.recent.is_empty() {
                        ui.separator();
                        if ui.button("Limpiar lista").clicked() {
                            self.set.recent.clear();
                            ui.close();
                        }
                    }
                    if let Some(p) = pick {
                        self.open_path(p);
                    }
                });
                ui.separator();
                let loaded = self.tabs.get(self.active).is_some_and(|t| t.loaded.is_some());
                if ui.add_enabled(loaded, egui::Button::new("Guardar formateado como…")).clicked() {
                    ui.close();
                    self.save_current(true);
                }
                if ui.add_enabled(loaded, egui::Button::new("Guardar minificado como…")).clicked() {
                    ui.close();
                    self.save_current(false);
                }
                ui.separator();
                if ui
                    .add_enabled(!self.tabs.is_empty(), egui::Button::new("Cerrar pestaña").shortcut_text("Ctrl+W"))
                    .clicked()
                {
                    ui.close();
                    self.close_tab(self.active);
                }
                if ui.button("Salir").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Ver", |ui| {
                for (v, label, key) in VIEWS {
                    if ui.add(egui::Button::selectable(self.set.view == v, label).shortcut_text(key)).clicked() {
                        self.set.view = v;
                        ui.close();
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.set.dark, "Tema oscuro");
                ui.checkbox(&mut self.set.show_settings, "Panel de ajustes");
            });
            ui.menu_button("Ayuda", |ui| {
                if ui.button("Atajos de teclado").clicked() {
                    self.show_help = true;
                    ui.close();
                }
            });
        });
    }

    fn save_current(&mut self, pretty: bool) {
        let indent = self.set.indent_str();
        let Some(l) = self.tabs.get(self.active).and_then(|t| t.shown()) else { return };
        let layout = if pretty { Layout::Pretty(&indent) } else { Layout::Minified };
        let content = json::to_string(&l.doc, Doc::ROOT, layout);
        self.save_as(content);
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let view = self.current_view();
        ui.horizontal(|ui| {
            for (v, label, key) in VIEWS {
                if ui.selectable_label(view == v, label).on_hover_text(key).clicked() {
                    self.set.view = v;
                }
            }
            ui.separator();

            let set = &mut self.set;
            let mut msg = None;
            let mut save: Option<String> = None;
            if view == View::Compare {
                self.compare_toolbar(ui);
            } else if let Some(tab) = self.tabs.get_mut(self.active) {
                match (view, tab.shown_mut()) {
                    (View::Tree, Some(l)) => {
                        if ui.button("Expandir todo").clicked() {
                            l.tree.set_all(Fold::Expanded);
                        }
                        if ui.button("Cerrar todo").clicked() {
                            l.tree.level(&l.doc, 1, Fold::Closed);
                        }
                        if ui.button("Compactar todo").on_hover_text("Raíz expandida, cada hijo en una línea").clicked()
                        {
                            l.tree.level(&l.doc, 1, Fold::Compact);
                        }
                        if ui
                            .button("Auto")
                            .on_hover_text("Compacta en una línea todo lo que quepa en el ancho indicado")
                            .clicked()
                        {
                            l.tree.auto(&l.doc, Doc::ROOT, set);
                        }
                        ui.add(egui::DragValue::new(&mut set.auto_width).range(20..=400).suffix(" col"))
                            .on_hover_text("Ancho para Auto");
                        ui.separator();
                        ui.label("Nivel:");
                        for lv in 1..=5 {
                            if ui.small_button(lv.to_string()).clicked() {
                                let rest = if set.level_compact { Fold::Compact } else { Fold::Closed };
                                l.tree.level(&l.doc, lv, rest);
                            }
                        }
                        ui.checkbox(&mut set.level_compact, "resto compacto");
                    }
                    (View::Table, Some(l)) => {
                        if ui.button("Expandir todo").clicked() {
                            l.table.set_open(&l.doc, Doc::ROOT, true, true);
                        }
                        if ui.button("Contraer todo").clicked() {
                            l.table.collapse_all();
                        }
                        ui.checkbox(&mut set.table_preview, "Vista previa de objetos");
                    }
                    (View::Text, Some(l)) => {
                        ui.label("Sangría:");
                        indent_picker(ui, &mut set.indent);
                        l.text.ensure(&l.doc, &set.indent_str());
                        if ui.button("Copiar todo").clicked() {
                            ui.ctx().copy_text(l.text.text().to_string());
                            msg = Some("Texto copiado");
                        }
                        if ui.button("Guardar como…").clicked() {
                            save = Some(l.text.text().to_string());
                        }
                    }
                    _ => {
                        if ui
                            .add_enabled(tab.source_dirty, egui::Button::new("Aplicar"))
                            .on_hover_text("Ctrl+Enter")
                            .clicked()
                        {
                            tab.parse(set);
                        }
                        let fmt = |src: &str, layout: Layout| {
                            json::parse(src).map(|d| json::to_string(&d, Doc::ROOT, layout))
                        };
                        if ui
                            .button("Formatear")
                            .on_hover_text("Reescribe la fuente con sangría (quita comentarios)")
                            .clicked()
                        {
                            match fmt(&tab.source, Layout::Pretty(&set.indent_str())) {
                                Ok(s) => {
                                    tab.source = s;
                                    tab.parse(set);
                                }
                                Err(_) => msg = Some("No se puede formatear: el JSON tiene errores"),
                            }
                        }
                        if ui.button("Minificar").clicked() {
                            match fmt(&tab.source, Layout::Minified) {
                                Ok(s) => {
                                    tab.source = s;
                                    tab.parse(set);
                                }
                                Err(_) => msg = Some("No se puede minificar: el JSON tiene errores"),
                            }
                        }
                        if ui.button("Copiar").clicked() {
                            ui.ctx().copy_text(tab.source.clone());
                            msg = Some("Fuente copiada");
                        }
                    }
                }
            }
            if let Some(m) = msg {
                self.notify(m);
            }
            if let Some(content) = save {
                self.save_as(content);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.set.show_settings, "Ajustes");
                ui.toggle_value(&mut self.set.show_filter, "Filtro por claves")
                    .on_hover_text("Mostrar solo ciertas claves y el camino hasta ellas");
            });
        });
    }

    fn search_box(&mut self, ui: &mut Ui) {
        {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut go = None;
                if ui.small_button("▼").on_hover_text("Siguiente (F3 / Enter)").clicked() {
                    go = Some(true);
                }
                if ui.small_button("▲").on_hover_text("Anterior (Mayús+F3 / Mayús+Enter)").clicked() {
                    go = Some(false);
                }
                if let Some(c) = self.match_count() {
                    ui.label(RichText::new(format!("{c} coinc.")).weak());
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .id(Id::new("search_box"))
                        .hint_text("Buscar clave o valor (Ctrl+F)")
                        .desired_width(240.0),
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
            let tabs_w = (ui.available_width() - 420.0).max(120.0);
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
                    let plus =
                        egui::Button::new(RichText::new("+").size(ui.text_style_height(&TextStyle::Button) + 3.0))
                            .frame(false);
                    if ui.add(plus).on_hover_text("Nuevo (Ctrl+N)").clicked() {
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
                if let Some(tab) = self.tabs.get(self.active) {
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
            ui.heading("Ajustes");
            ui.add_space(4.0);
            egui::Grid::new("settings_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Tema");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut s.dark, true, "Oscuro");
                    ui.selectable_value(&mut s.dark, false, "Claro");
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
            if ui.button("Restablecer valores predeterminados").clicked() {
                let recent = std::mem::take(&mut s.recent);
                let filter_keys = std::mem::take(&mut s.filter_keys);
                *s = Settings { recent, filter_keys, show_settings: true, ..Default::default() };
            }
        });
    }

    fn filter_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.strong("Filtro por claves");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new("×").frame(false)).on_hover_text("Ocultar panel").clicked() {
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
                            if ui.add(egui::Button::new("×").frame(false)).on_hover_text("Quitar esta clave").clicked()
                            {
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
            if ui.add_enabled(n > 1, egui::Button::new("Quitar todas")).clicked() {
                self.set.filter_keys.clear();
            }
        });
        ui.label(RichText::new("Tip: clic derecho en un nodo → «Filtrar por esta clave».").weak().small());
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() {
            if welcome(ui) {
                self.open_dialog();
            }
            return;
        }
        let view = self.current_view();
        if view == View::Compare {
            self.compare_ui(ui);
            return;
        }
        let keys = !ui.ctx().text_edit_focused();
        let query = self.search.to_ascii_lowercase();
        let set = &self.set;
        let pal = &self.pal;
        let tab = &mut self.tabs[self.active];

        if let Some(e) = tab.error.clone() {
            let mut goto = false;
            egui::Frame::new()
                .fill(ui.visuals().error_fg_color.gamma_multiply(0.15))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("⚠ {e}")).color(ui.visuals().error_fg_color));
                        if tab.loaded.is_some() {
                            ui.label(RichText::new("· se muestra la última versión válida").weak());
                        }
                        if ui.small_button("Ir al error").clicked() {
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
                        ui.label(format!("Filtro por claves: {total} {what}"));
                        if !missing.is_empty() {
                            ui.label(
                                RichText::new(format!("· sin coincidencias: {}", missing.join(", ")))
                                    .color(ui.visuals().warn_fg_color),
                            );
                        }
                        if ui.small_button("Quitar filtro").clicked() {
                            filter_off = true;
                        }
                    });
                });
        }

        let mut out = None;
        let mut reveal = None;
        match (view, tab.shown_mut()) {
            (View::Tree, Some(l)) => out = tree::ui(ui, &l.doc, &mut l.tree, set, pal, &query, keys),
            (View::Table, Some(l)) => out = table::ui(ui, &l.doc, &mut l.table, set, pal, &query, keys),
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
        if let Some(Out::FilterKey(k)) = out {
            self.add_filter_key(k);
        }
    }
}

const VIEWS: [(View, &str, &str); 5] = [
    (View::Tree, "Árbol", "Ctrl+1"),
    (View::Table, "Tabla", "Ctrl+2"),
    (View::Text, "Texto", "Ctrl+3"),
    (View::Source, "Fuente", "Ctrl+4"),
    (View::Compare, "Comparar", "Ctrl+5"),
];

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
        label.push_str(" ⚠");
    }
    let galley = ui.painter().layout_no_wrap(label, font, Color32::PLACEHOLDER);
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
    p.galley(pos2(rect.left() + TAB_PAD, rect.center().y - galley.size().y / 2.0 + y_off), galley, color);

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

    let hover = format!(
        "{}\nDoble clic o F2 para cambiar el nombre",
        t.path.as_ref().map_or("Sin archivo".to_string(), |p| p.display().to_string())
    );
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
        if !active && ui.add(egui::Button::new("Comparar con la pestaña activa").shortcut_text("Ctrl+5")).clicked() {
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

/// Pantalla inicial. Devuelve `true` si se pulsó "Abrir archivo".
fn welcome(ui: &mut Ui) -> bool {
    let mut open = false;
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        ui.heading("Visor JSON");
        ui.add_space(8.0);
        ui.label("Abre un archivo (Ctrl+O), arrástralo aquí o pega un JSON (Ctrl+V).");
        ui.label(RichText::new("Se aceptan comentarios // y /* */ y comas finales.").weak());
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let w = 260.0;
            ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
            if ui.button("Abrir archivo…").clicked() {
                open = true;
            }
            if ui.button("Pegar del portapapeles").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
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
            self.menu_bar(ui);
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
        ] {
            ui.label(RichText::new(k).monospace());
            ui.label(d);
            ui.end_row();
        }
    });
}
