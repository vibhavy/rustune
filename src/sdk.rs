//! Audio engine: Spotify's official Web Playback SDK running in a hidden WKWebView
//! attached to the egui window. Audio is decrypted with Apple FairPlay inside WebKit.
//!
//! Rust → JS: `evaluate_script` calls into `window.rt`.
//! JS → Rust: `window.ipc.postMessage(json)` → `SdkMsg`.
//!
//! The page is served from a tiny HTTP server on 127.0.0.1 because EME (DRM) needs a
//! secure context, and loopback http counts as one. No token is embedded in the page:
//! the SDK asks for one over IPC whenever it needs it.

use anyhow::Result;
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::mpsc::Sender,
};
use wry::{
    WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdkMsg {
    /// The SDK needs an access token (startup or expiry).
    Token,
    Ready { device_id: String },
    NotReady,
    State(SdkState),
    /// Playback moved to another device (SDK reports a null state).
    Inactive,
    Error { kind: String, message: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct SdkState {
    pub paused: bool,
    pub position: u32,
    pub duration: u32,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub album_uri: String,
    pub artist_uri: String,
    pub image: Option<String>,
    pub shuffle: bool,
    /// 0 = off, 1 = context, 2 = track
    pub repeat_mode: u8,
}

pub struct Sdk {
    webview: WebView,
}

impl Sdk {
    pub fn new(cc: &eframe::CreationContext<'_>, tx: Sender<SdkMsg>, volume: f32) -> Result<Self> {
        let port = serve_page(volume)?;
        let ctx = cc.egui_ctx.clone();
        let webview = WebViewBuilder::new()
            .with_url(format!("http://127.0.0.1:{port}/"))
            .with_autoplay(true)
            // 1×1 px in the window's (rounded, transparent) top-left corner. It stays
            // "visible" on purpose: WebKit throttles hidden pages' timers, which slowed
            // the SDK's start of playback.
            .with_bounds(wry::Rect {
                position: LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(1, 1).into(),
            })
            .with_ipc_handler(move |req| match serde_json::from_str::<SdkMsg>(req.body()) {
                Ok(msg) => {
                    let _ = tx.send(msg);
                    ctx.request_repaint();
                }
                Err(e) => log::warn!("bad SDK message {e}: {}", req.body()),
            })
            .build_as_child(cc)?;
        Ok(Self { webview })
    }

    fn eval(&self, js: &str) {
        if let Err(e) = self.webview.evaluate_script(js) {
            log::warn!("SDK script failed: {e}");
        }
    }

    pub fn set_token(&self, token: &str) {
        let t = serde_json::to_string(token).unwrap_or_default();
        self.eval(&format!("window.rt && window.rt.setToken({t})"));
    }

    pub fn toggle_play(&self) {
        self.eval("window.rt.player.togglePlay()");
    }
    pub fn next(&self) {
        self.eval("window.rt.player.nextTrack()");
    }
    pub fn prev(&self) {
        self.eval("window.rt.player.previousTrack()");
    }
    pub fn seek(&self, ms: u32) {
        self.eval(&format!("window.rt.player.seek({ms})"));
    }
    pub fn set_volume(&self, v: f32) {
        self.eval(&format!("window.rt.player.setVolume({})", v.clamp(0.0, 1.0)));
    }
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"></head><body>
<script>
const post = (m) => window.ipc.postMessage(JSON.stringify(m));
let waiting = [];
window.rt = {
  setToken(t) { const w = waiting; waiting = []; w.forEach(cb => cb(t)); },
};
function pickImage(imgs) {
  if (!imgs || !imgs.length) return null;
  return imgs.reduce((a, b) => Math.abs((b.width || 300) - 300) < Math.abs((a.width || 300) - 300) ? b : a).url;
}
window.onSpotifyWebPlaybackSDKReady = () => {
  const player = new Spotify.Player({
    name: 'rustune',
    getOAuthToken: cb => { waiting.push(cb); post({type: 'token'}); },
    volume: __VOLUME__,
  });
  window.rt.player = player;
  for (const kind of ['initialization_error', 'authentication_error', 'account_error', 'playback_error', 'autoplay_failed'])
    player.addListener(kind, (e) => post({type: 'error', kind, message: (e && e.message) || ''}));
  player.addListener('ready', ({device_id}) => post({type: 'ready', device_id}));
  player.addListener('not_ready', () => post({type: 'not_ready'}));
  player.addListener('player_state_changed', (s) => {
    if (!s) return post({type: 'inactive'});
    const t = s.track_window.current_track || {};
    post({
      type: 'state', paused: s.paused, position: Math.round(s.position), duration: Math.round(s.duration),
      uri: t.uri || '', name: t.name || '',
      artists: (t.artists || []).map(a => a.name).join(', '),
      album: (t.album || {}).name || '', album_uri: (t.album || {}).uri || '',
      artist_uri: ((t.artists || [])[0] || {}).uri || '',
      image: pickImage((t.album || {}).images),
      shuffle: !!s.shuffle, repeat_mode: s.repeat_mode | 0,
    });
  });
  player.connect();
};
</script>
<script src="https://sdk.scdn.co/spotify-player.js"></script>
</body></html>"#;

/// Serve the SDK host page on an ephemeral loopback port; returns the port.
fn serve_page(volume: f32) -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let page = PAGE.replace("__VOLUME__", &format!("{:.2}", volume.clamp(0.0, 1.0)));
    std::thread::Builder::new()
        .name("rustune-sdk-page".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let (status, body) = if line.starts_with("GET / ") {
                    ("200 OK", page.as_str())
                } else {
                    ("404 Not Found", "")
                };
                let mut s = stream;
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        })?;
    Ok(port)
}
