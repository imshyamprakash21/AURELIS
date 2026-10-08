use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use aurelis_app::{
    ArtworkImageFormat, ArtworkView, DesktopApplication, DesktopPlaybackState, LibraryScanSummary,
    PlaybackView, TrackId, TrackSummary,
};
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, FontId, Frame, Layout, RichText, Sense, Stroke, Vec2,
};

const CANVAS: Color32 = Color32::from_rgb(12, 14, 20);
const SIDEBAR: Color32 = Color32::from_rgb(16, 18, 26);
const SURFACE: Color32 = Color32::from_rgb(22, 25, 34);
const SURFACE_HOVER: Color32 = Color32::from_rgb(31, 35, 47);
const EDGE: Color32 = Color32::from_rgb(44, 49, 64);
const TEXT: Color32 = Color32::from_rgb(236, 239, 246);
const MUTED: Color32 = Color32::from_rgb(147, 155, 174);
const ACCENT: Color32 = Color32::from_rgb(151, 164, 255);
const ACCENT_DARK: Color32 = Color32::from_rgb(59, 69, 121);
const ERROR: Color32 = Color32::from_rgb(245, 136, 139);
const SUCCESS: Color32 = Color32::from_rgb(126, 210, 174);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Library,
    NowPlaying,
}

#[derive(Clone, Copy)]
enum MessageTone {
    Info,
    Success,
    Error,
}

struct BannerMessage {
    text: String,
    tone: MessageTone,
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
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("A").size(30.0).strong().color(ACCENT));
            ui.vertical(|ui| {
                ui.label(RichText::new("AURELIS").size(18.0).strong().color(TEXT));
                ui.label(RichText::new("LOCAL AUDIO").size(10.0).color(MUTED));
            });
        });

        ui.add_space(42.0);
        ui.label(RichText::new("YOUR SPACE").size(10.0).strong().color(MUTED));
        ui.add_space(10.0);
        if navigation_button(ui, "01", "Library", self.page == Page::Library) {
            self.page = Page::Library;
        }
        ui.add_space(6.0);
        if navigation_button(ui, "02", "Now Playing", self.page == Page::NowPlaying) {
            self.page = Page::NowPlaying;
        }

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new(format!("{} TRACKS", self.application.tracks().len()))
                    .size(10.0)
                    .strong()
                    .color(MUTED),
            );
            ui.label(RichText::new("AURELIS CORE").size(11.0).color(ACCENT));
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
            ui.add_space(5.0);
            if let Some(texture) = &self.artwork_texture {
                ui.add(
                    egui::Image::new((texture.id(), Vec2::splat(54.0)))
                        .fit_to_exact_size(Vec2::splat(54.0)),
                );
            } else {
                artwork_fallback(ui, Vec2::splat(54.0));
            }

            ui.add_space(14.0);
            ui.vertical(|ui| {
                if let Some(track) = &current_track {
                    ui.label(RichText::new(&track.title).size(14.0).strong().color(TEXT));
                    ui.label(RichText::new(&track.artist).size(12.0).color(MUTED));
                } else {
                    ui.label(
                        RichText::new("Nothing playing")
                            .size(14.0)
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new("Choose a track from your library")
                            .size(12.0)
                            .color(MUTED),
                    );
                }
            });

            ui.add_space(28.0);
            if ui
                .add_enabled(
                    playback.current_track_id.is_some(),
                    egui::Button::new(RichText::new("‹").size(26.0).color(TEXT))
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
                    egui::Button::new(RichText::new(play_label).size(20.0).color(TEXT))
                        .fill(ACCENT_DARK)
                        .stroke(Stroke::NONE)
                        .min_size(Vec2::splat(42.0)),
                )
                .clicked()
            {
                action = Some(TransportAction::Toggle);
            }
            if ui
                .add_enabled(
                    playback.current_track_id.is_some(),
                    egui::Button::new(RichText::new("■").size(15.0).color(TEXT))
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
                    egui::Button::new(RichText::new("›").size(26.0).color(TEXT))
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
                    .size(11.0)
                    .color(MUTED),
                );
                ui.add(
                    egui::ProgressBar::new(progress)
                        .desired_width(180.0)
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
        ui.add_space(34.0);
        ui.label(
            RichText::new("YOUR COLLECTION")
                .size(11.0)
                .strong()
                .color(ACCENT),
        );
        ui.add_space(8.0);
        ui.heading(RichText::new("Library").size(34.0).strong().color(TEXT));
        ui.add_space(8.0);

        let root_label = self.application.library_root().map_or(
            "A local library, indexed on this device.".to_owned(),
            |root| root.display().to_string(),
        );
        ui.label(RichText::new(root_label).size(13.0).color(MUTED));
        ui.add_space(22.0);

        Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, EDGE))
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.library_path)
                            .desired_width(ui.available_width() - 252.0)
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
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Scanning files and reading metadata…").color(MUTED));
            });
        }

        if let Some(message) = &self.message {
            ui.add_space(12.0);
            draw_message(ui, message);
        }

        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} tracks", self.application.tracks().len()))
                    .size(13.0)
                    .strong()
                    .color(TEXT),
            );
            ui.add_space(8.0);
            ui.label(RichText::new("TITLE").size(10.0).strong().color(MUTED));
            ui.label(RichText::new("·  ARTIST  ·  ALBUM").size(10.0).color(MUTED));
        });
        ui.add_space(8.0);

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
        ui.add_space(34.0);
        ui.label(
            RichText::new("AURELIS CORE")
                .size(11.0)
                .strong()
                .color(ACCENT),
        );
        ui.add_space(8.0);
        ui.heading(RichText::new("Now Playing").size(34.0).strong().color(TEXT));
        ui.add_space(22.0);

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
                        egui::Image::new((texture.id(), Vec2::splat(320.0)))
                            .fit_to_exact_size(Vec2::splat(320.0)),
                    );
                } else {
                    artwork_fallback(ui, Vec2::splat(320.0));
                }
                ui.add_space(10.0);
                ui.label(RichText::new(&self.artwork_caption).size(11.0).color(MUTED));
            });

            ui.add_space(32.0);
            ui.vertical(|ui| {
                ui.add_space(16.0);
                ui.label(RichText::new(&track.title).size(28.0).strong().color(TEXT));
                ui.add_space(5.0);
                ui.label(RichText::new(&track.artist).size(17.0).color(ACCENT));
                ui.add_space(4.0);
                ui.label(RichText::new(&track.album).size(14.0).color(MUTED));
                ui.add_space(25.0);

                ui.label(RichText::new("PLAYBACK").size(10.0).strong().color(MUTED));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(playback_state_label(playback.state))
                        .size(14.0)
                        .color(TEXT),
                );
                draw_seek_control(
                    ui,
                    &mut self.application,
                    &playback,
                    &mut self.seek_preview_seconds,
                    &mut self.message,
                );
                ui.add_space(22.0);

                ui.label(RichText::new("AUDIO FILE").size(10.0).strong().color(MUTED));
                ui.add_space(8.0);
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
            ui.add_space(26.0);
            ui.label(RichText::new("QUEUE").size(10.0).strong().color(MUTED));
            ui.add_space(7.0);
            let start = playback.current_queue_index.unwrap_or(0);
            for (index, queued_id) in playback.queue_track_ids.iter().enumerate().skip(start) {
                if let Some(queued_track) = self.application.track(*queued_id) {
                    let current = index == playback.current_queue_index.unwrap_or(usize::MAX);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{:02}", index + 1))
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.label(
                            RichText::new(&queued_track.title)
                                .size(13.0)
                                .color(if current { ACCENT } else { TEXT }),
                        );
                        ui.label(RichText::new(&queued_track.artist).size(12.0).color(MUTED));
                        if current {
                            ui.label(RichText::new("NOW").size(9.0).strong().color(ACCENT));
                        }
                    });
                    ui.add_space(4.0);
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
            .exact_size(224.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(SIDEBAR)
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ui, |ui| self.draw_navigation(ui));

        egui::Panel::bottom("transport")
            .exact_size(92.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(SIDEBAR)
                    .stroke(Stroke::new(1.0, EDGE))
                    .inner_margin(egui::Margin::symmetric(24, 14)),
            )
            .show(ui, |ui| self.draw_transport(ui));

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(CANVAS)
                    .inner_margin(egui::Margin::symmetric(32, 0)),
            )
            .show(ui, |ui| match self.page {
                Page::Library => self.draw_library(ui),
                Page::NowPlaying => self.draw_now_playing(ui),
            });

        context.request_repaint_after(Duration::from_millis(40));
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
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.window_fill = SURFACE;
        style.visuals.panel_fill = CANVAS;
        style.visuals.widgets.noninteractive.bg_fill = SURFACE;
        style.visuals.widgets.inactive.bg_fill = SURFACE;
        style.visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
        style.visuals.widgets.active.bg_fill = ACCENT_DARK;
        style.visuals.selection.bg_fill = ACCENT_DARK;
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        style.spacing.item_spacing = Vec2::new(12.0, 10.0);
    });
}
fn navigation_button(ui: &mut egui::Ui, number: &str, label: &str, selected: bool) -> bool {
    let color = if selected { ACCENT } else { MUTED };
    let fill = if selected { ACCENT_DARK } else { SIDEBAR };
    ui.add_sized(
        [ui.available_width(), 42.0],
        egui::Button::new(
            RichText::new(format!("{number}    {label}"))
                .size(13.0)
                .color(if selected { TEXT } else { color }),
        )
        .fill(fill)
        .stroke(Stroke::NONE),
    )
    .clicked()
}

fn track_row(
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
            .stroke(Stroke::new(1.0, if selected { ACCENT_DARK } else { EDGE }))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:02}", index + 1))
                            .size(11.0)
                            .color(MUTED),
                    );

                    if ui
                        .add_sized(
                            [270.0, 38.0],
                            egui::Button::new(
                                RichText::new(&track.title).size(13.0).strong().color(TEXT),
                            )
                            .fill(fill)
                            .stroke(Stroke::NONE),
                        )
                        .clicked()
                    {
                        play = true;
                    }

                    ui.vertical(|ui| {
                        ui.label(RichText::new(&track.artist).size(12.0).color(MUTED));
                        if available_width > 900.0 {
                            ui.label(RichText::new(&track.album).size(10.0).color(MUTED));
                        }
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(track.duration.map_or("--:--".to_owned(), format_time))
                                .size(11.0)
                                .color(MUTED),
                        );
                        if ui
                            .add_enabled(
                                !selected,
                                egui::Button::new(
                                    RichText::new("PLAY").size(9.0).strong().color(ACCENT),
                                )
                                .fill(fill)
                                .stroke(Stroke::new(1.0, EDGE)),
                            )
                            .clicked()
                        {
                            play = true;
                        }
                    });
                });
            });
        ui.add_space(5.0);
    });
    play
}

fn empty_state(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.add_space(58.0);
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, EDGE))
        .inner_margin(egui::Margin::same(28))
        .show(ui, |ui| {
            ui.set_min_height(170.0);
            ui.vertical_centered(|ui| {
                ui.add_space(28.0);
                ui.label(RichText::new("◌").size(26.0).color(ACCENT));
                ui.add_space(8.0);
                ui.label(RichText::new(title).size(18.0).strong().color(TEXT));
                ui.add_space(7.0);
                ui.label(RichText::new(description).size(12.0).color(MUTED));
                ui.add_space(28.0);
            });
        });
}

fn artwork_fallback(ui: &mut egui::Ui, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, SURFACE);
    painter.rect_stroke(rect, 8.0, Stroke::new(1.0, EDGE), egui::StrokeKind::Inside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "A",
        FontId::proportional(size.x.min(size.y) * 0.22),
        ACCENT_DARK,
    );
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
                .size(11.0)
                .color(MUTED),
        );
        return;
    };

    ui.add_space(5.0);
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
                .size(10.0)
                .color(MUTED),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format_time(duration)).size(10.0).color(MUTED));
        });
    });
}

fn metadata_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(11.0).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).size(11.0).color(TEXT));
        });
    });
}

fn draw_message(ui: &mut egui::Ui, message: &BannerMessage) {
    let (color, dot) = match message.tone {
        MessageTone::Info => (MUTED, "•"),
        MessageTone::Success => (SUCCESS, "•"),
        MessageTone::Error => (ERROR, "•"),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(dot).color(color));
        ui.label(RichText::new(&message.text).size(12.0).color(color));
    });
}

fn format_time(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60)
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
