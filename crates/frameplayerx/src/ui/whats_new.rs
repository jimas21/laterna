//! The "What's new" card: a few plain lines about the version just
//! installed, shown once after an update and from Settings > About.

use super::theme::{self, Weight};
use super::widgets::{self, Kind};
use super::{Action, View, icons};
use egui::RichText;

/// The card is no wider than this, however wide the panel.
const WIDTH: f32 = 760.0;

pub fn card(ui: &mut egui::Ui, v: &mut View) {
    let actions = &mut *v.actions;
    let Some(entry) = v.state.whats_new else {
        return;
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                let width = ui.available_width().min(WIDTH);
                egui::Frame::new()
                    .fill(theme::SURFACE_2)
                    .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                    .corner_radius(16)
                    .inner_margin(egui::Margin::same(28))
                    .show(ui, |ui| {
                        ui.set_width(width - 56.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(icons::SPARKLE)
                                        .font(theme::icon(32.0))
                                        .color(theme::ACCENT),
                                );
                                ui.label(
                                    RichText::new(tr!("whats-new-title", version = entry.version))
                                        .font(theme::font(Weight::Bold, 30.0))
                                        .color(theme::TEXT),
                                );
                            });
                            ui.add_space(14.0);
                            for bullet in entry.bullets {
                                ui.horizontal_top(|ui| {
                                    ui.label(
                                        RichText::new("•")
                                            .font(theme::font(Weight::Bold, 20.0))
                                            .color(theme::ACCENT),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(*bullet)
                                                .font(theme::font(Weight::Regular, 19.0))
                                                .color(theme::TEXT),
                                        )
                                        .wrap(),
                                    );
                                });
                                ui.add_space(6.0);
                            }
                            ui.add_space(14.0);
                            if widgets::button(
                                ui,
                                Some(icons::CHECK),
                                tr!("whats-new-ok"),
                                Kind::Primary,
                            )
                            .clicked()
                            {
                                actions.push(Action::DismissWhatsNew);
                            }
                        });
                    });
            });
            ui.add_space(24.0);
        });
}

/// The hints, in order: the icon, then the text.
fn tips_rows() -> [(&'static str, &'static str); 4] {
    [
        (icons::ARROWS_OUT_CARDINAL, tr!("tips-move")),
        (icons::FRAME_CORNERS, tr!("tips-resize")),
        (icons::ARMCHAIR, tr!("tips-seat")),
        (icons::SUN, tr!("tips-lights")),
    ]
}

/// The one-time hint card after an update: how to move and resize the
/// screen, pick a seat and dim the lights, and where the buttons can be
/// changed.
pub fn tips(ui: &mut egui::Ui, v: &mut View) {
    let actions = &mut *v.actions;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                let width = ui.available_width().min(WIDTH);
                egui::Frame::new()
                    .fill(theme::SURFACE_2)
                    .stroke(egui::Stroke::new(1.0_f32, theme::STROKE))
                    .corner_radius(16)
                    .inner_margin(egui::Margin::same(28))
                    .show(ui, |ui| {
                        ui.set_width(width - 56.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(icons::SPARKLE)
                                        .font(theme::icon(32.0))
                                        .color(theme::ACCENT),
                                );
                                ui.label(
                                    RichText::new(tr!("tips-title"))
                                        .font(theme::font(Weight::Bold, 30.0))
                                        .color(theme::TEXT),
                                );
                            });
                            ui.add_space(14.0);
                            for (icon, text) in tips_rows() {
                                ui.horizontal_top(|ui| {
                                    ui.add_sized(
                                        [36.0, 28.0],
                                        egui::Label::new(
                                            RichText::new(icon)
                                                .font(theme::icon(24.0))
                                                .color(theme::ACCENT),
                                        ),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(text)
                                                .font(theme::font(Weight::Regular, 19.0))
                                                .color(theme::TEXT),
                                        )
                                        .wrap(),
                                    );
                                });
                                ui.add_space(8.0);
                            }
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if widgets::button(
                                    ui,
                                    Some(icons::CHECK),
                                    tr!("tips-ok"),
                                    Kind::Primary,
                                )
                                .clicked()
                                {
                                    actions.push(Action::DismissTips);
                                }
                                if widgets::button(
                                    ui,
                                    Some(icons::GAME_CONTROLLER),
                                    tr!("tips-buttons"),
                                    Kind::Secondary,
                                )
                                .clicked()
                                {
                                    actions.push(Action::OpenControllerSettings);
                                }
                            });
                        });
                    });
            });
            ui.add_space(24.0);
        });
}
