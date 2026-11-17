//! Spike: can Spotify's official Web Playback SDK play audio inside a macOS WKWebView?
//! Run: cargo run --release --example sdk_spike
//! Prints SDK events (ready / errors / playback position) to stdout.

use librespot_oauth::OAuthClientBuilder;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

/// Same build-time setting as the app (see README → Setup).
const CLIENT_ID: &str = match option_env!("RUSTUNE_CLIENT_ID") {
    Some(id) if !id.is_empty() => id,
    _ => panic!("RUSTUNE_CLIENT_ID is not set: add your Spotify Client ID to .cargo/config.toml (see README → Setup)"),
};
const PORT: u16 = 8899;
/// "Beneath the Sakura Sky", from the user's Liked Songs.
const TRACK: &str = "spotify:track:3FHMLwjA5KqxAbDDfmzBT1";

const HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>sdk spike</title>
<style>body{background:#1a0f0f;color:#f6eeec;font:13px -apple-system;padding:12px}pre{white-space:pre-wrap}</style></head>
<body><b>rustune SDK spike</b><pre id="out"></pre>
<script>
const TOKEN = "__TOKEN__", TRACK = "__TRACK__";
function log(m){ document.getElementById('out').textContent += m + "\n";
  fetch('/log', {method:'POST', body: String(m)}).catch(()=>{}); }
log('UA: ' + navigator.userAgent);
log('EME api: ' + (!!navigator.requestMediaKeySystemAccess));
const cfg = [{initDataTypes:['cenc','sinf','skd','keyids'],
  audioCapabilities:[{contentType:'audio/mp4; codecs="mp4a.40.2"'},{contentType:'audio/mp4; codecs="flac"'}]}];
for (const ks of ['com.widevine.alpha','com.apple.fps','com.apple.fps.1_0','com.apple.fps.2_0','org.w3.clearkey']) {
  if (!navigator.requestMediaKeySystemAccess) break;
  navigator.requestMediaKeySystemAccess(ks, cfg).then(()=>log('keysystem ' + ks + ': OK'))
    .catch(e=>log('keysystem ' + ks + ': NO (' + e.name + ')'));
}
window.onSpotifyWebPlaybackSDKReady = () => {
  log('SDK loaded');
  const player = new Spotify.Player({ name: 'rustune-sdk-spike', getOAuthToken: cb => cb(TOKEN), volume: 0.6 });
  for (const e of ['initialization_error','authentication_error','account_error','playback_error','autoplay_failed'])
    player.addListener(e, (x) => log(e + ': ' + JSON.stringify(x)));
  player.addListener('not_ready', ({device_id}) => log('not_ready ' + device_id));
  player.addListener('ready', async ({device_id}) => {
    log('ready, device_id=' + device_id);
    try { await player.activateElement(); } catch(e) { log('activateElement: ' + e); }
    const r = await fetch('https://api.spotify.com/v1/me/player/play?device_id=' + device_id, {
      method: 'PUT', headers: {Authorization: 'Bearer ' + TOKEN, 'Content-Type': 'application/json'},
      body: JSON.stringify({uris: [TRACK]})});
    log('PUT play -> ' + r.status + ' ' + (await r.text()).slice(0, 200));
  });
  player.addListener('player_state_changed', s => { if (s) log('state: paused=' + s.paused + ' pos=' + s.position +
    ' track=' + (s.track_window.current_track||{}).name); });
  player.connect().then(ok => log('connect() -> ' + ok));
  setInterval(async () => { const s = await player.getCurrentState();
    log(s ? ('tick: paused=' + s.paused + ' pos=' + s.position + 'ms') : 'tick: no state'); }, 4000);
};
</script>
<script src="https://sdk.scdn.co/spotify-player.js"></script>
</body></html>"#;

fn serve(html: String) {
    let listener = TcpListener::bind(("127.0.0.1", PORT)).expect("bind spike server");
    for stream in listener.incoming().flatten() {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            continue;
        }
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; content_length];
        let _ = reader.read_exact(&mut body);
        let (status, ctype, payload) = if request_line.starts_with("POST /log") {
            println!("[sdk] {}", String::from_utf8_lossy(&body));
            let _ = std::io::stdout().flush();
            ("204 No Content", "text/plain", String::new())
        } else if request_line.starts_with("GET / ") {
            ("200 OK", "text/html; charset=utf-8", html.clone())
        } else {
            ("404 Not Found", "text/plain", String::new())
        };
        let mut s = stream;
        let _ = write!(
            s,
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
    }
}

fn main() -> wry::Result<()> {
    let token = OAuthClientBuilder::new(
        CLIENT_ID,
        "http://127.0.0.1:8898/login",
        vec![
            "streaming",
            "user-read-email",
            "user-read-private",
            "user-read-playback-state",
            "user-modify-playback-state",
        ],
    )
    .open_in_browser()
    .with_custom_message("rustune SDK test: you can close this tab.")
    .build()
    .expect("oauth client")
    .get_access_token()
    .expect("sign-in");
    println!("[spike] got token");

    let html = HTML.replace("__TOKEN__", &token.access_token).replace("__TRACK__", TRACK);
    std::thread::spawn(move || serve(html));

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("rustune SDK spike")
        .with_inner_size(LogicalSize::new(560.0, 420.0))
        .build(&event_loop)
        .unwrap();
    let _webview = WebViewBuilder::new()
        .with_url(format!("http://127.0.0.1:{PORT}/"))
        .with_autoplay(true)
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
