//! Look and feel of the viewer window: the beach palette of the landing
//! page, Nunito Sans, and rounded card-style widgets in a dark and a light
//! variant that follow the system theme.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontData, FontDefinitions, FontFamily, FontId, Margin, Pos2,
    Rect, Response, RichText, Sense, Shadow, Stroke, StrokeKind, TextStyle, Vec2,
};

pub const SKY: Color32 = Color32::from_rgb(63, 180, 230);
pub const SEA: Color32 = Color32::from_rgb(30, 151, 204);
pub const SUN: Color32 = Color32::from_rgb(255, 211, 92);
pub const INK: Color32 = Color32::from_rgb(23, 54, 74);
pub const CORAL: Color32 = Color32::from_rgb(240, 90, 90);
pub const GREEN: Color32 = Color32::from_rgb(70, 190, 120);
pub const AMBER: Color32 = Color32::from_rgb(240, 180, 60);

/// Font family names registered by [`fonts`].
const SEMIBOLD: &str = "NunitoSans-SemiBold";
const BOLD: &str = "NunitoSans-Bold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// The colours that differ between dark and light mode.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub accent: Color32,
    /// Text on an accent-filled surface.
    pub on_accent: Color32,
    /// The side panel's background.
    pub sidebar: Color32,
    /// The main area's background.
    pub canvas: Color32,
    pub card: Color32,
    pub card_stroke: Color32,
    pub button: Color32,
    pub button_hover: Color32,
    pub input: Color32,
    pub text: Color32,
    pub weak: Color32,
}

impl Palette {
    pub const DARK: Palette = Palette {
        dark: true,
        accent: SKY,
        on_accent: Color32::from_rgb(9, 30, 44),
        sidebar: Color32::from_rgb(16, 23, 30),
        canvas: Color32::from_rgb(22, 31, 40),
        card: Color32::from_rgb(29, 40, 52),
        card_stroke: Color32::from_rgb(45, 60, 75),
        button: Color32::from_rgb(39, 53, 67),
        button_hover: Color32::from_rgb(51, 68, 85),
        input: Color32::from_rgb(13, 19, 25),
        text: Color32::from_rgb(230, 237, 243),
        weak: Color32::from_rgb(139, 158, 174),
    };
    pub const LIGHT: Palette = Palette {
        dark: false,
        accent: SEA,
        on_accent: Color32::WHITE,
        sidebar: Color32::from_rgb(236, 241, 245),
        canvas: Color32::from_rgb(247, 249, 251),
        card: Color32::WHITE,
        card_stroke: Color32::from_rgb(220, 228, 235),
        button: Color32::from_rgb(241, 245, 248),
        button_hover: Color32::from_rgb(227, 235, 241),
        input: Color32::WHITE,
        text: INK,
        weak: Color32::from_rgb(98, 120, 136),
    };

    pub fn of(ui: &egui::Ui) -> Palette {
        if ui.visuals().dark_mode {
            Palette::DARK
        } else {
            Palette::LIGHT
        }
    }

    /// A translucent wash of `color` for tinted backgrounds.
    pub fn tint(&self, color: Color32) -> Color32 {
        color.gamma_multiply(if self.dark { 0.16 } else { 0.11 })
    }
}

/// Installs the fonts and both styles. Call once at start-up.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.style_mut_of(egui::Theme::Dark, |s| style(s, &Palette::DARK));
    ctx.style_mut_of(egui::Theme::Light, |s| style(s, &Palette::LIGHT));
}

/// Nunito Sans in front of egui's defaults, which stay as fallbacks for
/// symbols and emoji.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let regular = "NunitoSans-Regular";
    for (name, bytes) in [
        (
            regular,
            &include_bytes!("../assets/fonts/NunitoSans-Regular.ttf")[..],
        ),
        (
            SEMIBOLD,
            &include_bytes!("../assets/fonts/NunitoSans-SemiBold.ttf")[..],
        ),
        (BOLD, &include_bytes!("../assets/fonts/NunitoSans-Bold.ttf")[..]),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for name in [regular, SEMIBOLD, BOLD] {
        let family = if name == regular {
            FontFamily::Proportional
        } else {
            FontFamily::Name(name.into())
        };
        let mut list = vec![name.to_owned()];
        list.extend(fallbacks.iter().cloned());
        fonts.families.insert(family, list);
    }
    fonts
}

fn style(style: &mut egui::Style, p: &Palette) {
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(12.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Heading, bold(22.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
    ]
    .into();
    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 8.0);
    s.button_padding = Vec2::new(12.0, 6.0);
    s.interact_size = Vec2::new(40.0, 28.0);
    s.window_margin = Margin::same(18);
    s.menu_margin = Margin::same(8);
    s.combo_width = 260.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.indent = 18.0;

    let v = &mut style.visuals;
    let radius = CornerRadius::same(8);
    let widget = |fill: Color32, stroke: Color32, text_width: f32| egui::style::WidgetVisuals {
        weak_bg_fill: fill,
        bg_fill: fill,
        bg_stroke: Stroke::new(1.0, stroke),
        fg_stroke: Stroke::new(text_width, p.text),
        corner_radius: radius,
        expansion: 0.0,
    };
    v.widgets.noninteractive = widget(p.canvas, p.card_stroke, 1.0);
    v.widgets.inactive = widget(p.button, p.card_stroke, 1.0);
    v.widgets.hovered = widget(p.button_hover, p.accent.gamma_multiply(0.7), 1.5);
    v.widgets.active = widget(p.tint(p.accent), p.accent, 2.0);
    v.widgets.open = widget(p.button_hover, p.accent.gamma_multiply(0.7), 1.0);
    v.weak_text_color = Some(p.weak);
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.hyperlink_color = p.accent;
    v.panel_fill = p.canvas;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.card_stroke);
    v.window_corner_radius = CornerRadius::same(14);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 140 } else { 45 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 120 } else { 35 }),
    };
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.faint_bg_color = p.card;
    v.code_bg_color = p.button;
    v.warn_fg_color = AMBER;
    v.error_fg_color = CORAL;
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.resize_corner_size = 10.0;
}

/// A rounded, outlined surface for a group of related things.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.card_stroke))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
}

/// A tinted notice in `color` (updates, errors).
pub fn banner(ui: &egui::Ui, color: Color32) -> egui::Frame {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.tint(color))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 12))
}

/// Small upper-case caption above a group, like "FAVORITEN".
pub fn section_label(ui: &mut egui::Ui, text: &str) -> Response {
    let p = Palette::of(ui);
    ui.label(
        RichText::new(text.to_uppercase())
            .font(semibold(11.0))
            .color(p.weak),
    )
}

/// Caption above an input field.
pub fn field_label(ui: &mut egui::Ui, text: &str) {
    let p = Palette::of(ui);
    ui.label(RichText::new(text).font(semibold(12.5)).color(p.weak));
    ui.add_space(-3.0);
}

/// A small rounded tag, e.g. "Nur ansehen".
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> Response {
    let p = Palette::of(ui);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), semibold(11.0), color);
    let size = galley.size() + Vec2::new(14.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(99), p.tint(color));
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    response
}

/// A status dot with a soft halo.
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 7.0, color.gamma_multiply(0.25));
    painter.circle_filled(rect.center(), 4.0, color);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Filled with the accent colour: the one main action.
    Primary,
    /// Outlined on the button colour.
    Secondary,
    /// Like secondary, but in the danger colour.
    Danger,
    /// No frame until hovered: inline helpers like "anzeigen".
    Ghost,
}

/// A button drawn by us, so that hover and pressed states work with any
/// fill colour.
pub struct Button<'a> {
    text: &'a str,
    kind: Kind,
    enabled: bool,
    small: bool,
    min_size: Vec2,
}

impl<'a> Button<'a> {
    pub fn new(text: &'a str, kind: Kind) -> Self {
        Button {
            text,
            kind,
            enabled: true,
            small: false,
            min_size: Vec2::ZERO,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> Response {
        let p = Palette::of(ui);
        let font = match (self.kind, self.small) {
            (Kind::Primary, false) => semibold(14.0),
            (Kind::Primary, true) => semibold(12.5),
            (_, false) => FontId::proportional(14.0),
            (_, true) => FontId::proportional(12.5),
        };
        // PLACEHOLDER lets `galley()` apply the state-dependent colour below.
        let galley = ui
            .painter()
            .layout_no_wrap(self.text.to_owned(), font, Color32::PLACEHOLDER);
        let padding = if self.small {
            Vec2::new(10.0, 4.0)
        } else {
            Vec2::new(16.0, 7.0)
        };
        let size = (galley.size() + 2.0 * padding).max(self.min_size);
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                self.enabled && ui.is_enabled(),
                self.text,
            )
        });
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let hovered = self.enabled && response.hovered();
        let pressed = self.enabled && response.is_pointer_button_down_on();
        let (mut fill, mut stroke, mut text) = match self.kind {
            Kind::Primary => {
                let fill = if pressed {
                    p.accent.lerp_to_gamma(Color32::BLACK, 0.15)
                } else if hovered {
                    p.accent.lerp_to_gamma(Color32::WHITE, 0.12)
                } else {
                    p.accent
                };
                (fill, Color32::TRANSPARENT, p.on_accent)
            }
            Kind::Secondary | Kind::Danger => {
                let fill = if pressed || hovered {
                    p.button_hover
                } else {
                    p.button
                };
                let text = if self.kind == Kind::Danger { CORAL } else { p.text };
                (fill, p.card_stroke, text)
            }
            Kind::Ghost => {
                let fill = if pressed || hovered {
                    p.button_hover
                } else {
                    Color32::TRANSPARENT
                };
                (fill, Color32::TRANSPARENT, p.accent)
            }
        };
        if !self.enabled {
            fill = fill.gamma_multiply(0.5);
            stroke = stroke.gamma_multiply(0.5);
            text = text.gamma_multiply(0.45);
        }
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(if self.small { 7 } else { 9 }),
            fill,
            Stroke::new(1.0, stroke),
            StrokeKind::Inside,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, text);
        if self.enabled && response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(11),
                Stroke::new(2.0, p.accent),
                StrokeKind::Outside,
            );
        }
        if self.enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        }
    }
}

/// Convenience for the common cases.
pub fn button(ui: &mut egui::Ui, text: &str, kind: Kind) -> Response {
    Button::new(text, kind).show(ui)
}

/// The colour and monogram for a favorite's avatar, chosen by its name so
/// that every server keeps its look.
pub fn avatar(name: &str) -> (Color32, Color32, String) {
    const COLORS: [(Color32, bool); 7] = [
        (SKY, false),
        (SEA, true),
        (SUN, false),
        (Color32::from_rgb(242, 140, 110), true),
        (Color32::from_rgb(86, 196, 168), false),
        (Color32::from_rgb(152, 140, 232), true),
        (Color32::from_rgb(232, 180, 90), false),
    ];
    let hash = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let (fill, light_text) = COLORS[(hash % COLORS.len() as u32) as usize];
    let text = if light_text { Color32::WHITE } else { INK };
    let mut words = name.split_whitespace().filter_map(|w| w.chars().next());
    let mut monogram: String = words.next().into_iter().collect();
    if let Some(last) = words.next_back() {
        monogram.push(last);
    }
    if monogram.is_empty() {
        monogram.push('?');
    }
    (fill, text, monogram.to_uppercase())
}

/// Paints a rounded avatar square with the monogram.
pub fn paint_avatar(ui: &egui::Ui, rect: Rect, name: &str) {
    let (fill, text, monogram) = avatar(name);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        monogram,
        bold(if rect.width() > 40.0 { 18.0 } else { 14.0 }),
        text,
    );
}

/// A right-pointing play triangle, for "connect".
pub fn paint_play(ui: &egui::Ui, center: Pos2, size: f32, color: Color32) {
    let h = size / 2.0;
    let points = vec![
        center + Vec2::new(-h * 0.7, -h),
        center + Vec2::new(h * 0.9, 0.0),
        center + Vec2::new(-h * 0.7, h),
    ];
    ui.painter()
        .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// A chevron pointing right (`>`) or down, for collapsibles and rows.
pub fn paint_chevron(ui: &egui::Ui, center: Pos2, size: f32, down: bool, color: Color32) {
    let h = size / 2.0;
    let (a, b, c) = if down {
        (
            Vec2::new(-h, -h * 0.5),
            Vec2::new(0.0, h * 0.5),
            Vec2::new(h, -h * 0.5),
        )
    } else {
        (
            Vec2::new(-h * 0.5, -h),
            Vec2::new(h * 0.5, 0.0),
            Vec2::new(-h * 0.5, h),
        )
    };
    let stroke = Stroke::new(1.8, color);
    let painter = ui.painter();
    painter.line_segment([center + a, center + b], stroke);
    painter.line_segment([center + b, center + c], stroke);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms_take_first_and_last_word() {
        assert_eq!(avatar("Mac im Büro").2, "MB");
        assert_eq!(avatar("PC daheim").2, "PD");
        assert_eq!(avatar("mac.local").2, "M");
        assert_eq!(avatar("").2, "?");
        assert_eq!(avatar("ölaptop").2, "Ö");
    }

    #[test]
    fn avatar_colour_is_stable() {
        assert_eq!(avatar("Mac im Büro").0, avatar("Mac im Büro").0);
    }
}
