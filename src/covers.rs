//! Cover art for egui: downloads through one pooled HTTP client (a few at a time),
//! keeps the files in `~/Library/Caches/rustune/covers` so later launches skip the
//! network, and frees textures that haven't been drawn for a while.
//!
//! egui's stock loaders keep three copies of every image forever (the download, the
//! decoded pixels and the GPU texture). Here only the texture is kept, and only while
//! it's in use.

use eframe::egui::{
    ColorImage, Context, TextureHandle, TextureOptions, Vec2,
    load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint, SizedTexture, TextureLoadResult, TextureLoader, TexturePoll},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{runtime::Handle, sync::Semaphore};

/// Textures not drawn for this long are freed; they reload from disk if needed again.
const TEXTURE_TTL: Duration = Duration::from_secs(30);
/// Parallel downloads. Few enough that connections get reused.
const DOWNLOADS: usize = 6;
/// When the disk cache grows past this, the oldest files are removed down to half.
const DISK_LIMIT: u64 = 100 * 1024 * 1024;

pub fn dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("rustune").join("covers")
}

/// Replace egui's image loading for http(s) URLs.
pub fn install(ctx: &Context, rt: Handle) {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    {
        let dir = dir.clone();
        rt.spawn_blocking(move || prune(&dir));
    }
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .expect("http client");
    ctx.add_image_loader(Arc::new(CoverLoader {
        rt,
        http,
        dir,
        downloads: Arc::new(Semaphore::new(DOWNLOADS)),
        slots: Default::default(),
    }));
    ctx.add_texture_loader(Arc::new(CoverTextures::default()));
}

fn is_http(uri: &str) -> bool {
    uri.starts_with("https://") || uri.starts_with("http://")
}

// ---- decoded images --------------------------------------------------------

enum Slot {
    Pending,
    Ready(Arc<ColorImage>),
    Failed(String),
}

type Slots = Arc<Mutex<HashMap<String, Slot>>>;

struct CoverLoader {
    rt: Handle,
    http: reqwest::Client,
    dir: PathBuf,
    downloads: Arc<Semaphore>,
    slots: Slots,
}

impl ImageLoader for CoverLoader {
    fn id(&self) -> &str {
        "rustune::CoverLoader"
    }

    fn load(&self, ctx: &Context, uri: &str, _: SizeHint) -> ImageLoadResult {
        if !is_http(uri) {
            return Err(LoadError::NotSupported);
        }
        let mut slots = self.slots.lock().unwrap();
        match slots.get(uri) {
            Some(Slot::Pending) => return Ok(ImagePoll::Pending { size: None }),
            Some(Slot::Failed(e)) => return Err(LoadError::Loading(e.clone())),
            // Hand the pixels over once; the texture loader keeps the GPU copy.
            Some(Slot::Ready(_)) => {
                let Some(Slot::Ready(image)) = slots.remove(uri) else { unreachable!() };
                return Ok(ImagePoll::Ready { image });
            }
            None => {}
        }
        slots.insert(uri.to_owned(), Slot::Pending);
        drop(slots);

        let (uri, ctx) = (uri.to_owned(), ctx.clone());
        let (http, downloads, slots) = (self.http.clone(), self.downloads.clone(), self.slots.clone());
        let path = self.dir.join(format!("{:016x}", fnv1a(&uri)));
        self.rt.spawn(async move {
            let result = fetch(&http, &downloads, &uri, path).await;
            if let Err(e) = &result {
                log::debug!("cover {uri}: {e}");
            }
            // Skip if it was forgotten meanwhile.
            if let Some(slot) = slots.lock().unwrap().get_mut(&uri) {
                *slot = match result {
                    Ok(image) => Slot::Ready(Arc::new(image)),
                    Err(e) => Slot::Failed(e),
                };
            }
            ctx.request_repaint();
        });
        Ok(ImagePoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.slots.lock().unwrap().remove(uri);
    }

    fn forget_all(&self) {
        self.slots.lock().unwrap().clear();
    }

    fn byte_size(&self) -> usize {
        let slots = self.slots.lock().unwrap();
        slots
            .values()
            .map(|s| match s {
                Slot::Ready(i) => i.pixels.len() * 4,
                _ => 0,
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.slots.lock().unwrap().values().any(|s| matches!(s, Slot::Pending))
    }
}

/// From the disk cache if present, otherwise downloaded and saved there.
async fn fetch(http: &reqwest::Client, downloads: &Semaphore, uri: &str, path: PathBuf) -> Result<ColorImage, String> {
    let cached = path.clone();
    let from_disk = tokio::task::spawn_blocking(move || std::fs::read(&cached).ok().and_then(|b| decode(&b).ok()))
        .await
        .ok()
        .flatten();
    if let Some(image) = from_disk {
        return Ok(image);
    }

    let bytes = {
        let _permit = downloads.acquire().await.map_err(|e| e.to_string())?;
        let resp = http.get(uri).send().await.map_err(|e| e.to_string())?;
        let resp = resp.error_for_status().map_err(|e| e.to_string())?;
        resp.bytes().await.map_err(|e| e.to_string())?
    };
    tokio::task::spawn_blocking(move || {
        let image = decode(&bytes)?;
        // Write then rename, so a half-written file is never read back.
        let tmp = path.with_extension("part");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        Ok(image)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn decode(bytes: &[u8]) -> Result<ColorImage, String> {
    let rgba = image::load_from_memory(bytes).map_err(|e| e.to_string())?.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Stable across runs and Rust versions (unlike `DefaultHasher`), for file names.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Keep the disk cache bounded: past the limit, delete the oldest files down to half.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= DISK_LIMIT {
        return;
    }
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= DISK_LIMIT / 2 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

// ---- GPU textures ----------------------------------------------------------

struct Tex {
    handle: TextureHandle,
    size: Vec2,
    last_used: Instant,
}

#[derive(Default)]
struct CoverTextures {
    cache: Mutex<HashMap<(String, TextureOptions), Tex>>,
}

impl TextureLoader for CoverTextures {
    fn id(&self) -> &str {
        "rustune::CoverTextures"
    }

    fn load(&self, ctx: &Context, uri: &str, options: TextureOptions, hint: SizeHint) -> TextureLoadResult {
        if !is_http(uri) {
            return Err(LoadError::NotSupported);
        }
        let key = (uri.to_owned(), options);
        if let Some(t) = self.cache.lock().unwrap().get_mut(&key) {
            t.last_used = Instant::now();
            return Ok(TexturePoll::Ready { texture: SizedTexture::new(t.handle.id(), t.size) });
        }
        // No lock held while calling back into egui.
        match ctx.try_load_image(uri, hint)? {
            ImagePoll::Pending { size } => Ok(TexturePoll::Pending { size }),
            ImagePoll::Ready { image } => {
                let size = image.source_size;
                let handle = ctx.load_texture(uri, image, options);
                let texture = SizedTexture::new(handle.id(), size);
                self.cache.lock().unwrap().insert(key, Tex { handle, size, last_used: Instant::now() });
                Ok(TexturePoll::Ready { texture })
            }
        }
    }

    fn forget(&self, uri: &str) {
        self.cache.lock().unwrap().retain(|(u, _), _| u != uri);
    }

    fn forget_all(&self) {
        self.cache.lock().unwrap().clear();
    }

    fn end_pass(&self, _pass_index: u64) {
        // Dropping the handle frees the GPU texture.
        self.cache.lock().unwrap().retain(|_, t| t.last_used.elapsed() < TEXTURE_TTL);
    }

    fn byte_size(&self) -> usize {
        self.cache.lock().unwrap().values().map(|t| t.handle.byte_size()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_stable() {
        // Cache file names must not change between builds.
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }

}
