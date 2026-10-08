use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use crate::components::{
    BannerMessage, MessageTone, artwork_fallback, draw_message, empty_state, format_time,
    metadata_row, navigation_button, section_heading, track_row,
};
use crate::theme::{layout, motion, palette::*, radius, spacing, typography};
use aurelis_app::{
    ArtworkImageFormat, ArtworkView, DesktopApplication, DesktopPlaybackState, LibraryScanSummary,
    PlaybackView, TrackId,
};
use eframe::egui::{self, Align, ColorImage, Frame, Layout, RichText, Stroke, Vec2};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Library,
    NowPlaying,
}

struct DecodedArtwork {
    image: ColorImage,
    caption: String,
}

pub(crate) struct AurelisDesktop {
    application: DesktopApplication,
    page: Page,
    library_path: String,
    message: Option<BannerMessage>,
    artwork_track: Option<TrackId>,
    artwork_request_for: Option<TrackId>,
    artwork_texture: Option<egui::TextureHandle>,
    artwork_caption: String,
    artwork_decode_rx: Option<Receiver<(TrackId, Result<DecodedArtwork, String>)>>,
    seek_preview_seconds: Option<f64>,
}

impl AurelisDesktop {
    pub(crate) fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        Self {
            application: DesktopApplication::new(),
            page: Page::Library,
            library_path: String::new(),
            message: Some(BannerMessage {
                text: "Choose a local music folder to build your library.".to_owned(),
                tone: MessageTone::Info,
            }),
            artwork_track: None,
            artwork_request_for: None,
            artwork_texture: None,
            artwork_caption: "Artwork appears here when the selected track has a supported image."
                .to_owned(),
            artwork_decode_rx: None,
            seek_preview_seconds: None,
        }
    }

    fn show_message(&mut self, text: impl Into<String>, tone: MessageTone) {
        self.message = Some(BannerMessage {
            text: text.into(),
            tone,
        });
    }

    fn begin_scan(&mut self) {
        let path = self.library_path.trim();
        if path.is_empty() {
            self.show_message(
                "Enter a folder path or choose a music folder.",
                MessageTone::Error,
            );
            return;
        }

        match self.application.begin_scan(path) {
            Ok(()) => self.show_message(
                "Scanning the selected folder in the background…",
                MessageTone::Info,
            ),
            Err(error) => self.show_message(error, MessageTone::Error),
        }
    }

    fn choose_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Choose your music library")
            .pick_folder()
        {
            self.library_path = path.display().to_string();
            self.begin_scan();
        }
    }

    fn accept_scan(&mut self, summary: LibraryScanSummary) {
        let message = format!(
            "{} supported files found · {} added · {} refreshed · {} unreadable · {} scan issues",
            summary.discovered, summary.added, summary.updated, summary.failed, summary.issues,
        );
        let tone = if summary.issues > 0 && summary.added + summary.updated == 0 {
            MessageTone::Error
        } else if summary.issues > 0 || summary.discovered == 0 {
            MessageTone::Info
        } else {
            MessageTone::Success
        };
        self.show_message(message, tone);
    }

    fn poll_application(&mut self, context: &egui::Context) {
        if let Some(error) = self.application.poll_playback() {
            self.show_message(format!("Playback stopped: {error}"), MessageTone::Error);
        }

        if let Some(result) = self.application.poll_library_scan() {
            match result {
                Ok(summary) => self.accept_scan(summary),
                Err(error) => self.show_message(error, MessageTone::Error),
            }
        }

        self.sync_artwork(context);
    }
    fn sync_artwork(&mut self, context: &egui::Context) {
        let current_track = self.application.playback().current_track_id;
        if current_track != self.artwork_track {
            self.artwork_track = current_track;
            self.artwork_request_for = None;
            self.artwork_texture = None;
            self.artwork_decode_rx = None;
            self.artwork_caption = if current_track.is_some() {
                "Loading artwork…".to_owned()
            } else {
                "Select a track to view its artwork.".to_owned()
            };
            self.seek_preview_seconds = None;
        }

        if let Some((track_id, result)) = self.application.poll_artwork()
            && Some(track_id) == self.artwork_track
        {
            match result {
                Ok(Some(artwork)) => self.decode_artwork(context, track_id, artwork),
                Ok(None) => {
                    self.artwork_caption =
                        "No supported embedded or folder artwork was found.".to_owned();
                }
                Err(error) => {
                    self.artwork_caption = format!("Artwork could not be loaded: {error}");
                }
            }
        }

        if let Some(track_id) = current_track
            && self.artwork_request_for != Some(track_id)
            && self.application.artwork_loading_track().is_none()
        {
            match self.application.request_artwork(track_id) {
                Ok(()) => {
                    self.artwork_request_for = Some(track_id);
                    self.artwork_caption = "Loading artwork…".to_owned();
                }
                Err(error) => {
                    self.artwork_request_for = Some(track_id);
                    self.artwork_caption = error;
                }
            }
        }

        self.poll_artwork_decode(context);
    }

    fn decode_artwork(&mut self, context: &egui::Context, track_id: TrackId, artwork: ArtworkView) {
        let (result_tx, result_rx) = mpsc::channel();
        let caption = format!(
            "{} · {}",
            artwork_format_name(artwork.format),
            artwork.source
        );

        match thread::Builder::new()
            .name("aurelis-artwork-decode".to_owned())
            .spawn(move || {
                let result = image::load_from_memory(&artwork.data)
                    .map(|decoded| {
                        let rgba = decoded.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                        DecodedArtwork { image, caption }
                    })
                    .map_err(|error| error.to_string());
                let _ = result_tx.send((track_id, result));
            }) {
            Ok(_) => {
                self.artwork_decode_rx = Some(result_rx);
                self.artwork_caption = "Preparing artwork…".to_owned();
                context.request_repaint();
            }
            Err(error) => {
                self.artwork_caption = format!("Could not start artwork decoding: {error}");
            }
        }
    }

    fn poll_artwork_decode(&mut self, context: &egui::Context) {
        let Some(receiver) = self.artwork_decode_rx.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok((track_id, result)) => {
                if Some(track_id) != self.artwork_track {
                    return;
                }
                match result {
                    Ok(decoded) => {
                        self.artwork_texture = Some(context.load_texture(
                            format!("aurelis-artwork-{track_id:?}"),
                            decoded.image,
                            egui::TextureOptions::LINEAR,
                        ));
                        self.artwork_caption = decoded.caption;
                    }
                    Err(error) => {
                        self.artwork_caption = format!("Artwork could not be rendered: {error}");
                    }
                }
            }
            Err(TryRecvError::Empty) => self.artwork_decode_rx = Some(receiver),
            Err(TryRecvError::Disconnected) => {
                self.artwork_caption = "Artwork rendering ended unexpectedly.".to_owned();
            }
        }
    }

    fn handle_keyboard(&mut self, context: &egui::Context) {
        if context.egui_wants_keyboard_input() {
            return;
        }

        let (toggle, seek_back, seek_forward) = context.input(|input| {
            (
                input.key_pressed(egui::Key::Space),
                input.key_pressed(egui::Key::ArrowLeft),
                input.key_pressed(egui::Key::ArrowRight),
            )
        });

        if toggle && let Err(error) = self.application.toggle_playback() {
            self.show_message(error, MessageTone::Error);
        }
        if seek_back || seek_forward {
            let playback = self.application.playback();
            if playback.current_track_id.is_some() {
                let target = if seek_back {
                    playback.position.saturating_sub(Duration::from_secs(5))
                } else {
                    playback.position.saturating_add(Duration::from_secs(5))
                };
                if let Err(error) = self.application.seek_to(target) {
                    self.show_message(error, MessageTone::Error);
                }
            }
        }
    }

    fn draw_navigation(&mut self, ui: &mut egui::Ui) {
        ui.add_space(spacing::GAP_8);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("A")
                    .size(typography::BRAND_MARK)
                    .strong()
                    .color(ACCENT),
            );
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("AURELIS")
                        .size(typography::TITLE)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    RichText::new("LOCAL AUDIO")
                        .size(typography::OVERLINE)
                        .color(MUTED),
                );
            });
        });

        ui.add_space(spacing::GAP_42);
        ui.label(
            RichText::new("YOUR SPACE")
                .size(typography::OVERLINE)
                .strong()
                .color(MUTED),
        );
        ui.add_space(spacing::GAP_10);
        if navigation_button(ui, "01", "Library", self.page == Page::Library) {
            self.page = Page::Library;
        }
        ui.add_space(spacing::GAP_6);
        if navigation_button(ui, "02", "Now Playing", self.page == Page::NowPlaying) {
            self.page = Page::NowPlaying;
        }

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new(format!("{} TRACKS", self.application.tracks().len()))
                    .size(typography::OVERLINE)
                    .strong()
                    .color(MUTED),
            );
            ui.label(
                RichText::new("AURELIS CORE")
                    .size(typography::CAPTION)
                    .color(ACCENT),
            );
        });
    }
    fn draw_transport(&mut self, ui: &mut egui::Ui) {
        let playback = self.application.playback();
        let current_track = playback
            .current_track_id
            .and_then(|id| self.application.track(id))
            .cloned();
        let mut action = None;

        ui.horizontal(|ui| {
            ui.add_space(spacing::GAP_5);
            if let Some(texture) = &self.artwork_texture {
                ui.add(
                    egui::Image::new((texture.id(), Vec2::splat(layout::TRANSPORT_ARTWORK_SIZE)))
                        .fit_to_exact_size(Vec2::splat(layout::TRANSPORT_ARTWORK_SIZE)),
                );
            } else {
                artwork_fallback(ui, Vec2::splat(layout::TRANSPORT_ARTWORK_SIZE));
            }

            ui.add_space(spacing::GAP_14);
            ui.vertical(|ui| {
                if let Some(track) = &current_track {
                    ui.label(
                        RichText::new(&track.title)
                            .size(typography::BODY_LARGE)
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new(&track.artist)
                            .size(typography::BODY_SMALL)
                            .color(MUTED),
                    );
                } else {
                    ui.label(
                        RichText::new("Nothing playing")
                            .size(typography::BODY_LARGE)
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new("Choose a track from your library")
                            .size(typography::BODY_SMALL)
                            .color(MUTED),
                    );
                }
            });

            ui.add_space(spacing::GAP_28);
            if ui
                .add_enabled(
                    playback.current_track_id.is_some(),
                    egui::Button::new(
                        RichText::new("‹")
                            .size(typography::NAVIGATION_ICON)
                            .color(TEXT),
                    )
                    .fill(SIDEBAR)
                    .stroke(Stroke::NONE),
                )
                .clicked()
            {
                action = Some(TransportAction::Previous);
            }
            let play_label = match playback.state {
                DesktopPlaybackState::Playing => "Ⅱ",
                _ => "▶",
            };
            if ui
                .add_enabled(
                    playback.current_track_id.is_some(),
                    egui::Button::new(
                        RichText::new(play_label)
                            .size(typography::CONTROL_ICON)
                            .color(TEXT),
                    )
                    .fill(ACCENT_DARK)
                    .stroke(Stroke::NONE)
                    .min_size(Vec2::splat(layout::PRIMARY_BUTTON_SIZE)),
                )
                .clicked()
            {
                action = Some(TransportAction::Toggle);
            }
            if ui
                .add_enabled(
                    playback.current_track_id.is_some(),
                    egui::Button::new(
                        RichText::new("■")
                            .size(typography::CONTROL_ICON_SMALL)
                            .color(TEXT),
                    )
                    .fill(SIDEBAR)
                    .stroke(Stroke::NONE),
                )
                .clicked()
            {
                action = Some(TransportAction::Stop);
            }
            if ui
                .add_enabled(
                    playback.current_track_id.is_some(),
                    egui::Button::new(
                        RichText::new("›")
                            .size(typography::NAVIGATION_ICON)
                            .color(TEXT),
                    )
                    .fill(SIDEBAR)
                    .stroke(Stroke::NONE),
                )
                .clicked()
            {
                action = Some(TransportAction::Next);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let progress = playback
                    .duration
                    .filter(|duration| !duration.is_zero())
                    .map_or(0.0, |duration| {
                        (playback.position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
                    });
                ui.label(
                    RichText::new(format!(
                        "{} / {}",
                        format_time(playback.position),
                        playback.duration.map_or("--:--".to_owned(), format_time)
                    ))
                    .size(typography::CAPTION)
                    .color(MUTED),
                );
                ui.add(
                    egui::ProgressBar::new(progress)
                        .desired_width(layout::PROGRESS_WIDTH)
                        .fill(ACCENT),
                );
            });
        });

        if let Some(action) = action {
            self.perform_transport(action);
        }
    }
    fn perform_transport(&mut self, action: TransportAction) {
        match action {
            TransportAction::Previous => match self.application.play_previous() {
                Ok(Some(_)) => {}
                Ok(None) => self.show_message(
                    "You are at the first track in the queue.",
                    MessageTone::Info,
                ),
                Err(error) => self.show_message(error, MessageTone::Error),
            },
            TransportAction::Toggle => {
                if let Err(error) = self.application.toggle_playback() {
                    self.show_message(error, MessageTone::Error);
                }
            }
            TransportAction::Stop => self.application.stop(),
            TransportAction::Next => match self.application.play_next() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.show_message("There is no next track in the queue.", MessageTone::Info)
                }
                Err(error) => self.show_message(error, MessageTone::Error),
            },
        }
    }

    fn draw_library(&mut self, ui: &mut egui::Ui) {
        ui.add_space(spacing::GAP_34);
        ui.label(
            RichText::new("YOUR COLLECTION")
                .size(typography::CAPTION)
                .strong()
                .color(ACCENT),
        );
        ui.add_space(spacing::GAP_8);
        ui.heading(
            RichText::new("Library")
                .size(typography::PAGE_TITLE)
                .strong()
                .color(TEXT),
        );
        ui.add_space(spacing::GAP_8);

        let root_label = self.application.library_root().map_or(
            "A local library, indexed on this device.".to_owned(),
            |root| root.display().to_string(),
        );
        ui.label(
            RichText::new(root_label)
                .size(typography::BODY)
                .color(MUTED),
        );
        ui.add_space(spacing::GAP_22);

        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
            .inner_margin(egui::Margin::same(layout::CARD_INSET))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.library_path)
                            .desired_width(ui.available_width() - layout::LIBRARY_ACTIONS_WIDTH)
                            .hint_text("Enter a folder containing your music"),
                    );
                    if ui
                        .add_enabled(
                            !self.application.is_scanning(),
                            egui::Button::new("Choose folder"),
                        )
                        .clicked()
                    {
                        self.choose_folder();
                    }
                    if ui
                        .add_enabled(
                            !self.application.is_scanning(),
                            egui::Button::new("Scan library"),
                        )
                        .clicked()
                    {
                        self.begin_scan();
                    }
                });
            });

        if self.application.is_scanning() {
            ui.add_space(spacing::GAP_18);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Scanning files and reading metadata…").color(MUTED));
            });
        }

        if let Some(message) = &self.message {
            ui.add_space(spacing::GAP_12);
            draw_message(ui, message);
        }

        ui.add_space(spacing::GAP_18);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} tracks", self.application.tracks().len()))
                    .size(typography::BODY)
                    .strong()
                    .color(TEXT),
            );
            ui.add_space(spacing::GAP_8);
            ui.label(
                RichText::new("TITLE")
                    .size(typography::OVERLINE)
                    .strong()
                    .color(MUTED),
            );
            ui.label(
                RichText::new("·  ARTIST  ·  ALBUM")
                    .size(typography::OVERLINE)
                    .color(MUTED),
            );
        });
        ui.add_space(spacing::GAP_8);

        if self.application.tracks().is_empty() {
            empty_state(
                ui,
                "Your library is ready for music",
                "Choose a local folder above. AURELIS will read the files it supports and keep the scan off the audio and interface threads.",
            );
            return;
        }

        let current_track = self.application.playback().current_track_id;
        let mut selected_track = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (index, track) in self.application.tracks().iter().enumerate() {
                    if track_row(ui, index, track, current_track) {
                        selected_track = Some(track.id);
                    }
                }
            });
        if let Some(track_id) = selected_track {
            match self.application.play_track(track_id) {
                Ok(()) => {
                    self.page = Page::NowPlaying;
                    if let Some(track) = self.application.track(track_id) {
                        self.show_message(format!("Playing {}", track.title), MessageTone::Success);
                    }
                }
                Err(error) => self.show_message(error, MessageTone::Error),
            }
        }
    }
    fn draw_now_playing(&mut self, ui: &mut egui::Ui) {
        ui.add_space(spacing::GAP_34);
        ui.label(
            RichText::new("AURELIS CORE")
                .size(typography::CAPTION)
                .strong()
                .color(ACCENT),
        );
        ui.add_space(spacing::GAP_8);
        ui.heading(
            RichText::new("Now Playing")
                .size(typography::PAGE_TITLE)
                .strong()
                .color(TEXT),
        );
        ui.add_space(spacing::GAP_22);

        if let Some(message) = &self.message
            && matches!(message.tone, MessageTone::Error)
        {
            draw_message(ui, message);
            ui.add_space(spacing::GAP_12);
        }

        let playback = self.application.playback();
        let Some(track_id) = playback.current_track_id else {
            empty_state(
                ui,
                "The Core is waiting",
                "Select a track from your library to see its artwork, metadata, and playback state here.",
            );
            return;
        };
        let Some(track) = self.application.track(track_id).cloned() else {
            empty_state(
                ui,
                "Track details unavailable",
                "Rescan the selected library to refresh its track index.",
            );
            return;
        };

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                if let Some(texture) = &self.artwork_texture {
                    ui.add(
                        egui::Image::new((
                            texture.id(),
                            Vec2::splat(layout::NOW_PLAYING_ARTWORK_SIZE),
                        ))
                        .fit_to_exact_size(Vec2::splat(layout::NOW_PLAYING_ARTWORK_SIZE)),
                    );
                } else {
                    artwork_fallback(ui, Vec2::splat(layout::NOW_PLAYING_ARTWORK_SIZE));
                }
                ui.add_space(spacing::GAP_10);
                ui.label(
                    RichText::new(&self.artwork_caption)
                        .size(typography::CAPTION)
                        .color(MUTED),
                );
            });

            ui.add_space(spacing::GAP_32);
            ui.vertical(|ui| {
                ui.add_space(spacing::GAP_16);
                ui.label(
                    RichText::new(&track.title)
                        .size(typography::TRACK_TITLE)
                        .strong()
                        .color(TEXT),
                );
                ui.add_space(spacing::GAP_5);
                ui.label(
                    RichText::new(&track.artist)
                        .size(typography::ARTIST_TITLE)
                        .color(ACCENT),
                );
                ui.add_space(spacing::GAP_4);
                ui.label(
                    RichText::new(&track.album)
                        .size(typography::BODY_LARGE)
                        .color(MUTED),
                );
                ui.add_space(spacing::GAP_25);

                section_heading(ui, "PLAYBACK");
                ui.add_space(spacing::GAP_6);
                ui.label(
                    RichText::new(playback_state_label(playback.state))
                        .size(typography::BODY_LARGE)
                        .color(TEXT),
                );
                draw_seek_control(
                    ui,
                    &mut self.application,
                    &playback,
                    &mut self.seek_preview_seconds,
                    &mut self.message,
                );
                ui.add_space(spacing::GAP_22);

                section_heading(ui, "AUDIO FILE");
                ui.add_space(spacing::GAP_8);
                metadata_row(
                    ui,
                    "Format",
                    if track.codec.is_empty() {
                        "Unknown".to_owned()
                    } else {
                        track.codec.clone()
                    },
                );
                metadata_row(
                    ui,
                    "Sample rate",
                    track
                        .sample_rate
                        .map_or("Unknown".to_owned(), |rate| format!("{rate} Hz")),
                );
                metadata_row(
                    ui,
                    "Bit depth",
                    track
                        .bit_depth
                        .map_or("Unknown".to_owned(), |bits| format!("{bits}-bit")),
                );
                metadata_row(
                    ui,
                    "Channels",
                    track
                        .channels
                        .map_or("Unknown".to_owned(), |channels| channels.to_string()),
                );
                metadata_row(
                    ui,
                    "Coded bits/sample",
                    track
                        .bits_per_coded_sample
                        .map_or("Unknown".to_owned(), |bits| bits.to_string()),
                );
                metadata_row(
                    ui,
                    "Duration",
                    track.duration.map_or("--:--".to_owned(), format_time),
                );
            });
        });

        if !playback.queue_track_ids.is_empty() {
            ui.add_space(spacing::GAP_26);
            section_heading(ui, "QUEUE");
            ui.add_space(spacing::GAP_7);
            let start = playback.current_queue_index.unwrap_or(0);
            for (index, queued_id) in playback.queue_track_ids.iter().enumerate().skip(start) {
                if let Some(queued_track) = self.application.track(*queued_id) {
                    let current = index == playback.current_queue_index.unwrap_or(usize::MAX);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{:02}", index + 1))
                                .size(typography::CAPTION)
                                .color(MUTED),
                        );
                        ui.label(
                            RichText::new(&queued_track.title)
                                .size(typography::BODY)
                                .color(if current { ACCENT } else { TEXT }),
                        );
                        ui.label(
                            RichText::new(&queued_track.artist)
                                .size(typography::BODY_SMALL)
                                .color(MUTED),
                        );
                        if current {
                            ui.label(
                                RichText::new("NOW")
                                    .size(typography::MICRO)
                                    .strong()
                                    .color(ACCENT),
                            );
                        }
                    });
                    ui.add_space(spacing::GAP_4);
                }
            }
        }
    }
}

impl eframe::App for AurelisDesktop {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.poll_application(&context);
        self.handle_keyboard(&context);

        egui::Panel::left("navigation")
            .exact_size(layout::NAVIGATION_PANEL_WIDTH)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(SIDEBAR)
                    .inner_margin(egui::Margin::same(layout::NAVIGATION_INSET)),
            )
            .show(ui, |ui| self.draw_navigation(ui));

        egui::Panel::bottom("transport")
            .exact_size(layout::TRANSPORT_PANEL_HEIGHT)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(SIDEBAR)
                    .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
                    .inner_margin(egui::Margin::symmetric(
                        layout::TRANSPORT_INSET_X,
                        layout::TRANSPORT_INSET_Y,
                    )),
            )
            .show(ui, |ui| self.draw_transport(ui));

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(CANVAS)
                    .inner_margin(egui::Margin::symmetric(
                        layout::CONTENT_INSET_X,
                        layout::CONTENT_INSET_Y,
                    )),
            )
            .show(ui, |ui| match self.page {
                Page::Library => self.draw_library(ui),
                Page::NowPlaying => self.draw_now_playing(ui),
            });

        context.request_repaint_after(Duration::from_millis(motion::FRAME_REFRESH_MS));
    }
}
#[derive(Clone, Copy)]
enum TransportAction {
    Previous,
    Toggle,
    Stop,
    Next,
}
fn configure_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    context.global_style_mut(|style| {
        style.animation_time = motion::ANIMATION_SECONDS;
        style.visuals.window_corner_radius = radius::WINDOW;
        style.visuals.menu_corner_radius = radius::MENU;
        style.visuals.widgets.noninteractive.corner_radius = radius::WIDGET;
        style.visuals.widgets.inactive.corner_radius = radius::WIDGET;
        style.visuals.widgets.hovered.corner_radius = radius::WIDGET_HOVERED;
        style.visuals.widgets.active.corner_radius = radius::WIDGET;
        style.visuals.widgets.open.corner_radius = radius::WIDGET;

        style.visuals.override_text_color = Some(TEXT);
        style.visuals.window_fill = SURFACE;
        style.visuals.panel_fill = CANVAS;
        style.visuals.widgets.noninteractive.bg_fill = SURFACE;
        style.visuals.widgets.inactive.bg_fill = SURFACE;
        style.visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
        style.visuals.widgets.active.bg_fill = ACCENT_DARK;
        style.visuals.selection.bg_fill = ACCENT_DARK;
        style.visuals.selection.stroke = Stroke::new(layout::BORDER_WIDTH, ACCENT);
        style.spacing.item_spacing = Vec2::new(spacing::ITEM_HORIZONTAL, spacing::ITEM_VERTICAL);
    });
}

fn draw_seek_control(
    ui: &mut egui::Ui,
    application: &mut DesktopApplication,
    playback: &PlaybackView,
    seek_preview_seconds: &mut Option<f64>,
    message: &mut Option<BannerMessage>,
) {
    let Some(duration) = playback.duration.filter(|duration| !duration.is_zero()) else {
        ui.label(
            RichText::new("Position unavailable")
                .size(typography::CAPTION)
                .color(MUTED),
        );
        return;
    };

    ui.add_space(spacing::GAP_5);
    let duration_seconds = duration.as_secs_f64().max(1.0);
    let mut value = seek_preview_seconds
        .unwrap_or(playback.position.as_secs_f64())
        .min(duration_seconds);
    let response = ui.add(
        egui::Slider::new(&mut value, 0.0..=duration_seconds)
            .step_by(1.0)
            .show_value(false),
    );
    if response.dragged() {
        *seek_preview_seconds = Some(value);
    }
    if response.drag_stopped() || response.clicked() {
        *seek_preview_seconds = None;
        if let Err(error) = application.seek_to(Duration::from_secs_f64(value.max(0.0))) {
            *message = Some(BannerMessage {
                text: error,
                tone: MessageTone::Error,
            });
        }
    }
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format_time(Duration::from_secs_f64(value.max(0.0))))
                .size(typography::OVERLINE)
                .color(MUTED),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format_time(duration))
                    .size(typography::OVERLINE)
                    .color(MUTED),
            );
        });
    });
}

fn playback_state_label(state: DesktopPlaybackState) -> &'static str {
    match state {
        DesktopPlaybackState::Stopped => "Stopped",
        DesktopPlaybackState::Playing => "Playing",
        DesktopPlaybackState::Paused => "Paused",
        DesktopPlaybackState::Finished => "Finished",
    }
}

fn artwork_format_name(format: ArtworkImageFormat) -> &'static str {
    match format {
        ArtworkImageFormat::Jpeg => "JPEG",
        ArtworkImageFormat::Png => "PNG",
    }
}
