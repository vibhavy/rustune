//! Minimal Spotify Web API client: library, search, suggestions and playback control.
//! Audio itself is played by the Web Playback SDK (see `sdk.rs`).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;

const BASE: &str = "https://api.spotify.com/v1";

/// A Web API call that came back with an error status.
#[derive(Debug)]
pub struct ApiError {
    pub status: reqwest::StatusCode,
    body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Spotify API {}: {}", self.status, self.body)
    }
}

impl std::error::Error for ApiError {}

/// True if `e` is a Web API error with this status.
pub fn has_status(e: &anyhow::Error, status: reqwest::StatusCode) -> bool {
    e.downcast_ref::<ApiError>().is_some_and(|a| a.status == status)
}

#[derive(Clone, Debug)]
pub struct TrackRow {
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u32,
    /// Small (~64px) album art.
    pub image: Option<String>,
    /// ~300px album art for cards and the song page.
    pub cover: Option<String>,
    pub album_id: Option<String>,
    /// Primary artist.
    pub artist_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PlaylistSummary {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub owner: String,
    /// ~300px cover for cards.
    pub image: Option<String>,
    /// ~64px cover for the sidebar.
    pub thumb: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AlbumSummary {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub image: Option<String>,
}

/// A podcast the user follows.
#[derive(Clone, Debug)]
pub struct ShowSummary {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub total_episodes: u32,
    pub image: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ArtistSummary {
    pub uri: String,
    pub name: String,
    pub image: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub tracks: Vec<TrackRow>,
    pub albums: Vec<AlbumSummary>,
    pub artists: Vec<ArtistSummary>,
    pub playlists: Vec<PlaylistSummary>,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_active: bool,
}

/// One page of tracks plus the offset of the next page, if any.
pub struct TrackPage {
    pub tracks: Vec<TrackRow>,
    pub next_offset: Option<u32>,
}

// ---- raw JSON shapes -------------------------------------------------------

#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    items: Vec<Option<T>>,
    next: Option<String>,
    #[serde(default)]
    offset: u32,
}

#[derive(Deserialize)]
struct Image {
    url: String,
    width: Option<u32>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
    id: Option<String>,
}

#[derive(Deserialize)]
struct RawAlbumRef {
    id: Option<String>,
    name: String,
    images: Option<Vec<Image>>,
}

#[derive(Deserialize)]
struct RawShow {
    id: String,
    uri: String,
    name: String,
    #[serde(default)]
    total_episodes: u32,
    images: Option<Vec<Image>>,
}

#[derive(Deserialize)]
struct SavedShow {
    show: Option<RawShow>,
}

#[derive(Deserialize)]
struct ShowRef {
    name: String,
}

#[derive(Deserialize)]
struct RawEpisode {
    uri: String,
    name: String,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    release_date: String,
    is_playable: Option<bool>,
    images: Option<Vec<Image>>,
    /// Only present when the episode isn't listed under its show.
    show: Option<ShowRef>,
}

#[derive(Deserialize)]
struct SavedEpisode {
    episode: Option<RawEpisode>,
}

#[derive(Deserialize)]
struct RawTrack {
    uri: String,
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
    album: Option<RawAlbumRef>,
    #[serde(default)]
    duration_ms: u32,
}

#[derive(Deserialize)]
struct Owner {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct RawPlaylist {
    id: String,
    uri: String,
    name: String,
    owner: Option<Owner>,
    images: Option<Vec<Image>>,
}

#[derive(Deserialize)]
struct RawAlbum {
    id: String,
    uri: String,
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
    images: Option<Vec<Image>>,
}

#[derive(Deserialize)]
struct RawArtist {
    uri: String,
    name: String,
    images: Option<Vec<Image>>,
    #[serde(default)]
    genres: Vec<String>,
}

#[derive(Deserialize)]
struct PlayedItem {
    track: Option<RawTrack>,
}

#[derive(Deserialize)]
struct RawQueue {
    #[serde(default)]
    queue: Vec<Option<RawTrack>>,
}

#[derive(Deserialize)]
struct SavedItem {
    track: Option<RawTrack>,
}

/// Playlist entries: Spotify renamed `track` to `item` in Feb 2026; accept both.
#[derive(Deserialize)]
struct PlaylistItem {
    #[serde(alias = "track")]
    item: Option<RawTrack>,
}

#[derive(Deserialize)]
struct RawSearch {
    tracks: Option<Page<RawTrack>>,
    albums: Option<Page<RawAlbum>>,
    artists: Option<Page<RawArtist>>,
    playlists: Option<Page<RawPlaylist>>,
    shows: Option<Page<RawShow>>,
}

#[derive(Deserialize)]
struct RawDevices {
    devices: Vec<RawDevice>,
}

#[derive(Deserialize)]
struct RawDevice {
    id: Option<String>,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    is_active: bool,
}

#[derive(Deserialize)]
pub struct Me {
    pub id: String,
    pub display_name: Option<String>,
}

// ---- conversions -----------------------------------------------------------

fn join_names(v: &[Named]) -> String {
    v.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// Pick the image whose width is closest to `target`, so we don't decode 640px
/// covers just to draw them at 40px (decoded textures dominate memory).
fn pick_image(v: &Option<Vec<Image>>, target: u32) -> Option<String> {
    v.as_ref()?
        .iter()
        .min_by_key(|i| i.width.unwrap_or(300).abs_diff(target))
        .map(|i| i.url.clone())
}

fn first_image(v: Option<Vec<Image>>) -> Option<String> {
    pick_image(&v, 300)
}

impl From<RawTrack> for TrackRow {
    fn from(t: RawTrack) -> Self {
        let (album, image, cover, album_id) = t
            .album
            .map(|a| (a.name, pick_image(&a.images, 64), pick_image(&a.images, 300), a.id))
            .unwrap_or_default();
        TrackRow {
            artists: join_names(&t.artists),
            artist_id: t.artists.first().and_then(|a| a.id.clone()),
            album,
            image,
            cover,
            album_id,
            uri: t.uri,
            name: t.name,
            duration_ms: t.duration_ms,
        }
    }
}

impl From<RawShow> for ShowSummary {
    fn from(s: RawShow) -> Self {
        ShowSummary {
            image: first_image(s.images),
            id: s.id,
            uri: s.uri,
            name: s.name,
            total_episodes: s.total_episodes,
        }
    }
}

/// Episodes reuse `TrackRow`: the show goes where the artist would, the release date
/// where the album would. They have no album or artist ids.
fn episode_row(e: RawEpisode, show: &str) -> TrackRow {
    TrackRow {
        artists: e.show.map(|s| s.name).unwrap_or_else(|| show.to_string()),
        album: fmt_date(&e.release_date),
        image: pick_image(&e.images, 64),
        cover: pick_image(&e.images, 300),
        album_id: None,
        artist_id: None,
        uri: e.uri,
        name: e.name,
        duration_ms: e.duration_ms,
    }
}

/// "2026-11-14" → "14 Nov 2026"; other precisions ("2026", "2026-11") pass through.
fn fmt_date(d: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut it = d.splitn(3, '-');
    match (it.next(), it.next().and_then(|m| m.parse::<usize>().ok()), it.next().and_then(|x| x.parse::<u32>().ok())) {
        (Some(y), Some(m @ 1..=12), Some(day)) => format!("{day} {} {y}", MONTHS[m - 1]),
        _ => d.to_string(),
    }
}

fn album_summary(a: RawAlbum) -> AlbumSummary {
    AlbumSummary {
        artists: join_names(&a.artists),
        image: first_image(a.images),
        id: a.id,
        uri: a.uri,
        name: a.name,
    }
}

fn artist_with_genres(a: RawArtist) -> (ArtistSummary, Vec<String>) {
    (
        ArtistSummary {
            image: pick_image(&a.images, 300),
            uri: a.uri,
            name: a.name,
        },
        a.genres,
    )
}

impl From<RawPlaylist> for PlaylistSummary {
    fn from(p: RawPlaylist) -> Self {
        PlaylistSummary {
            owner: p.owner.and_then(|o| o.display_name).unwrap_or_default(),
            thumb: pick_image(&p.images, 64),
            image: first_image(p.images),
            id: p.id,
            uri: p.uri,
            name: p.name,
        }
    }
}

fn next_offset<T>(page: &Page<T>) -> Option<u32> {
    page.next
        .as_ref()
        .map(|_| page.offset + page.items.len() as u32)
}

// ---- client ----------------------------------------------------------------

#[derive(Clone)]
pub struct WebApi {
    http: reqwest::Client,
}

impl WebApi {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client"),
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        // One retry on 429, honouring Retry-After.
        for attempt in 0..2 {
            let r = req.try_clone().context("request not cloneable")?.build()?;
            let what = format!("{} {}", r.method(), r.url().path());
            let started = std::time::Instant::now();
            let resp = self.http.execute(r).await.with_context(|| what.clone())?;
            log::debug!("{what} -> {} in {:?}", resp.status(), started.elapsed());
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt == 0 {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2);
                log::warn!("{what} rate limited, Retry-After {wait}s");
                tokio::time::sleep(Duration::from_secs(wait.min(30))).await;
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ApiError { status, body: body.chars().take(200).collect() }.into());
            }
            return Ok(resp);
        }
        bail!("Spotify API rate limited")
    }

    async fn get<T: DeserializeOwned>(&self, token: &str, url: &str) -> Result<T> {
        let resp = self.send(self.http.get(url).bearer_auth(token)).await?;
        Ok(resp.json().await?)
    }

    pub async fn me(&self, token: &str) -> Result<Me> {
        self.get(token, &format!("{BASE}/me")).await
    }

    pub async fn my_playlists(&self, token: &str) -> Result<Vec<PlaylistSummary>> {
        let mut out = Vec::new();
        let mut url = Some(format!("{BASE}/me/playlists?limit=50"));
        // Cap at 20 pages (1000 playlists) to stay well clear of rate limits.
        for _ in 0..20 {
            let Some(u) = url.take() else { break };
            let page: Page<RawPlaylist> = self.get(token, &u).await?;
            url = page.next;
            out.extend(page.items.into_iter().flatten().map(Into::into));
        }
        Ok(out)
    }

    pub async fn saved_shows(&self, token: &str) -> Result<Vec<ShowSummary>> {
        let page: Page<SavedShow> = self.get(token, &format!("{BASE}/me/shows?limit=50")).await?;
        Ok(page.items.into_iter().flatten().filter_map(|i| i.show.map(Into::into)).collect())
    }

    pub async fn saved_episodes(&self, token: &str) -> Result<Vec<TrackRow>> {
        let page: Page<SavedEpisode> = self.get(token, &format!("{BASE}/me/episodes?limit=20")).await?;
        Ok(page
            .items
            .into_iter()
            .flatten()
            .filter_map(|i| i.episode)
            .filter(|e| e.is_playable != Some(false))
            .map(|e| episode_row(e, ""))
            .collect())
    }

    /// Newest first, as Spotify lists them.
    pub async fn show_episodes(&self, token: &str, id: &str, show: &str, offset: u32) -> Result<TrackPage> {
        let url = format!("{BASE}/shows/{id}/episodes?limit=50&offset={offset}");
        let page: Page<RawEpisode> = self.get(token, &url).await?;
        let next_offset = next_offset(&page);
        let tracks = page
            .items
            .into_iter()
            .flatten()
            .filter(|e| e.is_playable != Some(false))
            .map(|e| episode_row(e, show))
            .collect();
        Ok(TrackPage { tracks, next_offset })
    }

    /// The show's newest episode with its raw release date ("YYYY-MM-DD"), for sorting.
    pub async fn latest_episode(&self, token: &str, show: &ShowSummary) -> Result<Option<(String, TrackRow)>> {
        let url = format!("{BASE}/shows/{}/episodes?limit=1", show.id);
        let page: Page<RawEpisode> = self.get(token, &url).await?;
        Ok(page
            .items
            .into_iter()
            .flatten()
            .find(|e| e.is_playable != Some(false))
            .map(|e| (e.release_date.clone(), episode_row(e, &show.name))))
    }

    pub async fn liked_tracks(&self, token: &str, offset: u32) -> Result<TrackPage> {
        let url = format!("{BASE}/me/tracks?limit=50&offset={offset}");
        let page: Page<SavedItem> = self.get(token, &url).await?;
        let next_offset = next_offset(&page);
        let tracks = page
            .items
            .into_iter()
            .flatten()
            .filter_map(|i| i.track.map(Into::into))
            .collect();
        Ok(TrackPage { tracks, next_offset })
    }

    pub async fn playlist_tracks(&self, token: &str, id: &str, offset: u32) -> Result<TrackPage> {
        let url = format!("{BASE}/playlists/{id}/items?limit=50&offset={offset}");
        let page: Page<PlaylistItem> = self.get(token, &url).await?;
        let next_offset = next_offset(&page);
        let tracks = page
            .items
            .into_iter()
            .flatten()
            .filter_map(|i| i.item.map(Into::into))
            .collect();
        Ok(TrackPage { tracks, next_offset })
    }

    pub async fn album_tracks(
        &self,
        token: &str,
        id: &str,
        album_name: &str,
        offset: u32,
    ) -> Result<TrackPage> {
        let url = format!("{BASE}/albums/{id}/tracks?limit=50&offset={offset}");
        let page: Page<RawTrack> = self.get(token, &url).await?;
        let next_offset = next_offset(&page);
        let tracks = page
            .items
            .into_iter()
            .flatten()
            .map(|t| {
                let mut row: TrackRow = t.into();
                row.album = album_name.to_string();
                row.album_id = Some(id.to_string());
                row
            })
            .collect();
        Ok(TrackPage { tracks, next_offset })
    }

    pub async fn search(&self, token: &str, query: &str) -> Result<SearchResults> {
        // Since Feb 2026 the max search limit is 10 per type.
        let req = self
            .http
            .get(format!("{BASE}/search"))
            .bearer_auth(token)
            .query(&[
                ("q", query),
                ("type", "track,album,artist,playlist"),
                ("limit", "10"),
            ]);
        let raw: RawSearch = self.send(req).await?.json().await?;
        Ok(SearchResults {
            tracks: raw
                .tracks
                .map(|p| p.items.into_iter().flatten().map(Into::into).collect())
                .unwrap_or_default(),
            albums: raw
                .albums
                .map(|p| p.items.into_iter().flatten().map(album_summary).collect())
                .unwrap_or_default(),
            artists: raw
                .artists
                .map(|p| {
                    p.items
                        .into_iter()
                        .flatten()
                        .map(|a| ArtistSummary {
                            image: first_image(a.images),
                            uri: a.uri,
                            name: a.name,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            playlists: raw
                .playlists
                .map(|p| p.items.into_iter().flatten().map(Into::into).collect())
                .unwrap_or_default(),
        })
    }

    pub async fn devices(&self, token: &str) -> Result<Vec<Device>> {
        let raw: RawDevices = self.get(token, &format!("{BASE}/me/player/devices")).await?;
        Ok(raw
            .devices
            .into_iter()
            .map(|d| Device {
                id: d.id,
                name: d.name,
                kind: d.kind,
                is_active: d.is_active,
            })
            .collect())
    }

    async fn tracks_page(&self, token: &str, url: &str) -> Result<Vec<TrackRow>> {
        let page: Page<RawTrack> = self.get(token, url).await?;
        Ok(page.items.into_iter().flatten().map(Into::into).collect())
    }

    pub async fn top_tracks(&self, token: &str) -> Result<Vec<TrackRow>> {
        self.tracks_page(token, &format!("{BASE}/me/top/tracks?limit=10&time_range=short_term"))
            .await
    }

    /// Top artists with their genres (used for "Because you like …").
    pub async fn top_artists(&self, token: &str) -> Result<Vec<(ArtistSummary, Vec<String>)>> {
        let page: Page<RawArtist> = self
            .get(token, &format!("{BASE}/me/top/artists?limit=8&time_range=medium_term"))
            .await?;
        Ok(page.items.into_iter().flatten().map(artist_with_genres).collect())
    }

    /// Recently played tracks, newest first, without repeats.
    pub async fn recently_played(&self, token: &str) -> Result<Vec<TrackRow>> {
        let page: Page<PlayedItem> = self
            .get(token, &format!("{BASE}/me/player/recently-played?limit=40"))
            .await?;
        let mut seen = std::collections::HashSet::new();
        Ok(page
            .items
            .into_iter()
            .flatten()
            .filter_map(|i| i.track.map(TrackRow::from))
            .filter(|t| seen.insert(t.uri.clone()))
            .take(12)
            .collect())
    }

    pub async fn artist_genres(&self, token: &str, id: &str) -> Result<Vec<String>> {
        let a: RawArtist = self.get(token, &format!("{BASE}/artists/{id}")).await?;
        Ok(a.genres)
    }

    /// Track-only search (max 10 per page since Feb 2026).
    pub async fn search_tracks(&self, token: &str, query: &str) -> Result<Vec<TrackRow>> {
        let req = self
            .http
            .get(format!("{BASE}/search"))
            .bearer_auth(token)
            .query(&[("q", query), ("type", "track"), ("limit", "10")]);
        let raw: RawSearch = self.send(req).await?.json().await?;
        Ok(raw
            .tracks
            .map(|p| p.items.into_iter().flatten().map(Into::into).collect())
            .unwrap_or_default())
    }

    /// Track search that also returns each track's artist names (for finding
    /// collaborators of an artist).
    pub async fn search_track_credits(&self, token: &str, query: &str) -> Result<Vec<(TrackRow, Vec<String>)>> {
        let req = self
            .http
            .get(format!("{BASE}/search"))
            .bearer_auth(token)
            .query(&[("q", query), ("type", "track"), ("limit", "10")]);
        let raw: RawSearch = self.send(req).await?.json().await?;
        Ok(raw
            .tracks
            .map(|p| {
                p.items
                    .into_iter()
                    .flatten()
                    .map(|t| {
                        let names = t.artists.iter().map(|a| a.name.clone()).collect();
                        (TrackRow::from(t), names)
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The artist with this name (exact match preferred), for their picture.
    pub async fn find_artist(&self, token: &str, name: &str) -> Result<Option<ArtistSummary>> {
        let query = format!("artist:\"{name}\"");
        let req = self
            .http
            .get(format!("{BASE}/search"))
            .bearer_auth(token)
            .query(&[("q", query.as_str()), ("type", "artist"), ("limit", "5")]);
        let raw: RawSearch = self.send(req).await?.json().await?;
        let mut found: Vec<RawArtist> = raw.artists.map(|p| p.items.into_iter().flatten().collect()).unwrap_or_default();
        let i = found.iter().position(|a| a.name.eq_ignore_ascii_case(name)).unwrap_or(0);
        if found.is_empty() {
            return Ok(None);
        }
        let a = found.swap_remove(i);
        Ok(Some(ArtistSummary { image: pick_image(&a.images, 300), uri: a.uri, name: a.name }))
    }

    /// Podcast search, max 10.
    pub async fn search_shows(&self, token: &str, query: &str) -> Result<Vec<ShowSummary>> {
        let req = self
            .http
            .get(format!("{BASE}/search"))
            .bearer_auth(token)
            .query(&[("q", query), ("type", "show"), ("limit", "10")]);
        let raw: RawSearch = self.send(req).await?.json().await?;
        Ok(raw
            .shows
            .map(|p| p.items.into_iter().flatten().map(Into::into).collect())
            .unwrap_or_default())
    }

    /// Album search (e.g. `artist:"Name"`), max 10.
    pub async fn search_albums(&self, token: &str, query: &str) -> Result<Vec<AlbumSummary>> {
        let req = self
            .http
            .get(format!("{BASE}/search"))
            .bearer_auth(token)
            .query(&[("q", query), ("type", "album"), ("limit", "10")]);
        let raw: RawSearch = self.send(req).await?.json().await?;
        Ok(raw
            .albums
            .map(|p| p.items.into_iter().flatten().map(album_summary).collect())
            .unwrap_or_default())
    }

    pub async fn queue(&self, token: &str) -> Result<Vec<TrackRow>> {
        let q: RawQueue = self.get(token, &format!("{BASE}/me/player/queue")).await?;
        Ok(q.queue.into_iter().flatten().map(Into::into).take(20).collect())
    }

    pub async fn is_saved(&self, token: &str, uri: &str) -> Result<bool> {
        let req = self
            .http
            .get(format!("{BASE}/me/library/contains"))
            .bearer_auth(token)
            .query(&[("uris", uri)]);
        let v: Vec<bool> = self.send(req).await?.json().await?;
        Ok(v.first().copied().unwrap_or(false))
    }

    pub async fn set_saved(&self, token: &str, uri: &str, saved: bool) -> Result<()> {
        let url = format!("{BASE}/me/library");
        let req = if saved { self.http.put(url) } else { self.http.delete(url) };
        let req = req
            .bearer_auth(token)
            .query(&[("uris", uri)])
            .header("Content-Length", "0");
        self.send(req).await?;
        Ok(())
    }

    /// Start playback on `device_id` (body: `context_uri`/`uris` + optional `offset`).
    pub async fn play(&self, token: &str, device_id: &str, body: &serde_json::Value) -> Result<()> {
        let req = self
            .http
            .put(format!("{BASE}/me/player/play"))
            .bearer_auth(token)
            .query(&[("device_id", device_id)])
            .json(body);
        self.send(req).await?;
        Ok(())
    }

    pub async fn shuffle(&self, token: &str, device_id: &str, on: bool) -> Result<()> {
        let req = self
            .http
            .put(format!("{BASE}/me/player/shuffle"))
            .bearer_auth(token)
            .query(&[("state", if on { "true" } else { "false" }), ("device_id", device_id)])
            .header("Content-Length", "0");
        self.send(req).await?;
        Ok(())
    }

    /// `state`: "off" | "context" | "track".
    pub async fn repeat(&self, token: &str, device_id: &str, state: &str) -> Result<()> {
        let req = self
            .http
            .put(format!("{BASE}/me/player/repeat"))
            .bearer_auth(token)
            .query(&[("state", state), ("device_id", device_id)])
            .header("Content-Length", "0");
        self.send(req).await?;
        Ok(())
    }

    pub async fn transfer(&self, token: &str, device_id: &str) -> Result<()> {
        let req = self
            .http
            .put(format!("{BASE}/me/player"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "device_ids": [device_id], "play": true }));
        self.send(req).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_date;

    #[test]
    fn release_dates() {
        assert_eq!(fmt_date("2026-11-14"), "14 Nov 2026");
        assert_eq!(fmt_date("2026-01-03"), "3 Jan 2026");
        assert_eq!(fmt_date("2026"), "2026");
        assert_eq!(fmt_date("2026-11"), "2026-11");
        assert_eq!(fmt_date(""), "");
    }
}
