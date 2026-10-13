//! Íconos Lucide como glifos de texto (la fuente va incrustada en el ejecutable) y
//! botones de ícono con su nombre en el tooltip.

use std::sync::Arc;

use eframe::egui::{self, Button, FontData, FontDefinitions, FontFamily, Response, RichText, TextStyle, Ui, Vec2};
pub use lucide_icons::Icon;

const FONT: &str = "lucide";

/// Registra la fuente Lucide justo después de la fuente principal de cada familia,
/// así cualquier texto puede incluir un ícono.
pub fn install(fonts: &mut FontDefinitions) {
    fonts.font_data.insert(FONT.into(), Arc::new(FontData::from_static(lucide_icons::LUCIDE_FONT_BYTES)));
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        let list = fonts.families.entry(fam).or_default();
        list.insert(1.min(list.len()), FONT.into());
    }
}

/// Texto con el ícono delante, para menús y botones con etiqueta.
pub fn text(icon: Icon, label: &str) -> String {
    format!("{}  {label}", icon.unicode())
}

/// El ícono solo, al tamaño de un botón de barra.
pub fn rich(ui: &Ui, icon: Icon) -> RichText {
    let size = TextStyle::Button.resolve(ui.style()).size * 1.25;
    RichText::new(icon.unicode()).size(size)
}

fn square(ui: &Ui) -> Vec2 {
    Vec2::splat(ui.spacing().interact_size.y + 2.0)
}

/// Tooltip que también se muestra con el botón desactivado.
pub fn tip(resp: Response, tip: &str) -> Response {
    resp.on_hover_text(tip).on_disabled_hover_text(tip)
}

/// Botón plano de ícono (solo muestra marco al pasar el cursor).
pub fn button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    enabled_button(ui, true, icon, tooltip)
}

pub fn enabled_button(ui: &mut Ui, enabled: bool, icon: Icon, tooltip: &str) -> Response {
    let b = Button::new(rich(ui, icon)).frame_when_inactive(false).min_size(square(ui));
    tip(ui.add_enabled(enabled, b), tooltip)
}

/// Botón de ícono pequeño y sin marco, para incrustar junto a un campo de texto.
pub fn small(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let b = Button::new(RichText::new(icon.unicode())).frame_when_inactive(false).small();
    tip(ui.add(b), tooltip)
}

/// Botón de ícono que se ve «hundido» cuando está seleccionado.
pub fn selectable(ui: &mut Ui, selected: bool, icon: Icon, tooltip: &str) -> Response {
    tip(ui.add(Button::selectable(selected, rich(ui, icon)).min_size(square(ui))), tooltip)
}

/// Interruptor de ícono.
pub fn toggle(ui: &mut Ui, on: &mut bool, icon: Icon, tooltip: &str) -> Response {
    let r = selectable(ui, *on, icon, tooltip);
    if r.clicked() {
        *on = !*on;
    }
    r
}

/// Botón de ícono que despliega un menú.
pub fn menu<R>(ui: &mut Ui, icon: Icon, tooltip: &str, content: impl FnOnce(&mut Ui) -> R) -> Response {
    let b = Button::new(rich(ui, icon)).frame_when_inactive(false).min_size(square(ui));
    let (resp, _) = egui::containers::menu::MenuButton::from_button(b).ui(ui, content);
    tip(resp, tooltip)
}

/// Botón de menú (texto con ícono delante y atajo a la derecha).
pub fn item(ui: &mut Ui, icon: Icon, label: &str, shortcut: &str) -> Response {
    item_enabled(ui, true, icon, label, shortcut)
}

pub fn item_enabled(ui: &mut Ui, enabled: bool, icon: Icon, label: &str, shortcut: &str) -> Response {
    let r = ui.add_enabled(enabled, Button::new(text(icon, label)).shortcut_text(shortcut));
    if r.clicked() {
        ui.close();
    }
    r
}
