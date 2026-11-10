//! The window controls under a flat video screen: the move bar and the
//! resize handle. Plain greys and whites, like the Steam Frame's own windows.
//! No text; how big and where is `app::chrome::layout`.

use crate::app::chrome::layout::LOOK;
use egui::{Color32, Context, CornerRadius, LayerId, Pos2, Rect, Shape, Stroke, Vec2};

/// How a control looks: resting, with the laser on it, or held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleState {
    Rest,
    Hover,
    Held,
}

/// The control's white: dim at rest, brighter pointed at, full while held.
fn white(state: HandleState) -> Color32 {
    Color32::from_white_alpha(match state {
        HandleState::Rest => 140,
        HandleState::Hover => 230,
        HandleState::Held => 255,
    })
}

/// A dark rim behind the white, so it can be seen over a bright picture.
fn rim() -> Color32 {
    Color32::from_black_alpha(70)
}

/// The move bar: a pill in the middle of its panel.
pub fn move_bar(ctx: &Context, state: HandleState) {
    let area = ctx.screen_rect();
    let share = LOOK.pill_share();
    let size = Vec2::new(area.width() * share.x, area.height() * share.y);
    let pill = Rect::from_center_size(area.center(), size);
    let round = |r: Rect| CornerRadius::same((r.height() / 2.0) as u8);
    let painter = ctx.layer_painter(LayerId::background());
    let rimmed = pill.expand(pill.height() * 0.12);
    painter.rect_filled(rimmed, round(rimmed), rim());
    painter.rect_filled(pill, round(pill), white(state));
}

/// The resize handle: a quarter of a ring round the screen's corner, on the
/// outside of it (below and to the right).
pub fn resize_corner(ctx: &Context, state: HandleState) {
    let area = ctx.screen_rect();
    let share = LOOK.ring_centre();
    let centre = area.min + Vec2::new(area.width() * share.x, area.height() * share.y);
    let per_degree = area.width() / LOOK.corner_hit;
    let radius = LOOK.ring_radius * per_degree;
    let thickness = LOOK.ring_stroke * per_degree;
    const STEPS: usize = 24;
    let ring: Vec<Pos2> = (0..=STEPS)
        .map(|i| {
            let turn = std::f32::consts::FRAC_PI_2 * i as f32 / STEPS as f32;
            centre + Vec2::new(turn.cos(), turn.sin()) * radius
        })
        .collect();
    let painter = ctx.layer_painter(LayerId::background());
    let wide = Stroke::new(thickness + 6.0, rim());
    let narrow = Stroke::new(thickness, white(state));
    painter.add(Shape::line(ring.clone(), wide));
    painter.add(Shape::line(ring, narrow));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paints with `draw` into a panel's worth of points and returns how many
    /// shapes came out.
    fn painted(points: Vec2, draw: impl Fn(&Context)) -> usize {
        let ctx = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, points)),
            ..Default::default()
        };
        let out = ctx.run(input, draw);
        out.shapes.len()
    }

    #[test]
    fn the_controls_paint_in_every_state() {
        for state in [HandleState::Rest, HandleState::Hover, HandleState::Held] {
            let bar = painted(Vec2::new(320.0, 72.0), |ctx| move_bar(ctx, state));
            assert!(bar > 0, "{state:?}");
            let corner = painted(Vec2::new(128.0, 128.0), |ctx| resize_corner(ctx, state));
            assert!(corner > 0, "{state:?}");
        }
        // Brighter as it is pointed at, and brighter again held.
        let alpha = |s| white(s).a();
        assert!(alpha(HandleState::Rest) < alpha(HandleState::Hover));
        assert!(alpha(HandleState::Hover) < alpha(HandleState::Held));
    }
}
