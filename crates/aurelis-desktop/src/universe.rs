use aurelis_app::{DesktopPlaybackState, PlaybackView, TrackId, TrackSummary};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Frame, Layout, RichText, Sense, Stroke, Vec2,
};

use crate::theme::{layout, palette::*, spacing, typography};

const MAX_LIBRARY_STARS: usize = 72;
const MAX_VISIBLE_ORBITS: usize = 8;
const UNIVERSE_CANVAS_HEIGHT: f32 = 420.0;

pub(crate) enum UniverseAction {
    SelectTrack(TrackId),
    PlayTrack(TrackId),
    QueueTrack(TrackId),
    OpenNowPlaying,
}

pub(crate) fn draw(
    ui: &mut egui::Ui,
    tracks: &[TrackSummary],
    playback: &PlaybackView,
    selected_track_id: Option<TrackId>,
) -> Option<UniverseAction> {
    ui.add_space(spacing::GAP_34);
    ui.label(
        RichText::new("AURELIS UNIVERSE")
            .size(typography::OVERLINE)
            .strong()
            .color(ACCENT),
    );
    ui.add_space(spacing::GAP_8);
    ui.heading(
        RichText::new("Your music, in orbit")
            .size(typography::PAGE_TITLE)
            .strong()
            .color(TEXT),
    );
    ui.add_space(spacing::GAP_8);
    ui.label(
        RichText::new("Tracks appear as stars. Upcoming tracks orbit the Core.")
            .size(typography::BODY)
            .color(MUTED),
    );
    ui.add_space(spacing::GAP_18);

    let upcoming = upcoming_tracks(playback);
    let (canvas, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), UNIVERSE_CANVAS_HEIGHT),
        Sense::hover(),
    );
    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, egui::CornerRadius::same(6), SIDEBAR);
    painter.rect_stroke(
        canvas,
        egui::CornerRadius::same(6),
        Stroke::new(layout::BORDER_WIDTH, EDGE),
        egui::StrokeKind::Inside,
    );

    let center = canvas.center();
    let orbit_radius = (canvas.width().min(canvas.height()) * 0.43).max(120.0);
    for ring in [0.55, 0.76, 1.0] {
        painter.circle_stroke(
            center,
            orbit_radius * ring,
            Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), 42),
            ),
        );
    }

    let focus_track_id = selected_track_id.or(playback.current_track_id);
    let mut action = draw_library_stars(ui, &painter, center, orbit_radius, tracks, focus_track_id);

    let time = ui.input(|input| input.time) as f32;
    let is_playing = playback.state == DesktopPlaybackState::Playing;
    draw_queue_orbits(
        ui,
        &painter,
        QueueOrbit {
            center,
            radius: orbit_radius,
            track_ids: upcoming,
            tracks,
            selected_track_id: focus_track_id,
            phase: if is_playing { time * 0.12 } else { 0.0 },
        },
        &mut action,
    );

    let pulse = if is_playing {
        ((time * 1.8).sin() * 0.5 + 0.5) * 0.08
    } else {
        0.0
    };
    let core_radius = 46.0 * (1.0 + pulse);
    painter.circle_filled(
        center,
        core_radius + 13.0,
        Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), 22),
    );
    painter.circle_filled(center, core_radius, ACCENT_DARK);
    painter.circle_stroke(center, core_radius, Stroke::new(1.5, readable_accent()));
    let core_button = egui::Button::new(
        RichText::new("AURELIS\nCORE")
            .size(typography::CAPTION)
            .strong()
            .color(TEXT),
    )
    .fill(Color32::TRANSPARENT)
    .stroke(Stroke::NONE)
    .corner_radius(egui::CornerRadius::same(52));
    if ui
        .put(
            egui::Rect::from_center_size(center, Vec2::splat(core_radius * 2.0)),
            core_button,
        )
        .on_hover_text("Open Now Playing")
        .clicked()
    {
        return Some(UniverseAction::OpenNowPlaying);
    }

    let state_label = match playback.state {
        DesktopPlaybackState::Playing => "PLAYING",
        DesktopPlaybackState::Paused => "PAUSED",
        DesktopPlaybackState::Stopped => "STOPPED",
        DesktopPlaybackState::Finished => "FINISHED",
    };
    painter.text(
        egui::pos2(center.x, canvas.bottom() - 20.0),
        Align2::CENTER_CENTER,
        format!(
            "{} TRACKS  ·  {} UPCOMING  ·  {}",
            tracks.len(),
            upcoming.len(),
            state_label
        ),
        FontId::proportional(typography::CAPTION),
        MUTED,
    );

    if let Some(list_action) = draw_upcoming_list(ui, upcoming, tracks, focus_track_id) {
        action = Some(list_action);
    }
    if let Some(track) = focus_track_id.and_then(|id| find_track(tracks, id)) {
        ui.add_space(spacing::GAP_10);
        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
            .inner_margin(egui::Margin::same(layout::CARD_INSET))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(&track.title)
                                .size(typography::BODY_LARGE)
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new(format!("{} · {}", track.artist, track.album))
                                .size(typography::BODY_SMALL)
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Add to queue").clicked() {
                            action = Some(UniverseAction::QueueTrack(track.id));
                        }
                        if ui.button("Play").clicked() {
                            action = Some(UniverseAction::PlayTrack(track.id));
                        }
                    });
                });
            });
    } else if tracks.is_empty() {
        ui.add_space(spacing::GAP_10);
        ui.label(
            RichText::new("Scan a local music folder from Library to populate the stars.")
                .size(typography::BODY_SMALL)
                .color(MUTED),
        );
    } else {
        ui.add_space(spacing::GAP_10);
        ui.label(
            RichText::new("Select a star or orbiting track to inspect and play it.")
                .size(typography::BODY_SMALL)
                .color(MUTED),
        );
    }

    action
}

fn draw_library_stars(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    center: egui::Pos2,
    orbit_radius: f32,
    tracks: &[TrackSummary],
    selected_track_id: Option<TrackId>,
) -> Option<UniverseAction> {
    let visible_count = tracks.len().min(MAX_LIBRARY_STARS);
    let mut action = None;

    for slot in 0..visible_count {
        let track_index = visible_track_index(tracks.len(), MAX_LIBRARY_STARS, slot)
            .expect("slot is within the visible track count");
        let track = &tracks[track_index];
        let angle = slot as f32 * 2.399_963_1;
        let radial = 0.65 + (slot % 7) as f32 * 0.047;
        let star_radius = orbit_radius * radial;
        let position = egui::pos2(
            center.x + angle.cos() * star_radius,
            center.y + angle.sin() * star_radius,
        );
        let selected = selected_track_id == Some(track.id);
        let size = if selected {
            5.5
        } else {
            3.0 + (slot % 3) as f32 * 0.7
        };
        let color = if selected { TEXT } else { ACCENT };
        painter.circle_filled(position, size, color);

        let hit_rect = egui::Rect::from_center_size(position, Vec2::splat(22.0));
        let response = ui.interact(
            hit_rect,
            ui.id().with(("universe-library-star", track.id)),
            Sense::click(),
        );
        let hovered = response.hovered();
        let clicked = response.clicked();
        if hovered {
            painter.circle_stroke(position, size + 4.0, Stroke::new(1.0, TEXT));
            response.on_hover_text(format!("{} · {}", track.title, track.artist));
        }
        if clicked {
            action = Some(UniverseAction::SelectTrack(track.id));
        }
    }
    action
}

struct QueueOrbit<'a> {
    center: egui::Pos2,
    radius: f32,
    track_ids: &'a [TrackId],
    tracks: &'a [TrackSummary],
    selected_track_id: Option<TrackId>,
    phase: f32,
}

fn draw_queue_orbits(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    orbit: QueueOrbit<'_>,
    action: &mut Option<UniverseAction>,
) {
    if orbit.track_ids.is_empty() {
        return;
    }

    for (index, track_id) in orbit.track_ids.iter().take(MAX_VISIBLE_ORBITS).enumerate() {
        let Some(track) = find_track(orbit.tracks, *track_id) else {
            continue;
        };
        let ring = 0.38 + (index % 3) as f32 * 0.12;
        let angle = orbit.phase
            + index as f32 * std::f32::consts::TAU
                / orbit.track_ids.len().min(MAX_VISIBLE_ORBITS) as f32;
        let position = orbit.center + Vec2::angled(angle) * orbit.radius * ring;
        let selected = orbit.selected_track_id == Some(track.id);
        let radius = if selected { 9.0 } else { 6.0 };
        painter.circle_filled(
            position,
            radius + 3.0,
            Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), 40),
        );
        painter.circle_filled(position, radius, if selected { TEXT } else { ACCENT });

        let response = ui.interact(
            egui::Rect::from_center_size(position, Vec2::splat(26.0)),
            ui.id().with(("universe-queue-orbit", *track_id, index)),
            Sense::click(),
        );
        let hovered = response.hovered();
        let clicked = response.clicked();
        if hovered {
            painter.circle_stroke(position, radius + 5.0, Stroke::new(1.0, TEXT));
            response.on_hover_text(format!("Queued · {} · {}", track.title, track.artist));
        }
        if clicked {
            *action = Some(UniverseAction::SelectTrack(track.id));
        }
    }
}

fn draw_upcoming_list(
    ui: &mut egui::Ui,
    upcoming: &[TrackId],
    tracks: &[TrackSummary],
    selected_track_id: Option<TrackId>,
) -> Option<UniverseAction> {
    if upcoming.is_empty() {
        return None;
    }

    ui.add_space(spacing::GAP_14);
    ui.horizontal(|ui| {
        section_heading(ui, "UPCOMING ORBITS");
        ui.label(
            RichText::new(format!("{} tracks", upcoming.len()))
                .size(typography::CAPTION)
                .color(MUTED),
        );
    });
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        for (index, track_id) in upcoming.iter().take(MAX_VISIBLE_ORBITS).enumerate() {
            let Some(track) = find_track(tracks, *track_id) else {
                continue;
            };
            if ui
                .selectable_label(
                    selected_track_id == Some(*track_id),
                    format!("{:02}  {}", index + 1, track.title),
                )
                .clicked()
            {
                action = Some(UniverseAction::SelectTrack(*track_id));
            }
        }
    });
    action
}

fn find_track(tracks: &[TrackSummary], id: TrackId) -> Option<&TrackSummary> {
    tracks.iter().find(|track| track.id == id)
}

fn upcoming_tracks(playback: &PlaybackView) -> &[TrackId] {
    let first_upcoming = playback
        .current_queue_index
        .map_or(0, |index| index.saturating_add(1));
    playback
        .queue_track_ids
        .get(first_upcoming..)
        .unwrap_or_default()
}

fn visible_track_index(track_count: usize, limit: usize, slot: usize) -> Option<usize> {
    let visible_count = track_count.min(limit);
    if slot >= visible_count {
        return None;
    }
    if track_count <= limit {
        return Some(slot);
    }
    if limit == 1 {
        return Some(track_count / 2);
    }
    Some(slot * (track_count - 1) / (limit - 1))
}

fn readable_accent() -> Color32 {
    Color32::from_rgb(
        (ACCENT.r() as u16 * 2 / 3 + TEXT.r() as u16 / 3) as u8,
        (ACCENT.g() as u16 * 2 / 3 + TEXT.g() as u16 / 3) as u8,
        (ACCENT.b() as u16 * 2 / 3 + TEXT.b() as u16 / 3) as u8,
    )
}

fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(typography::OVERLINE)
            .strong()
            .color(MUTED),
    );
}

#[cfg(test)]
mod tests {
    use super::{MAX_LIBRARY_STARS, visible_track_index};

    #[test]
    fn visible_stars_are_deterministic_evenly_spaced_and_bounded() {
        assert_eq!(visible_track_index(0, MAX_LIBRARY_STARS, 0), None);
        assert_eq!(visible_track_index(4, MAX_LIBRARY_STARS, 3), Some(3));
        assert_eq!(visible_track_index(4, MAX_LIBRARY_STARS, 4), None);

        let sampled = (0..MAX_LIBRARY_STARS)
            .map(|slot| visible_track_index(10_000, MAX_LIBRARY_STARS, slot).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(sampled.first(), Some(&0));
        assert_eq!(sampled.last(), Some(&9_999));
        assert!(sampled.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            sampled,
            (0..MAX_LIBRARY_STARS)
                .map(|slot| visible_track_index(10_000, MAX_LIBRARY_STARS, slot).unwrap())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_single_visible_star_uses_a_middle_library_track() {
        assert_eq!(visible_track_index(10, 1, 0), Some(5));
        assert_eq!(visible_track_index(10, 1, 1), None);
    }
}
