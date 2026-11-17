//! Lyrics from LRCLIB (lrclib.net), a free, open lyrics database; Spotify's Web API has
//! none. Most songs come with a timestamp per line, so the lyrics view can follow along.
//!
//! A lookup sends only the song's title, artist and length, and runs only while the
//! lyrics view is open. Found lyrics are kept in `~/Library/Caches/rustune/lyrics`, so
//! each song is looked up once.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::OnceLock, time::Duration};

const API: &str = "https://lrclib.net/api";
/// A search result may be this far off the song's length and still count as a match.
const DURATION_SLACK_MS: u32 = 3_000;
/// The disk cache is pruned past this (a song's lyrics are a few KB).
const DISK_LIMIT: u64 = 10 * 1024 * 1024;

/// The song to find lyrics for.
#[derive(Debug, Clone)]
pub struct Query {
    pub uri: String,
    pub name: String,
    /// Comma-separated, as the player reports them.
    pub artists: String,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Start time; 0 for every line of unsynced lyrics.
    pub at_ms: u32,
    /// Empty for an instrumental break.
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// Lines carry timestamps.
    pub synced: bool,
}

/// Index of the line being sung at `pos_ms` (None before the first line).
pub fn current(lines: &[Line], pos_ms: u32) -> Option<usize> {
    lines.partition_point(|l| l.at_ms <= pos_ms).checked_sub(1)
}

pub fn dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("rustune").join("lyrics")
}

/// Lyrics for the song: from the disk cache, else LRCLIB. `None` when there are none.
pub async fn fetch(q: &Query) -> Result<Option<Lyrics>> {
    static PRUNED: OnceLock<()> = OnceLock::new();
    let dir = dir();
    PRUNED.get_or_init(|| crate::covers::prune(&dir, DISK_LIMIT));
    let path = dir.join(format!("{:016x}", crate::covers::fnv1a(&q.uri)));
    // A few KB, so plain blocking reads are fine here.
    if let Some(rec) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok()) {
        return Ok(rec.lyrics());
    }
    let Some(rec) = lookup(q).await? else { return Ok(None) };
    let lyrics = rec.lyrics();
    if lyrics.is_some() {
        let _ = std::fs::create_dir_all(&dir);
        let tmp = path.with_extension("part");
        if std::fs::write(&tmp, serde_json::to_vec(&rec)?).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    Ok(lyrics)
}

/// The parts of an LRCLIB record used here; also the disk cache format.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    artist_name: String,
    /// Seconds.
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

impl Record {
    fn has_synced(&self) -> bool {
        self.synced_lyrics.as_deref().is_some_and(|s| !s.trim().is_empty())
    }

    fn lyrics(&self) -> Option<Lyrics> {
        if let Some(s) = self.synced_lyrics.as_deref() {
            let lines = parse_lrc(s);
            if !lines.is_empty() {
                return Some(Lyrics { lines, synced: true });
            }
        }
        if let Some(p) = self.plain_lyrics.as_deref().filter(|p| !p.trim().is_empty()) {
            let lines = p.trim().lines().map(|l| Line { at_ms: 0, text: l.trim().to_string() }).collect();
            return Some(Lyrics { lines, synced: false });
        }
        if self.instrumental {
            let line = Line { at_ms: 0, text: "Instrumental".into() };
            return Some(Lyrics { lines: vec![line], synced: false });
        }
        None
    }
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // LRCLIB asks clients to name themselves.
            .user_agent(concat!("rustune/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client")
    })
}

/// Exact lookup first; Spotify titles often carry extras ("(From \"Brahmastra\")",
/// "- Remastered 2011") that LRCLIB's don't, so then a search on the bare title.
async fn lookup(q: &Query) -> Result<Option<Record>> {
    let artist = q.artists.split(", ").next().unwrap_or(&q.artists);
    let secs = (q.duration_ms / 1000).to_string();
    let resp = client()
        .get(format!("{API}/get"))
        .query(&[("track_name", q.name.as_str()), ("artist_name", artist), ("duration", &secs)])
        .send()
        .await?;
    if resp.status() != reqwest::StatusCode::NOT_FOUND {
        return Ok(Some(resp.error_for_status()?.json().await?));
    }
    let title = clean_title(&q.name);
    let search = |params: Vec<(&'static str, String)>| async move {
        let found: Vec<Record> =
            client().get(format!("{API}/search")).query(&params).send().await?.error_for_status()?.json().await?;
        anyhow::Ok(found)
    };
    let found = search(vec![("track_name", title.clone()), ("artist_name", artist.to_string())]).await?;
    if let Some(r) = pick(found, q.duration_ms, None) {
        return Ok(Some(r));
    }
    // The first credited artist isn't always the one LRCLIB lists (e.g. the composer).
    let found = search(vec![("track_name", title)]).await?;
    Ok(pick(found, q.duration_ms, Some(&q.artists)))
}

/// The closest-length result within the slack, preferring synced lyrics. With `artists`,
/// the result's artist must be one of them.
fn pick(found: Vec<Record>, duration_ms: u32, artists: Option<&str>) -> Option<Record> {
    let artist_ok = |r: &Record| match artists {
        None => true,
        Some(a) => {
            let theirs = r.artist_name.to_lowercase();
            a.split(", ").any(|x| !x.is_empty() && theirs.contains(&x.to_lowercase()))
        }
    };
    found
        .into_iter()
        .filter(|r| r.has_synced() || r.plain_lyrics.is_some() || r.instrumental)
        .map(|r| (((r.duration * 1000.0) as i64 - duration_ms as i64).unsigned_abs(), r))
        .filter(|(diff, r)| *diff <= DURATION_SLACK_MS as u64 && artist_ok(r))
        .min_by_key(|(diff, r)| (!r.has_synced(), *diff))
        .map(|(_, r)| r)
}

/// Drop bracketed parts and anything after " - ".
fn clean_title(t: &str) -> String {
    let t = t.split(" - ").next().unwrap_or(t);
    let mut out = String::new();
    let mut depth = 0u32;
    for c in t.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.is_empty() { t.trim().to_string() } else { out }
}

/// LRC: `[mm:ss.xx] text`, possibly several stamps per line; tags like `[ar:…]` skipped.
fn parse_lrc(s: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in s.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some((tag, after)) = r.split_once(']') else { break };
            let Some(ms) = parse_stamp(tag) else { break };
            stamps.push(ms);
            rest = after;
        }
        for ms in stamps {
            lines.push(Line { at_ms: ms, text: rest.trim().to_string() });
        }
    }
    lines.sort_by_key(|l| l.at_ms);
    lines
}

fn parse_stamp(tag: &str) -> Option<u32> {
    let (m, s) = tag.split_once(':')?;
    let m: u32 = m.trim().parse().ok()?;
    let s: f64 = s.trim().parse().ok()?;
    (0.0..60.0).contains(&s).then(|| m * 60_000 + (s * 1000.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at_ms: u32, text: &str) -> Line {
        Line { at_ms, text: text.into() }
    }

    #[test]
    fn parses_lrc() {
        let lrc = "[ar:Someone]\n[00:09.44] First\n[00:13.40] \n[01:02.5][00:20.123] Twice\nno stamp";
        assert_eq!(
            parse_lrc(lrc),
            vec![line(9_440, "First"), line(13_400, ""), line(20_123, "Twice"), line(62_500, "Twice")]
        );
    }

    #[test]
    fn current_line() {
        let lines = [line(1_000, "a"), line(2_000, "b"), line(3_000, "c")];
        assert_eq!(current(&lines, 500), None);
        assert_eq!(current(&lines, 1_000), Some(0));
        assert_eq!(current(&lines, 2_999), Some(1));
        assert_eq!(current(&lines, 99_000), Some(2));
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Kesariya (From \"Brahmastra\")"), "Kesariya");
        assert_eq!(clean_title("Bohemian Rhapsody - Remastered 2011"), "Bohemian Rhapsody");
        assert_eq!(clean_title("Song [Live] (feat. X)"), "Song");
        assert_eq!(clean_title("(Intro)"), "(Intro)");
    }

    #[test]
    fn picks_closest_synced_match() {
        let rec = |artist: &str, duration: f64, synced: bool| Record {
            artist_name: artist.into(),
            duration,
            plain_lyrics: Some("words".into()),
            synced_lyrics: synced.then(|| "[00:01.00] words".into()),
            ..Default::default()
        };
        // Synced beats a closer plain-only match; too far off is rejected.
        let found = vec![rec("A", 200.0, false), rec("A", 202.0, true), rec("A", 260.0, true)];
        assert_eq!(pick(found, 200_000, None).map(|r| r.duration), Some(202.0));
        let found = vec![rec("Someone Else", 200.0, true), rec("Arijit Singh", 201.0, true)];
        assert_eq!(pick(found, 200_000, Some("Pritam, Arijit Singh")).map(|r| r.duration), Some(201.0));
        assert!(pick(vec![rec("A", 230.0, true)], 200_000, None).is_none());
    }
}
