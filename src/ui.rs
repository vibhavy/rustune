use crate::api::{AlbumSummary, ArtistSummary, Device, PlaylistSummary, SearchResults, ShowSummary, TrackRow};
use crate::backend::{Command, Event, HomeSection, RepeatMode, SongSection, ViewKey};
use crate::icons::{self, Icon};
use crate::lyrics::{self, Lyrics};
use crate::sdk::{Sdk, SdkMsg};
use crate::tray::{Tray, TrayMsg};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Mesh,
    Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2,
};
use egui_extras::{Column, TableBuilder};
use std::{
    collections::HashMap,
    sync::mpsc::{Receiver, Sender},
    sync::{Arc, Mutex},
    time::Instant,
};
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

/// Height of the transparent title bar the content runs under (traffic lights live here).
const TITLEBAR_H: f32 = 32.0;
/// Size of the menu bar mini player window.
const MINI_SIZE: Vec2 = Vec2::new(360.0, 172.0);
/// Extra height of the mini player while it shows lyrics.
const MINI_LYRICS_H: f32 = 64.0;

/// Starting volume for the SDK player (0..1).
const DEFAULT_VOLUME: f32 = 0.6;

/// Search runs this long after the last keystroke.
const TYPEAHEAD_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

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
    /// Shared so drawing a long list each frame doesn't copy it.
    tracks: Arc<Vec<TrackRow>>,
    next_offset: Option<u32>,
    loading: bool,
}

struct SongView {
    track: TrackRow,
    album: Option<Arc<Vec<TrackRow>>>,
    by_artist: Option<Arc<Vec<TrackRow>>>,
    albums: Option<Arc<Vec<AlbumSummary>>>,
    similar: Option<Arc<(String, Vec<TrackRow>)>>,
}

impl SongView {
    fn new(track: TrackRow) -> Self {
        Self { track, album: None, by_artist: None, albums: None, similar: None }
    }

    fn apply(&mut self, section: SongSection) {
        match section {
            SongSection::Album(v) => self.album = Some(Arc::new(v)),
            SongSection::Artist(v) => self.by_artist = Some(Arc::new(v)),
            SongSection::ArtistAlbums(v) => self.albums = Some(Arc::new(v)),
            SongSection::Similar { genre, tracks } => self.similar = Some(Arc::new((genre, tracks))),
        }
    }
}

enum View {
    Home,
    Search,
    Tracks(TrackView),
    Song(SongView),
    NowPlaying,
    /// "View all" for a Home shelf.
    Section(SectionPage),
}

#[derive(Clone)]
enum LyricsState {
    Loading,
    Ready(Arc<Lyrics>),
    /// The song has no lyrics.
    Missing,
    Failed(String),
}

/// Cards per Home shelf; the rest are on its "View all" page.
const SHELF: usize = 8;

/// The full list behind a Home shelf's "View all".
#[derive(Clone)]
enum SectionItems {
    Tracks(Arc<Vec<TrackRow>>),
    Episodes(Arc<Vec<TrackRow>>),
    Albums(Arc<Vec<AlbumSummary>>),
    Shows(Arc<Vec<ShowSummary>>),
    Artists(Arc<Vec<ArtistSummary>>),
}

struct SectionPage {
    title: String,
    items: SectionItems,
}

/// What Home shows: everything, only music, or only podcasts.
#[derive(Clone, Copy, PartialEq, Default)]
enum HomeFilter {
    #[default]
    All,
    Music,
    Podcasts,
}

#[derive(Default)]
struct HomeData {
    recent: Vec<TrackRow>,
    top_tracks: Vec<TrackRow>,
    top_artists: Vec<ArtistSummary>,
    because: Option<(String, Vec<TrackRow>)>,
    more_from: Vec<(String, Vec<TrackRow>)>,
    albums: Vec<AlbumSummary>,
    /// None until loaded.
    shows: Option<Vec<ShowSummary>>,
    new_episodes: Vec<TrackRow>,
    saved_episodes: Vec<TrackRow>,
    discover_artists: Vec<ArtistSummary>,
    discover_tracks: Vec<TrackRow>,
    new_releases: Vec<AlbumSummary>,
    /// (followed show, shows like it)
    shows_like: Vec<(String, Vec<ShowSummary>)>,
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
    /// The query most recently sent; results for any other query are stale.
    search_sent: String,
    /// When the search box was last edited, until that edit has been searched.
    search_edited: Option<Instant>,
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
    queue: Arc<Vec<TrackRow>>,
    saved: HashMap<String, bool>,
    /// Set when the user asks to play something; cleared when the SDK starts playing.
    loading_play: Option<Instant>,
    sdk: Option<Sdk>,
    sdk_rx: Receiver<SdkMsg>,
    /// True while the SDK player on this Mac holds the playback session.
    local_active: bool,
    error: Option<(String, Instant)>,
    np_tab: NpTab,
    home_filter: HomeFilter,
    /// Album / artist tracks for the Now Playing panel (same shape as a song page).
    np_related: Option<SongView>,
    tray: Option<Tray>,
    tray_rx: Receiver<TrayMsg>,
    /// Main window closed to the menu bar (music keeps playing).
    main_hidden: bool,
    /// Set by "Quit": lets the close request through instead of hiding.
    quitting: bool,
    /// Mini player window: Some(position) while open.
    mini: Option<Option<Pos2>>,
    /// Snapshot the mini player window draws from (it runs as its own deferred viewport).
    mini_state: Arc<Mutex<MiniState>>,
    mini_tx: Sender<MiniAct>,
    mini_rx: Receiver<MiniAct>,
    /// Lyrics for the song with this URI.
    lyrics: Option<(String, LyricsState)>,
    /// The line the lyrics last scrolled to.
    lyrics_line: Option<usize>,
    /// Now Playing shows lyrics instead of the cover.
    np_lyrics: bool,
    /// The mini player shows lyrics under the controls.
    mini_lyrics: bool,
}

/// What the mini player shows. Updated by the main window every pass.
#[derive(Default)]
struct MiniState {
    now: Option<NowPlaying>,
    playing: bool,
    loading: bool,
    pos_ms: u32,
    pos_at: Option<Instant>,
    shuffle: bool,
    repeat: Option<RepeatMode>,
    seek_drag: Option<f32>,
    /// The window had focus at least once, so losing it means "clicked away".
    focused_once: bool,
    show_lyrics: bool,
    /// The playing song's lyrics, once loaded.
    lyrics: Option<Arc<Lyrics>>,
    /// Shown instead of lyrics while there are none to show.
    lyrics_note: &'static str,
}

impl MiniState {
    fn position(&self) -> u32 {
        let dur = self.now.as_ref().map_or(0, |n| n.duration_ms);
        let p = match (self.playing, self.pos_at) {
            (true, Some(at)) => self.pos_ms + at.elapsed().as_millis() as u32,
            _ => self.pos_ms,
        };
        if dur > 0 { p.min(dur) } else { p }
    }
}

/// Button presses in the mini player, handled by the main window.
enum MiniAct {
    Close,
    OpenApp,
    PlayPause,
    Next,
    Prev,
    Shuffle,
    Repeat,
    Seek(u32),
    ToggleLyrics,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd: UnboundedSender<Command>,
        rx: Receiver<Event>,
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        apply_theme(&cc.egui_ctx);
        let (sdk_tx, sdk_rx) = std::sync::mpsc::channel();
        let (tray_tx, tray_rx) = std::sync::mpsc::channel();
        let (mini_tx, mini_rx) = std::sync::mpsc::channel();
        crate::reopen::install({
            let (tx, ctx) = (tray_tx.clone(), cc.egui_ctx.clone());
            move || {
                let _ = tx.send(TrayMsg::ShowApp);
                ctx.request_repaint();
            }
        });
        let tray = Tray::new(&cc.egui_ctx, tray_tx)
            .map_err(|e| log::warn!("menu bar icon unavailable: {e:#}"))
            .ok();
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
            search_sent: String::new(),
            search_edited: None,
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
            queue: Arc::default(),
            saved: HashMap::new(),
            loading_play: None,
            sdk,
            sdk_rx,
            local_active: false,
            error,
            np_tab: NpTab::Queue,
            home_filter: HomeFilter::All,
            np_related: None,
            tray,
            tray_rx,
            main_hidden: false,
            quitting: false,
            mini: None,
            mini_state: Arc::new(Mutex::new(MiniState::default())),
            mini_tx,
            mini_rx,
            lyrics: None,
            lyrics_line: None,
            np_lyrics: false,
            mini_lyrics: false,
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
            tracks: Arc::default(),
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
            tracks: Arc::default(),
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

    fn open_show(&mut self, sh: &ShowSummary) {
        self.open_tracks(TrackView {
            key: ViewKey::Show(sh.id.clone()),
            title: sh.name.clone(),
            subtitle: String::new(),
            image: sh.image.clone(),
            context_uri: sh.uri.clone(),
            album_name: sh.name.clone(),
            tracks: Arc::default(),
            next_offset: None,
            loading: true,
        });
    }

    fn open_album(&mut self, a: &AlbumSummary) {
        self.open_tracks(TrackView {
            key: ViewKey::Album(a.id.clone()),
            title: a.name.clone(),
            subtitle: a.artists.clone(),
            image: a.image.clone(),
            context_uri: a.uri.clone(),
            album_name: a.name.clone(),
            tracks: Arc::default(),
            next_offset: None,
            loading: true,
        });
    }

    fn open_now_playing(&mut self) {
        if !matches!(self.view, View::NowPlaying) {
            // Lyrics start at the current line again.
            self.lyrics_line = None;
            self.send(Command::LoadQueue);
            self.load_np_related();
            self.go(View::NowPlaying);
        }
    }

    /// Now Playing with the lyrics showing.
    fn open_lyrics(&mut self) {
        self.np_lyrics = true;
        self.open_now_playing();
    }

    /// Ask for the playing song's lyrics unless they're loaded or on the way.
    fn request_lyrics(&mut self) {
        let Some(n) = &self.now else { return };
        if n.uri.is_empty() || n.uri.starts_with("spotify:episode:") {
            return;
        }
        if self.lyrics.as_ref().is_some_and(|(uri, _)| *uri == n.uri) {
            return;
        }
        self.send(Command::LoadLyrics(lyrics::Query {
            uri: n.uri.clone(),
            name: n.name.clone(),
            artists: n.artists.clone(),
            duration_ms: n.duration_ms,
        }));
        self.lyrics = Some((n.uri.clone(), LyricsState::Loading));
        self.lyrics_line = None;
    }

    /// What the mini player's lyrics area shows: the lyrics, or a note instead.
    fn mini_lyrics_snapshot(&self) -> (Option<Arc<Lyrics>>, &'static str) {
        let Some(n) = &self.now else { return (None, "Nothing playing") };
        if n.uri.starts_with("spotify:episode:") {
            return (None, "Podcasts don't have lyrics");
        }
        match self.lyrics.as_ref().filter(|(uri, _)| *uri == n.uri).map(|(_, s)| s) {
            Some(LyricsState::Ready(l)) if l.synced => (Some(l.clone()), ""),
            Some(LyricsState::Ready(_)) => (None, "These lyrics aren't synced"),
            Some(LyricsState::Missing) => (None, "No lyrics for this song"),
            Some(LyricsState::Failed(_)) => (None, "Couldn't load the lyrics"),
            None | Some(LyricsState::Loading) => (None, "Finding lyrics…"),
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
        // Keep what the user typed (e.g. a trailing space) if it's the same query.
        if self.search_query.trim() != q {
            self.search_query = q.to_string();
        }
        self.search_sent = q.to_string();
        self.search_edited = None;
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

    /// Episodes play from the clicked one onwards; they have no song page to open.
    fn act_on_episodes(&mut self, episodes: &[TrackRow], i: usize, act: RowAct) {
        if act != RowAct::None {
            let uris = episodes.iter().map(|t| t.uri.clone()).collect();
            self.play_track(&episodes[i].clone(), PlayFrom::List(uris, i as u32));
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
                                Arc::make_mut(&mut tv.tracks).extend(tracks);
                            } else {
                                tv.tracks = Arc::new(tracks);
                            }
                            tv.next_offset = next_offset;
                            tv.loading = false;
                        }
                    }
                }
                Event::SearchResults { query, results } => {
                    if query == self.search_sent {
                        self.search = Some(results);
                        self.searching = false;
                    }
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
                    HomeSection::Shows(v) => self.home.shows = Some(v),
                    HomeSection::NewEpisodes(v) => self.home.new_episodes = v,
                    HomeSection::SavedEpisodes(v) => self.home.saved_episodes = v,
                    HomeSection::Discover { artists, tracks } => {
                        self.home.discover_artists = artists;
                        self.home.discover_tracks = tracks;
                    }
                    HomeSection::NewReleases(v) => self.home.new_releases = v,
                    HomeSection::ShowsLike { seed, shows } => self.home.shows_like.push((seed, shows)),
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
                Event::Queue(q) => self.queue = Arc::new(q),
                Event::Saved { uri, saved } => {
                    self.saved.insert(uri, saved);
                }
                Event::Lyrics { uri, result } => {
                    if let Some((u, state)) = &mut self.lyrics
                        && *u == uri
                    {
                        *state = match result {
                            Ok(Some(l)) => LyricsState::Ready(Arc::new(l)),
                            Ok(None) => LyricsState::Missing,
                            Err(e) => LyricsState::Failed(e),
                        };
                    }
                }
            }
        }
    }

    fn drain_tray(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.tray_rx.try_recv() {
            match msg {
                TrayMsg::ToggleMini { anchor } => {
                    log::debug!("tray click: mini open={} main_hidden={}", self.mini.is_some(), self.main_hidden);
                    if self.mini.is_some() {
                        self.mini = None;
                    } else {
                        // Physical screen pixels → points, centred under the icon.
                        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(2.0);
                        let pos = anchor.map(|(x, y)| {
                            Pos2::new((x / ppp as f64) as f32 - MINI_SIZE.x / 2.0, (y / ppp as f64) as f32 + 6.0)
                        });
                        self.open_mini(ctx, pos);
                    }
                }
                TrayMsg::ShowMini => self.open_mini(ctx, None),
                TrayMsg::PlayPause => self.toggle_play(),
                TrayMsg::Next => {
                    if let Some(sdk) = &self.sdk {
                        sdk.next();
                    }
                }
                TrayMsg::Prev => {
                    if let Some(sdk) = &self.sdk {
                        sdk.prev();
                    }
                }
                TrayMsg::ShowApp => self.show_app(ctx),
                TrayMsg::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn open_mini(&mut self, ctx: &egui::Context, pos: Option<Pos2>) {
        // Without an anchor (menu item), sit at the top-right of the screen.
        let pos = pos.or_else(|| {
            ctx.input(|i| i.viewport().monitor_size)
                .map(|m| Pos2::new(m.x - MINI_SIZE.x - 12.0, 34.0))
        });
        self.mini = Some(pos.map(|p| Pos2::new(p.x.max(8.0), p.y)));
        if let Ok(mut st) = self.mini_state.lock() {
            st.focused_once = false;
            st.seek_drag = None;
        }
        ctx.send_viewport_cmd_to(mini_viewport_id(), egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn show_app(&mut self, ctx: &egui::Context) {
        self.mini = None;
        self.main_hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// The mini player is its own small borderless, always-on-top window. It is a
    /// *deferred* viewport: eframe creates and paints it on its own events, which also
    /// works while the main window is hidden (an immediate viewport can't be created
    /// from that state and makes egui panic).
    fn show_mini(&mut self, ctx: &egui::Context) {
        let Some(pos) = self.mini else { return };
        if self.mini_lyrics {
            self.request_lyrics();
        }
        let (lyrics, lyrics_note) = self.mini_lyrics_snapshot();
        if let Ok(mut st) = self.mini_state.lock() {
            // Lyrics can arrive while the mini player is idle; wake it to show them.
            let changed = st.show_lyrics != self.mini_lyrics
                || st.lyrics_note != lyrics_note
                || match (&st.lyrics, &lyrics) {
                    (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                    (a, b) => a.is_some() != b.is_some(),
                };
            if changed {
                ctx.request_repaint_of(mini_viewport_id());
            }
            st.show_lyrics = self.mini_lyrics;
            st.lyrics = lyrics;
            st.lyrics_note = lyrics_note;
            st.now = self.now.clone();
            st.playing = self.playing;
            st.loading = self.loading_play.is_some();
            st.pos_ms = self.pos_ms;
            st.pos_at = Some(self.pos_at);
            st.shuffle = self.shuffle;
            st.repeat = Some(self.repeat);
        }
        let size = if self.mini_lyrics { MINI_SIZE + Vec2::new(0.0, MINI_LYRICS_H) } else { MINI_SIZE };
        let mut builder = egui::ViewportBuilder::default()
            .with_title("rustune mini player")
            .with_inner_size(size)
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top();
        if let Some(p) = pos {
            builder = builder.with_position(p);
        }
        let (state, tx) = (self.mini_state.clone(), self.mini_tx.clone());
        ctx.show_viewport_deferred(mini_viewport_id(), builder, move |ui, _| {
            let acts = mini_ui(ui, &state);
            if !acts.is_empty() {
                for a in acts {
                    let _ = tx.send(a);
                }
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
        });
    }

    fn drain_mini(&mut self, ctx: &egui::Context) {
        while let Ok(act) = self.mini_rx.try_recv() {
            match act {
                MiniAct::Close => {
                    log::debug!("mini: close requested");
                    self.mini = None;
                }
                MiniAct::OpenApp => self.show_app(ctx),
                MiniAct::PlayPause => self.toggle_play(),
                MiniAct::Next => {
                    if let Some(sdk) = &self.sdk {
                        sdk.next();
                    }
                }
                MiniAct::Prev => {
                    if let Some(sdk) = &self.sdk {
                        sdk.prev();
                    }
                }
                MiniAct::Shuffle => self.send(Command::Shuffle(!self.shuffle)),
                MiniAct::Repeat => {
                    self.repeat = self.repeat.next();
                    self.send(Command::Repeat(self.repeat));
                }
                MiniAct::Seek(ms) => self.seek(ms),
                MiniAct::ToggleLyrics => self.mini_lyrics = !self.mini_lyrics,
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
/// fonts stay as fallbacks for symbols. Both weights share one memory-mapped copy of the
/// font file, so its 8 MB are paged in from disk as needed rather than copied.
fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontTweak, VariationCoords};
    let mut fonts = egui::FontDefinitions::default();
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut bold_family = defaults.clone();
    let file = std::fs::File::open("/System/Library/Fonts/SFNS.ttf");
    // SAFETY: the font lives on the read-only system volume, so the mapped bytes can't
    // change underneath us.
    if let Ok(map) = file.and_then(|f| unsafe { memmap2::Mmap::map(&f) }) {
        let blob: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync> = std::sync::Arc::new(map);
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
    // SF has no Devanagari, which many Hindi lyrics are written in.
    let file = std::fs::File::open("/System/Library/Fonts/Kohinoor.ttc");
    // SAFETY: as above, a read-only system font.
    if let Ok(map) = file.and_then(|f| unsafe { memmap2::Mmap::map(&f) }) {
        let blob: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync> = std::sync::Arc::new(map);
        // Faces in the collection: 0 = Regular, 2 = Semibold.
        fonts.font_data.insert("deva".into(), std::sync::Arc::new(egui::FontData::from_blob(blob.clone(), 0)));
        fonts.font_data.insert("deva-bold".into(), std::sync::Arc::new(egui::FontData::from_blob(blob, 2)));
        if let Some(p) = fonts.families.get_mut(&FontFamily::Proportional) {
            p.insert(1.min(p.len()), "deva".into());
        }
        bold_family.insert(1.min(bold_family.len()), "deva-bold".into());
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
    if s >= 3600 {
        // Podcast episodes run for hours.
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
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

/// Waveform-style seek bar. `drag` holds the in-progress drag position. Returns the
/// new position (0..1) when the user releases it.
fn wave_bar(ui: &mut egui::Ui, width: f32, height: f32, seed: &str, frac: f32, drag: &mut Option<f32>) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    let n = ((width / 4.5) as usize).max(1);
    let heights = wave_heights(seed, n);
    let step = width / n as f32;
    let hover = resp.hover_pos().map(|p| ((p.x - rect.left()) / width).clamp(0.0, 1.0));
    let frac = drag.unwrap_or(frac);
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
        *drag = at;
    }
    if resp.drag_stopped() || resp.clicked() {
        *drag = None;
        return at;
    }
    None
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
    /// Clear to fully transparent: the main window paints over everything anyway, and
    /// the mini player's rounded corners need to show what's behind the window.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.drain_sdk();
        self.drain_tray(ctx);
        self.drain_mini(ctx);

        // The red close button hides the window; music keeps playing from the menu bar.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            log::debug!("window closed to the menu bar");
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.main_hidden = true;
            // Free the covers; the mini player reloads its one from the disk cache.
            ctx.forget_all_images();
        }
        // Let the mini window draw while the main one is hidden or minimised.
        if self.mini.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
        if let Some(tray) = &mut self.tray {
            let track = self.now.as_ref().map(|n| (n.name.as_str(), n.artists.as_str()));
            tray.update(track, self.playing);
        }
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
        self.show_mini(ui.ctx());
        if self.main_hidden {
            return;
        }
        paint_background(ui);
        titlebar_drag(ui);
        match &self.auth {
            Auth::LoggedIn { .. } => {}
            _ => return self.login_screen(ui),
        }
        let now_playing = matches!(self.view, View::NowPlaying);
        if !now_playing {
            self.player_bar(ui);
        }
        self.sidebar(ui);
        let margin = if now_playing {
            Margin::ZERO
        } else {
            Margin { left: 8, right: 20, top: TITLEBAR_H as i8 + 4, bottom: 6 }
        };
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
                    View::Section(_) => self.section_view(ui),
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
            .frame(Frame::new().inner_margin(Margin { left: 14, right: 6, top: TITLEBAR_H as i8 + 6, bottom: 14 }))
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
                    let bar = ui.max_rect();
                    let w = ui.available_width();
                    let center_w = (w - (w * 0.27).max(220.0) * 2.0).max(280.0);
                    let side = (w - center_w) / 2.0;
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

                        // Center: transport + waveform, on the bar's centre line whatever
                        // width the left side ends up using.
                        let center = Rect::from_center_size(bar.center(), Vec2::new(center_w, 72.0));
                        let builder = egui::UiBuilder::new().max_rect(center).layout(Layout::top_down(Align::Center));
                        ui.scope_builder(builder, |ui| {
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
                            ui.add_space(6.0);
                            if icons::button(ui, Icon::Lyrics, 17.0, MUTED, TEXT).on_hover_text("Lyrics").clicked() {
                                self.open_lyrics();
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
        let seed = self.now.as_ref().map_or("rustune", |n| n.uri.as_str()).to_string();
        wave_bar(ui, width, height, &seed, frac, &mut self.seek_drag)
    }

    // ---- Home ----

    fn home_view(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("home").auto_shrink([false, false]).show(ui, |ui| {
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
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                for (f, label) in [(HomeFilter::All, "All"), (HomeFilter::Music, "Music"), (HomeFilter::Podcasts, "Podcasts")] {
                    if chip(ui, label, self.home_filter == f).clicked() {
                        self.home_filter = f;
                    }
                }
            });
            ui.add_space(14.0);
            let music = self.home_filter != HomeFilter::Podcasts;
            let podcasts = self.home_filter != HomeFilter::Music;

            if music {
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
            }

            let home = std::mem::take(&mut self.home);
            let mut more: Option<SectionPage> = None;
            let page = |title: &str, items: SectionItems| Some(SectionPage { title: title.to_string(), items });
            if music {
                if !home.recent.is_empty() {
                    if section_head(ui, "Jump back in", home.recent.len() > SHELF) {
                        more = page("Jump back in", SectionItems::Tracks(Arc::new(home.recent.clone())));
                    }
                    if let Some((i, act)) = card_row(ui, "recent", shelf(&home.recent)) {
                        self.act_on_list(&home.recent, i, act);
                    }
                }
                if !home.discover_artists.is_empty() {
                    let title = "Artists you might like";
                    if section_head(ui, title, home.discover_artists.len() > SHELF) {
                        more = page(title, SectionItems::Artists(Arc::new(home.discover_artists.clone())));
                    }
                    if let Some(i) = artist_row(ui, "discover_artists", shelf(&home.discover_artists)) {
                        self.run_search(&home.discover_artists[i].name.clone());
                    }
                }
                if !home.discover_tracks.is_empty() {
                    let title = "Discover new music";
                    if section_head(ui, title, home.discover_tracks.len() > SHELF) {
                        more = page(title, SectionItems::Tracks(Arc::new(home.discover_tracks.clone())));
                    }
                    if let Some((i, act)) = card_row(ui, "discover", shelf(&home.discover_tracks)) {
                        self.act_on_list(&home.discover_tracks, i, act);
                    }
                }
                if !home.new_releases.is_empty() {
                    let title = "New releases";
                    if section_head(ui, title, home.new_releases.len() > SHELF) {
                        more = page(title, SectionItems::Albums(Arc::new(home.new_releases.clone())));
                    }
                    match album_row(ui, "new_releases", shelf(&home.new_releases)) {
                        Some((i, RowAct::Play)) => self.play_context(home.new_releases[i].uri.clone()),
                        Some((i, _)) => self.open_album(&home.new_releases[i]),
                        None => {}
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
                    let title = format!("More from {artist}");
                    if section_head(ui, &title, tracks.len() > SHELF) {
                        more = page(&title, SectionItems::Tracks(Arc::new(tracks.clone())));
                    }
                    if let Some((i, act)) = card_row(ui, &format!("more_from_{k}"), shelf(tracks)) {
                        self.act_on_list(tracks, i, act);
                    }
                }
                if !home.albums.is_empty() {
                    let title = "Albums for you";
                    if section_head(ui, title, home.albums.len() > SHELF) {
                        more = page(title, SectionItems::Albums(Arc::new(home.albums.clone())));
                    }
                    match album_row(ui, "home_albums", shelf(&home.albums)) {
                        Some((i, RowAct::Play)) => self.play_context(home.albums[i].uri.clone()),
                        Some((i, _)) => self.open_album(&home.albums[i]),
                        None => {}
                    }
                }
                if !home.top_artists.is_empty() {
                    let title = "Your top artists";
                    if section_head(ui, title, home.top_artists.len() > SHELF) {
                        more = page(title, SectionItems::Artists(Arc::new(home.top_artists.clone())));
                    }
                    if let Some(i) = artist_row(ui, "top_artists", shelf(&home.top_artists)) {
                        self.run_search(&home.top_artists[i].name.clone());
                    }
                }
                if home.recent.is_empty() && home.top_tracks.is_empty() && home.top_artists.is_empty() {
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Finding suggestions for you…").color(MUTED));
                    });
                }
            }
            if podcasts && let Some(p) = self.podcast_sections(ui, &home) {
                more = Some(p);
            }
            self.home = home;
            if let Some(p) = more {
                self.go(View::Section(p));
                return;
            }

            if music {
                if !self.playlists.is_empty() {
                    section_title(ui, "Your playlists");
                    // A grid, not `horizontal_wrapped`: framed cards don't wrap, and a row
                    // wider than the window stopped Home from scrolling.
                    let list = &self.playlists[..self.playlists.len().min(12)];
                    let hit = grid(ui, list.len(), 170.0, |ui, i| card(ui, list[i].image.as_deref(), &list[i].name, &list[i].owner, 150.0));
                    match hit {
                        Some((i, RowAct::Play)) => self.play_context(self.playlists[i].uri.clone()),
                        Some((i, _)) => {
                            let p = self.playlists[i].clone();
                            self.open_playlist(&p);
                        }
                        None => {}
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
            }
            ui.add_space(16.0);
        });
    }

    /// Home's podcast sections: newest episodes of followed shows, the shows, shows like
    /// them, saved episodes. Returns a "View all" page if one was clicked.
    fn podcast_sections(&mut self, ui: &mut egui::Ui, home: &HomeData) -> Option<SectionPage> {
        let Some(shows) = &home.shows else {
            if self.home_filter == HomeFilter::Podcasts {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Loading your podcasts…").color(MUTED));
                });
            }
            return None;
        };
        if shows.is_empty() && home.saved_episodes.is_empty() {
            if self.home_filter == HomeFilter::Podcasts {
                section_title(ui, "Podcasts");
                ui.label(RichText::new("Shows you follow on Spotify appear here.").color(MUTED));
            }
            return None;
        }
        let mut more = None;
        let page = |title: &str, items: SectionItems| Some(SectionPage { title: title.to_string(), items });
        let now = self.now.as_ref().map(|n| n.uri.clone());

        let (title, list) = ("New episodes", &home.new_episodes);
        if !list.is_empty() {
            if section_head(ui, title, list.len() > 6) {
                more = page(title, SectionItems::Episodes(Arc::new(list.clone())));
            }
            if let Some((i, act)) = episode_rows(ui, &list[..list.len().min(6)], now.as_deref(), self.playing) {
                self.act_on_episodes(list, i, act);
            }
        }
        let mut show_shelves: Vec<(String, &Vec<ShowSummary>)> = vec![("Your shows".to_string(), shows)];
        for (seed, like) in &home.shows_like {
            show_shelves.push((format!("Because you follow {seed}"), like));
        }
        for (k, (title, list)) in show_shelves.iter().enumerate() {
            if list.is_empty() {
                continue;
            }
            if section_head(ui, title, list.len() > SHELF) {
                more = page(title, SectionItems::Shows(Arc::new((*list).clone())));
            }
            match show_row(ui, &format!("shows_{k}"), shelf(list)) {
                Some((i, RowAct::Play)) => self.play_context(list[i].uri.clone()),
                Some((i, _)) => self.open_show(&list[i].clone()),
                None => {}
            }
        }
        let (title, list) = ("Saved episodes", &home.saved_episodes);
        if !list.is_empty() {
            if section_head(ui, title, list.len() > 10) {
                more = page(title, SectionItems::Episodes(Arc::new(list.clone())));
            }
            if let Some((i, act)) = episode_rows(ui, &list[..list.len().min(10)], now.as_deref(), self.playing) {
                self.act_on_episodes(list, i, act);
            }
        }
        more
    }

    /// "View all" page for a Home shelf.
    fn section_view(&mut self, ui: &mut egui::Ui) {
        let View::Section(p) = &self.view else { return };
        let (title, items) = (p.title.clone(), p.items.clone());
        ui.label(heading(&title, 28.0));
        ui.add_space(12.0);
        let now = self.now.as_ref().map(|n| n.uri.clone());
        egui::ScrollArea::vertical().id_salt(("section", &title)).auto_shrink([false, false]).show(ui, |ui| match &items {
            SectionItems::Tracks(list) | SectionItems::Episodes(list) => {
                if let Some((i, act)) = episode_rows(ui, list, now.as_deref(), self.playing) {
                    if matches!(items, SectionItems::Episodes(_)) {
                        self.act_on_episodes(list, i, act);
                    } else {
                        self.act_on_list(list, i, act);
                    }
                }
            }
            SectionItems::Albums(list) => match grid(ui, list.len(), 170.0, |ui, i| {
                card(ui, list[i].image.as_deref(), &list[i].name, &list[i].artists, 150.0)
            }) {
                Some((i, RowAct::Play)) => self.play_context(list[i].uri.clone()),
                Some((i, _)) => self.open_album(&list[i].clone()),
                None => {}
            },
            SectionItems::Shows(list) => match grid(ui, list.len(), 170.0, |ui, i| {
                let sub = format!("{} episodes", list[i].total_episodes);
                card(ui, list[i].image.as_deref(), &list[i].name, &sub, 150.0)
            }) {
                Some((i, RowAct::Play)) => self.play_context(list[i].uri.clone()),
                Some((i, _)) => self.open_show(&list[i].clone()),
                None => {}
            },
            SectionItems::Artists(list) => {
                let hit = grid(ui, list.len(), 140.0, |ui, i| {
                    if artist_bubble(ui, list[i].image.as_deref(), &list[i].name) { RowAct::Open } else { RowAct::None }
                });
                if let Some((i, _)) = hit {
                    self.run_search(&list[i].name.clone());
                }
            }
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

    /// Typeahead: search once typing pauses. Clearing the box clears the results.
    fn search_as_you_type(&mut self, ctx: &egui::Context) {
        let Some(at) = self.search_edited else { return };
        let wait = TYPEAHEAD_DELAY.saturating_sub(at.elapsed());
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        self.search_edited = None;
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.search = None;
            self.search_sent.clear();
            self.searching = false;
        } else if q.chars().count() >= 2 && q != self.search_sent {
            self.run_search(&q);
        }
    }

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
                if resp.changed() {
                    self.search_edited = Some(Instant::now());
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
        self.search_as_you_type(ui.ctx());
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

        egui::ScrollArea::vertical().id_salt("search").auto_shrink([false, false]).show(ui, |ui| {
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
                                tracks: Arc::default(),
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

        egui::ScrollArea::vertical().id_salt("song").auto_shrink([false, false]).show(ui, |ui| {
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
            match similar.as_deref() {
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

    /// Lyrics in place of the cover on the Now Playing screen.
    fn np_lyrics(&mut self, ui: &mut egui::Ui, rect: Rect, np: &NowPlaying) {
        self.request_lyrics();
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)));
        if np.uri.starts_with("spotify:episode:") {
            lyrics_note(&mut ui, "Podcasts don't have lyrics");
            return;
        }
        let state = self.lyrics.as_ref().filter(|(uri, _)| *uri == np.uri).map(|(_, s)| s.clone());
        match state {
            None | Some(LyricsState::Loading) => {
                lyrics_note(&mut ui, "Finding lyrics…");
            }
            Some(LyricsState::Missing) => {
                lyrics_note(&mut ui, "No lyrics for this song");
            }
            Some(LyricsState::Failed(e)) => {
                lyrics_note(&mut ui, "Couldn't load the lyrics").on_hover_text(e);
                if ui.link(RichText::new("Try again").color(ACCENT)).clicked() {
                    self.lyrics = None;
                }
            }
            Some(LyricsState::Ready(l)) => self.lyrics_lines(&mut ui, &l),
        }
    }

    /// Synced lyrics highlight the line being sung and keep it in the middle; clicking a
    /// line jumps there. Unsynced ones are shown as plain text.
    fn lyrics_lines(&mut self, ui: &mut egui::Ui, l: &Lyrics) {
        let pos = self.position();
        let cur = if l.synced { lyrics::current(&l.lines, pos) } else { None };
        let scroll_to = cur.filter(|&c| self.lyrics_line != Some(c));
        // Room above the first line and below the last so both can reach the middle.
        let pad = ui.max_rect().height() * 0.4;
        let mut seek_to = None;
        egui::ScrollArea::vertical().id_salt("lyrics").auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = if l.synced { 14.0 } else { 6.0 };
                ui.add_space(if l.synced { pad } else { 8.0 });
                if !l.synced {
                    ui.label(RichText::new("These lyrics aren't synced to the song.").size(12.0).color(DIM));
                    ui.add_space(8.0);
                }
                for (i, line) in l.lines.iter().enumerate() {
                    let text = if line.text.is_empty() { "♪" } else { &line.text };
                    if !l.synced {
                        ui.label(RichText::new(text).size(17.0).color(TEXT));
                        continue;
                    }
                    let color = match cur {
                        Some(c) if i == c => TEXT,
                        Some(c) if i < c => MUTED,
                        _ => DIM,
                    };
                    let resp = ui.add(
                        egui::Label::new(RichText::new(text).font(bold(24.0)).color(color))
                            .wrap()
                            .sense(Sense::click())
                            .selectable(false),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        seek_to = Some(line.at_ms);
                    }
                    if scroll_to == Some(i) {
                        resp.scroll_to_me(Some(Align::Center));
                    }
                }
                ui.add_space(20.0);
                ui.label(RichText::new("Lyrics from LRCLIB").size(11.0).color(DIM));
                ui.add_space(pad);
            });
        });
        if scroll_to.is_some() {
            self.lyrics_line = scroll_to;
        }
        if let Some(ms) = seek_to {
            self.seek(ms);
        }
        // Wake up exactly when the next line starts.
        if l.synced && self.playing {
            let next = cur.map_or(0, |c| c + 1);
            if let Some(line) = l.lines.get(next) {
                let wait = line.at_ms.saturating_sub(pos).max(16);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(wait as u64));
            }
        }
    }

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
        let top = Rect::from_min_size(main.min + Vec2::new(18.0, TITLEBAR_H), Vec2::new(main.width() - 36.0, 44.0));
        let mut top_ui = ui.new_child(egui::UiBuilder::new().max_rect(top).layout(Layout::left_to_right(Align::Center)));
        if icons::button(&mut top_ui, Icon::ChevronDown, 22.0, TEXT, Color32::WHITE).on_hover_text("Close").clicked() {
            self.back();
            return;
        }
        top_ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (c, h) = if self.np_lyrics { (ACCENT, ACCENT_HOVER) } else { (MUTED, TEXT) };
            let tip = if self.np_lyrics { "Show the cover" } else { "Lyrics" };
            if icons::button(ui, Icon::Lyrics, 20.0, c, h).on_hover_text(tip).clicked() {
                self.np_lyrics = !self.np_lyrics;
                self.lyrics_line = None;
            }
        });
        ui.painter().text(top.center(), Align2::CENTER_CENTER, "Now Playing", bold(16.0), TEXT);

        // Circular cover with progress ring, or the lyrics in its place
        let content_top = top.bottom() + 10.0;
        let avail_h = main.bottom() - content_top;
        let d = (main.width() * 0.42).min(avail_h * 0.44).clamp(160.0, 340.0);
        let center = Pos2::new(main.center().x, content_top + d / 2.0 + 34.0);
        let ring_r = d / 2.0 + 18.0;
        // Volume buttons sit beside the cover, or at the edges to give lyrics the width.
        let vol_dx = if self.np_lyrics { main.width() / 2.0 - 44.0 } else { ring_r + 52.0 };
        if self.np_lyrics {
            let w = (main.width() - 2.0 * 88.0).min(680.0);
            let area = Rect::from_min_max(
                Pos2::new(main.center().x - w / 2.0, content_top + 6.0),
                Pos2::new(main.center().x + w / 2.0, center.y + ring_r + 22.0),
            );
            self.np_lyrics(ui, area, &np);
        } else {
            let p = ui.painter();
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
        }

        // Volume -/+
        for (icon, x, delta) in [(Icon::VolumeLow, center.x - vol_dx, -0.1f32), (Icon::Volume, center.x + vol_dx, 0.1)] {
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
                Pos2::new(full.right() - queue_w + 4.0, full.top() + TITLEBAR_H + 6.0),
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
                let (list, empty): (Option<Arc<Vec<TrackRow>>>, &str) = match self.np_tab {
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
            ViewKey::Show(_) => "PODCAST",
        };
        let is_show = matches!(tv.key, ViewKey::Show(_));
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
                    let what = if is_show { "episodes" } else { "songs" };
                    format!("{} {what}{}", tracks.len(), if next_offset.is_some() { "+" } else { "" })
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
        let date_col = if is_show { "RELEASED" } else { "ALBUM" };
        if let Some((i, open)) = self.track_table(ui, &tracks, image.as_deref(), next_offset.is_some(), date_col) {
            if i == tracks.len() {
                if let View::Tracks(tv) = &mut self.view {
                    tv.loading = true;
                }
                self.send(Command::LoadTracks { view: key, offset: next_offset.unwrap_or(0), album_name });
            } else {
                let mut row = tracks[i].clone();
                row.cover = row.cover.or_else(|| image.clone());
                self.play_track(&row, PlayFrom::Context(ctx_uri));
                // Episodes have no song page.
                if open && !is_show {
                    self.open_song(&row);
                }
            }
        }
    }

    /// Virtualised track list (only visible rows are laid out). Returns (row, open_song):
    /// double-click plays, clicking the title opens the song page too.
    /// Row `tracks.len()` is the "load more" row.
    /// `album_col` names the fourth column ("ALBUM", or "RELEASED" for episodes).
    fn track_table(
        &self,
        ui: &mut egui::Ui,
        tracks: &[TrackRow],
        fallback: Option<&str>,
        more: bool,
        album_col: &str,
    ) -> Option<(usize, bool)> {
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
                for t in ["#", "", "TITLE", album_col, "TIME"] {
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

fn mini_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("rustune-mini")
}

/// The mini player window's UI. Runs on its own (deferred viewport), so it reads a
/// shared snapshot and reports button presses instead of touching the app directly.
fn mini_ui(ui: &mut egui::Ui, state: &Mutex<MiniState>) -> Vec<MiniAct> {
    let mut acts = Vec::new();
    let Ok(mut st) = state.lock() else { return acts };
    let ctx = ui.ctx().clone();

    // Popover behaviour: clicking elsewhere closes it.
    match ctx.input(|i| i.viewport().focused) {
        Some(true) => st.focused_once = true,
        Some(false) if st.focused_once => {
            log::debug!("mini: lost focus, closing");
            acts.push(MiniAct::Close);
            return acts;
        }
        _ => {}
    }
    if st.playing || st.loading {
        ctx.request_repaint_after(std::time::Duration::from_millis(if st.loading { 40 } else { 500 }));
    }

    // Card: dark base, the cover stretched across it as a soft colour wash (like the
    // Now Playing backdrop), a dark veil for legibility, and a thin rim. All layers
    // share the same rounding, so there are no seams.
    let r = ui.max_rect();
    ui.painter().rect_filled(r, 20.0, Color32::from_rgb(0x1a, 0x0f, 0x0f));
    if let Some(url) = st.now.as_ref().and_then(|n| n.cover.clone()) {
        egui::Image::from_uri(url)
            .corner_radius(20)
            .tint(Color32::from_white_alpha(70))
            .paint_at(ui, r);
    }
    let p = ui.painter();
    p.rect_filled(r, 20.0, Color32::from_rgba_premultiplied(14, 8, 8, 170));
    p.rect_stroke(r.shrink(0.5), 20.0, Stroke::new(1.0, GLASS_STROKE), egui::StrokeKind::Inside);
    // Drag the background to move the mini player.
    if ui.interact(r, egui::Id::unique("mini_drag"), Sense::drag()).drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    let inner = r.shrink2(Vec2::new(16.0, 14.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
    let ui = &mut child;
    ui.spacing_mut().item_spacing.y = 4.0;
    let w = inner.width();

    // Cover, title, open/close
    ui.horizontal(|ui| {
        art(ui, st.now.as_ref().and_then(|n| n.cover.as_deref()), 60.0, 14);
        ui.vertical(|ui| {
            ui.set_width(w - 60.0 - 10.0 - 3.0 * icons::button_side(14.0) - 16.0);
            ui.add_space(8.0);
            match &st.now {
                Some(n) => {
                    ui.add(egui::Label::new(RichText::new(&n.name).font(bold(15.0))).truncate().selectable(false));
                    ui.add(egui::Label::new(RichText::new(&n.artists).size(12.5).color(MUTED)).truncate().selectable(false));
                }
                None => {
                    ui.label(RichText::new("Nothing playing").font(bold(15.0)).color(MUTED));
                    ui.label(RichText::new("Pick a song in rustune").size(12.5).color(DIM));
                }
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            if icons::button(ui, Icon::Close, 14.0, DIM, TEXT).on_hover_text("Close").clicked() {
                acts.push(MiniAct::Close);
            }
            if icons::button(ui, Icon::Expand, 14.0, DIM, TEXT).on_hover_text("Open rustune").clicked() {
                acts.push(MiniAct::OpenApp);
            }
            let (c, h) = if st.show_lyrics { (ACCENT, ACCENT_HOVER) } else { (DIM, TEXT) };
            if icons::button(ui, Icon::Lyrics, 14.0, c, h).on_hover_text("Lyrics").clicked() {
                acts.push(MiniAct::ToggleLyrics);
            }
        });
    });

    // Transport: same layout rules as the main player (equal slots, one centre line).
    let (icon, play, gap) = (16.0_f32, 38.0_f32, 16.0_f32);
    let small = icons::button_side(icon);
    let (row, _) = ui.allocate_exact_size(Vec2::new(w, play + 8.0), Sense::hover());
    let total = small * 4.0 + play + gap * 4.0;
    let mut x = row.center().x - total / 2.0;
    let mut slot = |size: f32| {
        let rect = Rect::from_center_size(Pos2::new(x + size / 2.0, row.center().y), Vec2::splat(size));
        x += size + gap;
        rect
    };
    let toggle = |on: bool| if on { (ACCENT, ACCENT_HOVER) } else { (MUTED, TEXT) };
    let (sc, sh) = toggle(st.shuffle);
    if icons::button_at(ui, slot(small), Icon::Shuffle, icon, sc, sh).on_hover_text("Shuffle").clicked() {
        acts.push(MiniAct::Shuffle);
    }
    if icons::button_at(ui, slot(small), Icon::SkipPrev, icon, TEXT, Color32::WHITE).clicked() {
        acts.push(MiniAct::Prev);
    }
    if play_button_at(ui, slot(play), st.playing, st.loading).clicked() {
        acts.push(MiniAct::PlayPause);
    }
    if icons::button_at(ui, slot(small), Icon::SkipNext, icon, TEXT, Color32::WHITE).clicked() {
        acts.push(MiniAct::Next);
    }
    let repeat = st.repeat.unwrap_or(RepeatMode::Off);
    let (rc, rh) = toggle(repeat != RepeatMode::Off);
    let glyph = if repeat == RepeatMode::Track { Icon::RepeatOne } else { Icon::Repeat };
    if icons::button_at(ui, slot(small), glyph, icon, rc, rh).on_hover_text(repeat.label()).clicked() {
        acts.push(MiniAct::Repeat);
    }

    // Progress
    let dur = st.now.as_ref().map_or(0, |n| n.duration_ms);
    let pos = st.position();
    ui.horizontal(|ui| {
        ui.add_sized([40.0, 16.0], egui::Label::new(RichText::new(fmt_ms(pos)).size(11.5).color(MUTED)));
        let seed = st.now.as_ref().map_or("rustune".to_string(), |n| n.uri.clone());
        let frac = if dur > 0 { pos as f32 / dur as f32 } else { 0.0 };
        let mut drag = st.seek_drag;
        if let Some(new) = wave_bar(ui, w - 100.0, 16.0, &seed, frac, &mut drag) {
            if dur > 0 {
                acts.push(MiniAct::Seek((new * dur as f32) as u32));
            }
        }
        st.seek_drag = drag;
        ui.add_sized([40.0, 16.0], egui::Label::new(RichText::new(fmt_ms(dur)).size(11.5).color(MUTED)));
    });

    if st.show_lyrics {
        mini_lyrics(ui, &st, pos);
    }
    acts
}

/// The line being sung (white, up to two rows) and the next sung one (dim), centred.
fn mini_lyrics(ui: &mut egui::Ui, st: &MiniState, pos: u32) {
    ui.add_space(8.0);
    let area = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), MINI_LYRICS_H - 14.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(Layout::top_down(Align::Center)));
    ui.set_clip_rect(area.intersect(ui.clip_rect()));
    ui.spacing_mut().item_spacing.y = 2.0;
    let Some(l) = &st.lyrics else {
        ui.add_space(14.0);
        ui.label(RichText::new(st.lyrics_note).size(13.0).color(MUTED));
        return;
    };
    let line = |i: usize| l.lines.get(i).map(|x| if x.text.is_empty() { "♪" } else { x.text.as_str() });
    let cur = lyrics::current(&l.lines, pos);
    let next = cur.map_or(0, |c| c + 1);
    let now = cur.and_then(line).unwrap_or("♪");
    ui.add(egui::Label::new(RichText::new(now).font(bold(15.0)).color(TEXT)).wrap().selectable(false));
    // Skip instrumental gaps when previewing what comes next.
    if let Some(t) = l.lines.iter().skip(next).find(|x| !x.text.is_empty()) {
        ui.add(egui::Label::new(RichText::new(&t.text).size(13.0).color(DIM)).truncate().selectable(false));
    }
    // Wake up when the next line starts (the player otherwise ticks twice a second).
    if st.playing
        && let Some(n) = l.lines.get(next)
    {
        let wait = n.at_ms.saturating_sub(pos).clamp(16, 500);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(wait as u64));
    }
}

// ---- reusable rows/cards ----------------------------------------------------

/// The window has no visible title bar, so the top strip acts as one: drag to move,
/// double-click to zoom. Registered first, so real widgets in the strip still win.
/// A status message in the middle of the lyrics area.
fn lyrics_note(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_space(ui.max_rect().height() * 0.4);
    ui.label(RichText::new(text).size(16.0).color(MUTED))
}

fn titlebar_drag(ui: &mut egui::Ui) {
    let strip = Rect::from_min_size(ui.max_rect().min, Vec2::new(ui.max_rect().width(), TITLEBAR_H));
    let resp = ui.interact(strip, egui::Id::unique("titlebar_drag"), Sense::click_and_drag());
    if resp.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
}

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
/// Song/episode rows; returns the clicked row.
fn episode_rows(ui: &mut egui::Ui, list: &[TrackRow], now: Option<&str>, playing: bool) -> Option<(usize, RowAct)> {
    let mut hit = None;
    for (i, t) in list.iter().enumerate() {
        let act = song_row(ui, t, None, now == Some(t.uri.as_str()), playing, None);
        if act != RowAct::None {
            hit = Some((i, act));
        }
    }
    hit
}

/// The first `SHELF` items, for a Home shelf.
fn shelf<T>(v: &[T]) -> &[T] {
    &v[..v.len().min(SHELF)]
}

/// Section heading with an optional "View all" link on the right; true when clicked.
fn section_head(ui: &mut egui::Ui, title: &str, view_all: bool) -> bool {
    ui.add_space(18.0);
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(heading(title, 20.0));
        if view_all {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Clear of the scroll bar.
                ui.add_space(10.0);
                let r = ui.add(
                    egui::Label::new(RichText::new("View all").font(bold(13.0)).color(MUTED))
                        .sense(Sense::click())
                        .selectable(false),
                );
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                clicked = r.clicked();
            });
        }
    });
    ui.add_space(6.0);
    clicked
}

/// Lays `n` cells out in rows that fit the width (framed cards can't use
/// `horizontal_wrapped`). Returns the clicked cell and action.
fn grid(ui: &mut egui::Ui, n: usize, cell_w: f32, mut cell: impl FnMut(&mut egui::Ui, usize) -> RowAct) -> Option<(usize, RowAct)> {
    let gap = ui.spacing().item_spacing.x;
    let cols = (((ui.available_width() + gap) / (cell_w + gap)).floor() as usize).max(1);
    let mut hit = None;
    for row in (0..n).collect::<Vec<_>>().chunks(cols) {
        ui.horizontal(|ui| {
            for &i in row {
                let act = cell(ui, i);
                if act != RowAct::None {
                    hit = Some((i, act));
                }
            }
        });
        ui.add_space(12.0);
    }
    hit
}

/// Horizontally scrolling row of artist bubbles; returns the clicked index.
fn artist_row(ui: &mut egui::Ui, id: &str, artists: &[ArtistSummary]) -> Option<usize> {
    let mut hit = None;
    egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, a) in artists.iter().enumerate() {
                if artist_bubble(ui, a.image.as_deref(), &a.name) {
                    hit = Some(i);
                }
            }
        });
    });
    hit
}

/// Horizontally scrolling row of show cards.
fn show_row(ui: &mut egui::Ui, id: &str, shows: &[ShowSummary]) -> Option<(usize, RowAct)> {
    let mut hit = None;
    egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, sh) in shows.iter().enumerate() {
                let sub = format!("{} episodes", sh.total_episodes);
                let act = card(ui, sh.image.as_deref(), &sh.name, &sub, 150.0);
                if act != RowAct::None {
                    hit = Some((i, act));
                }
            }
        });
    });
    hit
}

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
