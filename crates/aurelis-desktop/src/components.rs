use std::time::Duration;

use aurelis_app::{TrackId, TrackSummary};
use eframe::egui::{self, Align, Align2, FontId, Frame, Layout, RichText, Sense, Stroke, Vec2};

use crate::theme::{layout, palette::*, radius, spacing, typography};

#[derive(Clone, Copy)]
pub(crate) enum MessageTone {
    Info,
    Success,
    Error,
}

pub(crate) struct BannerMessage {
    pub(crate) text: String,
    pub(crate) tone: MessageTone,
}

pub(crate) fn section_heading(ui: &mut egui::Ui, label: &str) {
    ui.label(
        RichText::new(label)
            .size(typography::OVERLINE)
            .strong()
            .color(MUTED),
    );
}

pub(crate) fn navigation_button(
    ui: &mut egui::Ui,
    number: &str,
    label: &str,
    selected: bool,
) -> bool {
    let color = if selected { ACCENT } else { MUTED };
    let fill = if selected { ACCENT_DARK } else { SIDEBAR };
    ui.add_sized(
        [ui.available_width(), layout::NAVIGATION_BUTTON_HEIGHT],
        egui::Button::new(
            RichText::new(format!("{number}    {label}"))
                .size(typography::BODY)
                .color(if selected { TEXT } else { color }),
        )
        .fill(fill)
        .stroke(Stroke::NONE),
    )
    .clicked()
}

pub(crate) fn track_row(
    ui: &mut egui::Ui,
    index: usize,
    track: &TrackSummary,
    current_track: Option<TrackId>,
) -> bool {
    let selected = current_track == Some(track.id);
    let fill = if selected { ACCENT_DARK } else { SURFACE };
    let available_width = ui.available_width();
    let mut play = false;

    ui.push_id(track.id, |ui| {
        Frame::new()
            .fill(fill)
            .stroke(Stroke::new(
                layout::BORDER_WIDTH,
                if selected { ACCENT_DARK } else { EDGE },
            ))
            .inner_margin(egui::Margin::symmetric(
                layout::TRACK_ROW_INSET_X,
                layout::TRACK_ROW_INSET_Y,
            ))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:02}", index + 1))
                            .size(typography::CAPTION)
                            .color(MUTED),
                    );

                    if ui
                        .add_sized(
                            [layout::TRACK_TITLE_WIDTH, layout::TRACK_ROW_BUTTON_HEIGHT],
                            egui::Button::new(
                                RichText::new(&track.title)
                                    .size(typography::BODY)
                                    .strong()
                                    .color(TEXT),
                            )
                            .fill(fill)
                            .stroke(Stroke::NONE),
                        )
                        .clicked()
                    {
                        play = true;
                    }

                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(&track.artist)
                                .size(typography::BODY_SMALL)
                                .color(MUTED),
                        );
                        if available_width > layout::WIDE_LIBRARY_BREAKPOINT {
                            ui.label(
                                RichText::new(&track.album)
                                    .size(typography::OVERLINE)
                                    .color(MUTED),
                            );
                        }
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(track.duration.map_or("--:--".to_owned(), format_time))
                                .size(typography::CAPTION)
                                .color(MUTED),
                        );
                        if ui
                            .add_enabled(
                                !selected,
                                egui::Button::new(
                                    RichText::new("PLAY")
                                        .size(typography::MICRO)
                                        .strong()
                                        .color(ACCENT),
                                )
                                .fill(fill)
                                .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE)),
                            )
                            .clicked()
                        {
                            play = true;
                        }
                    });
                });
            });
        ui.add_space(spacing::GAP_5);
    });
    play
}

pub(crate) fn collection_row(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    track_count: usize,
) -> bool {
    let label = format!(
        "{title}
{subtitle} · {track_count} tracks"
    );
    ui.add_sized(
        [ui.available_width(), layout::COLLECTION_ROW_HEIGHT],
        egui::Button::new(
            RichText::new(label)
                .size(typography::BODY_SMALL)
                .color(TEXT),
        )
        .fill(SURFACE)
        .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE)),
    )
    .clicked()
}

pub(crate) fn library_statistic(ui: &mut egui::Ui, label: &str, count: usize) {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
        .inner_margin(egui::Margin::same(layout::CARD_INSET))
        .show(ui, |ui| {
            ui.set_min_width(layout::STATISTIC_CARD_MIN_WIDTH);
            ui.label(RichText::new(label).size(typography::CAPTION).color(MUTED));
            ui.label(
                RichText::new(count.to_string())
                    .size(typography::TITLE)
                    .strong()
                    .color(TEXT),
            );
        });
}
pub(crate) fn empty_state(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.add_space(spacing::GAP_58);
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
        .inner_margin(egui::Margin::same(layout::EMPTY_STATE_INSET))
        .show(ui, |ui| {
            ui.set_min_height(layout::EMPTY_STATE_MIN_HEIGHT);
            ui.vertical_centered(|ui| {
                ui.add_space(spacing::GAP_28);
                ui.label(
                    RichText::new("◌")
                        .size(typography::NAVIGATION_ICON)
                        .color(ACCENT),
                );
                ui.add_space(spacing::GAP_8);
                ui.label(
                    RichText::new(title)
                        .size(typography::TITLE)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(spacing::GAP_7);
                ui.label(
                    RichText::new(description)
                        .size(typography::BODY_SMALL)
                        .color(MUTED),
                );
                ui.add_space(spacing::GAP_28);
            });
        });
}

pub(crate) fn artwork_fallback(ui: &mut egui::Ui, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, radius::ARTWORK_FALLBACK, SURFACE);
    painter.rect_stroke(
        rect,
        radius::ARTWORK_FALLBACK,
        Stroke::new(layout::BORDER_WIDTH, EDGE),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "A",
        FontId::proportional(size.x.min(size.y) * 0.22),
        ACCENT_DARK,
    );
}

pub(crate) fn format_time(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60)
}

pub(crate) fn metadata_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(typography::CAPTION).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(typography::CAPTION).color(TEXT));
        });
    });
}

pub(crate) fn draw_message(ui: &mut egui::Ui, message: &BannerMessage) {
    let (color, dot) = match message.tone {
        MessageTone::Info => (MUTED, "•"),
        MessageTone::Success => (SUCCESS, "•"),
        MessageTone::Error => (ERROR, "•"),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(dot).color(color));
        ui.label(
            RichText::new(&message.text)
                .size(typography::BODY_SMALL)
                .color(color),
        );
    });
}
