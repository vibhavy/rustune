//! Background thread: owns the tokio runtime, sign-in, Web API calls and the optional
//! librespot metadata session. Talks to the UI only through `Command`/`Event` channels.
//!
//! Audio is played by the Web Playback SDK (see `sdk.rs`); this side starts playback on
//! the SDK's device through the Web API.

use crate::api::{AlbumSummary, ArtistSummary, Device, PlaylistSummary, SearchResults, ShowSummary, TrackRow, WebApi};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::join_all;
use librespot_core::{
    authentication::Credentials, cache::Cache, config::SessionConfig, session::Session,
    spotify_uri::SpotifyUri,
};
use librespot_metadata::{Metadata, Playlist, Track};
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use std::{
    path::PathBuf,
    sync::Arc,
    sync::mpsc::Sender,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
/// Your Spotify app's Client ID, used for everything except the optional librespot
/// metadata session. Set at build time (see README → Setup), e.g. in a git-ignored
/// `.cargo/config.toml`:
///
/// ```toml
/// [env]
/// RUSTUNE_CLIENT_ID = "your-client-id"
/// ```
///
/// Not a secret: sign-in uses PKCE, so no client secret is involved.
const WEB_CLIENT_ID: &str = match option_env!("RUSTUNE_CLIENT_ID") {
    Some(id) if !id.is_empty() => id,
    _ => panic!("RUSTUNE_CLIENT_ID is not set: add your Spotify Client ID to .cargo/config.toml (see README → Setup)"),
};
/// `streaming` + `user-read-email` are required by the Web Playback SDK.
const WEB_SCOPES: &[&str] = &[
    "streaming",
    "user-read-email",
    "user-read-private",
    "user-library-read",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-read-playback-state",
    "user-modify-playback-state",
    // Home suggestions and the heart button.
    "user-top-read",
    "user-read-recently-played",
    "user-library-modify",
];
/// When Web API refuses a playlist, librespot metadata is used for at most this many tracks.
const FALLBACK_TRACK_LIMIT: usize = 200;

/// Spotify's three repeat states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    /// Repeat the playlist/album (the "context").
    Context,
    /// Repeat the current track.
    Track,
}

impl RepeatMode {
    /// From the Web Playback SDK's `repeat_mode` (0 off, 1 context, 2 track).
    pub fn from_sdk(mode: u8) -> Self {
        match mode {
            1 => Self::Context,
            2 => Self::Track,
            _ => Self::Off,
        }
    }

    /// Button order: off → repeat playlist → repeat one → off.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Context,
            Self::Context => Self::Track,
            Self::Track => Self::Off,
        }
    }

    pub fn api_state(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Repeat: off",
            Self::Context => "Repeat: playlist",
            Self::Track => "Repeat: this song",
        }
    }
}

/// Which list a page of tracks belongs to, so stale responses can be ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewKey {
    Liked,
    Playlist(String),
    Album(String),
    /// A podcast's episodes.
    Show(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    Login,
    Logout,
    /// The SDK asked for an access token.
    SdkToken,
    /// The SDK's Connect device id (None when it disconnects).
    SetDevice(Option<String>),
    Shuffle(bool),
    Repeat(RepeatMode),
    LoadPlaylists,
    LoadTracks { view: ViewKey, offset: u32, album_name: String },
    Search(String),
    /// Play a context (playlist/album/artist/collection), optionally starting at a track.
    PlayContext { context_uri: String, track_uri: Option<String> },
    /// Play an ad-hoc list of tracks (e.g. search results).
    PlayTracks { uris: Vec<String>, index: u32 },
    /// Pull whatever is playing elsewhere onto this Mac.
    TransferHere,
    LoadDevices,
    TransferTo(String),
    /// Suggestions for the Home screen.
    LoadHome,
    /// Album / same-artist / similar tracks for the song page.
    LoadSong(TrackRow),
    LoadQueue,
    CheckSaved(String),
    SetSaved { uri: String, saved: bool },
    /// Lyrics for a song (from LRCLIB, not Spotify, so no sign-in needed).
    LoadLyrics(crate::lyrics::Query),
}

#[derive(Debug)]
pub enum HomeSection {
    Recent(Vec<TrackRow>),
    TopTracks(Vec<TrackRow>),
    TopArtists(Vec<ArtistSummary>),
    BecauseYouLike { artist: String, tracks: Vec<TrackRow> },
    /// "More from {artist}" for one of the user's top artists.
    MoreFrom { artist: String, tracks: Vec<TrackRow> },
    /// Albums by the user's top artists.
    Albums(Vec<AlbumSummary>),
    /// Podcasts the user follows (sent even when empty, so the UI stops waiting).
    Shows(Vec<ShowSummary>),
    /// The newest episode of each followed show, newest first.
    NewEpisodes(Vec<TrackRow>),
    SavedEpisodes(Vec<TrackRow>),
    /// Artists who work with the user's top artists, and some of their songs.
    Discover { artists: Vec<ArtistSummary>, tracks: Vec<TrackRow> },
    /// Albums released in the last two weeks (Spotify's `tag:new`).
    NewReleases(Vec<AlbumSummary>),
    /// Shows that come up when searching for one the user follows.
    ShowsLike { seed: String, shows: Vec<ShowSummary> },
}

#[derive(Debug, Clone)]
pub enum SongSection {
    Album(Vec<TrackRow>),
    Artist(Vec<TrackRow>),
    ArtistAlbums(Vec<AlbumSummary>),
    /// Tracks from the primary artist's genre (empty genre = none found).
    Similar { genre: String, tracks: Vec<TrackRow> },
}

#[derive(Debug)]
pub enum Event {
    NeedsLogin,
    LoggingIn { browser: bool },
    LoggedIn { username: String, display_name: String },
    Error(String),
    Playlists(Vec<PlaylistSummary>),
    Tracks { view: ViewKey, tracks: Vec<TrackRow>, append: bool, next_offset: Option<u32> },
    /// Results tagged with their query, so the UI can drop ones for older queries.
    SearchResults { query: String, results: SearchResults },
    Devices(Vec<Device>),
    SdkToken(String),
    Home(HomeSection),
    Song { uri: String, section: SongSection },
    Queue(Vec<TrackRow>),
    Saved { uri: String, saved: bool },
    /// Spotify no longer knows this Mac's player (it drops it after the Mac sleeps), so
    /// this command failed. The UI reconnects the player and sends it again.
    DeviceLost(Command),
    /// `Ok(None)`: the song has no lyrics.
    Lyrics { uri: String, result: Result<Option<crate::lyrics::Lyrics>, String> },
}

/// Sends events to the UI and wakes it up.
#[derive(Clone)]
struct Ui {
    tx: Sender<Event>,
    ctx: eframe::egui::Context,
}

impl Ui {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
    fn error(&self, e: impl std::fmt::Display) {
        log::warn!("{e:#}");
        self.send(Event::Error(format!("{e:#}")));
    }
}

struct Conn {
    web: WebAuth,
    /// librespot session, only used to list songs in playlists the Web API hides.
    session: Option<Session>,
}

/// Web API access token, refreshed on demand from the saved refresh token.
#[derive(Clone)]
struct WebAuth(Arc<tokio::sync::Mutex<WebToken>>);

struct WebToken {
    client_id: String,
    access: String,
    refresh: String,
    expires_at: Instant,
}

impl WebAuth {
    fn from_oauth(client_id: &str, t: OAuthToken) -> Self {
        save_refresh_token(client_id, &t.refresh_token);
        Self(Arc::new(tokio::sync::Mutex::new(WebToken {
            client_id: client_id.to_string(),
            access: t.access_token,
            refresh: t.refresh_token,
            expires_at: t.expires_at,
        })))
    }

    /// The saved file is "<client id>\n<refresh token>"; a token minted for a
    /// different Client ID can't be refreshed with this one, so it is ignored.
    async fn from_saved(client_id: &str) -> Result<Self> {
        let saved = std::fs::read_to_string(refresh_token_path())
            .context("no saved sign-in")?;
        let (saved_id, refresh) = saved
            .split_once('\n')
            .filter(|(id, _)| *id == client_id)
            .context("saved sign-in was for a different Client ID")?;
        let refresh = refresh.trim();
        let mut t = web_oauth_client(client_id)?
            .refresh_token_async(refresh)
            .await
            .map_err(|e| anyhow!("token refresh failed: {e}"))?;
        if t.refresh_token.is_empty() {
            t.refresh_token = refresh.to_string();
        }
        Ok(Self::from_oauth(saved_id, t))
    }

    async fn token(&self) -> Result<String> {
        let mut g = self.0.lock().await;
        if g.expires_at > Instant::now() + Duration::from_secs(60) {
            return Ok(g.access.clone());
        }
        let t = web_oauth_client(&g.client_id)?
            .refresh_token_async(&g.refresh)
            .await
            .map_err(|e| anyhow!("token refresh failed: {e}"))?;
        if !t.refresh_token.is_empty() {
            g.refresh = t.refresh_token;
            save_refresh_token(&g.client_id, &g.refresh);
        }
        g.access = t.access_token;
        g.expires_at = t.expires_at;
        Ok(g.access.clone())
    }
}

/// Bumped whenever WEB_SCOPES grows: tokens saved earlier lack the new scopes, so the
/// old file is deliberately not read and the user signs in once more.
/// (v2: streaming for the SDK, v3: top items / recently played / library writes.)
fn refresh_token_path() -> PathBuf {
    cache_dir().join("web_token_v3")
}

fn save_refresh_token(client_id: &str, token: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::create_dir_all(cache_dir());
    let res = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(refresh_token_path())
        .and_then(|mut f| f.write_all(format!("{client_id}\n{token}").as_bytes()));
    if let Err(e) = res {
        log::warn!("could not save refresh token: {e}");
    }
}

fn oauth_client(client_id: &str, scopes: &[&str]) -> Result<OAuthClient> {
    OAuthClientBuilder::new(client_id, REDIRECT_URI, scopes.to_vec())
        .open_in_browser()
        .with_custom_message("Signed in to rustune. You can close this tab.")
        .build()
        .map_err(|e| anyhow!("{e}"))
}

fn web_oauth_client(client_id: &str) -> Result<OAuthClient> {
    oauth_client(client_id, WEB_SCOPES)
}

/// Starts the backend thread. Also returns its runtime, which the cover loader shares.
pub fn spawn(ctx: eframe::egui::Context, tx: Sender<Event>) -> (UnboundedSender<Command>, tokio::runtime::Handle) {
    let (cmd_tx, cmd_rx) = unbounded_channel();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    std::thread::Builder::new()
        .name("rustune-backend".into())
        .spawn(move || rt.block_on(run(Ui { tx, ctx }, cmd_rx)))
        .expect("spawn backend thread");
    (cmd_tx, handle)
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rustune")
}

/// librespot's credential cache (no audio cache any more: the SDK streams audio).
fn open_cache() -> Result<Cache> {
    let dir = cache_dir();
    Ok(Cache::new(Some(&dir), None, None, None)?)
}

async fn run(ui: Ui, mut rx: UnboundedReceiver<Command>) {
    let api = WebApi::new();
    let mut conn: Option<Conn> = None;
    let mut device: Option<String> = None;
    let mut token_pending = false;

    // Try the saved sign-in silently at startup.
    if let Ok(web) = WebAuth::from_saved(WEB_CLIENT_ID).await {
        ui.send(Event::LoggingIn { browser: false });
        let session = metadata_session(open_cache().ok().and_then(|c| c.credentials())).await;
        let c = Conn { web, session };
        after_login(&ui, &api, &c, &mut token_pending).await;
        conn = Some(c);
    } else {
        ui.send(Event::NeedsLogin);
    }

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Login => {
                ui.send(Event::LoggingIn { browser: true });
                match login().await {
                    Ok((creds, web)) => {
                        let c = Conn { web, session: metadata_session(creds).await };
                        after_login(&ui, &api, &c, &mut token_pending).await;
                        conn = Some(c);
                    }
                    Err(e) => {
                        ui.error(e);
                        ui.send(Event::NeedsLogin);
                    }
                }
            }
            Command::Logout => {
                if let Some(Conn { session: Some(s), .. }) = conn.take() {
                    s.shutdown();
                }
                let _ = std::fs::remove_file(cache_dir().join("credentials.json"));
                let _ = std::fs::remove_file(refresh_token_path());
                let _ = std::fs::remove_dir_all(crate::covers::dir());
                let _ = std::fs::remove_dir_all(crate::lyrics::dir());
                ui.send(Event::NeedsLogin);
            }
            Command::SetDevice(d) => device = d,
            Command::LoadLyrics(q) => {
                let ui = ui.clone();
                tokio::spawn(async move {
                    let result = crate::lyrics::fetch(&q).await.map_err(|e| {
                        log::warn!("lyrics for {}: {e:#}", q.uri);
                        format!("{e:#}")
                    });
                    ui.send(Event::Lyrics { uri: q.uri, result });
                });
            }
            Command::SdkToken => match &conn {
                Some(c) => send_sdk_token(&ui, &c.web).await,
                // Answered right after sign-in.
                None => token_pending = true,
            },
            other => match &conn {
                Some(c) => {
                    let (ui, api, web, session, device) = (
                        ui.clone(),
                        api.clone(),
                        c.web.clone(),
                        c.session.clone(),
                        device.clone(),
                    );
                    tokio::spawn(async move {
                        let replay = matches!(
                            other,
                            Command::PlayContext { .. } | Command::PlayTracks { .. } | Command::TransferHere
                        )
                        .then(|| other.clone());
                        let res = web_command(&ui, &api, &web, session.as_ref(), device, other).await;
                        if let Err(e) = &res
                            && crate::api::has_status(e, reqwest::StatusCode::NOT_FOUND)
                            && let Some(cmd) = replay
                        {
                            log::warn!("player device unknown to Spotify ({e:#}); reconnecting");
                            ui.send(Event::DeviceLost(cmd));
                        } else if let Err(e) = res {
                            ui.error(e);
                        }
                    });
                }
                None => ui.send(Event::NeedsLogin),
            },
        }
    }
}

async fn send_sdk_token(ui: &Ui, web: &WebAuth) {
    match web.token().await {
        Ok(t) => ui.send(Event::SdkToken(t)),
        Err(e) => ui.error(e),
    }
}

/// Main sign-in uses rustune's Client ID. The librespot login (Spotify's desktop client
/// ID) is reused from cache when present; it is only needed for the playlist fallback,
/// so it is requested in the browser only if it was never granted.
async fn login() -> Result<(Option<Credentials>, WebAuth)> {
    let t = web_oauth_client(WEB_CLIENT_ID)?
        .get_access_token_async()
        .await
        .map_err(|e| anyhow!("Spotify sign-in failed: {e}"))?;
    let web = WebAuth::from_oauth(WEB_CLIENT_ID, t);
    let creds = match open_cache().ok().and_then(|c| c.credentials()) {
        Some(c) => Some(c),
        None => oauth_client(&SessionConfig::default().client_id, &["streaming"])?
            .get_access_token_async()
            .await
            .map(|t| Credentials::with_access_token(t.access_token))
            .map_err(|e| log::warn!("metadata sign-in skipped: {e}"))
            .ok(),
    };
    Ok((creds, web))
}

/// Connects the librespot session used for the playlist fallback. Failure is not fatal.
async fn metadata_session(creds: Option<Credentials>) -> Option<Session> {
    let creds = creds?;
    let session = Session::new(SessionConfig::default(), open_cache().ok());
    match session.connect(creds, true).await {
        Ok(()) => Some(session),
        Err(e) => {
            log::warn!("librespot metadata session unavailable: {e}");
            None
        }
    }
}

async fn after_login(ui: &Ui, api: &WebApi, c: &Conn, token_pending: &mut bool) {
    let me = match c.web.token().await {
        Ok(t) => api.me(&t).await,
        Err(e) => Err(e),
    };
    match me {
        Ok(me) => {
            let display_name = me.display_name.unwrap_or_else(|| me.id.clone());
            ui.send(Event::LoggedIn { username: me.id, display_name });
        }
        Err(e) => {
            ui.error(format!("Could not load your profile: {e:#}"));
            ui.send(Event::NeedsLogin);
            return;
        }
    }
    if std::mem::take(token_pending) {
        send_sdk_token(ui, &c.web).await;
    }
}

async fn web_command(
    ui: &Ui,
    api: &WebApi,
    web: &WebAuth,
    session: Option<&Session>,
    device: Option<String>,
    cmd: Command,
) -> Result<()> {
    let t = web.token().await?;
    let device_id = || device.clone().context("The player is still starting, try again in a moment");
    match cmd {
        Command::LoadPlaylists => ui.send(Event::Playlists(api.my_playlists(&t).await?)),
        Command::LoadTracks { view, offset, album_name } => {
            let page = match &view {
                ViewKey::Liked => api.liked_tracks(&t, offset).await?,
                ViewKey::Album(id) => api.album_tracks(&t, id, &album_name, offset).await?,
                // `album_name` carries the show's name here.
                ViewKey::Show(id) => api.show_episodes(&t, id, &album_name, offset).await?,
                ViewKey::Playlist(id) => match api.playlist_tracks(&t, id, offset).await {
                    Ok(p) => p,
                    // Web API hides contents of playlists you don't own (and Spotify's own
                    // editorial ones). librespot's metadata API can still read them.
                    Err(e) if offset == 0 => match session {
                        Some(s) => {
                            log::info!("Web API playlist read failed ({e:#}); using librespot metadata");
                            crate::api::TrackPage {
                                tracks: playlist_via_metadata(s, id).await?,
                                next_offset: None,
                            }
                        }
                        None => bail!(
                            "Spotify only shows songs of playlists you own here. Press Play to play it."
                        ),
                    },
                    Err(e) => return Err(e),
                },
            };
            ui.send(Event::Tracks {
                view,
                tracks: page.tracks,
                append: offset > 0,
                next_offset: page.next_offset,
            });
        }
        Command::Search(q) => {
            let results = api.search(&t, &q).await?;
            ui.send(Event::SearchResults { query: q, results });
        }
        Command::LoadDevices => ui.send(Event::Devices(api.devices(&t).await?)),
        Command::TransferTo(id) => api.transfer(&t, &id).await?,
        Command::TransferHere => api.transfer(&t, &device_id()?).await?,
        Command::PlayContext { context_uri, track_uri } => {
            let mut body = serde_json::json!({ "context_uri": context_uri });
            if let Some(uri) = track_uri {
                body["offset"] = serde_json::json!({ "uri": uri });
            }
            let started = Instant::now();
            api.play(&t, &device_id()?, &body).await?;
            log::info!("play request accepted in {:?}", started.elapsed());
        }
        Command::PlayTracks { uris, index } => {
            let body = serde_json::json!({ "uris": uris, "offset": { "position": index } });
            let started = Instant::now();
            api.play(&t, &device_id()?, &body).await?;
            log::info!("play request accepted in {:?}", started.elapsed());
        }
        Command::LoadHome => {
            tokio::join!(load_home(ui, api, &t), load_podcasts(ui, api, &t));
        }
        Command::LoadSong(track) => load_song(ui, api, &t, track).await,
        Command::LoadQueue => ui.send(Event::Queue(api.queue(&t).await?)),
        Command::CheckSaved(uri) => {
            let saved = api.is_saved(&t, &uri).await?;
            ui.send(Event::Saved { uri, saved });
        }
        Command::SetSaved { uri, saved } => {
            api.set_saved(&t, &uri, saved).await?;
            ui.send(Event::Saved { uri, saved });
        }
        Command::Shuffle(on) => api.shuffle(&t, &device_id()?, on).await?,
        Command::Repeat(mode) => api.repeat(&t, &device_id()?, mode.api_state()).await?,
        Command::Login | Command::Logout | Command::SdkToken | Command::SetDevice(_) | Command::LoadLyrics(_) => {}
    }
    Ok(())
}

/// Home's podcast sections: followed shows, their newest episodes, saved episodes.
async fn load_podcasts(ui: &Ui, api: &WebApi, t: &str) {
    let (shows, saved) = tokio::join!(api.saved_shows(t), api.saved_episodes(t));
    let shows = shows.unwrap_or_else(|e| {
        log::warn!("saved shows: {e:#}");
        Vec::new()
    });
    match saved {
        Ok(v) => ui.send(Event::Home(HomeSection::SavedEpisodes(v))),
        Err(e) => log::warn!("saved episodes: {e:#}"),
    }
    ui.send(Event::Home(HomeSection::Shows(shows.clone())));
    let seeds: Vec<&ShowSummary> = shows.iter().take(3).collect();
    let (latest, found) = tokio::join!(
        join_all(shows.iter().take(12).map(|s| api.latest_episode(t, s))),
        join_all(seeds.iter().map(|s| api.search_shows(t, &s.name))),
    );
    // Searching for a followed show's name brings up similar shows.
    let mut seen: std::collections::HashSet<String> = shows.iter().map(|s| s.id.clone()).collect();
    for (seed, res) in seeds.iter().zip(found) {
        match res {
            Ok(list) => {
                let similar = similar_shows(&seed.name, list, &mut seen);
                if !similar.is_empty() {
                    ui.send(Event::Home(HomeSection::ShowsLike { seed: seed.name.clone(), shows: similar }));
                }
            }
            Err(e) => log::warn!("shows like {}: {e:#}", seed.name),
        }
    }
    let mut episodes: Vec<(String, TrackRow)> = latest
        .into_iter()
        .filter_map(|r| r.map_err(|e| log::warn!("latest episode: {e:#}")).ok().flatten())
        .collect();
    episodes.sort_by(|a, b| b.0.cmp(&a.0));
    ui.send(Event::Home(HomeSection::NewEpisodes(episodes.into_iter().map(|(_, e)| e).collect())));
}

/// Artists who appear on the given songs' credits but aren't among `known`, most
/// frequent first (ties keep first-seen order), at most `max`.
fn collaborators<'a>(credits: impl Iterator<Item = &'a [String]>, known: &[String], max: usize) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for name in credits.flatten() {
        if known.iter().any(|k| k.eq_ignore_ascii_case(name)) {
            continue;
        }
        match counts.iter_mut().find(|(n, _)| n == name) {
            Some((_, c)) => *c += 1,
            None => counts.push((name.clone(), 1)),
        }
    }
    // Stable sort keeps first-seen order among equal counts.
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    counts.into_iter().take(max).map(|(n, _)| n).collect()
}

/// Search results for a followed show, minus shows already followed or shown, minus
/// the show's own spin-offs (names that contain, or are contained in, its name), and
/// minus single-episode uploads (usually clips of other shows).
fn similar_shows(seed: &str, found: Vec<ShowSummary>, seen: &mut std::collections::HashSet<String>) -> Vec<ShowSummary> {
    let seed = seed.trim().to_lowercase();
    found
        .into_iter()
        .filter(|s| {
            let name = s.name.trim().to_lowercase();
            !name.contains(&seed) && !seed.contains(&name) && s.total_episodes > 1
        })
        .filter(|s| seen.insert(s.id.clone()))
        .collect()
}

/// Spotify retired its recommendations endpoint for new apps, so suggestions are built
/// from what is still available: recently played, top tracks/artists, and genre search.
async fn load_home(ui: &Ui, api: &WebApi, t: &str) {
    let (recent, top, artists) =
        tokio::join!(api.recently_played(t), api.top_tracks(t), api.top_artists(t));
    match recent {
        Ok(v) => ui.send(Event::Home(HomeSection::Recent(v))),
        Err(e) => log::warn!("recently played: {e:#}"),
    }
    match top {
        Ok(v) => ui.send(Event::Home(HomeSection::TopTracks(v))),
        Err(e) => log::warn!("top tracks: {e:#}"),
    }
    let artists = match artists {
        Ok(a) => a,
        Err(e) => return log::warn!("top artists: {e:#}"),
    };
    ui.send(Event::Home(HomeSection::TopArtists(
        artists.iter().map(|(a, _)| a.clone()).collect(),
    )));
    // Artist-based suggestions: Spotify no longer returns genres or related artists, so
    // these come from search. Each top artist's songs list their collaborators, which
    // seed "Artists you might like".
    let names: Vec<String> = artists.iter().map(|(a, _)| a.name.clone()).collect();
    let seeds: Vec<String> = names.iter().take(4).cloned().collect();
    let album_queries: Vec<String> = names.iter().take(3).map(|n| format!("artist:\"{n}\"")).collect();
    let song_queries: Vec<String> = seeds.iter().map(|n| format!("artist:\"{n}\"")).collect();
    let (credits, albums, new) = tokio::join!(
        join_all(song_queries.iter().map(|q| api.search_track_credits(t, q))),
        join_all(album_queries.iter().map(|q| api.search_albums(t, q))),
        api.search_albums(t, "tag:new"),
    );
    let credits: Vec<Vec<(TrackRow, Vec<String>)>> = credits
        .into_iter()
        .zip(&seeds)
        .map(|(r, n)| r.unwrap_or_else(|e| {
            log::warn!("songs by {n}: {e:#}");
            Vec::new()
        }))
        .collect();
    for (artist, list) in seeds.iter().zip(&credits).take(2) {
        if !list.is_empty() {
            ui.send(Event::Home(HomeSection::MoreFrom {
                artist: artist.clone(),
                tracks: list.iter().map(|(t, _)| t.clone()).collect(),
            }));
        }
    }
    // Interleave a few albums from each artist.
    let lists: Vec<Vec<AlbumSummary>> = albums.into_iter().filter_map(|r| r.ok()).collect();
    let mut mixed = Vec::new();
    for i in 0..4 {
        for l in &lists {
            if let Some(a) = l.get(i) {
                mixed.push(a.clone());
            }
        }
    }
    if !mixed.is_empty() {
        ui.send(Event::Home(HomeSection::Albums(mixed)));
    }
    match new {
        Ok(v) if !v.is_empty() => ui.send(Event::Home(HomeSection::NewReleases(v))),
        Ok(_) => {}
        Err(e) => log::warn!("new releases: {e:#}"),
    }

    let picks = collaborators(credits.iter().flatten().map(|(_, a)| a.as_slice()), &names, 6);
    if !picks.is_empty() {
        let pick_queries: Vec<String> = picks.iter().take(3).map(|n| format!("artist:\"{n}\"")).collect();
        let (found, songs) = tokio::join!(
            join_all(picks.iter().map(|n| api.find_artist(t, n))),
            join_all(pick_queries.iter().map(|q| api.search_tracks(t, q))),
        );
        let artists: Vec<ArtistSummary> = found.into_iter().filter_map(|r| r.ok().flatten()).collect();
        let lists: Vec<Vec<TrackRow>> = songs.into_iter().filter_map(|r| r.ok()).collect();
        let known = |t: &TrackRow| names.iter().any(|n| t.artists.split(", ").next() == Some(n.as_str()));
        let mut tracks: Vec<TrackRow> = Vec::new();
        for i in 0..4 {
            for l in &lists {
                if let Some(tr) = l.get(i).filter(|tr| !known(tr) && !tracks.iter().any(|x| x.uri == tr.uri)) {
                    tracks.push(tr.clone());
                }
            }
        }
        ui.send(Event::Home(HomeSection::Discover { artists, tracks }));
    }

    if let Some((artist, genres)) = artists.iter().find(|(_, g)| !g.is_empty()) {
        match api.search_tracks(t, &format!("genre:\"{}\"", genres[0])).await {
            Ok(tracks) => {
                let tracks = tracks
                    .into_iter()
                    .filter(|tr| !tr.artists.contains(&artist.name))
                    .collect();
                ui.send(Event::Home(HomeSection::BecauseYouLike {
                    artist: artist.name.clone(),
                    tracks,
                }));
            }
            Err(e) => log::warn!("genre suggestions: {e:#}"),
        }
    }
}

/// The three song-page sections, fetched concurrently and sent as each arrives.
async fn load_song(ui: &Ui, api: &WebApi, t: &str, track: TrackRow) {
    let uri = track.uri.clone();
    let artist = track.artists.split(", ").next().unwrap_or_default().to_string();
    let album = async {
        match &track.album_id {
            Some(id) => api.album_tracks(t, id, &track.album, 0).await.map(|p| p.tracks),
            None => Ok(Vec::new()),
        }
    };
    let artist_query = format!("artist:\"{artist}\"");
    let by_artist = api.search_tracks(t, &artist_query);
    let genres = async {
        match &track.artist_id {
            Some(id) => api.artist_genres(t, id).await,
            None => Ok(Vec::new()),
        }
    };
    let artist_albums = api.search_albums(t, &artist_query);
    let (album, by_artist, genres, artist_albums) = tokio::join!(album, by_artist, genres, artist_albums);

    let song = |section| ui.send(Event::Song { uri: uri.clone(), section });
    song(SongSection::Album(album.unwrap_or_else(|e| {
        log::warn!("album tracks: {e:#}");
        Vec::new()
    })));
    song(SongSection::Artist(
        by_artist
            .unwrap_or_else(|e| {
                log::warn!("artist tracks: {e:#}");
                Vec::new()
            })
            .into_iter()
            .filter(|x| x.uri != uri)
            .collect(),
    ));
    song(SongSection::ArtistAlbums(
        artist_albums
            .unwrap_or_else(|e| {
                log::warn!("artist albums: {e:#}");
                Vec::new()
            })
            .into_iter()
            .filter(|a| Some(&a.id) != track.album_id.as_ref())
            .collect(),
    ));
    let genre = genres.ok().and_then(|g| g.into_iter().next()).unwrap_or_default();
    let similar = if genre.is_empty() {
        Vec::new()
    } else {
        api.search_tracks(t, &format!("genre:\"{genre}\""))
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|x| !x.artists.contains(&artist))
            .collect()
    };
    song(SongSection::Similar { genre, tracks: similar });
}

async fn playlist_via_metadata(session: &Session, id: &str) -> Result<Vec<TrackRow>> {
    let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))?;
    let playlist = Playlist::get(session, &uri).await?;
    let ids: Vec<SpotifyUri> = playlist.tracks().take(FALLBACK_TRACK_LIMIT).cloned().collect();

    let mut set = tokio::task::JoinSet::new();
    for (i, tid) in ids.into_iter().enumerate() {
        let session = session.clone();
        set.spawn(async move { (i, Track::get(&session, &tid).await) });
    }
    let mut rows: Vec<(usize, TrackRow)> = Vec::new();
    while let Some(Ok((i, res))) = set.join_next().await {
        if let Ok(t) = res {
            rows.push((
                i,
                TrackRow {
                    uri: t.id.to_uri()?,
                    name: t.name,
                    artists: t
                        .artists
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    album: t.album.name,
                    duration_ms: t.duration.max(0) as u32,
                    image: None,
                    cover: None,
                    album_id: None,
                    artist_id: None,
                },
            ));
        }
    }
    rows.sort_by_key(|(i, _)| *i);
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn collaborators_rank_by_frequency_and_skip_known() {
        let credits = [names(&["Drake", "Future"]), names(&["Drake", "21 Savage", "Future"]), names(&["Wizkid", "drake"])];
        let got = collaborators(credits.iter().map(|c| c.as_slice()), &names(&["Drake"]), 2);
        assert_eq!(got, names(&["Future", "21 Savage"]));
    }

    fn show(id: &str, name: &str) -> ShowSummary {
        ShowSummary { id: id.into(), uri: format!("spotify:show:{id}"), name: name.into(), total_episodes: 10, image: None }
    }

    #[test]
    fn similar_shows_drop_followed_and_spin_offs() {
        let mut seen: std::collections::HashSet<String> = ["followed".to_string()].into();
        let found = vec![
            show("followed", "Some Show I Follow"),
            show("a", "Figuring Out with Raj Shamani"),
            show("b", "Raj Shamani"),
            show("c", "The Diary Of A CEO"),
            show("c", "The Diary Of A CEO"),
            ShowSummary { total_episodes: 1, ..show("d", "Raj Shamani 's Figuring Out clip") },
        ];
        let got = similar_shows("Figuring Out with Raj Shamani", found, &mut seen);
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["c"]);
    }
}
