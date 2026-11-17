use crate::api::{AlbumSummary, ArtistSummary, Device, PlaylistSummary, SearchResults, TrackRow};
use crate::backend::{Command, Event, HomeSection, RepeatMode, SongSection, ViewKey};
use crate::icons::{self, Icon};
use crate::sdk::{Sdk, SdkMsg};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Mesh,
    Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
};
use egui_extras::{Column, TableBuilder};
use std::{collections::HashMap, sync::mpsc::Receiver, time::Instant};
use tokio::sync::mpsc::UnboundedSender;

// ---- palette: warm maroon → black, coral accent, frosted-glass surfaces ----
const BG_TOP: Color32 = Color32::from_rgb(0x2b, 0x13, 0x13);
const BG_BOTTOM: Color32 = Color32::from_rgb(0x0d, 0x09, 0x0a);
const GLASS: Color32 = Color32::from_rgba_premultiplied(14, 12, 12, 16);
const GLASS_HOVER: Color32 = Color32::from_rgba_premultiplied(28, 24, 24, 30);
const GLASS_STROKE: Color32 = Color32::from_rgba_premultiplied(24, 22, 22, 26);
const ACCENT: Color32 = Color32::from_rgb(0xf0, 0x52, 0x4a);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0xff, 0x6a, 0x60);
const TEXT: Color32 = Color32::from_rgb(0xf6, 0xee, 0xec);
const MUTED: Color32 = Color32::from_rgb(0xb3, 0xa1, 0x9e);
const DIM: Color32 = Color32::from_rgb(0x6e, 0x60, 0x5e);

/// Starting volume for the SDK player (0..1).
const DEFAULT_VOLUME: f32 = 0.6;

/// Mood chips run a search when clicked.
const MOODS: &[&str] = &[
    "Chill", "Focus", "Party", "Romance", "Workout", "Lo-fi", "Jazz", "Bollywood", "Acoustic",
];

#[derive(Clone, Debug)]
struct NowPlaying {
    uri: String,
    name: String,
    artists: String,
    album: String,
    cover: Option<String>,
    duration_ms: u32,
    album_id: Option<String>,
    artist_id: Option<String>,
}

impl NowPlaying {
    fn as_track(&self) -> TrackRow {
        TrackRow {
            uri: self.uri.clone(),
            name: self.name.clone(),
            artists: self.artists.clone(),
            album: self.album.clone(),
            duration_ms: self.duration_ms,
            image: self.cover.clone(),
            cover: self.cover.clone(),
            album_id: self.album_id.clone(),
            artist_id: self.artist_id.clone(),
        }
    }
}

/// Tabs of the Now Playing side panel.
#[derive(Clone, Copy, PartialEq)]
enum NpTab {
    Queue,
    Album,
    Artist,
}

enum Auth {
    Checking,
    NeedsLogin,
    LoggingIn { browser: bool },
    LoggedIn { username: String, display_name: String },
}

struct TrackView {
    key: ViewKey,
    title: String,
    subtitle: String,
    image: Option<String>,
    context_uri: String,
    album_name: String,
    tracks: Vec<TrackRow>,
    next_offset: Option<u32>,
    loading: bool,
}

struct SongView {
    track: TrackRow,
    album: Option<Vec<TrackRow>>,
    by_artist: Option<Vec<TrackRow>>,
    albums: Option<Vec<AlbumSummary>>,
    similar: Option<(String, Vec<TrackRow>)>,
}

impl SongView {
    fn new(track: TrackRow) -> Self {
        Self { track, album: None, by_artist: None, albums: None, similar: None }
    }

    fn apply(&mut self, section: SongSection) {
        match section {
            SongSection::Album(v) => self.album = Some(v),
            SongSection::Artist(v) => self.by_artist = Some(v),
            SongSection::ArtistAlbums(v) => self.albums = Some(v),
            SongSection::Similar { genre, tracks } => self.similar = Some((genre, tracks)),
        }
    }
}

enum View {
    Home,
    Search,
    Tracks(TrackView),
    Song(SongView),
    NowPlaying,
}

#[derive(Default)]
struct HomeData {
    recent: Vec<TrackRow>,
    top_tracks: Vec<TrackRow>,
    top_artists: Vec<ArtistSummary>,
    because: Option<(String, Vec<TrackRow>)>,
    more_from: Vec<(String, Vec<TrackRow>)>,
    albums: Vec<AlbumSummary>,
}

/// Where a clicked track should play from.
enum PlayFrom {
    /// A context (album/playlist/collection), starting at the track.
    Context(String),
    /// An ad-hoc list (search results, suggestions), starting at an index.
    List(Vec<String>, u32),
}

/// Transport buttons for `App::controls`.
#[derive(Clone, Copy, PartialEq)]
enum Ctl {
    Shuffle,
    Prev,
    Back10,
    Play,
    Fwd10,
    Next,
    Repeat,
}

/// What a click on a song row/card asked for.
#[derive(PartialEq)]
enum RowAct {
    None,
    /// Row clicked: play it and open the song page.
    Open,
    /// Small play button: just play.
    Play,
}

pub struct App {
    cmd: UnboundedSender<Command>,
    rx: Receiver<Event>,
    auth: Auth,
    playlists: Vec<PlaylistSummary>,
    liked_preview: Vec<TrackRow>,
    home: HomeData,
    hero_index: usize,
    view: View,
    history: Vec<View>,
    search_query: String,
    search: Option<SearchResults>,
    searching: bool,
    now: Option<NowPlaying>,
    playing: bool,
    pos_ms: u32,
    pos_at: Instant,
    seek_drag: Option<f32>,
    volume: f32,
    volume_before_mute: f32,
    shuffle: bool,
    repeat: RepeatMode,
    devices: Vec<Device>,
    queue: Vec<TrackRow>,
    saved: HashMap<String, bool>,
    /// Set when the user asks to play something; cleared when the SDK starts playing.
    loading_play: Option<Instant>,
    sdk: Option<Sdk>,
    sdk_rx: Receiver<SdkMsg>,
    /// True while the SDK player on this Mac holds the playback session.
    local_active: bool,
    error: Option<(String, Instant)>,
    np_tab: NpTab,
    /// Album / artist tracks for the Now Playing panel (same shape as a song page).
    np_related: Option<SongView>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd: UnboundedSender<Command>,
        rx: Receiver<Event>,
    ) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        install_fonts(&cc.egui_ctx);
        apply_theme(&cc.egui_ctx);
        let (sdk_tx, sdk_rx) = std::sync::mpsc::channel();
        let (sdk, error) = match Sdk::new(cc, sdk_tx, DEFAULT_VOLUME) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some((format!("Player failed to start: {e:#}"), Instant::now()))),
        };
        Self {
            cmd,
            rx,
            auth: Auth::Checking,
            playlists: Vec::new(),
            liked_preview: Vec::new(),
            home: HomeData::default(),
            hero_index: 0,
            view: View::Home,
            history: Vec::new(),
            search_query: String::new(),
            search: None,
            searching: false,
            now: None,
            playing: false,
            pos_ms: 0,
            pos_at: Instant::now(),
            seek_drag: None,
            volume: DEFAULT_VOLUME,
            volume_before_mute: DEFAULT_VOLUME,
            shuffle: false,
            repeat: RepeatMode::Off,
            devices: Vec::new(),
            queue: Vec::new(),
            saved: HashMap::new(),
            loading_play: None,
            sdk,
            sdk_rx,
            local_active: false,
            error,
            np_tab: NpTab::Queue,
            np_related: None,
        }
    }

    fn send(&self, c: Command) {
        let _ = self.cmd.send(c);
    }

    fn username(&self) -> &str {
        match &self.auth {
            Auth::LoggedIn { username, .. } => username,
            _ => "",
        }
    }

    fn first_name(&self) -> String {
        match &self.auth {
            Auth::LoggedIn { display_name, .. } => {
                display_name.split_whitespace().next().unwrap_or("there").to_string()
            }
            _ => String::new(),
        }
    }

    fn liked_uri(&self) -> String {
        format!("spotify:user:{}:collection", self.username())
    }

    fn position(&self) -> u32 {
        let dur = self.now.as_ref().map_or(0, |n| n.duration_ms);
        let p = if self.playing {
            self.pos_ms + self.pos_at.elapsed().as_millis() as u32
        } else {
            self.pos_ms
        };
        if dur > 0 { p.min(dur) } else { p }
    }

    fn is_saved(&self, uri: &str) -> bool {
        self.saved.get(uri).copied().unwrap_or(false)
    }

    fn toggle_saved(&mut self, uri: &str) {
        let saved = !self.is_saved(uri);
        self.saved.insert(uri.to_string(), saved);
        self.send(Command::SetSaved { uri: uri.to_string(), saved });
    }

    // ---- navigation ----

    /// Open a view, remembering the current one for Back.
    fn go(&mut self, v: View) {
        let old = std::mem::replace(&mut self.view, v);
        self.history.push(old);
        if self.history.len() > 30 {
            self.history.remove(0);
        }
    }

    /// Sidebar navigation: a fresh start, no Back history.
    fn nav(&mut self, v: View) {
        self.history.clear();
        self.view = v;
    }

    fn back(&mut self) {
        if let Some(v) = self.history.pop() {
            self.view = v;
        }
    }

    fn open_tracks(&mut self, tv: TrackView) {
        self.send(Command::LoadTracks {
            view: tv.key.clone(),
            offset: 0,
            album_name: tv.album_name.clone(),
        });
        self.go(View::Tracks(tv));
    }

    fn open_liked(&mut self) {
        let context_uri = self.liked_uri();
        self.open_tracks(TrackView {
            key: ViewKey::Liked,
            title: "Liked Songs".into(),
            subtitle: "Your saved tracks".into(),
            image: None,
            context_uri,
            album_name: String::new(),
            tracks: Vec::new(),
            next_offset: None,
            loading: true,
        });
    }

    fn open_playlist(&mut self, p: &PlaylistSummary) {
        self.open_tracks(TrackView {
            key: ViewKey::Playlist(p.id.clone()),
            title: p.name.clone(),
            subtitle: if p.owner.is_empty() { String::new() } else { format!("by {}", p.owner) },
            image: p.image.clone(),
            context_uri: p.uri.clone(),
            album_name: String::new(),
            tracks: Vec::new(),
            next_offset: None,
            loading: true,
        });
    }

    fn open_song(&mut self, t: &TrackRow) {
        self.send(Command::LoadSong(t.clone()));
        if !self.saved.contains_key(&t.uri) {
            self.send(Command::CheckSaved(t.uri.clone()));
        }
        self.go(View::Song(SongView::new(t.clone())));
    }

    fn open_album(&mut self, a: &AlbumSummary) {
        self.open_tracks(TrackView {
            key: ViewKey::Album(a.id.clone()),
            title: a.name.clone(),
            subtitle: a.artists.clone(),
            image: a.image.clone(),
            context_uri: a.uri.clone(),
            album_name: a.name.clone(),
            tracks: Vec::new(),
            next_offset: None,
            loading: true,
        });
    }

    fn open_now_playing(&mut self) {
        if !matches!(self.view, View::NowPlaying) {
            self.send(Command::LoadQueue);
            self.load_np_related();
            self.go(View::NowPlaying);
        }
    }

    /// Fetch album/artist tracks for the Now Playing panel (once per track).
    fn load_np_related(&mut self) {
        let Some(n) = &self.now else { return };
        if self.np_related.as_ref().is_some_and(|r| r.track.uri == n.uri) {
            return;
        }
        let t = n.as_track();
        self.send(Command::LoadSong(t.clone()));
        self.np_related = Some(SongView::new(t));
    }

    fn run_search(&mut self, q: &str) {
        self.search_query = q.to_string();
        self.searching = true;
        if !matches!(self.view, View::Search) {
            self.go(View::Search);
        }
        self.send(Command::Search(q.to_string()));
    }

    // ---- playback ----

    /// Start a track, showing it in the player bar right away (before the SDK confirms).
    fn play_track(&mut self, t: &TrackRow, from: PlayFrom) {
        self.now = Some(NowPlaying {
            uri: t.uri.clone(),
            name: t.name.clone(),
            artists: t.artists.clone(),
            album: t.album.clone(),
            cover: t.cover.clone().or_else(|| t.image.clone()),
            duration_ms: t.duration_ms,
            album_id: t.album_id.clone(),
            artist_id: t.artist_id.clone(),
        });
        self.pos_ms = 0;
        self.pos_at = Instant::now();
        self.playing = false;
        self.loading_play = Some(Instant::now());
        match from {
            PlayFrom::Context(context_uri) => {
                self.send(Command::PlayContext { context_uri, track_uri: Some(t.uri.clone()) })
            }
            PlayFrom::List(uris, index) => self.send(Command::PlayTracks { uris, index }),
        }
    }

    fn play_context(&mut self, context_uri: String) {
        self.loading_play = Some(Instant::now());
        self.send(Command::PlayContext { context_uri, track_uri: None });
    }

    /// Row click on a list of tracks: play from that list (and open the song page on `Open`).
    fn act_on_list(&mut self, tracks: &[TrackRow], i: usize, act: RowAct) {
        if act == RowAct::None {
            return;
        }
        let uris = tracks.iter().map(|t| t.uri.clone()).collect();
        let t = tracks[i].clone();
        self.play_track(&t, PlayFrom::List(uris, i as u32));
        if act == RowAct::Open {
            self.open_song(&t);
        }
    }

    fn toggle_play(&mut self) {
        match &self.sdk {
            Some(sdk) if self.local_active => sdk.toggle_play(),
            // Nothing loaded here yet: pull the current session onto this Mac.
            _ => {
                self.loading_play = Some(Instant::now());
                self.send(Command::TransferHere);
            }
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(sdk) = &self.sdk {
            sdk.set_volume(self.volume);
        }
    }

    fn seek(&mut self, ms: u32) {
        if let Some(sdk) = &self.sdk {
            sdk.seek(ms);
        }
        self.pos_ms = ms;
        self.pos_at = Instant::now();
    }

    // ---- events ----

    fn drain_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::NeedsLogin => self.auth = Auth::NeedsLogin,
                Event::LoggingIn { browser } => self.auth = Auth::LoggingIn { browser },
                Event::LoggedIn { username, display_name } => {
                    self.auth = Auth::LoggedIn { username, display_name };
                    self.send(Command::LoadPlaylists);
                    self.send(Command::LoadHome);
                    // First page of Liked Songs feeds the Home "Recently liked" list.
                    self.send(Command::LoadTracks {
                        view: ViewKey::Liked,
                        offset: 0,
                        album_name: String::new(),
                    });
                    self.nav(View::Home);
                }
                Event::Error(e) => {
                    self.error = Some((e, Instant::now()));
                    self.searching = false;
                    self.loading_play = None;
                    if let View::Tracks(tv) = &mut self.view {
                        tv.loading = false;
                    }
                }
                Event::Playlists(p) => self.playlists = p,
                Event::Tracks { view, tracks, append, next_offset } => {
                    if view == ViewKey::Liked && !append {
                        self.liked_preview = tracks.iter().take(6).cloned().collect();
                    }
                    if let View::Tracks(tv) = &mut self.view {
                        if tv.key == view {
                            if append {
                                tv.tracks.extend(tracks);
                            } else {
                                tv.tracks = tracks;
                            }
                            tv.next_offset = next_offset;
                            tv.loading = false;
                        }
                    }
                }
                Event::SearchResults(r) => {
                    self.search = Some(r);
                    self.searching = false;
                }
                Event::Devices(d) => self.devices = d,
                Event::SdkToken(t) => {
                    if let Some(sdk) = &self.sdk {
                        sdk.set_token(&t);
                    }
                }
                Event::Home(section) => match section {
                    HomeSection::Recent(v) => self.home.recent = v,
                    HomeSection::TopTracks(v) => self.home.top_tracks = v,
                    HomeSection::TopArtists(v) => self.home.top_artists = v,
                    HomeSection::BecauseYouLike { artist, tracks } => {
                        self.home.because = Some((artist, tracks))
                    }
                    HomeSection::MoreFrom { artist, tracks } => self.home.more_from.push((artist, tracks)),
                    HomeSection::Albums(v) => self.home.albums = v,
                },
                Event::Song { uri, section } => {
                    // The same song can be open as a page and in Now Playing.
                    let page = match &mut self.view {
                        View::Song(sv) if sv.track.uri == uri => Some(sv),
                        _ => None,
                    };
                    let np = self.np_related.as_mut().filter(|r| r.track.uri == uri);
                    match (page, np) {
                        (Some(sv), Some(r)) => {
                            r.apply(section.clone());
                            sv.apply(section);
                        }
                        (Some(sv), None) => sv.apply(section),
                        (None, Some(r)) => r.apply(section),
                        (None, None) => {}
                    }
                }
                Event::Queue(q) => self.queue = q,
                Event::Saved { uri, saved } => {
                    self.saved.insert(uri, saved);
                }
            }
        }
    }

    fn drain_sdk(&mut self) {
        while let Ok(msg) = self.sdk_rx.try_recv() {
            match msg {
                SdkMsg::Token => self.send(Command::SdkToken),
                SdkMsg::Ready { device_id } => {
                    log::info!("SDK player ready: {device_id}");
                    self.send(Command::SetDevice(Some(device_id)));
                }
                SdkMsg::NotReady => {
                    self.local_active = false;
                    self.send(Command::SetDevice(None));
                }
                SdkMsg::State(s) => {
                    if !s.paused {
                        if let Some(t) = self.loading_play.take() {
                            log::info!("playback started {:?} after the click", t.elapsed());
                        }
                    }
                    let changed = self.now.as_ref().is_none_or(|n| n.uri != s.uri);
                    self.local_active = true;
                    self.playing = !s.paused;
                    self.pos_ms = s.position;
                    self.pos_at = Instant::now();
                    self.shuffle = s.shuffle;
                    self.repeat = RepeatMode::from_sdk(s.repeat_mode);
                    if changed && !s.uri.is_empty() {
                        if !self.saved.contains_key(&s.uri) {
                            self.send(Command::CheckSaved(s.uri.clone()));
                        }
                    }
                    let id = |uri: &str, kind: &str| {
                        uri.strip_prefix(&format!("spotify:{kind}:")).map(str::to_string)
                    };
                    self.now = Some(NowPlaying {
                        album_id: id(&s.album_uri, "album"),
                        artist_id: id(&s.artist_uri, "artist"),
                        uri: s.uri,
                        name: s.name,
                        artists: s.artists,
                        album: s.album,
                        cover: s.image,
                        duration_ms: s.duration,
                    });
                    if changed && matches!(self.view, View::NowPlaying) {
                        self.send(Command::LoadQueue);
                        self.load_np_related();
                    }
                }
                SdkMsg::Inactive => {
                    self.local_active = false;
                    self.playing = false;
                }
                SdkMsg::Error { kind, message } => {
                    log::warn!("SDK {kind}: {message}");
                    self.loading_play = None;
                    self.error = Some((format!("Player {kind}: {message}"), Instant::now()));
                }
            }
        }
    }
}

// ---- theme, fonts & drawing helpers ----------------------------------------

/// Use the macOS system font (SF Pro) at two weights when available; egui's built-in
/// fonts stay as fallbacks for symbols. Both weights share one copy of the font bytes.
fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontTweak, VariationCoords};
    let mut fonts = egui::FontDefinitions::default();
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut bold_family = defaults.clone();
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        let blob: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync> = std::sync::Arc::new(bytes);
        let weight = |w: f32| FontTweak { coords: VariationCoords::new([("wght", w)]), ..Default::default() };
        fonts.font_data.insert(
            "sf".into(),
            std::sync::Arc::new(egui::FontData::from_blob(blob.clone(), 0).tweak(weight(440.0))),
        );
        fonts.font_data.insert(
            "sf-bold".into(),
            std::sync::Arc::new(egui::FontData::from_blob(blob, 0).tweak(weight(690.0))),
        );
        if let Some(p) = fonts.families.get_mut(&FontFamily::Proportional) {
            p.insert(0, "sf".into());
        }
        bold_family.insert(0, "sf-bold".into());
    }
    fonts.families.insert(FontFamily::Name("bold".into()), bold_family);
    ctx.set_fonts(fonts);
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn heading(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(bold(size)).color(TEXT)
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = Color32::from_rgb(0x24, 0x15, 0x15);
    v.window_stroke = Stroke::new(1.0, GLASS_STROKE);
    v.extreme_bg_color = Color32::from_rgb(0x1c, 0x12, 0x12);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.widgets.inactive.weak_bg_fill = GLASS;
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x4a, 0x30, 0x2e);
    v.widgets.hovered.weak_bg_fill = GLASS_HOVER;
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x5a, 0x3a, 0x38);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.6);
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, GLASS_STROKE);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.corner_radius = CornerRadius::same(12);
    }
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(14);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(10.0, 8.0);
        s.spacing.button_padding = Vec2::new(14.0, 7.0);
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
    });
}

fn glass(radius: u8, margin: i8) -> Frame {
    Frame::new()
        .fill(GLASS)
        .stroke(Stroke::new(1.0, GLASS_STROKE))
        .corner_radius(radius)
        .inner_margin(Margin::same(margin))
}

fn gradient(rect: Rect, top: Color32, bottom: Color32) -> Shape {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    Shape::mesh(mesh)
}

/// Full-window vertical gradient plus a soft warm glow in the top-left.
fn paint_background(ui: &egui::Ui) {
    let r = ui.max_rect();
    let p = ui.painter();
    p.add(gradient(r, BG_TOP, BG_BOTTOM));
    for (i, radius) in [420.0_f32, 300.0, 180.0].iter().enumerate() {
        let a = 6 + i as u8 * 4;
        p.circle_filled(
            r.left_top() + Vec2::new(180.0, 120.0),
            *radius,
            Color32::from_rgba_premultiplied(a * 2, a / 2, a / 2, a),
        );
    }
}

/// Artwork (or a placeholder) at a fixed size.
fn art(ui: &mut egui::Ui, url: Option<&str>, size: f32, radius: u8) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    paint_art(ui, rect, url, radius);
    resp
}

fn paint_art(ui: &egui::Ui, rect: Rect, url: Option<&str>, radius: u8) {
    match url {
        Some(u) => egui::Image::from_uri(u.to_string())
            .corner_radius(radius)
            .bg_fill(GLASS)
            .paint_at(ui, rect),
        None => {
            ui.painter().rect_filled(rect, radius, Color32::from_rgb(0x3a, 0x1f, 0x1e));
            let s = rect.width().min(rect.height()) * 0.42;
            icons::paint(ui.painter(), Icon::Music, Rect::from_center_size(rect.center(), Vec2::splat(s)), ACCENT);
        }
    }
}

/// Round coral play/pause button; shows a spinner while playback is starting.
fn play_button_at(ui: &mut egui::Ui, rect: Rect, playing: bool, loading: bool) -> egui::Response {
    let size = rect.width();
    let resp = ui.interact(rect, ui.scope_id().with(("play_btn", rect.min.x as i32, rect.min.y as i32)), Sense::click());
    let fill = if resp.hovered() { ACCENT_HOVER } else { ACCENT };
    let p = ui.painter();
    p.circle_filled(rect.center(), size / 2.0 + 5.0, ACCENT.gamma_multiply(0.16));
    p.circle_filled(rect.center(), size / 2.0, fill);
    if loading {
        let t = ui.input(|i| i.time);
        icons::spinner(p, rect.center(), size * 0.24, Color32::WHITE, t);
    } else {
        let icon = if playing { Icon::Pause } else { Icon::Play };
        let s = size * 0.42;
        // Optical centring: a play triangle looks off-centre unless nudged right.
        let nudge = if playing { 0.0 } else { size * 0.03 };
        icons::paint(p, icon, Rect::from_center_size(rect.center() + Vec2::new(nudge, 0.0), Vec2::splat(s)), Color32::WHITE);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Pill-shaped primary button with an icon (e.g. "▶ Play").
fn pill_button(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), bold(15.0), Color32::WHITE);
    let size = Vec2::new(galley.size().x + 64.0, 44.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 22.0, if resp.hovered() { ACCENT_HOVER } else { ACCENT });
    icons::paint(p, icon, Rect::from_center_size(rect.left_center() + Vec2::new(26.0, 0.0), Vec2::splat(16.0)), Color32::WHITE);
    p.galley(rect.left_center() + Vec2::new(42.0, -galley.size().y / 2.0), galley, Color32::WHITE);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Small round glass play button used on rows.
fn mini_play(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let p = ui.painter();
    let fill = if resp.hovered() { ACCENT } else { GLASS_HOVER };
    p.circle_filled(rect.center(), size / 2.0, fill);
    icons::paint(p, Icon::Play, Rect::from_center_size(rect.center() + Vec2::new(1.0, 0.0), Vec2::splat(size * 0.38)), TEXT);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn chip(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let (fill, color) = if selected { (ACCENT, Color32::WHITE) } else { (GLASS, TEXT) };
    ui.add(
        egui::Button::new(RichText::new(label).size(13.5).color(color))
            .fill(fill)
            .stroke(Stroke::new(1.0, if selected { ACCENT } else { GLASS_STROKE }))
            .corner_radius(17)
            .min_size(Vec2::new(0.0, 34.0)),
    )
}

/// Slim slider: 4 px rail, coral fill, knob on hover. Returns true when changed.
fn slim_slider(ui: &mut egui::Ui, value: &mut f32, width: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::click_and_drag());
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() {
            *value = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            changed = true;
        }
    }
    let rail = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 4.0));
    let p = ui.painter();
    p.rect_filled(rail, 2.0, Color32::from_rgb(0x55, 0x40, 0x3e));
    let x = rail.left() + rail.width() * *value;
    p.rect_filled(Rect::from_min_max(rail.left_top(), Pos2::new(x, rail.bottom())), 2.0, ACCENT);
    if resp.hovered() || resp.dragged() {
        p.circle_filled(Pos2::new(x, rail.center().y), 6.0, Color32::WHITE);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    changed
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(18.0);
    ui.label(heading(title, 20.0));
    ui.add_space(6.0);
}

/// rustune logo: little waveform + wordmark.
fn logo(ui: &mut egui::Ui, size: f32) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(size * 1.1, size), Sense::hover());
        let heights = [0.35, 0.7, 1.0, 0.55, 0.85, 0.4];
        let w = rect.width() / (heights.len() as f32 * 1.6);
        for (i, h) in heights.iter().enumerate() {
            let x = rect.left() + w * 0.8 + i as f32 * w * 1.6;
            let bar = Rect::from_center_size(Pos2::new(x, rect.center().y), Vec2::new(w, rect.height() * h));
            ui.painter().rect_filled(bar, w / 2.0, ACCENT);
        }
        ui.label(heading("rustune", size * 0.85));
    });
}

fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Deterministic pseudo-random bar heights per track, so each song has its own "waveform".
fn wave_heights(seed: &str, n: usize) -> Vec<f32> {
    let mut x: u64 = seed
        .bytes()
        .fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let r = (x % 1000) as f32 / 1000.0;
            // Smooth envelope so it reads like music, not noise.
            let env = 0.55 + 0.45 * ((i as f32 / n as f32) * std::f32::consts::PI * 3.0).sin().abs();
            (0.2 + 0.8 * r) * env
        })
        .collect()
}

/// Three little bars marking the current track in lists.
fn eq_icon(ui: &mut egui::Ui, playing: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
    let hs = if playing { [0.55, 1.0, 0.75] } else { [0.4, 0.4, 0.4] };
    for (i, h) in hs.iter().enumerate() {
        let x = rect.left() + 3.0 + i as f32 * 5.0;
        let bar = Rect::from_min_max(Pos2::new(x, rect.bottom() - rect.height() * h), Pos2::new(x + 3.0, rect.bottom()));
        ui.painter().rect_filled(bar, 1.0, ACCENT);
    }
}

// ---- app -------------------------------------------------------------------

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.drain_sdk();
        if self.loading_play.is_some_and(|t| t.elapsed().as_secs() > 20) {
            self.loading_play = None;
        }
        if self.loading_play.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(40)); // spinner
        } else if self.playing {
            // Only tick the clock while music plays; otherwise idle at ~0% CPU.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        paint_background(ui);
        match &self.auth {
            Auth::LoggedIn { .. } => {}
            _ => return self.login_screen(ui),
        }
        let now_playing = matches!(self.view, View::NowPlaying);
        if !now_playing {
            self.player_bar(ui);
        }
        self.sidebar(ui);
        let margin = if now_playing { Margin::ZERO } else { Margin { left: 8, right: 20, top: 16, bottom: 6 } };
        egui::CentralPanel::default()
            .frame(Frame::new().inner_margin(margin))
            .show(ui, |ui| {
                if now_playing {
                    return self.now_playing_view(ui);
                }
                if !self.history.is_empty() && !matches!(self.view, View::Home) {
                    if icons::button(ui, Icon::ChevronLeft, 20.0, MUTED, TEXT).on_hover_text("Back").clicked() {
                        self.back();
                        return;
                    }
                }
                match &self.view {
                    View::Home => self.home_view(ui),
                    View::Search => self.search_view(ui),
                    View::Tracks(_) => self.track_view(ui),
                    View::Song(_) => self.song_view(ui),
                    View::NowPlaying => {}
                }
            });
        self.error_toast(ui);
    }
}

impl App {
    fn login_screen(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(Frame::new()).show(ui, |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space((ui.available_height() * 0.5 - 170.0).max(20.0));
                glass(24, 32).show(ui, |ui| {
                    ui.set_width(360.0);
                    ui.vertical_centered(|ui| {
                        ui.scope(|ui| {
                            ui.set_max_width(190.0);
                            logo(ui, 40.0);
                        });
                        ui.add_space(8.0);
                        ui.label(heading("Discover music made for you", 20.0));
                        ui.label(
                            RichText::new("A lightweight native player for your Spotify Premium")
                                .size(13.0)
                                .color(MUTED),
                        );
                        ui.add_space(26.0);
                        match &self.auth {
                            Auth::Checking => {
                                ui.spinner();
                            }
                            Auth::LoggingIn { browser } => {
                                ui.spinner();
                                ui.label(
                                    RichText::new(if *browser {
                                        "Finish signing in in your browser…"
                                    } else {
                                        "Connecting…"
                                    })
                                    .color(MUTED),
                                );
                            }
                            _ => {
                                if pill_button(ui, Icon::Play, "Sign in with Spotify").clicked() {
                                    self.error = None;
                                    self.send(Command::Login);
                                }
                            }
                        }
                        if let Some((e, _)) = &self.error {
                            ui.add_space(14.0);
                            ui.label(RichText::new(e).size(12.0).color(ACCENT_HOVER));
                        }
                    });
                });
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(250.0)
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(Margin { left: 14, right: 6, top: 14, bottom: 14 }))
            .show(ui, |ui| {
                glass(22, 14).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.add_space(2.0);
                    logo(ui, 26.0);
                    ui.add_space(16.0);

                    let in_home = matches!(self.view, View::Home);
                    let in_search = matches!(self.view, View::Search);
                    let in_liked = matches!(&self.view, View::Tracks(t) if t.key == ViewKey::Liked);
                    if nav_item(ui, Icon::Home, "Home", in_home) {
                        self.nav(View::Home);
                    }
                    if nav_item(ui, Icon::Search, "Search", in_search) {
                        self.nav(View::Search);
                    }
                    if nav_item(ui, Icon::Heart, "Liked Songs", in_liked) {
                        self.history.clear();
                        self.open_liked();
                        self.history.clear();
                    }

                    ui.add_space(16.0);
                    ui.label(RichText::new("YOUR PLAYLISTS").font(bold(11.0)).color(DIM));
                    ui.add_space(4.0);

                    let current = match &self.view {
                        View::Tracks(t) => Some(t.key.clone()),
                        _ => None,
                    };
                    let mut clicked: Option<usize> = None;
                    let bottom_h = 48.0;
                    egui::ScrollArea::vertical()
                        .max_height((ui.available_height() - bottom_h).max(60.0))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, p) in self.playlists.iter().enumerate() {
                                let sel = current == Some(ViewKey::Playlist(p.id.clone()));
                                if playlist_row(ui, p, sel) {
                                    clicked = Some(i);
                                }
                            }
                            if self.playlists.is_empty() {
                                ui.label(RichText::new("Loading…").color(MUTED));
                            }
                        });
                    if let Some(i) = clicked {
                        let p = self.playlists[i].clone();
                        self.history.clear();
                        self.open_playlist(&p);
                        self.history.clear();
                    }

                    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                        ui.horizontal(|ui| {
                            if let Auth::LoggedIn { display_name, .. } = &self.auth {
                                let initial = display_name
                                    .chars()
                                    .next()
                                    .unwrap_or('?')
                                    .to_uppercase()
                                    .to_string();
                                let (r, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                                ui.painter().circle_filled(r.center(), 15.0, ACCENT);
                                ui.painter().text(r.center(), Align2::CENTER_CENTER, initial, bold(14.0), Color32::WHITE);
                                ui.add(egui::Label::new(RichText::new(display_name).color(TEXT)).truncate());
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icons::button(ui, Icon::LogOut, 16.0, DIM, TEXT).on_hover_text("Sign out").clicked() {
                                    self.send(Command::Logout);
                                }
                            });
                        });
                    });
                });
            });
    }

    fn player_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("player")
            .exact_size(112.0)
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(Margin { left: 14, right: 14, top: 4, bottom: 14 }))
            .show(ui, |ui| {
                glass(22, 12).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    let w = ui.available_width();
                    let side = (w * 0.27).max(220.0);
                    ui.horizontal_centered(|ui| {
                        // Left: now playing (click to open the full player)
                        let left = ui
                            .allocate_ui_with_layout(Vec2::new(side, 62.0), Layout::left_to_right(Align::Center), |ui| {
                                art(ui, self.now.as_ref().and_then(|n| n.cover.as_deref()), 58.0, 14);
                                ui.vertical(|ui| {
                                    ui.add_space(10.0);
                                    if let Some(n) = &self.now {
                                        ui.add(egui::Label::new(RichText::new(&n.name).font(bold(15.0))).truncate().selectable(false));
                                        ui.add(egui::Label::new(RichText::new(&n.artists).color(MUTED).size(12.5)).truncate().selectable(false));
                                    } else {
                                        ui.label(RichText::new("Nothing playing").color(MUTED));
                                        ui.label(RichText::new("Pick a song to start").size(12.0).color(DIM));
                                    }
                                });
                            })
                            .response
                            .interact(Sense::click());
                        if self.now.is_some() {
                            if left.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if left.on_hover_text("Open Now Playing").clicked() {
                                self.open_now_playing();
                            }
                        }

                        // Center: transport + waveform
                        let center_w = (w - side * 2.0).max(280.0);
                        ui.allocate_ui_with_layout(Vec2::new(center_w, 72.0), Layout::top_down(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            self.controls(
                                ui,
                                center_w,
                                &[Ctl::Shuffle, Ctl::Prev, Ctl::Play, Ctl::Next, Ctl::Repeat],
                                17.0,
                                38.0,
                                14.0,
                            );
                            let wave_w = (center_w - 100.0).max(120.0);
                            self.progress_row(ui, center_w, wave_w, 18.0);
                        });

                        // Right: devices + volume
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(4.0);
                            self.devices_button(ui);
                            let mut v = self.volume;
                            if slim_slider(ui, &mut v, 96.0) {
                                self.set_volume(v);
                            }
                            let vol_icon = if self.volume <= 0.001 {
                                Icon::VolumeMute
                            } else if self.volume < 0.5 {
                                Icon::VolumeLow
                            } else {
                                Icon::Volume
                            };
                            if icons::button(ui, vol_icon, 17.0, MUTED, TEXT).on_hover_text("Mute").clicked() {
                                if self.volume > 0.001 {
                                    self.volume_before_mute = self.volume;
                                    self.set_volume(0.0);
                                } else {
                                    self.set_volume(self.volume_before_mute.max(0.1));
                                }
                            }
                        });
                    });
                });
            });
    }

    fn devices_button(&mut self, ui: &mut egui::Ui) {
        let devices = icons::button(ui, Icon::Devices, 17.0, MUTED, TEXT).on_hover_text("Play on another device");
        if devices.clicked() {
            self.send(Command::LoadDevices);
        }
        egui::Popup::menu(&devices).show(|ui| {
            ui.set_min_width(240.0);
            ui.label(RichText::new("PLAY ON").font(bold(11.0)).color(DIM));
            ui.add_space(4.0);
            if self.devices.is_empty() {
                ui.label(RichText::new("Looking for devices…").color(MUTED));
            }
            let mut target = None;
            for d in &self.devices {
                let color = if d.is_active { ACCENT } else { TEXT };
                let label = RichText::new(format!("{}  ·  {}", d.name, d.kind.to_lowercase())).color(color);
                if ui.selectable_label(d.is_active, label).clicked() {
                    target = d.id.clone();
                }
            }
            if let Some(id) = target {
                self.send(Command::TransferTo(id));
                ui.close();
            }
        });
    }

    /// A row of transport buttons laid out by hand: every button shares one centre line,
    /// gaps are equal, and the row is centred in `width`.
    fn controls(&mut self, ui: &mut egui::Ui, width: f32, set: &[Ctl], icon: f32, play: f32, gap: f32) {
        let small = icons::button_side(icon);
        let slot = |c: Ctl| if c == Ctl::Play { play } else { small };
        let total = set.iter().map(|&c| slot(c)).sum::<f32>() + gap * (set.len() as f32 - 1.0);
        // Extra height leaves room for the play button's soft halo.
        let (row, _) = ui.allocate_exact_size(Vec2::new(width, play.max(small) + 8.0), Sense::hover());
        let mut x = row.center().x - total / 2.0;
        let toggle = |on: bool| if on { (ACCENT, ACCENT_HOVER) } else { (MUTED, TEXT) };
        for &c in set {
            let s = slot(c);
            let rect = Rect::from_center_size(Pos2::new(x + s / 2.0, row.center().y), Vec2::splat(s));
            x += s + gap;
            match c {
                Ctl::Play => {
                    if play_button_at(ui, rect, self.playing, self.loading_play.is_some()).clicked() {
                        self.toggle_play();
                    }
                }
                Ctl::Shuffle => {
                    let (col, hov) = toggle(self.shuffle);
                    if icons::button_at(ui, rect, Icon::Shuffle, icon, col, hov).on_hover_text("Shuffle").clicked() {
                        self.send(Command::Shuffle(!self.shuffle));
                    }
                }
                Ctl::Repeat => {
                    let (col, hov) = toggle(self.repeat != RepeatMode::Off);
                    let glyph = if self.repeat == RepeatMode::Track { Icon::RepeatOne } else { Icon::Repeat };
                    if icons::button_at(ui, rect, glyph, icon, col, hov).on_hover_text(self.repeat.label()).clicked() {
                        // Optimistic: the SDK confirms with a state update shortly after.
                        self.repeat = self.repeat.next();
                        self.send(Command::Repeat(self.repeat));
                    }
                }
                Ctl::Prev => {
                    if icons::button_at(ui, rect, Icon::SkipPrev, icon, TEXT, Color32::WHITE).on_hover_text("Previous").clicked() {
                        if let Some(sdk) = &self.sdk {
                            sdk.prev();
                        }
                    }
                }
                Ctl::Next => {
                    if icons::button_at(ui, rect, Icon::SkipNext, icon, TEXT, Color32::WHITE).on_hover_text("Next").clicked() {
                        if let Some(sdk) = &self.sdk {
                            sdk.next();
                        }
                    }
                }
                Ctl::Back10 => {
                    if icons::button_at(ui, rect, Icon::Back10, icon + 2.0, TEXT, Color32::WHITE).on_hover_text("Back 10 s").clicked() {
                        self.seek(self.position().saturating_sub(10_000));
                    }
                }
                Ctl::Fwd10 => {
                    if icons::button_at(ui, rect, Icon::Fwd10, icon + 2.0, TEXT, Color32::WHITE).on_hover_text("Forward 10 s").clicked() {
                        let dur = self.now.as_ref().map_or(0, |n| n.duration_ms);
                        self.seek((self.position() + 10_000).min(dur));
                    }
                }
            }
        }
    }

    /// "0:42 ▂▅▇▃… 3:15" centred in `width`.
    fn progress_row(&mut self, ui: &mut egui::Ui, width: f32, wave_w: f32, wave_h: f32) {
        let dur = self.now.as_ref().map_or(0, |n| n.duration_ms);
        let pos = self.position();
        ui.horizontal(|ui| {
            let total = wave_w + 2.0 * 40.0 + ui.spacing().item_spacing.x * 2.0;
            ui.add_space(((width - total) / 2.0).max(0.0));
            ui.add_sized([40.0, wave_h], egui::Label::new(RichText::new(fmt_ms(pos)).size(11.5).color(MUTED)));
            let frac = self
                .seek_drag
                .unwrap_or(if dur > 0 { pos as f32 / dur as f32 } else { 0.0 });
            if let Some(new) = self.waveform(ui, wave_w, wave_h, frac) {
                if dur > 0 {
                    self.seek((new * dur as f32) as u32);
                }
            }
            ui.add_sized([40.0, wave_h], egui::Label::new(RichText::new(fmt_ms(dur)).size(11.5).color(MUTED)));
        });
    }

    /// Waveform-style seek bar. Returns the new position (0..1) when the user releases it.
    fn waveform(&mut self, ui: &mut egui::Ui, width: f32, height: f32, frac: f32) -> Option<f32> {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
        let seed = self.now.as_ref().map_or("rustune", |n| n.uri.as_str());
        let n = ((width / 4.5) as usize).max(1);
        let heights = wave_heights(seed, n);
        let step = width / n as f32;
        let hover = resp.hover_pos().map(|p| ((p.x - rect.left()) / width).clamp(0.0, 1.0));
        let p = ui.painter();
        for (i, h) in heights.iter().enumerate() {
            let x = rect.left() + step * (i as f32 + 0.5);
            let t = i as f32 / n as f32;
            let color = if t <= frac {
                ACCENT
            } else if hover.is_some_and(|hv| t <= hv) {
                Color32::from_rgb(0x8a, 0x5a, 0x56)
            } else {
                Color32::from_rgb(0x55, 0x40, 0x3e)
            };
            let bar = Rect::from_center_size(Pos2::new(x, rect.center().y), Vec2::new(step * 0.5, rect.height() * h));
            p.rect_filled(bar, step * 0.25, color);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let at = resp.interact_pointer_pos().map(|p| ((p.x - rect.left()) / width).clamp(0.0, 1.0));
        if resp.dragged() {
            self.seek_drag = at;
        }
        if resp.drag_stopped() || resp.clicked() {
            self.seek_drag = None;
            return at;
        }
        None
    }

    // ---- Home ----

    fn home_view(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(heading(format!("Hi, {}", self.first_name()), 28.0));
                    ui.label(RichText::new("What do you want to hear today?").size(14.0).color(MUTED));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if search_pill(ui, 260.0).clicked() {
                        self.go(View::Search);
                    }
                });
            });
            ui.add_space(16.0);

            self.hero(ui);

            ui.add_space(10.0);
            let mut mood = None;
            ui.horizontal_wrapped(|ui| {
                for m in MOODS {
                    if chip(ui, m, false).clicked() {
                        mood = Some(*m);
                    }
                }
            });
            if let Some(m) = mood {
                self.run_search(m);
            }

            let home = std::mem::take(&mut self.home);
            if !home.recent.is_empty() {
                section_title(ui, "Jump back in");
                if let Some((i, act)) = card_row(ui, "recent", &home.recent) {
                    self.act_on_list(&home.recent, i, act);
                }
            }
            if !home.top_tracks.is_empty() {
                section_title(ui, "Your top tracks");
                let now = self.now.as_ref().map(|n| n.uri.clone());
                let half = home.top_tracks.len().div_ceil(2);
                let mut hit = None;
                ui.columns(2, |cols| {
                    for (c, col) in cols.iter_mut().enumerate() {
                        for i in (c * half)..((c + 1) * half).min(home.top_tracks.len()) {
                            let t = &home.top_tracks[i];
                            let act = song_row(col, t, None, now.as_deref() == Some(t.uri.as_str()), self.playing, Some(i + 1));
                            if act != RowAct::None {
                                hit = Some((i, act));
                            }
                        }
                    }
                });
                if let Some((i, act)) = hit {
                    self.act_on_list(&home.top_tracks, i, act);
                }
            }
            if let Some((artist, tracks)) = home.because.as_ref().filter(|(_, t)| !t.is_empty()) {
                section_title(ui, &format!("Because you like {artist}"));
                if let Some((i, act)) = card_row(ui, "because", tracks) {
                    self.act_on_list(tracks, i, act);
                }
            }
            for (k, (artist, tracks)) in home.more_from.iter().enumerate() {
                section_title(ui, &format!("More from {artist}"));
                if let Some((i, act)) = card_row(ui, &format!("more_from_{k}"), tracks) {
                    self.act_on_list(tracks, i, act);
                }
            }
            if !home.albums.is_empty() {
                section_title(ui, "Albums for you");
                match album_row(ui, "home_albums", &home.albums) {
                    Some((i, RowAct::Play)) => self.play_context(home.albums[i].uri.clone()),
                    Some((i, _)) => self.open_album(&home.albums[i]),
                    None => {}
                }
            }
            if !home.top_artists.is_empty() {
                section_title(ui, "Your top artists");
                let mut clicked = None;
                egui::ScrollArea::horizontal().id_salt("top_artists").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for a in &home.top_artists {
                            if artist_bubble(ui, a.image.as_deref(), &a.name) {
                                clicked = Some(a.name.clone());
                            }
                        }
                    });
                });
                if let Some(name) = clicked {
                    self.run_search(&name);
                }
            }
            if home.recent.is_empty() && home.top_tracks.is_empty() && home.top_artists.is_empty() {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Finding suggestions for you…").color(MUTED));
                });
            }
            self.home = home;

            if !self.playlists.is_empty() {
                section_title(ui, "Your playlists");
                let mut open = None;
                let mut play = None;
                ui.horizontal_wrapped(|ui| {
                    for (i, p) in self.playlists.iter().take(12).enumerate() {
                        match card(ui, p.image.as_deref(), &p.name, &p.owner, 150.0) {
                            RowAct::Open => open = Some(i),
                            RowAct::Play => play = Some(i),
                            RowAct::None => {}
                        }
                    }
                });
                if let Some(i) = open {
                    let p = self.playlists[i].clone();
                    self.open_playlist(&p);
                }
                if let Some(i) = play {
                    self.play_context(self.playlists[i].uri.clone());
                }
            }

            if !self.liked_preview.is_empty() {
                section_title(ui, "Recently liked");
                let now = self.now.as_ref().map(|n| n.uri.clone());
                let mut hit = None;
                for (i, t) in self.liked_preview.iter().enumerate() {
                    let act = song_row(ui, t, None, now.as_deref() == Some(t.uri.as_str()), self.playing, None);
                    if act != RowAct::None {
                        hit = Some((i, act));
                    }
                }
                if let Some((i, act)) = hit {
                    let t = self.liked_preview[i].clone();
                    self.play_track(&t, PlayFrom::Context(self.liked_uri()));
                    if act == RowAct::Open {
                        self.open_song(&t);
                    }
                }
            }
            ui.add_space(16.0);
        });
    }

    /// Carousel of playlist covers: big featured card in the middle, smaller ones fanned out.
    fn hero(&mut self, ui: &mut egui::Ui) {
        let featured: Vec<usize> = self
            .playlists
            .iter()
            .enumerate()
            .filter(|(_, p)| p.image.is_some())
            .map(|(i, _)| i)
            .take(9)
            .collect();
        if featured.is_empty() {
            return;
        }
        let n = featured.len() as i32;
        self.hero_index %= featured.len();

        let width = ui.available_width();
        let (area, _) = ui.allocate_exact_size(Vec2::new(width, 300.0), Sense::hover());
        let (cx, cy) = (area.center().x, area.center().y);
        // Paint back-to-front: outer pair, inner pair, then the featured card.
        let slots: [(i32, f32, Vec2, u8); 5] = [
            (-2, -300.0, Vec2::new(150.0, 200.0), 110),
            (2, 300.0, Vec2::new(150.0, 200.0), 110),
            (-1, -175.0, Vec2::new(190.0, 250.0), 170),
            (1, 175.0, Vec2::new(190.0, 250.0), 170),
            (0, 0.0, Vec2::new(250.0, 300.0), 255),
        ];
        enum HeroClick {
            Play(usize),
            Open(usize),
            Rotate(i32),
        }
        let mut clicked = None;
        for (slot, dx, size, tint) in slots {
            if slot.abs() >= n.max(1) && slot != 0 {
                continue;
            }
            let rect = Rect::from_center_size(Pos2::new(cx + dx, cy), size);
            if slot != 0 && !area.contains_rect(rect) {
                continue;
            }
            let idx = featured[(self.hero_index as i32 + slot).rem_euclid(n) as usize];
            let p = &self.playlists[idx];
            egui::Image::from_uri(p.image.clone().unwrap_or_default())
                .corner_radius(22)
                .tint(Color32::from_gray(tint))
                .paint_at(ui, rect);
            let resp = ui.interact(rect, ui.scope_id().with(("hero", slot)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if slot == 0 {
                let painter = ui.painter_at(rect);
                let fade = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + rect.height() * 0.45), rect.right_bottom());
                painter.add(gradient(fade, Color32::TRANSPARENT, Color32::from_rgba_premultiplied(0, 0, 0, 230)));
                painter.text(rect.left_bottom() + Vec2::new(18.0, -46.0), Align2::LEFT_BOTTOM, &p.name, bold(20.0), Color32::WHITE);
                let owner = if p.owner.is_empty() { "Playlist".to_string() } else { format!("by {}", p.owner) };
                painter.text(rect.left_bottom() + Vec2::new(18.0, -24.0), Align2::LEFT_BOTTOM, owner, FontId::proportional(13.0), MUTED);
                let btn_c = rect.right_bottom() + Vec2::new(-38.0, -40.0);
                painter.circle_filled(btn_c, 21.0, ACCENT);
                icons::paint(&painter, Icon::Play, Rect::from_center_size(btn_c + Vec2::new(1.5, 0.0), Vec2::splat(16.0)), Color32::WHITE);
                if resp.clicked() {
                    let on_btn = resp.interact_pointer_pos().is_some_and(|pp| pp.distance(btn_c) < 24.0);
                    clicked = Some(if on_btn { HeroClick::Play(idx) } else { HeroClick::Open(idx) });
                }
            } else if resp.clicked() {
                clicked = Some(HeroClick::Rotate(slot));
            }
        }
        for (dir, icon, x) in [(-1i32, Icon::ChevronLeft, area.left() + 20.0), (1, Icon::ChevronRight, area.right() - 20.0)] {
            let r = Rect::from_center_size(Pos2::new(x, cy), Vec2::splat(36.0));
            let resp = ui.interact(r, ui.scope_id().with(("hero_arrow", dir)), Sense::click());
            let p = ui.painter();
            let fill = if resp.hovered() { Color32::from_rgba_premultiplied(40, 30, 30, 200) } else { Color32::from_rgba_premultiplied(0, 0, 0, 140) };
            p.circle_filled(r.center(), 18.0, fill);
            icons::paint(p, icon, Rect::from_center_size(r.center(), Vec2::splat(16.0)), TEXT);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                clicked = Some(HeroClick::Rotate(dir));
            }
        }
        match clicked {
            Some(HeroClick::Play(idx)) => self.play_context(self.playlists[idx].uri.clone()),
            Some(HeroClick::Open(idx)) => {
                let p = self.playlists[idx].clone();
                self.open_playlist(&p);
            }
            Some(HeroClick::Rotate(d)) => self.hero_index = (self.hero_index as i32 + d).rem_euclid(n) as usize,
            None => {}
        }
    }

    // ---- Search ----

    fn search_view(&mut self, ui: &mut egui::Ui) {
        ui.label(heading("Search", 28.0));
        ui.add_space(10.0);
        glass(26, 0).inner_margin(Margin::symmetric(18, 12)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                icons::paint(ui.painter(), Icon::Search, r, MUTED);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("What do you want to listen to?")
                        .frame(Frame::NONE)
                        .desired_width(ui.available_width() - 40.0)
                        .font(FontId::proportional(16.0)),
                );
                if self.search.is_none() && self.search_query.is_empty() {
                    resp.request_focus();
                }
                if resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && !self.search_query.trim().is_empty()
                {
                    let q = self.search_query.trim().to_string();
                    self.run_search(&q);
                }
                if self.searching {
                    ui.spinner();
                }
            });
        });
        ui.add_space(12.0);
        let mut mood = None;
        ui.horizontal_wrapped(|ui| {
            for m in MOODS {
                if chip(ui, m, self.search_query.eq_ignore_ascii_case(m)).clicked() {
                    mood = Some(*m);
                }
            }
        });
        if let Some(m) = mood {
            self.run_search(m);
        }

        let Some(res) = self.search.take() else {
            ui.add_space(30.0);
            ui.label(RichText::new("Search for songs, albums, artists or playlists.").color(MUTED));
            return;
        };

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if !res.tracks.is_empty() {
                section_title(ui, "Songs");
                let now = self.now.as_ref().map(|n| n.uri.clone());
                let mut hit = None;
                for (i, t) in res.tracks.iter().enumerate() {
                    let act = song_row(ui, t, None, now.as_deref() == Some(t.uri.as_str()), self.playing, None);
                    if act != RowAct::None {
                        hit = Some((i, act));
                    }
                }
                if let Some((i, act)) = hit {
                    self.act_on_list(&res.tracks, i, act);
                }
            }
            if !res.artists.is_empty() {
                section_title(ui, "Artists");
                let mut clicked = None;
                egui::ScrollArea::horizontal().id_salt("search_artists").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for a in &res.artists {
                            if artist_bubble(ui, a.image.as_deref(), &a.name) {
                                clicked = Some(a.uri.clone());
                            }
                        }
                    });
                });
                if let Some(uri) = clicked {
                    self.play_context(uri);
                }
            }
            if !res.albums.is_empty() {
                section_title(ui, "Albums");
                ui.horizontal_wrapped(|ui| {
                    for a in &res.albums {
                        match card(ui, a.image.as_deref(), &a.name, &a.artists, 150.0) {
                            RowAct::Open => self.open_tracks(TrackView {
                                key: ViewKey::Album(a.id.clone()),
                                title: a.name.clone(),
                                subtitle: a.artists.clone(),
                                image: a.image.clone(),
                                context_uri: a.uri.clone(),
                                album_name: a.name.clone(),
                                tracks: Vec::new(),
                                next_offset: None,
                                loading: true,
                            }),
                            RowAct::Play => self.play_context(a.uri.clone()),
                            RowAct::None => {}
                        }
                    }
                });
            }
            if !res.playlists.is_empty() {
                section_title(ui, "Playlists");
                ui.horizontal_wrapped(|ui| {
                    for p in &res.playlists {
                        match card(ui, p.image.as_deref(), &p.name, &p.owner, 150.0) {
                            RowAct::Open => self.open_playlist(p),
                            RowAct::Play => self.play_context(p.uri.clone()),
                            RowAct::None => {}
                        }
                    }
                });
            }
            ui.add_space(16.0);
        });
        // The view may have changed (opening a song/album pushes Search onto history).
        if self.search.is_none() {
            self.search = Some(res);
        }
    }

    // ---- Song page ----

    fn song_view(&mut self, ui: &mut egui::Ui) {
        let View::Song(sv) = &self.view else { return };
        let t = sv.track.clone();
        let album = sv.album.clone();
        let by_artist = sv.by_artist.clone();
        let sv_albums = sv.albums.clone();
        let similar = sv.similar.clone();
        let artist = t.artists.split(", ").next().unwrap_or_default().to_string();
        let current = self.now.as_ref().is_some_and(|n| n.uri == t.uri);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                art(ui, t.cover.as_deref().or(t.image.as_deref()), 200.0, 22);
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(26.0);
                    ui.label(RichText::new("SONG").font(bold(11.0)).color(MUTED));
                    ui.label(heading(&t.name, 34.0));
                    ui.label(
                        RichText::new(format!("{}  ·  {}  ·  {}", t.artists, t.album, fmt_ms(t.duration_ms)))
                            .size(14.0)
                            .color(MUTED),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        let (icon, label) = if current && self.playing { (Icon::Pause, "Pause") } else { (Icon::Play, "Play") };
                        if pill_button(ui, icon, label).clicked() {
                            if current {
                                self.toggle_play();
                            } else {
                                match &t.album_id {
                                    Some(id) => self.play_track(&t, PlayFrom::Context(format!("spotify:album:{id}"))),
                                    None => self.play_track(&t, PlayFrom::List(vec![t.uri.clone()], 0)),
                                }
                            }
                        }
                        let saved = self.is_saved(&t.uri);
                        let (icon, c) = if saved { (Icon::HeartFilled, ACCENT) } else { (Icon::Heart, MUTED) };
                        if icons::button(ui, icon, 22.0, c, if saved { ACCENT_HOVER } else { TEXT })
                            .on_hover_text(if saved { "Remove from Liked Songs" } else { "Save to Liked Songs" })
                            .clicked()
                        {
                            self.toggle_saved(&t.uri);
                        }
                    });
                });
            });

            let now = self.now.as_ref().map(|n| n.uri.clone());
            // From the album
            section_title(ui, &format!("From {}", t.album));
            match &album {
                None => {
                    ui.spinner();
                }
                Some(list) if list.is_empty() => {
                    ui.label(RichText::new("Album tracks unavailable.").color(MUTED));
                }
                Some(list) => {
                    let mut hit = None;
                    for (i, row) in list.iter().enumerate() {
                        let act = song_row(ui, row, t.cover.as_deref(), now.as_deref() == Some(row.uri.as_str()), self.playing, Some(i + 1));
                        if act != RowAct::None {
                            hit = Some((i, act));
                        }
                    }
                    if let Some((i, act)) = hit {
                        let mut row = list[i].clone();
                        row.cover = row.cover.or_else(|| t.cover.clone());
                        row.image = row.image.or_else(|| t.image.clone());
                        row.artist_id = row.artist_id.or_else(|| t.artist_id.clone());
                        let ctx = match &t.album_id {
                            Some(id) => PlayFrom::Context(format!("spotify:album:{id}")),
                            None => PlayFrom::List(list.iter().map(|x| x.uri.clone()).collect(), i as u32),
                        };
                        self.play_track(&row, ctx);
                        if act == RowAct::Open {
                            self.open_song(&row);
                        }
                    }
                }
            }

            // More by the artist
            section_title(ui, &format!("More by {artist}"));
            match &by_artist {
                None => {
                    ui.spinner();
                }
                Some(list) if list.is_empty() => {
                    ui.label(RichText::new("Nothing else found.").color(MUTED));
                }
                Some(list) => {
                    if let Some((i, act)) = card_row(ui, "by_artist", list) {
                        self.act_on_list(list, i, act);
                    }
                }
            }

            // Albums by the artist
            if let Some(list) = sv_albums.as_ref().filter(|l| !l.is_empty()) {
                section_title(ui, &format!("Albums by {artist}"));
                match album_row(ui, "song_albums", list) {
                    Some((i, RowAct::Play)) => self.play_context(list[i].uri.clone()),
                    Some((i, _)) => self.open_album(&list[i]),
                    None => {}
                }
            }

            // Similar (same genre)
            match &similar {
                None => {
                    section_title(ui, "You might also like");
                    ui.spinner();
                }
                Some((genre, list)) if !list.is_empty() => {
                    section_title(ui, "You might also like");
                    ui.label(RichText::new(format!("More {genre}")).size(13.0).color(MUTED));
                    ui.add_space(4.0);
                    if let Some((i, act)) = card_row(ui, "similar", list) {
                        self.act_on_list(list, i, act);
                    }
                }
                Some(_) => {}
            }
            ui.add_space(16.0);
        });
    }

    // ---- Now Playing ----

    fn now_playing_view(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let Some(np) = self.now.clone() else {
            self.back();
            return;
        };

        // Backdrop: the cover stretched over everything reads as a soft blur; a warm
        // gradient on top keeps text legible.
        if let Some(url) = &np.cover {
            let side = full.width().max(full.height()) * 1.15;
            egui::Image::from_uri(url.clone())
                .tint(Color32::from_white_alpha(70))
                .paint_at(ui, Rect::from_center_size(full.center(), Vec2::splat(side)));
        }
        ui.painter().add(gradient(
            full,
            Color32::from_rgba_premultiplied(30, 12, 12, 170),
            Color32::from_rgba_premultiplied(9, 6, 7, 240),
        ));

        let queue_w = if full.width() > 1050.0 { 320.0 } else { 0.0 };
        let main = Rect::from_min_max(full.min, Pos2::new(full.right() - queue_w, full.bottom()));

        // Top bar
        let top = Rect::from_min_size(main.min + Vec2::new(18.0, 14.0), Vec2::new(main.width() - 36.0, 44.0));
        let mut top_ui = ui.new_child(egui::UiBuilder::new().max_rect(top).layout(Layout::left_to_right(Align::Center)));
        if icons::button(&mut top_ui, Icon::ChevronDown, 22.0, TEXT, Color32::WHITE).on_hover_text("Close").clicked() {
            self.back();
            return;
        }
        ui.painter().text(top.center(), Align2::CENTER_CENTER, "Now Playing", bold(16.0), TEXT);

        // Circular cover with progress ring
        let content_top = top.bottom() + 10.0;
        let avail_h = main.bottom() - content_top;
        let d = (main.width() * 0.42).min(avail_h * 0.44).clamp(160.0, 340.0);
        let center = Pos2::new(main.center().x, content_top + d / 2.0 + 34.0);
        let p = ui.painter();
        let ring_r = d / 2.0 + 18.0;
        for i in 0..72 {
            let a = i as f32 / 72.0 * std::f32::consts::TAU;
            let dir = Vec2::new(a.cos(), a.sin());
            p.line_segment(
                [center + dir * (ring_r + 9.0), center + dir * (ring_r + 13.0)],
                Stroke::new(1.5, Color32::from_rgba_premultiplied(40, 30, 30, 60)),
            );
        }
        p.circle_stroke(center, ring_r, Stroke::new(3.0, Color32::from_rgba_premultiplied(60, 40, 40, 90)));
        let dur = np.duration_ms.max(1);
        let frac = (self.position() as f32 / dur as f32).clamp(0.0, 1.0);
        if frac > 0.0 {
            let n = (frac * 120.0).ceil().max(2.0) as usize;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + frac * std::f32::consts::TAU * i as f32 / n as f32;
                    center + Vec2::new(a.cos(), a.sin()) * ring_r
                })
                .collect();
            let knob = *pts.last().unwrap();
            p.add(Shape::line(pts, Stroke::new(3.5, ACCENT)));
            p.circle_filled(knob, 7.0, Color32::WHITE);
            p.circle_filled(knob, 4.0, ACCENT);
        }
        p.circle_filled(center, d / 2.0 + 4.0, Color32::from_rgba_premultiplied(0, 0, 0, 90));
        paint_art(ui, Rect::from_center_size(center, Vec2::splat(d)), np.cover.as_deref(), (d / 2.0).min(255.0) as u8);

        // Volume -/+ either side of the cover
        for (icon, x, delta) in [(Icon::VolumeLow, center.x - ring_r - 52.0, -0.1f32), (Icon::Volume, center.x + ring_r + 52.0, 0.1)] {
            let r = Rect::from_center_size(Pos2::new(x, center.y), Vec2::splat(44.0));
            let mut b = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(Layout::left_to_right(Align::Center)));
            if icons::button(&mut b, icon, 20.0, MUTED, TEXT)
                .on_hover_text(format!("Volume {}%", (self.volume * 100.0).round()))
                .clicked()
            {
                self.set_volume(self.volume + delta);
            }
        }

        // Glass card: title, heart, waveform, controls
        let card_w = (main.width() - 80.0).min(620.0);
        let card_rect = Rect::from_min_size(
            Pos2::new(main.center().x - card_w / 2.0, center.y + ring_r + 34.0),
            Vec2::new(card_w, 196.0),
        );
        let mut card_ui = ui.new_child(egui::UiBuilder::new().max_rect(card_rect).layout(Layout::top_down(Align::Min)));
        glass(26, 18).show(&mut card_ui, |ui| {
            ui.set_width(card_w - 36.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(format!("By {}  ·  {}", np.artists, np.album)).size(13.0).color(MUTED)).truncate());
                    ui.add(egui::Label::new(heading(&np.name, 22.0)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let saved = self.is_saved(&np.uri);
                    let (icon, c) = if saved { (Icon::HeartFilled, ACCENT) } else { (Icon::Heart, MUTED) };
                    if icons::button(ui, icon, 22.0, c, if saved { ACCENT_HOVER } else { TEXT }).clicked() {
                        self.toggle_saved(&np.uri);
                    }
                });
            });
            ui.add_space(8.0);
            let w = ui.available_width();
            self.progress_row(ui, w, w - 104.0, 34.0);
            ui.add_space(6.0);
            self.controls(
                ui,
                w,
                &[Ctl::Shuffle, Ctl::Prev, Ctl::Back10, Ctl::Play, Ctl::Fwd10, Ctl::Next, Ctl::Repeat],
                20.0,
                60.0,
                18.0,
            );
        });

        // Side panel: Up next / From the album / More by the artist
        if queue_w > 0.0 {
            let q = Rect::from_min_max(
                Pos2::new(full.right() - queue_w + 4.0, full.top() + 16.0),
                full.right_bottom() - Vec2::new(16.0, 16.0),
            );
            let mut q_ui = ui.new_child(egui::UiBuilder::new().max_rect(q).layout(Layout::top_down(Align::Min)));
            glass(22, 14).show(&mut q_ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.horizontal(|ui| {
                    for (tab, label) in [(NpTab::Queue, "Up next"), (NpTab::Album, "Album"), (NpTab::Artist, "Artist")] {
                        if chip(ui, label, self.np_tab == tab).clicked() {
                            self.np_tab = tab;
                        }
                    }
                });
                ui.add_space(10.0);
                let related = self.np_related.as_ref();
                let (list, empty): (Option<Vec<TrackRow>>, &str) = match self.np_tab {
                    NpTab::Queue => (Some(self.queue.clone()), "Nothing queued."),
                    NpTab::Album => (related.and_then(|r| r.album.clone()), "No album tracks."),
                    NpTab::Artist => (related.and_then(|r| r.by_artist.clone()), "Nothing else by this artist."),
                };
                let Some(list) = list else {
                    ui.spinner();
                    return;
                };
                if list.is_empty() {
                    ui.label(RichText::new(empty).color(MUTED));
                    return;
                }
                let mut clicked = None;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (i, t) in list.iter().enumerate() {
                        let current = t.uri == np.uri;
                        if compact_row(ui, t, np.cover.as_deref(), current, self.playing) {
                            clicked = Some(i);
                        }
                    }
                });
                if let Some(i) = clicked {
                    let mut t = list[i].clone();
                    match self.np_tab {
                        // The queue can't be jumped into via the API; play from that track on.
                        NpTab::Queue => {
                            let uris = list[i..].iter().map(|x| x.uri.clone()).collect();
                            self.play_track(&t, PlayFrom::List(uris, 0));
                        }
                        NpTab::Album => {
                            t.cover = t.cover.or_else(|| np.cover.clone());
                            t.artist_id = t.artist_id.or_else(|| np.artist_id.clone());
                            match &np.album_id {
                                Some(id) => self.play_track(&t, PlayFrom::Context(format!("spotify:album:{id}"))),
                                None => self.play_track(&t, PlayFrom::List(vec![t.uri.clone()], 0)),
                            }
                        }
                        NpTab::Artist => {
                            let uris = list.iter().map(|x| x.uri.clone()).collect();
                            self.play_track(&t, PlayFrom::List(uris, i as u32));
                        }
                    }
                }
            });
        }
    }

    // ---- Playlist / album / liked ----

    fn track_view(&mut self, ui: &mut egui::Ui) {
        let View::Tracks(tv) = &self.view else { return };
        let (title, subtitle, image, ctx_uri) =
            (tv.title.clone(), tv.subtitle.clone(), tv.image.clone(), tv.context_uri.clone());
        let kind = match tv.key {
            ViewKey::Liked => "COLLECTION",
            ViewKey::Playlist(_) => "PLAYLIST",
            ViewKey::Album(_) => "ALBUM",
        };
        let tracks = tv.tracks.clone();
        let (loading, next_offset, key, album_name) =
            (tv.loading, tv.next_offset, tv.key.clone(), tv.album_name.clone());

        ui.horizontal(|ui| {
            if image.is_some() {
                art(ui, image.as_deref(), 160.0, 22);
            } else {
                // Liked Songs: coral heart tile
                let (r, _) = ui.allocate_exact_size(Vec2::splat(160.0), Sense::hover());
                ui.painter().add(gradient(r, ACCENT_HOVER, Color32::from_rgb(0xb8, 0x2e, 0x2a)));
                icons::paint(ui.painter(), Icon::HeartFilled, Rect::from_center_size(r.center(), Vec2::splat(64.0)), Color32::WHITE);
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(22.0);
                ui.label(RichText::new(kind).font(bold(11.0)).color(MUTED));
                ui.label(heading(&title, 34.0));
                let count = if tracks.is_empty() {
                    String::new()
                } else {
                    format!("{} songs{}", tracks.len(), if next_offset.is_some() { "+" } else { "" })
                };
                let meta: Vec<&str> = [subtitle.as_str(), count.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
                ui.label(RichText::new(meta.join("  ·  ")).size(14.0).color(MUTED));
                ui.add_space(12.0);
                if pill_button(ui, Icon::Play, "Play").clicked() {
                    self.play_context(ctx_uri.clone());
                }
            });
        });
        ui.add_space(16.0);

        if loading && tracks.is_empty() {
            ui.spinner();
            return;
        }
        if let Some((i, open)) = self.track_table(ui, &tracks, image.as_deref(), next_offset.is_some()) {
            if i == tracks.len() {
                if let View::Tracks(tv) = &mut self.view {
                    tv.loading = true;
                }
                self.send(Command::LoadTracks { view: key, offset: next_offset.unwrap_or(0), album_name });
            } else {
                let mut row = tracks[i].clone();
                row.cover = row.cover.or_else(|| image.clone());
                self.play_track(&row, PlayFrom::Context(ctx_uri));
                if open {
                    self.open_song(&row);
                }
            }
        }
    }

    /// Virtualised track list (only visible rows are laid out). Returns (row, open_song):
    /// double-click plays, clicking the title opens the song page too.
    /// Row `tracks.len()` is the "load more" row.
    fn track_table(&self, ui: &mut egui::Ui, tracks: &[TrackRow], fallback: Option<&str>, more: bool) -> Option<(usize, bool)> {
        let now_uri = self.now.as_ref().map(|n| n.uri.as_str());
        let mut hit = None;
        let rows = tracks.len() + more as usize;
        TableBuilder::new(ui)
            .id_salt("tracks")
            .striped(false)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::exact(36.0))
            .column(Column::exact(54.0))
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::initial(240.0).at_least(80.0).clip(true).resizable(true))
            .column(Column::exact(56.0))
            .header(26.0, |mut h| {
                for t in ["#", "", "TITLE", "ALBUM", "TIME"] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).font(bold(11.0)).color(DIM));
                    });
                }
            })
            .body(|body| {
                body.rows(60.0, rows, |mut row| {
                    let i = row.index();
                    if i == tracks.len() {
                        for c in 0..5 {
                            row.col(|ui| {
                                if c == 2 {
                                    ui.label(RichText::new("Load more").font(bold(14.0)).color(ACCENT));
                                }
                            });
                        }
                        if row.response().clicked() {
                            hit = Some((i, false));
                        }
                        return;
                    }
                    let t = &tracks[i];
                    let current = now_uri == Some(t.uri.as_str());
                    row.col(|ui| {
                        if current {
                            eq_icon(ui, self.playing);
                        } else {
                            ui.label(RichText::new((i + 1).to_string()).color(DIM));
                        }
                    });
                    row.col(|ui| {
                        art(ui, t.image.as_deref().or(fallback), 44.0, 10);
                    });
                    let mut title_clicked = false;
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            let color = if current { ACCENT } else { TEXT };
                            let r = ui.add(
                                egui::Label::new(RichText::new(&t.name).size(14.5).color(color))
                                    .truncate()
                                    .selectable(false)
                                    .sense(Sense::click()),
                            );
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            title_clicked = r.clicked();
                            ui.add(egui::Label::new(RichText::new(&t.artists).size(12.5).color(MUTED)).truncate().selectable(false));
                        });
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(&t.album).color(MUTED)).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(fmt_ms(t.duration_ms)).color(MUTED));
                    });
                    if title_clicked {
                        hit = Some((i, true));
                    } else if row.response().double_clicked() {
                        hit = Some((i, false));
                    }
                });
            });
        hit
    }

    fn error_toast(&mut self, ui: &mut egui::Ui) {
        let Some((msg, at)) = &self.error else { return };
        if at.elapsed().as_secs() > 6 {
            self.error = None;
            return;
        }
        let msg = msg.clone();
        egui::Area::new(egui::Id::unique("error_toast"))
            .anchor(Align2::RIGHT_TOP, [-20.0, 20.0])
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(Color32::from_rgb(0x4a, 0x1c, 0x1a))
                    .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.5)))
                    .corner_radius(16)
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.label(RichText::new(msg).size(13.5));
                    });
            });
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
}

// ---- reusable rows/cards ----------------------------------------------------

fn search_pill(ui: &mut egui::Ui, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 21.0, if resp.hovered() { GLASS_HOVER } else { GLASS });
    p.rect_stroke(rect, 21.0, Stroke::new(1.0, GLASS_STROKE), egui::StrokeKind::Inside);
    icons::paint(p, Icon::Search, Rect::from_center_size(rect.left_center() + Vec2::new(22.0, 0.0), Vec2::splat(16.0)), MUTED);
    p.text(rect.left_center() + Vec2::new(40.0, 0.0), Align2::LEFT_CENTER, "Search songs, artists…", FontId::proportional(14.0), MUTED);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn nav_item(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 12.0, GLASS_HOVER);
        p.rect_filled(Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 11.0), Vec2::new(3.0, 18.0)), 2.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, 12.0, GLASS);
    }
    let c = if selected { ACCENT } else if resp.hovered() { TEXT } else { MUTED };
    icons::paint(p, icon, Rect::from_center_size(rect.left_center() + Vec2::new(22.0, 0.0), Vec2::splat(18.0)), c);
    let font = if selected { bold(14.5) } else { FontId::proportional(14.5) };
    p.text(rect.left_center() + Vec2::new(44.0, 0.0), Align2::LEFT_CENTER, label, font, if selected || resp.hovered() { TEXT } else { MUTED });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// Square cover card with title/subtitle; a play button appears on hover.
fn card(ui: &mut egui::Ui, image: Option<&str>, title: &str, subtitle: &str, size: f32) -> RowAct {
    let mut action = RowAct::None;
    let resp = glass(18, 10)
        .show(ui, |ui| {
            ui.set_width(size);
            ui.vertical(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
                paint_art(ui, rect, image, 14);
                if ui.rect_contains_pointer(rect.expand(10.0)) {
                    let c = rect.right_bottom() + Vec2::new(-26.0, -26.0);
                    let r = Rect::from_center_size(c, Vec2::splat(40.0));
                    let btn = ui.interact(r, ui.scope_id().with(("card_play", title, subtitle)), Sense::click());
                    let p = ui.painter();
                    p.circle_filled(c, 20.0, if btn.hovered() { ACCENT_HOVER } else { ACCENT });
                    icons::paint(p, Icon::Play, Rect::from_center_size(c + Vec2::new(1.5, 0.0), Vec2::splat(15.0)), Color32::WHITE);
                    if btn.clicked() {
                        action = RowAct::Play;
                    }
                }
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(title).font(bold(14.0))).truncate().selectable(false));
                ui.add(egui::Label::new(RichText::new(subtitle).size(12.5).color(MUTED)).truncate().selectable(false));
            });
        })
        .response
        .interact(Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() && action == RowAct::None {
        action = RowAct::Open;
    }
    action
}

/// Horizontally scrolling row of track cards. Returns the clicked index and action.
fn card_row(ui: &mut egui::Ui, id: &str, tracks: &[TrackRow]) -> Option<(usize, RowAct)> {
    let mut hit = None;
    egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, t) in tracks.iter().enumerate() {
                let act = card(ui, t.cover.as_deref().or(t.image.as_deref()), &t.name, &t.artists, 140.0);
                if act != RowAct::None {
                    hit = Some((i, act));
                }
            }
        });
    });
    hit
}

/// Horizontally scrolling row of album cards.
fn album_row(ui: &mut egui::Ui, id: &str, albums: &[AlbumSummary]) -> Option<(usize, RowAct)> {
    let mut hit = None;
    egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, a) in albums.iter().enumerate() {
                let act = card(ui, a.image.as_deref(), &a.name, &a.artists, 140.0);
                if act != RowAct::None {
                    hit = Some((i, act));
                }
            }
        });
    });
    hit
}

/// Dense row for side panels: art, title/artist, playing marker. True when clicked.
fn compact_row(ui: &mut egui::Ui, t: &TrackRow, fallback: Option<&str>, current: bool, playing: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::click());
    if current || resp.hovered() {
        ui.painter().rect_filled(rect, 12.0, if current { GLASS_HOVER } else { GLASS });
    }
    let img = Rect::from_min_size(rect.left_top() + Vec2::new(6.0, 6.0), Vec2::splat(42.0));
    paint_art(ui, img, t.image.as_deref().or(fallback), 10);
    let text = Rect::from_min_max(Pos2::new(img.right() + 10.0, rect.top()), rect.right_bottom() - Vec2::new(30.0, 0.0));
    let p = ui.painter_at(text);
    p.text(text.left_center() - Vec2::new(0.0, 9.0), Align2::LEFT_CENTER, &t.name, FontId::proportional(14.0), if current { ACCENT } else { TEXT });
    p.text(text.left_center() + Vec2::new(0.0, 10.0), Align2::LEFT_CENTER, &t.artists, FontId::proportional(12.0), MUTED);
    if current {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(rect.right_center() - Vec2::new(18.0, 0.0), Vec2::splat(16.0))));
        eq_icon(&mut child, playing);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// Glass pill row: rank or art, title/artist, play button. Row click = Open, button = Play.
fn song_row(
    ui: &mut egui::Ui,
    t: &TrackRow,
    fallback: Option<&str>,
    current: bool,
    playing: bool,
    rank: Option<usize>,
) -> RowAct {
    let mut act = RowAct::None;
    let resp = glass(30, 0)
        .inner_margin(Margin { left: 8, right: 10, top: 6, bottom: 6 })
        .fill(if current { GLASS_HOVER } else { GLASS })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(n) = rank {
                    ui.add_sized([22.0, 44.0], egui::Label::new(RichText::new(n.to_string()).font(bold(13.0)).color(DIM)));
                }
                art(ui, t.image.as_deref().or(fallback), 44.0, 22);
                ui.vertical(|ui| {
                    ui.add_space(3.0);
                    let color = if current { ACCENT } else { TEXT };
                    ui.add(egui::Label::new(RichText::new(&t.name).size(14.5).color(color)).truncate().selectable(false));
                    ui.add(
                        egui::Label::new(RichText::new(format!("{}  ·  {}", t.artists, fmt_ms(t.duration_ms))).size(12.5).color(MUTED))
                            .truncate()
                            .selectable(false),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if current {
                        eq_icon(ui, playing);
                    } else if mini_play(ui, 34.0).on_hover_text("Play").clicked() {
                        act = RowAct::Play;
                    }
                });
            });
        })
        .response
        .interact(Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() && act == RowAct::None {
        act = RowAct::Open;
    }
    ui.add_space(2.0);
    act
}

fn playlist_row(ui: &mut egui::Ui, p: &PlaylistSummary, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 10.0, GLASS_HOVER);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 10.0, GLASS);
    }
    let img = Rect::from_min_size(rect.left_top() + Vec2::new(6.0, 6.0), Vec2::splat(32.0));
    paint_art(ui, img, p.thumb.as_deref(), 8);
    let text_rect = Rect::from_min_max(Pos2::new(img.right() + 10.0, rect.top()), rect.right_bottom() - Vec2::new(4.0, 0.0));
    let font = if selected { bold(13.5) } else { FontId::proportional(13.5) };
    ui.painter_at(text_rect).text(text_rect.left_center(), Align2::LEFT_CENTER, &p.name, font, if selected { TEXT } else { MUTED });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

fn artist_bubble(ui: &mut egui::Ui, image: Option<&str>, name: &str) -> bool {
    let resp = ui
        .vertical(|ui| {
            ui.set_width(128.0);
            let r = art(ui, image, 128.0, 64);
            ui.vertical_centered(|ui| {
                ui.add(egui::Label::new(RichText::new(name).font(bold(13.5))).truncate().selectable(false));
                ui.label(RichText::new("Artist").size(12.0).color(MUTED));
            });
            r
        })
        .inner;
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}
