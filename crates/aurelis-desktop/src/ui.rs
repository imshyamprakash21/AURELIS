use std::{
    borrow::Cow,
    collections::HashMap,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use crate::components::{
    BannerMessage, MessageTone, TrackRowAction, artwork_fallback, collection_row, draw_message,
    empty_state, format_time, library_statistic, metadata_row, navigation_button, section_heading,
    track_row,
};
use crate::theme::{layout, motion, palette::*, radius, spacing, typography};
use crate::universe::{self, UniverseAction};
use aurelis_app::{
    AlbumSummary, ArtistSummary, ArtworkImageFormat, ArtworkView, ArtworkVisual,
    DesktopApplication, DesktopPlaybackState, FolderSummary, GenreSummary, LibraryScanSummary,
    LibrarySearchResults, Lyrics, PlaybackView, PlaylistId, PlaylistTrackSummary,
    SPECTRUM_BAND_COUNT, SpectrumSnapshot, TrackId, TrackSummary,
};
use eframe::egui::{self, Align, Color32, ColorImage, Frame, Layout, RichText, Stroke, Vec2};

const MAX_ARTWORK_IMAGE_DIMENSION: u32 = 8192;
const MAX_ARTWORK_DECODE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Library,
    Universe,
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
    Playlists,
}

impl LibrarySection {
    const ALL: [Self; 7] = [
        Self::Home,
        Self::Tracks,
        Self::Albums,
        Self::Artists,
        Self::Genres,
        Self::Folders,
        Self::Playlists,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Tracks => "Tracks",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Genres => "Genres",
            Self::Folders => "Folders",
            Self::Playlists => "Playlists",
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
    TrackAction(TrackRowAction),
    Collection(CollectionTarget),
}

#[derive(Clone, Copy)]
enum QueueAction {
    ClearUpcoming,
    Move { from: usize, to: usize },
    Remove(usize),
    Play(TrackId),
}

enum PlaylistListAction {
    Open(PlaylistId),
    BeginRename(PlaylistId, String),
    SaveRename(PlaylistId, String),
    CancelRename,
    AskDelete(PlaylistId),
    CancelDelete,
    ConfirmDelete(PlaylistId),
}

enum PlaylistTrackAction {
    Play(TrackId),
    Queue(TrackId),
    MoveUp(usize),
    MoveDown(usize),
    Remove(usize),
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
    selected_playlist: Option<PlaylistId>,
    new_playlist_name: String,
    editing_playlist: Option<PlaylistId>,
    renamed_playlist_name: String,
    pending_delete_playlist: Option<PlaylistId>,
    playlist_add_target: Option<PlaylistId>,
    playlist_add_search: String,
    universe_selected_track: Option<TrackId>,
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
            selected_playlist: None,
            new_playlist_name: String::new(),
            editing_playlist: None,
            renamed_playlist_name: String::new(),
            pending_delete_playlist: None,
            playlist_add_target: None,
            playlist_add_search: String::new(),
            universe_selected_track: None,
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
        self.selected_playlist = None;
        self.editing_playlist = None;
        self.pending_delete_playlist = None;
        self.playlist_add_target = None;
        self.playlist_add_search.clear();
        self.universe_selected_track = None;
        self.artwork_visual_cache.clear();
        let mut message = format!(
            "{} supported files found · {} added · {} refreshed · {} unreadable · {} scan issues",
            summary.discovered, summary.added, summary.updated, summary.failed, summary.issues,
        );
        let mut tone = if summary.issues > 0 && summary.added + summary.updated == 0 {
            MessageTone::Error
        } else if summary.issues > 0 || summary.discovered == 0 {
            MessageTone::Info
        } else {
            MessageTone::Success
        };
        if let Some(error) = self.application.playlist_storage_error() {
            message.push_str(" · ");
            message.push_str(error);
            tone = MessageTone::Error;
        }
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
                let result = decode_artwork_image(&artwork.data)
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
        ui.add_space(spacing::GAP_6);
        if navigation_button(ui, "03", "Universe", self.page == Page::Universe) {
            self.page = Page::Universe;
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
        let mut volume = playback.volume;
        let mut volume_changed = false;

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
                DesktopPlaybackState::Playing => "||",
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
                let volume_response = ui.add_sized(
                    Vec2::new(88.0, 18.0),
                    egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false),
                );
                volume_changed = volume_response.changed();
                volume_response.on_hover_text("Output volume");
                ui.label(
                    RichText::new(format!("{}%", (volume * 100.0).round() as u32))
                        .size(typography::CAPTION)
                        .color(MUTED),
                );
                ui.label(RichText::new("VOL").size(typography::CAPTION).color(MUTED));
                ui.add_space(spacing::GAP_8);
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

        if volume_changed {
            self.application.set_volume(volume);
        }
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
                if let Some(action) = render_track_list(ui, &collection.tracks, current_track) {
                    self.handle_track_row_action(action);
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
                    SearchSelection::TrackAction(action) => self.handle_track_row_action(action),
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
                    if section != LibrarySection::Playlists {
                        self.selected_playlist = None;
                    }
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
            let selected_action = if let Some(collection) = self.selected_collection.as_ref() {
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
            if let Some(action) = selected_action {
                self.handle_track_row_action(action);
            }
            return;
        }

        match self.library_section {
            LibrarySection::Home => self.draw_library_home(ui),
            LibrarySection::Tracks => {
                let current_track = self.application.playback().current_track_id;
                if let Some(action) =
                    render_track_list(ui, self.application.tracks(), current_track)
                {
                    self.handle_track_row_action(action);
                }
            }
            LibrarySection::Albums => self.draw_album_groups(ui),
            LibrarySection::Artists => self.draw_artist_groups(ui),
            LibrarySection::Genres => self.draw_genre_groups(ui),
            LibrarySection::Folders => self.draw_folder_groups(ui),
            LibrarySection::Playlists => self.draw_playlists(ui),
        }
    }

    fn draw_playlists(&mut self, ui: &mut egui::Ui) {
        if self.application.library_root().is_none() {
            empty_state(
                ui,
                "Scan a music folder first",
                "Playlists are saved locally alongside the selected library.",
            );
            return;
        }

        if let Some(error) = self.application.playlist_storage_error() {
            draw_message(
                ui,
                &BannerMessage {
                    text: error.to_owned(),
                    tone: MessageTone::Error,
                },
            );
            empty_state(
                ui,
                "Saved playlists need attention",
                "The saved playlist file could not be loaded. AURELIS will not overwrite it.",
            );
            return;
        }

        if let Some(playlist_id) = self.selected_playlist {
            self.draw_playlist_detail(ui, playlist_id);
            return;
        }

        section_heading(ui, "PLAYLISTS");
        ui.add_space(spacing::GAP_8);
        let mut create_requested = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_playlist_name)
                    .desired_width(ui.available_width() - 130.0)
                    .hint_text("Name a playlist"),
            );
            create_requested = ui
                .add_enabled(
                    self.application.can_edit_playlists(),
                    egui::Button::new("Create playlist"),
                )
                .clicked();
        });
        if create_requested {
            match self
                .application
                .create_playlist(self.new_playlist_name.trim())
            {
                Ok(playlist_id) => {
                    self.new_playlist_name.clear();
                    self.selected_playlist = Some(playlist_id);
                    self.show_message("Playlist created.", MessageTone::Success);
                }
                Err(error) => self.show_message(error, MessageTone::Error),
            }
        }

        let playlists = self.application.playlists();
        if playlists.is_empty() {
            empty_state(
                ui,
                "No playlists yet",
                "Create a playlist, then add tracks from your library.",
            );
            return;
        }

        ui.add_space(spacing::GAP_12);
        let mut action = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for playlist in &playlists {
                    egui::Frame::new()
                        .fill(SURFACE)
                        .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
                        .inner_margin(egui::Margin::same(layout::CARD_INSET))
                        .show(ui, |ui| {
                            if self.editing_playlist == Some(playlist.id) {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.renamed_playlist_name)
                                            .desired_width(ui.available_width() - 112.0),
                                    );
                                    if ui.button("Save").clicked() {
                                        action = Some(PlaylistListAction::SaveRename(
                                            playlist.id,
                                            self.renamed_playlist_name.clone(),
                                        ));
                                    }
                                    if ui.button("Cancel").clicked() {
                                        action = Some(PlaylistListAction::CancelRename);
                                    }
                                });
                            } else if self.pending_delete_playlist == Some(playlist.id) {
                                ui.horizontal(|ui| {
                                    ui.label(format!("Delete ‘{}’?", playlist.name));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.button("Delete playlist").clicked() {
                                            action = Some(PlaylistListAction::ConfirmDelete(
                                                playlist.id,
                                            ));
                                        }
                                        if ui.button("Cancel").clicked() {
                                            action = Some(PlaylistListAction::CancelDelete);
                                        }
                                    });
                                });
                            } else {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(&playlist.name)
                                                .size(typography::BODY_LARGE)
                                                .strong()
                                                .color(TEXT),
                                        );
                                        ui.label(
                                            RichText::new(format!(
                                                "{} tracks",
                                                playlist.track_count
                                            ))
                                            .size(typography::CAPTION)
                                            .color(MUTED),
                                        );
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.button("Open").clicked() {
                                            action = Some(PlaylistListAction::Open(playlist.id));
                                        }
                                        if ui.button("Delete").clicked() {
                                            action =
                                                Some(PlaylistListAction::AskDelete(playlist.id));
                                        }
                                        if ui.button("Rename").clicked() {
                                            action = Some(PlaylistListAction::BeginRename(
                                                playlist.id,
                                                playlist.name.clone(),
                                            ));
                                        }
                                    });
                                });
                            }
                        });
                    ui.add_space(spacing::GAP_5);
                }
            });

        match action {
            Some(PlaylistListAction::Open(playlist_id)) => {
                self.selected_playlist = Some(playlist_id);
            }
            Some(PlaylistListAction::BeginRename(playlist_id, name)) => {
                self.editing_playlist = Some(playlist_id);
                self.renamed_playlist_name = name;
            }
            Some(PlaylistListAction::SaveRename(playlist_id, name)) => {
                match self.application.rename_playlist(playlist_id, &name) {
                    Ok(()) => {
                        self.editing_playlist = None;
                        self.show_message("Playlist renamed.", MessageTone::Success);
                    }
                    Err(error) => self.show_message(error, MessageTone::Error),
                }
            }
            Some(PlaylistListAction::CancelRename) => self.editing_playlist = None,
            Some(PlaylistListAction::AskDelete(playlist_id)) => {
                self.pending_delete_playlist = Some(playlist_id);
            }
            Some(PlaylistListAction::CancelDelete) => self.pending_delete_playlist = None,
            Some(PlaylistListAction::ConfirmDelete(playlist_id)) => {
                match self.application.delete_playlist(playlist_id) {
                    Ok(()) => {
                        self.pending_delete_playlist = None;
                        self.show_message("Playlist deleted.", MessageTone::Success);
                    }
                    Err(error) => self.show_message(error, MessageTone::Error),
                }
            }
            None => {}
        }
    }

    fn draw_playlist_detail(&mut self, ui: &mut egui::Ui, playlist_id: PlaylistId) {
        let Some(playlist) = self
            .application
            .playlists()
            .into_iter()
            .find(|playlist| playlist.id == playlist_id)
        else {
            self.selected_playlist = None;
            empty_state(ui, "Playlist unavailable", "Open another playlist.");
            return;
        };

        if ui.button("← Back to playlists").clicked() {
            self.selected_playlist = None;
            return;
        }
        ui.add_space(spacing::GAP_8);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(
                    RichText::new(&playlist.name)
                        .size(typography::PAGE_TITLE)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    RichText::new(format!("{} tracks", playlist.track_count))
                        .size(typography::BODY)
                        .color(MUTED),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Add tracks").clicked() && self.application.can_edit_playlists() {
                    self.playlist_add_target = Some(playlist_id);
                    self.playlist_add_search.clear();
                }
                if ui.button("Queue all").clicked() {
                    match self.application.queue_playlist(playlist_id) {
                        Ok(()) => self.show_message(
                            format!(
                                "Queued {} tracks from {}.",
                                playlist.track_count, playlist.name
                            ),
                            MessageTone::Success,
                        ),
                        Err(error) => self.show_message(error, MessageTone::Error),
                    }
                }
                if ui.button("Play all").clicked() {
                    match self.application.play_playlist(playlist_id) {
                        Ok(()) => {
                            self.page = Page::NowPlaying;
                            self.show_message(
                                format!("Playing {}.", playlist.name),
                                MessageTone::Success,
                            );
                        }
                        Err(error) => self.show_message(error, MessageTone::Error),
                    }
                }
            });
        });

        let entries = match self.application.playlist_tracks(playlist_id) {
            Ok(entries) => entries,
            Err(error) => {
                draw_message(
                    ui,
                    &BannerMessage {
                        text: error,
                        tone: MessageTone::Error,
                    },
                );
                return;
            }
        };
        if let Some(action) = render_playlist_track_rows(
            ui,
            playlist_id,
            &entries,
            self.application.playback().current_track_id,
        ) {
            match action {
                PlaylistTrackAction::Play(track_id) => self.play_selected_track(track_id),
                PlaylistTrackAction::Queue(track_id) => {
                    match self.application.enqueue_track(track_id) {
                        Ok(()) => {
                            self.show_message("Track added to the queue.", MessageTone::Success)
                        }
                        Err(error) => self.show_message(error, MessageTone::Error),
                    }
                }
                PlaylistTrackAction::MoveUp(index) => {
                    if let Err(error) = self.application.move_playlist_track(
                        playlist_id,
                        index,
                        index.saturating_sub(1),
                    ) {
                        self.show_message(error, MessageTone::Error);
                    }
                }
                PlaylistTrackAction::MoveDown(index) => {
                    if let Err(error) =
                        self.application
                            .move_playlist_track(playlist_id, index, index + 1)
                    {
                        self.show_message(error, MessageTone::Error);
                    }
                }
                PlaylistTrackAction::Remove(index) => {
                    match self.application.remove_playlist_track(playlist_id, index) {
                        Ok(()) => {
                            self.show_message("Track removed from the playlist.", MessageTone::Info)
                        }
                        Err(error) => self.show_message(error, MessageTone::Error),
                    }
                }
            }
        }
    }

    fn draw_playlist_track_picker(&mut self, context: &egui::Context) {
        let Some(playlist_id) = self.playlist_add_target else {
            return;
        };
        if !self.application.can_edit_playlists() {
            self.playlist_add_target = None;
            return;
        }
        let Some(playlist) = self
            .application
            .playlists()
            .into_iter()
            .find(|playlist| playlist.id == playlist_id)
        else {
            self.playlist_add_target = None;
            return;
        };

        let search = self.playlist_add_search.trim().to_owned();
        let tracks = if search.is_empty() {
            self.application.tracks().to_vec()
        } else {
            self.application.search_tracks(&search)
        };
        let mut open = true;
        let mut selected_track = None;
        egui::Window::new(format!("Add tracks to {}", playlist.name))
            .id(egui::Id::new(("playlist-track-picker", playlist_id)))
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .open(&mut open)
            .show(context, |ui| {
                ui.label("Choose tracks from the current library.");
                ui.add_space(spacing::GAP_8);
                ui.add(
                    egui::TextEdit::singleline(&mut self.playlist_add_search)
                        .desired_width(ui.available_width())
                        .hint_text("Search titles and artists"),
                );
                if tracks.is_empty() {
                    empty_state(ui, "No matching tracks", "Try another title or artist.");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(440.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for track in &tracks {
                                ui.push_id(track.id, |ui| {
                                    egui::Frame::new()
                                        .fill(SURFACE)
                                        .stroke(Stroke::new(layout::BORDER_WIDTH, EDGE))
                                        .inner_margin(egui::Margin::same(layout::CARD_INSET))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.vertical(|ui| {
                                                    ui.label(
                                                        RichText::new(&track.title)
                                                            .strong()
                                                            .color(TEXT),
                                                    );
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} · {}",
                                                            track.artist, track.album
                                                        ))
                                                        .size(typography::CAPTION)
                                                        .color(MUTED),
                                                    );
                                                });
                                                ui.with_layout(
                                                    Layout::right_to_left(Align::Center),
                                                    |ui| {
                                                        if ui.button("Add").clicked() {
                                                            selected_track = Some(track.id);
                                                        }
                                                    },
                                                );
                                            });
                                        });
                                    ui.add_space(spacing::GAP_5);
                                });
                            }
                        });
                }
            });

        if let Some(track_id) = selected_track {
            match self
                .application
                .add_track_to_playlist(playlist_id, track_id)
            {
                Ok(()) => self.show_message("Track added to the playlist.", MessageTone::Success),
                Err(error) => self.show_message(error, MessageTone::Error),
            }
        }
        if !open {
            self.playlist_add_target = None;
            self.playlist_add_search.clear();
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

    fn draw_universe(&mut self, ui: &mut egui::Ui) {
        if let Some(message) = &self.message {
            draw_message(ui, message);
            ui.add_space(spacing::GAP_10);
        }
        let playback = self.application.playback();
        let action = universe::draw(
            ui,
            self.application.tracks(),
            &playback,
            self.universe_selected_track,
        );
        match action {
            Some(UniverseAction::SelectTrack(track_id)) => {
                self.universe_selected_track = Some(track_id);
            }
            Some(UniverseAction::PlayTrack(track_id)) => self.play_selected_track(track_id),
            Some(UniverseAction::QueueTrack(track_id)) => {
                let title = self
                    .application
                    .track(track_id)
                    .map(|track| track.title.clone())
                    .unwrap_or_else(|| "Track".to_owned());
                match self.application.enqueue_track(track_id) {
                    Ok(()) => self
                        .show_message(format!("Added {title} to the queue"), MessageTone::Success),
                    Err(error) => self.show_message(error, MessageTone::Error),
                }
            }
            Some(UniverseAction::OpenNowPlaying) => self.page = Page::NowPlaying,
            None => {}
        }
    }

    fn handle_track_row_action(&mut self, action: TrackRowAction) {
        match action {
            TrackRowAction::Play(track_id) => self.play_selected_track(track_id),
            TrackRowAction::Queue(track_id) => {
                let title = self
                    .application
                    .track(track_id)
                    .map(|track| track.title.clone())
                    .unwrap_or_else(|| "Track".to_owned());
                match self.application.enqueue_track(track_id) {
                    Ok(()) => self
                        .show_message(format!("Added {title} to the queue"), MessageTone::Success),
                    Err(error) => self.show_message(error, MessageTone::Error),
                }
            }
        }
    }

    fn handle_queue_action(&mut self, action: QueueAction) {
        match action {
            QueueAction::ClearUpcoming => {
                let removed = self.application.clear_upcoming_queue();
                if removed > 0 {
                    self.show_message(
                        format!("Cleared {removed} upcoming track(s)"),
                        MessageTone::Info,
                    );
                }
            }
            QueueAction::Move { from, to } => {
                if let Err(error) = self.application.move_upcoming_track(from, to) {
                    self.show_message(error, MessageTone::Error);
                }
            }
            QueueAction::Remove(index) => match self.application.remove_upcoming_track(index) {
                Ok(track_id) => {
                    let title = self
                        .application
                        .track(track_id)
                        .map(|track| track.title.clone())
                        .unwrap_or_else(|| "Track".to_owned());
                    self.show_message(format!("Removed {title} from the queue"), MessageTone::Info);
                }
                Err(error) => self.show_message(error, MessageTone::Error),
            },
            QueueAction::Play(track_id) => self.play_selected_track(track_id),
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

        ui.add_space(spacing::GAP_26);
        section_heading(ui, "REAL-TIME SPECTRUM · 40 BANDS");
        ui.add_space(spacing::GAP_8);
        draw_spectrum(
            ui,
            playback.spectrum,
            playback.state,
            artwork_tint.map_or(ACCENT, readable_artwork_accent),
        );

        if !playback.queue_track_ids.is_empty() {
            ui.add_space(spacing::GAP_26);
            let upcoming_start = playback
                .current_queue_index
                .map_or(0, |index| index.saturating_add(1));
            let mut queue_action = None;
            ui.horizontal(|ui| {
                section_heading(ui, "QUEUE");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if playback.queue_track_ids.len() > upcoming_start
                        && ui.button("CLEAR UPCOMING").clicked()
                    {
                        queue_action = Some(QueueAction::ClearUpcoming);
                    }
                });
            });
            ui.add_space(spacing::GAP_8);
            let start = playback.current_queue_index.unwrap_or(0);
            let visible_count = playback.queue_track_ids.len().saturating_sub(start);
            egui::ScrollArea::vertical()
                .id_salt("now-playing-queue")
                .max_height(300.0)
                .show_rows(ui, layout::QUEUE_ROW_HEIGHT, visible_count, |ui, rows| {
                    for visible_index in rows {
                        let index = start + visible_index;
                        if let Some(queued_id) = playback.queue_track_ids.get(index)
                            && let Some(queued_track) = self.application.track(*queued_id)
                        {
                            let current =
                                index == playback.current_queue_index.unwrap_or(usize::MAX);
                            let upcoming = index >= upcoming_start;
                            let row_size =
                                Vec2::new(ui.available_width(), layout::QUEUE_ROW_HEIGHT);
                            ui.allocate_ui_with_layout(
                                row_size,
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    let controls_reserve = if upcoming { 210.0 } else { 56.0 };
                                    let metadata_width =
                                        (ui.available_width() - controls_reserve - 40.0).max(120.0);
                                    let title_width = metadata_width * 0.62;
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("{:02}", index + 1))
                                                .size(typography::CAPTION)
                                                .color(MUTED),
                                        );
                                        ui.add_sized(
                                            [title_width, layout::QUEUE_ROW_HEIGHT],
                                            egui::Label::new(
                                                RichText::new(&queued_track.title)
                                                    .size(typography::BODY)
                                                    .color(if current { ACCENT } else { TEXT }),
                                            )
                                            .truncate(),
                                        );
                                        ui.add_sized(
                                            [
                                                metadata_width - title_width,
                                                layout::QUEUE_ROW_HEIGHT,
                                            ],
                                            egui::Label::new(
                                                RichText::new(&queued_track.artist)
                                                    .size(typography::BODY_SMALL)
                                                    .color(MUTED),
                                            )
                                            .truncate(),
                                        );
                                        if current {
                                            ui.label(
                                                RichText::new("NOW")
                                                    .size(typography::MICRO)
                                                    .strong()
                                                    .color(ACCENT),
                                            );
                                        } else if upcoming {
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if ui.button("REMOVE").clicked() {
                                                        queue_action =
                                                            Some(QueueAction::Remove(index));
                                                    }
                                                    if ui.button("PLAY").clicked() {
                                                        queue_action =
                                                            Some(QueueAction::Play(*queued_id));
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            index + 1
                                                                < playback.queue_track_ids.len(),
                                                            egui::Button::new("↓"),
                                                        )
                                                        .on_hover_text("Move down")
                                                        .clicked()
                                                    {
                                                        queue_action = Some(QueueAction::Move {
                                                            from: index,
                                                            to: index + 1,
                                                        });
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            index > upcoming_start,
                                                            egui::Button::new("↑"),
                                                        )
                                                        .on_hover_text("Move up")
                                                        .clicked()
                                                    {
                                                        queue_action = Some(QueueAction::Move {
                                                            from: index,
                                                            to: index - 1,
                                                        });
                                                    }
                                                },
                                            );
                                        }
                                    });
                                },
                            );
                        }
                    }
                });
            if let Some(action) = queue_action {
                self.handle_queue_action(action);
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
                                let response = if line.segments().is_empty() || !is_current {
                                    let text = RichText::new(line.text()).size(size).color(color);
                                    let text = if is_current { text.strong() } else { text };
                                    ui.add(egui::Label::new(text).wrap())
                                } else {
                                    let current_segment = current_synced_lyric_segment_index(
                                        line.segments(),
                                        position,
                                    );
                                    let mut job = egui::text::LayoutJob::default();
                                    for (segment_index, segment) in
                                        line.segments().iter().enumerate()
                                    {
                                        let segment_color =
                                            if current_segment == Some(segment_index) {
                                                ACCENT
                                            } else if current_segment.is_some() {
                                                TEXT
                                            } else {
                                                ACCENT
                                            };
                                        job.append(
                                            segment.text(),
                                            0.0,
                                            egui::TextFormat::simple(
                                                egui::FontId::proportional(size),
                                                segment_color,
                                            ),
                                        );
                                    }
                                    ui.add(egui::Label::new(job).wrap())
                                };
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
                Page::Universe => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.draw_universe(ui));
                }
                Page::NowPlaying => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.draw_now_playing(ui));
                }
            });

        self.draw_playlist_track_picker(&context);

        context.request_repaint_after(Duration::from_millis(motion::FRAME_REFRESH_MS));
    }
}

fn render_playlist_track_rows(
    ui: &mut egui::Ui,
    playlist_id: PlaylistId,
    entries: &[PlaylistTrackSummary],
    current_track: Option<TrackId>,
) -> Option<PlaylistTrackAction> {
    if entries.is_empty() {
        empty_state(
            ui,
            "No tracks in this playlist",
            "Add tracks from the current library to build this playlist.",
        );
        return None;
    }

    let mut selected_action = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, entry) in entries.iter().enumerate() {
                let selected = entry
                    .track
                    .as_ref()
                    .is_some_and(|track| current_track == Some(track.id));
                let fill = if selected { ACCENT_DARK } else { SURFACE };
                ui.push_id((playlist_id, index), |ui| {
                    egui::Frame::new()
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
                                ui.vertical(|ui| {
                                    if let Some(track) = &entry.track {
                                        ui.label(RichText::new(&track.title).strong().color(TEXT));
                                        ui.label(
                                            RichText::new(format!(
                                                "{} · {}",
                                                track.artist, track.album
                                            ))
                                            .size(typography::CAPTION)
                                            .color(MUTED),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new("Missing track").strong().color(TEXT),
                                        );
                                        ui.label(
                                            RichText::new(entry.path.display().to_string())
                                                .size(typography::CAPTION)
                                                .color(MUTED),
                                        );
                                    }
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.button("Remove").clicked() {
                                        selected_action = Some(PlaylistTrackAction::Remove(index));
                                    }
                                    if ui
                                        .add_enabled(
                                            index + 1 < entries.len(),
                                            egui::Button::new("↓"),
                                        )
                                        .clicked()
                                    {
                                        selected_action =
                                            Some(PlaylistTrackAction::MoveDown(index));
                                    }
                                    if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked()
                                    {
                                        selected_action = Some(PlaylistTrackAction::MoveUp(index));
                                    }
                                    if let Some(track) = &entry.track {
                                        if ui.button("Queue").clicked() {
                                            selected_action =
                                                Some(PlaylistTrackAction::Queue(track.id));
                                        }
                                        if ui.button("Play").clicked() {
                                            selected_action =
                                                Some(PlaylistTrackAction::Play(track.id));
                                        }
                                    }
                                });
                            });
                        });
                    ui.add_space(spacing::GAP_5);
                });
            }
        });
    selected_action
}

fn render_track_list(
    ui: &mut egui::Ui,
    tracks: &[TrackSummary],
    current_track: Option<TrackId>,
) -> Option<TrackRowAction> {
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

    let mut selected_action = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, track) in tracks.iter().enumerate() {
                if let Some(action) = track_row(ui, index, track, current_track) {
                    selected_action = Some(action);
                }
            }
        });
    selected_action
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
                    if let Some(action) = track_row(ui, index, track, current_track) {
                        selection = Some(SearchSelection::TrackAction(action));
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

fn draw_spectrum(
    ui: &mut egui::Ui,
    spectrum: SpectrumSnapshot,
    playback_state: DesktopPlaybackState,
    accent: Color32,
) {
    const HEIGHT: f32 = 112.0;
    const BAR_GAP: f32 = 2.0;

    let width = ui.available_width().max(0.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), egui::Sense::hover());
    let _ = response.on_hover_text("Frequency energy measured from the audio output samples.");
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, radius::WIDGET, SURFACE);
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.bottom() - 1.0),
            egui::pos2(rect.right(), rect.bottom() - 1.0),
        ],
        Stroke::new(layout::BORDER_WIDTH, EDGE),
    );

    let is_active = matches!(
        playback_state,
        DesktopPlaybackState::Playing | DesktopPlaybackState::Paused
    );
    let bands = spectrum.bands();
    let has_signal = bands.iter().any(|band| *band > 0.01);

    if is_active && has_signal {
        let bar_width = (rect.width() - BAR_GAP * (SPECTRUM_BAND_COUNT.saturating_sub(1) as f32))
            .max(0.0)
            / SPECTRUM_BAND_COUNT as f32;
        let max_height = (rect.height() - spacing::GAP_12).max(0.0);
        let bar_color = if playback_state == DesktopPlaybackState::Paused {
            accent.gamma_multiply(0.58)
        } else {
            accent
        };

        for (index, energy) in bands.iter().enumerate() {
            let bar_height = energy.clamp(0.0, 1.0) * max_height;
            if bar_height <= 0.5 || bar_width <= 0.0 {
                continue;
            }

            let left = rect.left() + index as f32 * (bar_width + BAR_GAP);
            let right = (left + bar_width).min(rect.right());
            let top = rect.bottom() - spacing::GAP_6 - bar_height;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(left, top),
                    egui::pos2(right, rect.bottom() - spacing::GAP_6),
                ),
                radius::WIDGET,
                bar_color,
            );
        }
    } else {
        let message = if !is_active {
            "Play a track to see its spectrum"
        } else if playback_state == DesktopPlaybackState::Paused {
            "No recent output signal"
        } else {
            "Waiting for an audio signal"
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(typography::CAPTION),
            MUTED,
        );
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new("LOW").size(typography::MICRO).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new("HIGH").size(typography::MICRO).color(MUTED));
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

fn decode_artwork_image(data: &[u8]) -> image::ImageResult<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ARTWORK_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_ARTWORK_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_ARTWORK_DECODE_BYTES);
    reader.limits(limits);
    reader.decode()
}

fn current_synced_lyric_index(
    lines: &[aurelis_app::SyncedLyricLine],
    position: Duration,
) -> Option<usize> {
    lines
        .partition_point(|line| line.timestamp() <= position)
        .checked_sub(1)
}

fn current_synced_lyric_segment_index(
    segments: &[aurelis_app::SyncedLyricSegment],
    position: Duration,
) -> Option<usize> {
    segments
        .iter()
        .enumerate()
        .filter(|(_, segment)| segment.timestamp() <= position)
        .max_by_key(|(_, segment)| segment.timestamp())
        .map(|(index, _)| index)
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
mod artwork_decode_tests {
    use super::{MAX_ARTWORK_IMAGE_DIMENSION, decode_artwork_image};
    use image::{ColorType, ImageEncoder, ImageError, codecs::png::PngEncoder};

    fn one_pixel_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&[255, 0, 0, 255], 1, 1, ColorType::Rgba8.into())
            .unwrap();
        bytes
    }

    fn png_crc32(data: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
            }
        }
        !crc
    }

    #[test]
    fn decodes_normal_artwork_and_rejects_dimensions_over_the_limit() {
        let mut oversized = one_pixel_png();
        oversized[16..20].copy_from_slice(&(MAX_ARTWORK_IMAGE_DIMENSION + 1).to_be_bytes());
        let header_crc = png_crc32(&oversized[12..29]);
        oversized[29..33].copy_from_slice(&header_crc.to_be_bytes());

        assert!(decode_artwork_image(&one_pixel_png()).is_ok());
        assert!(matches!(
            decode_artwork_image(&oversized),
            Err(ImageError::Limits(_))
        ));
    }
}

#[cfg(test)]
mod lyric_sync_tests {
    use super::{current_synced_lyric_index, current_synced_lyric_segment_index};
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

    #[test]
    fn follows_enhanced_lyric_segments_using_the_playback_position() {
        let lyrics = Lyrics::parse_lrc("[00:01.00]<00:01.25>First <00:01.75>second");
        let segments = lyrics.synced_lines().unwrap()[0].segments();

        assert_eq!(
            current_synced_lyric_segment_index(segments, Duration::from_millis(1_249)),
            None
        );
        assert_eq!(
            current_synced_lyric_segment_index(segments, Duration::from_millis(1_250)),
            Some(0)
        );
        assert_eq!(
            current_synced_lyric_segment_index(segments, Duration::from_millis(1_749)),
            Some(0)
        );
        assert_eq!(
            current_synced_lyric_segment_index(segments, Duration::from_millis(1_750)),
            Some(1)
        );
    }
}
