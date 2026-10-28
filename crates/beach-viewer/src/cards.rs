//! The overview's building blocks: a responsive grid and the computer card
//! (favorites and account devices look the same).

use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Vec2};

use crate::theme::{self, Palette};

/// Height of a computer card.
pub const CARD_HEIGHT: f32 = 104.0;
/// Narrowest a card gets before the grid drops a column.
pub const CARD_MIN_WIDTH: f32 = 230.0;
/// Space between cards.
pub const GAP: f32 = 12.0;

/// Lays `count` items out in rows of equal-width cells. `item` gets the
/// cell's size and allocates it itself.
pub fn grid(ui: &mut egui::Ui, count: usize, mut item: impl FnMut(&mut egui::Ui, usize, Vec2)) {
    let avail = ui.available_width();
    let columns = (((avail + GAP) / (CARD_MIN_WIDTH + GAP)).floor() as usize).max(1);
    let width = ((avail - GAP * (columns - 1) as f32) / columns as f32).floor();
    let size = Vec2::new(width, CARD_HEIGHT);
    ui.spacing_mut().item_spacing = Vec2::splat(GAP);
    for row in 0..count.div_ceil(columns) {
        ui.horizontal(|ui| {
            for column in 0..columns {
                let index = row * columns + column;
                if index < count {
                    item(ui, index, size);
                }
            }
        });
    }
}

/// What a card shows.
pub struct Card<'a> {
    pub name: &'a str,
    /// Address or status line under the name.
    pub line: &'a str,
    /// Short labels at the bottom (settings that deviate).
    pub tags: &'a [&'a str],
    /// Online dot in front of the line: `Some(true)` green, `Some(false)` off.
    pub online: Option<bool>,
    /// Offer the "Bearbeiten" button.
    pub editable: bool,
}

/// What a click on a card asked for.
pub struct CardResponse {
    pub connect: bool,
    pub edit: bool,
    /// The card itself, for a context menu.
    pub response: Response,
}

/// One computer: avatar with monogram, name, address line, tags, and on
/// hover a play button (connect) and "Bearbeiten". A click anywhere else
/// on the card connects too.
pub fn card(ui: &mut egui::Ui, size: Vec2, card: Card<'_>) -> CardResponse {
    let p = Palette::of(ui);
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };
    let mut out = CardResponse {
        connect: false,
        edit: false,
        response: response.clone(),
    };
    // Registered after the card, so they take the clicks on their rects.
    let play_rect = Rect::from_center_size(rect.right_top() + Vec2::new(-30.0, 30.0), Vec2::splat(36.0));
    let play = ui
        .interact(play_rect, response.id.with("connect"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Verbinden");
    let edit_galley = ui
        .painter()
        .layout_no_wrap("Bearbeiten".into(), FontId::proportional(12.5), p.accent);
    let edit_size = edit_galley.size() + Vec2::new(16.0, 8.0);
    let edit_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 10.0 - edit_size.x, rect.bottom() - 10.0 - edit_size.y),
        edit_size,
    );
    let edit = if card.editable {
        Some(
            ui.interact(edit_rect, response.id.with("edit"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand),
        )
    } else {
        None
    };
    let edit_clicked = edit.as_ref().is_some_and(|e| e.clicked());
    if enabled {
        out.edit = edit_clicked;
        out.connect = !edit_clicked && (play.clicked() || response.clicked());
    }
    if !ui.is_rect_visible(rect) {
        return out;
    }
    let hovered = enabled
        && (response.contains_pointer() || play.contains_pointer() || edit.as_ref().is_some_and(|e| e.contains_pointer()));
    let (fill, stroke) = if hovered {
        (p.button, Stroke::new(1.0, p.accent.gamma_multiply(0.6)))
    } else {
        (p.card, Stroke::new(1.0, p.card_stroke))
    };
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(12), fill, stroke, StrokeKind::Inside);

    let avatar = Rect::from_min_size(rect.left_top() + Vec2::new(14.0, 14.0), Vec2::splat(40.0));
    theme::paint_avatar(ui, avatar, card.name);

    // Name and line stop before the play button.
    let text_left = avatar.right() + 12.0;
    let text_right = play_rect.left() - 6.0;
    let clip = Rect::from_min_max(Pos2::new(text_left, rect.top()), Pos2::new(text_right, rect.bottom()));
    let clipped = painter.with_clip_rect(clip);
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };
    clipped.text(
        Pos2::new(text_left, avatar.top() + 1.0),
        Align2::LEFT_TOP,
        card.name,
        theme::semibold(15.0),
        dim(p.text),
    );
    let mut line_left = text_left;
    if let Some(online) = card.online {
        let center = Pos2::new(text_left + 5.0, avatar.top() + 30.0);
        let color = if online { theme::GREEN } else { p.weak };
        clipped.circle_filled(center, 5.5, color.gamma_multiply(0.25));
        clipped.circle_filled(center, 3.0, color);
        line_left += 15.0;
    }
    clipped.text(
        Pos2::new(line_left, avatar.top() + 22.0),
        Align2::LEFT_TOP,
        card.line,
        FontId::proportional(12.5),
        dim(p.weak),
    );

    // Tags along the bottom; the edit button keeps the right corner.
    let mut tag_left = rect.left() + 14.0;
    let tag_limit = if card.editable { edit_rect.left() - 8.0 } else { rect.right() - 14.0 };
    for tag in card.tags {
        let galley = painter.layout_no_wrap((*tag).to_owned(), theme::semibold(10.5), p.weak);
        let size = galley.size() + Vec2::new(12.0, 5.0);
        if tag_left + size.x > tag_limit {
            break;
        }
        let tag_rect = Rect::from_min_size(Pos2::new(tag_left, rect.bottom() - 14.0 - size.y), size);
        painter.rect_filled(tag_rect, CornerRadius::same(99), p.tint(p.weak));
        painter.galley(tag_rect.center() - galley.size() / 2.0, galley, p.weak);
        tag_left = tag_rect.right() + 6.0;
    }

    if hovered {
        let fill = if play.hovered() {
            p.accent.lerp_to_gamma(Color32::WHITE, 0.15)
        } else {
            p.accent
        };
        painter.circle_filled(play_rect.center(), 16.0, fill);
        theme::paint_play(ui, play_rect.center() + Vec2::new(1.0, 0.0), 12.0, p.on_accent);
        if let Some(edit) = &edit {
            let fill = if edit.hovered() { p.button_hover } else { Color32::TRANSPARENT };
            painter.rect_filled(edit_rect, CornerRadius::same(7), fill);
            painter.galley(edit_rect.center() - edit_galley.size() / 2.0, edit_galley, p.accent);
        }
    } else {
        theme::paint_chevron(ui, play_rect.center(), 9.0, false, p.weak.gamma_multiply(0.5));
    }
    out
}

/// A dashed "+ Neu" cell at the end of the favorites grid.
pub fn add_card(ui: &mut egui::Ui, size: Vec2) -> Response {
    let p = Palette::of(ui);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = response.hovered();
    let color = if hovered { p.accent } else { p.weak };
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, CornerRadius::same(12), p.tint(p.accent));
    }
    // A dashed outline, drawn by hand: egui's rect stroke is solid.
    let inner = rect.shrink(0.5);
    let dash = 6.0;
    let mut edges = Vec::new();
    for (a, b) in [
        (inner.left_top(), inner.right_top()),
        (inner.right_top(), inner.right_bottom()),
        (inner.right_bottom(), inner.left_bottom()),
        (inner.left_bottom(), inner.left_top()),
    ] {
        edges.extend(egui::Shape::dashed_line(&[a, b], Stroke::new(1.0, color.gamma_multiply(0.7)), dash, dash));
    }
    painter.extend(edges);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "+ Neue Verbindung",
        theme::semibold(13.5),
        color,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, width: f32, mut run: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 600.0))),
                ..Default::default()
            },
            |ui| run(ui),
        );
        output.textures_delta.clear();
        output
    }

    #[test]
    fn grid_fills_rows_with_equal_cells() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut cells = Vec::new();
        frame(&ctx, 720.0, |ui| {
            grid(ui, 5, |ui, index, size| {
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                cells.push((index, rect));
            });
        });
        assert_eq!(cells.len(), 5);
        let width = cells[0].1.width();
        assert!(cells.iter().all(|(_, r)| (r.width() - width).abs() < 0.5));
        // 720 px fit three 230 px columns; the fourth card starts a new row.
        assert_eq!(cells[0].1.top(), cells[2].1.top());
        assert!(cells[3].1.top() > cells[2].1.bottom());
        assert_eq!(cells[3].1.left(), cells[0].1.left());
    }

    #[test]
    fn card_draws_name_line_and_tags() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let output = frame(&ctx, 400.0, |ui| {
            card(
                ui,
                Vec2::new(300.0, CARD_HEIGHT),
                Card {
                    name: "Mac Büro",
                    line: "192.168.1.20",
                    tags: &["Nur ansehen", "256 Farben"],
                    online: Some(true),
                    editable: true,
                },
            );
        });
        fn append(shape: &egui::epaint::Shape, text: &mut String) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    text.push_str(t.galley.text());
                    text.push('\n');
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|s| append(s, text)),
                _ => {}
            }
        }
        let mut text = String::new();
        for shape in &output.shapes {
            append(&shape.shape, &mut text);
        }
        assert!(text.contains("Mac Büro"), "{text}");
        assert!(text.contains("192.168.1.20"), "{text}");
        assert!(text.contains("Nur ansehen") && text.contains("256 Farben"), "{text}");
        // "Bearbeiten" only shows on hover.
        assert!(!text.contains("Bearbeiten"), "{text}");
    }
}
