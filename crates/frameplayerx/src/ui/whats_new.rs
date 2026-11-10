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
