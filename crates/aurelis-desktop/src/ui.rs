use std::{
    borrow::Cow,
    collections::HashMap,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use crate::components::{
    BannerMessage, MessageTone, artwork_fallback, collection_row, draw_message, empty_state,
    format_time, library_statistic, metadata_row, navigation_button, section_heading, track_row,
};
use crate::theme::{layout, motion, palette::*, radius, spacing, typography};
use aurelis_app::{
    AlbumSummary, ArtistSummary, ArtworkImageFormat, ArtworkView, ArtworkVisual,
    DesktopApplication, DesktopPlaybackState, FolderSummary, GenreSummary, LibraryScanSummary,
    LibrarySearchResults, Lyrics, PlaybackView, TrackId, TrackSummary,
};
use eframe::egui::{self, Align, Color32, ColorImage, Frame, Layout, RichText, Stroke, Vec2};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Library,
    NowPlaying,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LibrarySection {
    Home,
    Tracks,
    Albums,
    Artists,
    Genres,
    Folders,
}

impl LibrarySection {
    const ALL: [Self; 6] = [
        Self::Home,
        Self::Tracks,
        Self::Albums,
        Self::Artists,
        Self::Genres,
        Self::Folders,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Tracks => "Tracks",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Genres => "Genres",
            Self::Folders => "Folders",
        }
    }
}

trait BrowseGroup {
    fn title(&self) -> Cow<'_, str>;
    fn subtitle(&self) -> Cow<'_, str>;
    fn track_ids(&self) -> &[TrackId];
}

impl BrowseGroup for AlbumSummary {
    fn title(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.title)
    }

    fn subtitle(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.album_artist)
    }

    fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }
}

impl BrowseGroup for ArtistSummary {
    fn title(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.name)
    }

    fn subtitle(&self) -> Cow<'_, str> {
        Cow::Borrowed("Artist")
    }

    fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }
}

impl BrowseGroup for GenreSummary {
    fn title(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.name)
    }

    fn subtitle(&self) -> Cow<'_, str> {
        Cow::Borrowed("Genre")
    }

    fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }
}

impl BrowseGroup for FolderSummary {
    fn title(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.name)
    }

    fn subtitle(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display_path)
    }

    fn track_ids(&self) -> &[TrackId] {
        &self.track_ids
    }
}

struct CollectionTarget {
    title: String,
    subtitle: String,
    track_ids: Vec<TrackId>,
}

enum SearchSelection {
    Track(TrackId),
    Collection(CollectionTarget),
}

struct SelectedCollection {
    title: String,
    subtitle: String,
    tracks: Vec<TrackSummary>,
}

struct DecodedArtwork {
    image: ColorImage,
    caption: String,
    visual: Option<ArtworkVisual>,
}

#[derive(Clone, Copy)]
struct ArtworkTint {
    primary: [u8; 3],
    accent: [u8; 3],
}

fn artwork_tint_from_visual(visual: ArtworkVisual) -> ArtworkTint {
    let dominant = visual.dominant_color();
    let accent = visual.accent_color();
    ArtworkTint {
        primary: [dominant.red(), dominant.green(), dominant.blue()],
        accent: [accent.red(), accent.green(), accent.blue()],
    }
}

fn neutral_artwork_tint() -> ArtworkTint {
    ArtworkTint {
        primary: [CANVAS.r(), CANVAS.g(), CANVAS.b()],
        accent: [ACCENT.r(), ACCENT.g(), ACCENT.b()],
    }
}

fn interpolate_artwork_tint(from: ArtworkTint, target: ArtworkTint, amount: f32) -> ArtworkTint {
    ArtworkTint {
        primary: mix_rgb(from.primary, target.primary, amount),
        accent: mix_rgb(from.accent, target.accent, amount),
    }
}

fn artwork_background_tint(palette: ArtworkTint) -> Color32 {
    let canvas = [CANVAS.r(), CANVAS.g(), CANVAS.b()];
    let color = mix_rgb(canvas, palette.primary, 0.16);
    Color32::from_rgb(color[0], color[1], color[2])
}

fn readable_artwork_accent(palette: ArtworkTint) -> Color32 {
    let text = [TEXT.r(), TEXT.g(), TEXT.b()];
    let color = mix_rgb(palette.accent, text, 0.34);
    Color32::from_rgb(color[0], color[1], color[2])
}

fn mix_rgb(base: [u8; 3], tint: [u8; 3], amount: f32) -> [u8; 3] {
    std::array::from_fn(|index| {
        (base[index] as f32 * (1.0 - amount) + tint[index] as f32 * amount).round() as u8
    })
}

pub(crate) struct AurelisDesktop {
    application: DesktopApplication,
    page: Page,
    library_path: String,
    library_section: LibrarySection,
    library_search: String,
    search_results: LibrarySearchResults,
    search_results_stale: bool,
    selected_collection: Option<SelectedCollection>,
    message: Option<BannerMessage>,
    artwork_track: Option<TrackId>,
    artwork_request_for: Option<TrackId>,
    artwork_texture: Option<egui::TextureHandle>,
    artwork_visual_cache: HashMap<TrackId, ArtworkVisual>,
    artwork_tint: Option<ArtworkTint>,
    artwork_transition_from: Option<ArtworkTint>,
    artwork_transition_started: Option<Instant>,
    artwork_caption: String,
    artwork_decode_rx: Option<Receiver<(TrackId, Result<DecodedArtwork, String>)>>,
    seek_preview_seconds: Option<f64>,
    lyrics_track: Option<TrackId>,
    lyrics_request_for: Option<TrackId>,
    lyrics: Option<Lyrics>,
    lyrics_caption: String,
    active_lyric_index: Option<usize>,
}

impl AurelisDesktop {
    pub(crate) fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);
        Self {
            application: DesktopApplication::new(),
            page: Page::Library,
            library_path: String::new(),
            library_section: LibrarySection::Home,
            library_search: String::new(),
            search_results: LibrarySearchResults::default(),
            search_results_stale: true,
            selected_collection: None,
            message: Some(BannerMessage {
                text: "Choose a local music folder to build your library.".to_owned(),
                tone: MessageTone::Info,
            }),
            artwork_track: None,
            artwork_request_for: None,
            artwork_texture: None,
            artwork_visual_cache: HashMap::new(),
            artwork_tint: None,
            artwork_transition_from: None,
            artwork_transition_started: None,
            artwork_caption: "Artwork appears here when the selected track has a supported image."
                .to_owned(),
            artwork_decode_rx: None,
            seek_preview_seconds: None,
            lyrics_track: None,
            lyrics_request_for: None,
            lyrics: None,
            lyrics_caption: "Local lyrics appear here when available.".to_owned(),
            active_lyric_index: None,
        }
    }

    fn show_message(&mut self, text: impl Into<String>, tone: MessageTone) {
        self.message = Some(BannerMessage {
            text: text.into(),
            tone,
        });
    }

    fn transition_artwork_tint(&mut self, tint: Option<ArtworkTint>) {
        if tint.is_none() && self.artwork_tint.is_none() {
            self.artwork_transition_from = None;
            self.artwork_transition_started = None;
            return;
        }

        let current = self
            .displayed_artwork_tint()
            .unwrap_or_else(neutral_artwork_tint);
        self.artwork_tint = tint;
        self.artwork_transition_from = Some(current);
        self.artwork_transition_started = Some(Instant::now());
    }

    fn displayed_artwork_tint(&mut self) -> Option<ArtworkTint> {
        let (Some(from), Some(started_at)) = (
            self.artwork_transition_from,
            self.artwork_transition_started,
        ) else {
            return self.artwork_tint;
        };

        let elapsed = Instant::now().duration_since(started_at).as_secs_f32();
        let progress = (elapsed / motion::ANIMATION_SECONDS).clamp(0.0, 1.0);
        let target = self.artwork_tint.unwrap_or_else(neutral_artwork_tint);
        let displayed = interpolate_artwork_tint(from, target, progress);

        if progress >= 1.0 {
            self.artwork_transition_from = None;
            self.artwork_transition_started = None;
            self.artwork_tint
        } else {
            Some(displayed)
        }
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
        self.search_results_stale = true;
        self.selected_collection = None;
        self.artwork_visual_cache.clear();
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
        self.sync_lyrics();
    }
    fn sync_artwork(&mut self, context: &egui::Context) {
        let current_track = self.application.playback().current_track_id;
        if current_track != self.artwork_track {
            self.artwork_track = current_track;
            self.artwork_request_for = None;
            self.artwork_texture = None;
            self.artwork_decode_rx = None;
            if current_track.is_none() {
                self.artwork_tint = None;
                self.artwork_transition_from = None;
                self.artwork_transition_started = None;
            }
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
                    self.transition_artwork_tint(None);
                    self.artwork_caption =
                        "No supported embedded or folder artwork was found.".to_owned();
                }
                Err(error) => {
                    self.transition_artwork_tint(None);
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
                    self.transition_artwork_tint(None);
                    self.artwork_caption = error;
                }
            }
        }

        self.poll_artwork_decode(context);
    }

    fn sync_lyrics(&mut self) {
        let current_track = self.application.playback().current_track_id;
        if current_track != self.lyrics_track {
            self.lyrics_track = current_track;
            self.lyrics_request_for = None;
            self.lyrics = None;
            self.active_lyric_index = None;
            self.lyrics_caption = if current_track.is_some() {
                "Loading local lyrics…".to_owned()
            } else {
                "Local lyrics appear here when available.".to_owned()
            };
        }

        if let Some((track_id, result)) = self.application.poll_lyrics()
            && Some(track_id) == self.lyrics_track
        {
            match result {
                Ok(Some(lyrics)) => {
                    self.lyrics = Some(lyrics);
                    self.lyrics_caption.clear();
                }
                Ok(None) => {
                    self.lyrics = None;
                    self.lyrics_caption =
                        "No local lyrics found. Add a same-stem .lrc or .txt sidecar beside the track."
                            .to_owned();
                }
                Err(error) => {
                    self.lyrics = None;
                    self.lyrics_caption = format!("Local lyrics could not be loaded: {error}");
                }
            }
        }

        if let Some(track_id) = current_track
            && self.lyrics_request_for != Some(track_id)
            && self.application.lyrics_loading_track().is_none()
        {
            match self.application.request_lyrics(track_id) {
                Ok(()) => {
                    self.lyrics_request_for = Some(track_id);
                    self.lyrics_caption = "Loading local lyrics…".to_owned();
                }
                Err(error) => {
                    self.lyrics_request_for = Some(track_id);
                    self.lyrics_caption = format!("Local lyrics could not be loaded: {error}");
                }
            }
        }
    }

    fn decode_artwork(&mut self, context: &egui::Context, track_id: TrackId, artwork: ArtworkView) {
        let (result_tx, result_rx) = mpsc::channel();
        let caption = format!(
            "{} · {}",
            artwork_format_name(artwork.format),
            artwork.source
        );
        let cached_visual = self.artwork_visual_cache.get(&track_id).copied();

        match thread::Builder::new()
            .name("aurelis-artwork-decode".to_owned())
            .spawn(move || {
                let result = image::load_from_memory(&artwork.data)
                    .map(|decoded| {
                        let rgba = decoded.to_rgba8();
                        let visual = cached_visual.or_else(|| {
                            let sample = sample_artwork_pixels(&rgba);
                            ArtworkVisual::from_rgba_pixels(&sample).ok()
                        });
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                        DecodedArtwork {
                            image,
                            caption,
                            visual,
                        }
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
                self.transition_artwork_tint(None);
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
                        if let Some(visual) = decoded.visual {
                            self.artwork_visual_cache.insert(track_id, visual);
                            self.transition_artwork_tint(Some(artwork_tint_from_visual(visual)));
                        } else {
                            self.transition_artwork_tint(None);
                        }
                        self.artwork_texture = Some(context.load_texture(
                            format!("aurelis-artwork-{track_id:?}"),
                            decoded.image,
                            egui::TextureOptions::LINEAR,
                        ));
                        self.artwork_caption = decoded.caption;
                    }
                    Err(error) => {
                        self.transition_artwork_tint(None);
                        self.artwork_caption = format!("Artwork could not be rendered: {error}");
                    }
                }
            }
            Err(TryRecvError::Empty) => self.artwork_decode_rx = Some(receiver),
            Err(TryRecvError::Disconnected) => {
                self.transition_artwork_tint(None);
                self.artwork_caption = "Artwork decoding ended unexpectedly.".to_owned();
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

        ui.add_space(spacing::GAP_16);
        let search_response = ui.add(
            egui::TextEdit::singleline(&mut self.library_search)
                .desired_width(ui.available_width())
                .hint_text("Search tracks, artists, albums, or genres"),
        );
        if search_response.changed() {
            self.search_results_stale = true;
            self.selected_collection = None;
        }

        if !self.library_search.trim().is_empty() {
            if self.search_results_stale {
                self.search_results = self.application.search_library(&self.library_search);
                self.search_results_stale = false;
            }
            ui.add_space(spacing::GAP_12);
            ui.label(
                RichText::new(format!(
                    "{} tracks · {} albums · {} artists · {} genres",
                    self.search_results.tracks.len(),
                    self.search_results.albums.len(),
                    self.search_results.artists.len(),
                    self.search_results.genres.len(),
                ))
                .size(typography::BODY)
                .strong()
                .color(TEXT),
            );
            let current_track = self.application.playback().current_track_id;
            if let Some(collection) = self.selected_collection.as_ref() {
                if ui.button("← Back to search results").clicked() {
                    self.selected_collection = None;
                    return;
                }
                section_heading(ui, &collection.title);
                ui.label(
                    RichText::new(&collection.subtitle)
                        .size(typography::BODY_SMALL)
                        .color(MUTED),
                );
                if let Some(track_id) = render_track_list(ui, &collection.tracks, current_track) {
                    self.play_selected_track(track_id);
                }
            } else if self.search_results.tracks.is_empty()
                && self.search_results.albums.is_empty()
                && self.search_results.artists.is_empty()
                && self.search_results.genres.is_empty()
            {
                empty_state(
                    ui,
                    "No matching music",
                    "Try another title, artist, album, or genre.",
                );
            } else if let Some(selection) =
                render_search_results(ui, &self.search_results, current_track)
            {
                match selection {
                    SearchSelection::Track(track_id) => self.play_selected_track(track_id),
                    SearchSelection::Collection(target) => self.open_collection(target),
                }
            }
            return;
        }

        ui.horizontal_wrapped(|ui| {
            for section in LibrarySection::ALL {
                if ui
                    .selectable_label(self.library_section == section, section.label())
                    .clicked()
                {
                    self.library_section = section;
                    self.selected_collection = None;
                }
            }
        });

        if self.selected_collection.is_some() {
            if ui
                .button(format!("← Back to {}", self.library_section.label()))
                .clicked()
            {
                self.selected_collection = None;
                return;
            }
            let current_track = self.application.playback().current_track_id;
            let selected_track = if let Some(collection) = self.selected_collection.as_ref() {
                section_heading(ui, &collection.title);
                ui.label(
                    RichText::new(&collection.subtitle)
                        .size(typography::BODY_SMALL)
                        .color(MUTED),
                );
                render_track_list(ui, &collection.tracks, current_track)
            } else {
                None
            };
            if let Some(track_id) = selected_track {
                self.play_selected_track(track_id);
            }
            return;
        }

        match self.library_section {
            LibrarySection::Home => self.draw_library_home(ui),
            LibrarySection::Tracks => {
                let current_track = self.application.playback().current_track_id;
                if let Some(track_id) =
                    render_track_list(ui, self.application.tracks(), current_track)
                {
                    self.play_selected_track(track_id);
                }
            }
            LibrarySection::Albums => self.draw_album_groups(ui),
            LibrarySection::Artists => self.draw_artist_groups(ui),
            LibrarySection::Genres => self.draw_genre_groups(ui),
            LibrarySection::Folders => self.draw_folder_groups(ui),
        }
    }

    fn draw_library_home(&self, ui: &mut egui::Ui) {
        if self.application.tracks().is_empty() {
            empty_state(
                ui,
                "Your library is ready for music",
                "Choose a local folder above. AURELIS will read the files it supports and keep the scan off the audio and interface threads.",
            );
            return;
        }

        ui.add_space(spacing::GAP_18);
        section_heading(ui, "LIBRARY OVERVIEW");
        ui.add_space(spacing::GAP_8);
        ui.horizontal_wrapped(|ui| {
            library_statistic(ui, "Tracks", self.application.tracks().len());
            library_statistic(ui, "Albums", self.application.albums().len());
            library_statistic(ui, "Artists", self.application.artists().len());
            library_statistic(ui, "Genres", self.application.genres().len());
            library_statistic(ui, "Folders", self.application.folders().len());
        });
    }

    fn draw_album_groups(&mut self, ui: &mut egui::Ui) {
        let groups = self.application.albums();
        if groups.is_empty() {
            empty_state(
                ui,
                "No album groups",
                "Scan a folder with tagged or organized music.",
            );
            return;
        }
        section_heading(ui, "ALBUMS");
        if let Some(target) = render_group_list(ui, groups) {
            self.open_collection(target);
        }
    }

    fn draw_artist_groups(&mut self, ui: &mut egui::Ui) {
        let groups = self.application.artists();
        if groups.is_empty() {
            empty_state(
                ui,
                "No artist groups",
                "Scan a folder containing audio tracks.",
            );
            return;
        }
        section_heading(ui, "ARTISTS");
        if let Some(target) = render_group_list(ui, groups) {
            self.open_collection(target);
        }
    }

    fn draw_genre_groups(&mut self, ui: &mut egui::Ui) {
        let groups = self.application.genres();
        if groups.is_empty() {
            empty_state(
                ui,
                "No genre tags",
                "Genre groups appear when tracks include genre metadata.",
            );
            return;
        }
        section_heading(ui, "GENRES");
        if let Some(target) = render_group_list(ui, groups) {
            self.open_collection(target);
        }
    }

    fn draw_folder_groups(&mut self, ui: &mut egui::Ui) {
        let groups = self.application.folders();
        if groups.is_empty() {
            empty_state(
                ui,
                "No folders",
                "Scan a local music folder to browse its files.",
            );
            return;
        }
        section_heading(ui, "FOLDERS");
        if let Some(target) = render_group_list(ui, groups) {
            self.open_collection(target);
        }
    }

    fn open_collection(&mut self, target: CollectionTarget) {
        let tracks = target
            .track_ids
            .iter()
            .filter_map(|track_id| self.application.track(*track_id).cloned())
            .collect();
        self.selected_collection = Some(SelectedCollection {
            title: target.title,
            subtitle: target.subtitle,
            tracks,
        });
    }

    fn play_selected_track(&mut self, track_id: TrackId) {
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
    fn draw_now_playing(&mut self, ui: &mut egui::Ui) {
        let artwork_tint = self.displayed_artwork_tint();
        if let Some(tint) = artwork_tint {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, artwork_background_tint(tint));
        }
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
                        .color(artwork_tint.map_or(ACCENT, readable_artwork_accent)),
                );
                ui.add_space(spacing::GAP_4);
                ui.label(
                    RichText::new(&track.album)
                        .size(typography::BODY_LARGE)
                        .color(MUTED),
                );
                ui.add_space(spacing::GAP_25);

                let has_track_tags = track.year.is_some()
                    || track.genre.is_some()
                    || track.track_number.is_some()
                    || track.disc_number.is_some();
                if has_track_tags {
                    ui.add_space(spacing::GAP_22);
                    section_heading(ui, "TRACK INFO");
                    ui.add_space(spacing::GAP_8);
                    if let Some(year) = &track.year {
                        metadata_row(ui, "Release year", year.clone());
                    }
                    if let Some(genre) = &track.genre {
                        metadata_row(ui, "Genre", genre.clone());
                    }
                    if let Some(track_number) = track.track_number {
                        metadata_row(ui, "Track", track_number.to_string());
                    }
                    if let Some(disc_number) = track.disc_number {
                        metadata_row(ui, "Disc", disc_number.to_string());
                    }
                }
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

        ui.add_space(spacing::GAP_26);
        section_heading(ui, "LYRICS");
        ui.add_space(spacing::GAP_8);
        self.draw_lyrics(ui, track_id, playback.position);
    }

    fn draw_lyrics(&mut self, ui: &mut egui::Ui, track_id: TrackId, position: Duration) {
        match self.lyrics.as_ref() {
            None => {
                self.active_lyric_index = None;
                ui.label(
                    RichText::new(&self.lyrics_caption)
                        .size(typography::BODY)
                        .color(MUTED),
                );
            }
            Some(Lyrics::Plain { text }) => {
                self.active_lyric_index = None;
                if text.trim().is_empty() {
                    ui.label(
                        RichText::new("This local lyrics file is empty.")
                            .size(typography::BODY)
                            .color(MUTED),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("plain-lyrics", track_id))
                        .max_height(240.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).size(typography::BODY_LARGE).color(TEXT));
                        });
                }
            }
            Some(Lyrics::Synced { lines }) => {
                let current_index = current_synced_lyric_index(lines, position);
                let current_changed = current_index != self.active_lyric_index;
                self.active_lyric_index = current_index;

                if lines.is_empty() {
                    ui.label(
                        RichText::new("No timestamped lyrics are available.")
                            .size(typography::BODY)
                            .color(MUTED),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("synced-lyrics", track_id))
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (index, line) in lines.iter().enumerate() {
                                let is_current = current_index == Some(index);
                                let nearby = current_index
                                    .is_some_and(|current| current.abs_diff(index) <= 1);
                                let color = if is_current {
                                    ACCENT
                                } else if nearby {
                                    TEXT
                                } else {
                                    MUTED
                                };
                                let size = if is_current {
                                    typography::ARTIST_TITLE
                                } else if nearby {
                                    typography::BODY_LARGE
                                } else {
                                    typography::BODY
                                };
                                let text = RichText::new(line.text()).size(size).color(color);
                                let text = if is_current { text.strong() } else { text };
                                let response = ui.add(egui::Label::new(text).wrap());
                                if is_current && current_changed {
                                    response.scroll_to_me(Some(Align::Center));
                                }
                                ui.add_space(spacing::GAP_10);
                            }
                        });
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
                Page::NowPlaying => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.draw_now_playing(ui));
                }
            });

        context.request_repaint_after(Duration::from_millis(motion::FRAME_REFRESH_MS));
    }
}
fn render_track_list(
    ui: &mut egui::Ui,
    tracks: &[TrackSummary],
    current_track: Option<TrackId>,
) -> Option<TrackId> {
    if tracks.is_empty() {
        empty_state(
            ui,
            "No tracks in this view",
            "Try another library section or search.",
        );
        return None;
    }

    ui.add_space(spacing::GAP_12);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{} tracks", tracks.len()))
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

    let mut selected_track = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, track) in tracks.iter().enumerate() {
                if track_row(ui, index, track, current_track) {
                    selected_track = Some(track.id);
                }
            }
        });
    selected_track
}

fn render_group_list<T: BrowseGroup>(ui: &mut egui::Ui, groups: &[T]) -> Option<CollectionTarget> {
    let mut selected = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            selected = render_group_rows(ui, groups);
        });
    selected
}

fn render_group_rows<T: BrowseGroup>(ui: &mut egui::Ui, groups: &[T]) -> Option<CollectionTarget> {
    for group in groups {
        let title = group.title();
        let subtitle = group.subtitle();
        if collection_row(
            ui,
            title.as_ref(),
            subtitle.as_ref(),
            group.track_ids().len(),
        ) {
            return Some(CollectionTarget {
                title: title.into_owned(),
                subtitle: subtitle.into_owned(),
                track_ids: group.track_ids().to_vec(),
            });
        }
        ui.add_space(spacing::GAP_5);
    }
    None
}

fn render_search_results(
    ui: &mut egui::Ui,
    results: &LibrarySearchResults,
    current_track: Option<TrackId>,
) -> Option<SearchSelection> {
    let mut selection = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if !results.tracks.is_empty() {
                section_heading(ui, "TRACKS");
                for (index, track) in results.tracks.iter().enumerate() {
                    if track_row(ui, index, track, current_track) {
                        selection = Some(SearchSelection::Track(track.id));
                    }
                }
                ui.add_space(spacing::GAP_12);
            }

            if !results.artists.is_empty() {
                section_heading(ui, "ARTISTS");
                let target = render_group_rows(ui, &results.artists);
                if selection.is_none() {
                    selection = target.map(SearchSelection::Collection);
                }
                ui.add_space(spacing::GAP_12);
            }
            if !results.albums.is_empty() {
                section_heading(ui, "ALBUMS");
                let target = render_group_rows(ui, &results.albums);
                if selection.is_none() {
                    selection = target.map(SearchSelection::Collection);
                }
                ui.add_space(spacing::GAP_12);
            }
            if !results.genres.is_empty() {
                section_heading(ui, "GENRES");
                let target = render_group_rows(ui, &results.genres);
                if selection.is_none() {
                    selection = target.map(SearchSelection::Collection);
                }
            }
        });
    selection
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

fn current_synced_lyric_index(
    lines: &[aurelis_app::SyncedLyricLine],
    position: Duration,
) -> Option<usize> {
    lines
        .partition_point(|line| line.timestamp() <= position)
        .checked_sub(1)
}

fn sample_artwork_pixels(image: &image::RgbaImage) -> Vec<[u8; 4]> {
    const MAX_SAMPLE_EDGE: usize = 32;

    let x_step = image.width().div_ceil(MAX_SAMPLE_EDGE as u32).max(1) as usize;
    let y_step = image.height().div_ceil(MAX_SAMPLE_EDGE as u32).max(1) as usize;
    let mut sample = Vec::with_capacity(MAX_SAMPLE_EDGE * MAX_SAMPLE_EDGE);

    for y in (0..image.height()).step_by(y_step) {
        for x in (0..image.width()).step_by(x_step) {
            sample.push(image.get_pixel(x, y).0);
        }
    }

    sample
}

#[cfg(test)]
mod artwork_tint_tests {
    use super::{ArtworkTint, interpolate_artwork_tint, sample_artwork_pixels};
    use aurelis_app::ArtworkVisual;
    use image::{Rgba, RgbaImage};

    #[test]
    fn sampled_pixels_are_bounded_and_use_the_core_artwork_visual_model() {
        let image = RgbaImage::from_fn(128, 96, |x, _| {
            if x < 96 {
                Rgba([220, 40, 50, 255])
            } else {
                Rgba([40, 60, 220, 255])
            }
        });

        let sample = sample_artwork_pixels(&image);
        let visual = ArtworkVisual::from_rgba_pixels(&sample).expect("artwork visual");

        assert_eq!(sample.len(), 32 * 32);
        assert!(visual.dominant_color().red() > visual.dominant_color().blue());
        assert!(visual.accent_color().blue() > visual.accent_color().red());
    }

    #[test]
    fn artwork_transition_interpolates_background_and_accent_tints() {
        let start = ArtworkTint {
            primary: [0, 20, 40],
            accent: [20, 40, 60],
        };
        let target = ArtworkTint {
            primary: [100, 120, 140],
            accent: [120, 140, 160],
        };

        let middle = interpolate_artwork_tint(start, target, 0.5);

        assert_eq!(middle.primary, [50, 70, 90]);
        assert_eq!(middle.accent, [70, 90, 110]);
    }
}

#[cfg(test)]
mod lyric_sync_tests {
    use super::current_synced_lyric_index;
    use aurelis_app::Lyrics;
    use std::time::Duration;

    #[test]
    fn follows_synced_lyrics_using_the_playback_position() {
        let lyrics = Lyrics::parse_lrc("[00:01.00]First\n[00:03.00]Second");
        let lines = lyrics.synced_lines().expect("synced lyrics");

        assert_eq!(
            current_synced_lyric_index(lines, Duration::from_millis(999)),
            None
        );
        assert_eq!(
            current_synced_lyric_index(lines, Duration::from_secs(1)),
            Some(0)
        );
        assert_eq!(
            current_synced_lyric_index(lines, Duration::from_secs(2)),
            Some(0)
        );
        assert_eq!(
            current_synced_lyric_index(lines, Duration::from_secs(3)),
            Some(1)
        );
    }
}
