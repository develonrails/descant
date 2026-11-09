//! Cover art: fetched once, kept within a memory budget.
//!
//! Every cover in the app comes through here. Covers are asked for at the
//! size they are shown at (the 64 px rendition for a row, not the 640 px one),
//! decoded, shrunk to the pixels they fill on screen (Spotify's 300 px
//! rendition for a 160 px card at 150 % is cut to 240 px), and kept in an
//! LRU bounded by decoded bytes. Covers without transparency (Spotify's are
//! JPEGs) keep three bytes a pixel, not four. Fetched files also go to the cache directory, so a restart does not
//! download them again; that directory is kept under its own cap, the covers
//! used longest ago going first. Requests for a cover already on its way
//! share it. Reading, fetching and decoding happen off the GTK thread, a few
//! covers at a time, and only for images that still want them by their turn
//! (a fling through a long list rebinds rows faster than covers arrive). The
//! covers in memory are let go when the window is put away and when the
//! system runs low on memory.

use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use gtk::{gdk, gio, glib, prelude::*};
use tokio::sync::{Semaphore, oneshot};

use crate::runtime;

/// Decoded covers kept in memory, in bytes (width × height × bytes a pixel); covers on
/// screen stay alive through their widgets anyway, and the disk is close.
const MEMORY_BUDGET: usize = 12 * 1024 * 1024;
/// Cover files kept on disk, in bytes.
const DISK_BUDGET: u64 = 50 * 1024 * 1024;
/// Covers read, fetched and decoded at once; the rest wait their turn.
const FETCHES: usize = 4;

/// A least-recently-used map that evicts by total cost, not by count.
pub struct Lru<V> {
    entries: HashMap<String, (V, usize, u64)>,
    total: usize,
    budget: usize,
    clock: u64,
}

impl<V: Clone> Lru<V> {
    pub fn new(budget: usize) -> Lru<V> {
        Lru {
            entries: HashMap::new(),
            total: 0,
            budget,
            clock: 0,
        }
    }

    pub fn get(&mut self, key: &str) -> Option<V> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|(value, _, used)| {
            *used = clock;
            value.clone()
        })
    }

    pub fn insert(&mut self, key: String, value: V, cost: usize) {
        self.clock += 1;
        if let Some((_, old, _)) = self.entries.insert(key, (value, cost, self.clock)) {
            self.total -= old;
        }
        self.total += cost;
        while self.total > self.budget && self.entries.len() > 1 {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest
                && let Some((_, cost, _)) = self.entries.remove(&key)
            {
                self.total -= cost;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total = 0;
    }

    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.total
    }
}

type Waiters = HashMap<String, Vec<oneshot::Sender<Option<gdk::Texture>>>>;

pub struct Images {
    memory: RefCell<Lru<gdk::Texture>>,
    waiting: RefCell<Waiters>,
    dir: PathBuf,
    fetches: Semaphore,
    /// Kept so its warnings keep coming.
    _memory_monitor: gio::MemoryMonitor,
}

thread_local! {
    static IMAGES: Rc<Images> = Rc::new(Images {
        memory: RefCell::new(Lru::new(MEMORY_BUDGET)),
        waiting: RefCell::new(HashMap::new()),
        dir: cache_dir(),
        fetches: Semaphore::new(FETCHES),
        _memory_monitor: release_when_memory_runs_low(),
    });
}

/// Lets go of the covers kept in memory. Those on screen stay with their
/// widgets, and the rest are a disk read away when they are needed again.
/// For when the window is put away while music plays on, and when the
/// system runs low on memory.
pub fn release() {
    IMAGES.with(|i| i.memory.borrow_mut().clear());
}

fn release_when_memory_runs_low() -> gio::MemoryMonitor {
    let monitor = gio::MemoryMonitor::dup_default();
    monitor.connect_low_memory_warning(|_, level| {
        log::debug!("low memory ({level:?}): letting go of covers");
        release();
    });
    monitor
}

/// The cover at `url`, at most `pixels` on its longest side (0: as it
/// comes), from memory, disk or the network. GTK thread only.
///
/// A fetch waits for one of [`FETCHES`] turns; `wanted` is asked when its
/// turn comes, and a cover no longer wanted by then is not fetched (`None`).
async fn texture(url: String, pixels: u32, wanted: impl Fn() -> bool) -> Option<gdk::Texture> {
    let images = IMAGES.with(Rc::clone);
    let key = memory_key(&url, pixels);
    let mut turn = None;
    let turn = loop {
        if let Some(texture) = images.memory.borrow_mut().get(&key) {
            return Some(texture);
        }
        // Someone already fetches it: wait for that. The borrow must end
        // before the await, or the fetcher could not hand the result over.
        let pending = images.waiting.borrow_mut().get_mut(&key).map(|waiters| {
            let (tx, rx) = oneshot::channel();
            waiters.push(tx);
            rx
        });
        if let Some(rx) = pending {
            return rx.await.ok().flatten();
        }
        if let Some(turn) = turn.take() {
            break turn;
        }
        // While this waits, another fetch may bring the same cover (checked
        // again above), or the row it was for may scroll away.
        turn = Some(images.fetches.acquire().await.ok()?);
        if !wanted() {
            return None;
        }
    };
    images.waiting.borrow_mut().insert(key.clone(), Vec::new());

    let path = images.dir.join(file_name(&url));
    let fetch_url = url.clone();
    let texture = runtime()
        .spawn(async move {
            let disk = path.clone();
            let cached = tokio::task::spawn_blocking(move || read_touched(&disk))
                .await
                .ok()
                .flatten();
            let (bytes, fetched) = match cached {
                Some(bytes) => (bytes, false),
                None => (download(&fetch_url).await?, true),
            };
            tokio::task::spawn_blocking(move || {
                if fetched {
                    keep(&path, &bytes);
                }
                decode(bytes, pixels)
            })
            .await
            .ok()
            .flatten()
        })
        .await
        .ok()
        .flatten();
    drop(turn);
    if let Some(t) = &texture {
        let cost = (t.width().max(1) * t.height().max(1)) as usize * bytes_per_pixel(t.format());
        images
            .memory
            .borrow_mut()
            .insert(key.clone(), t.clone(), cost);
    }
    for waiter in images.waiting.borrow_mut().remove(&key).unwrap_or_default() {
        let _ = waiter.send(texture.clone());
    }
    texture
}

/// Shows the cover at `url` in `image`, unless the image has been given
/// another cover (rows are recycled while scrolling) or is gone by the time
/// the cover's turn to be fetched comes, or by the time it arrives. The
/// image's widget name holds the URL it should show.
pub fn show(image: &gtk::Image, url: Option<String>) {
    let Some(url) = url else {
        image.set_paintable(None::<&gdk::Paintable>);
        image.set_widget_name("");
        return;
    };
    // Rebinding a row to the same track: the cover is already there.
    if image.widget_name() == url && image.paintable().is_some() {
        return;
    }
    image.set_widget_name(&url);
    let pixels = match image.pixel_size() {
        size if size > 0 => crate::widgets::device_pixels(size, image),
        _ => 0,
    };
    // In memory: no detour through the main loop, so no blank frame.
    let key = memory_key(&url, pixels);
    if let Some(texture) = IMAGES.with(|i| i.memory.borrow_mut().get(&key)) {
        image.set_paintable(Some(&texture));
        return;
    }
    image.set_paintable(None::<&gdk::Paintable>);
    let image = image.downgrade();
    glib::spawn_future_local(async move {
        let wanting = || image.upgrade().filter(|i| i.widget_name() == url.as_str());
        let texture = texture(url.clone(), pixels, || wanting().is_some()).await;
        if let Some(image) = wanting() {
            image.set_paintable(texture.as_ref());
        }
    });
}

/// The same cover at two sizes is two entries.
fn memory_key(url: &str, pixels: u32) -> String {
    format!("{pixels}:{url}")
}

/// Decodes a cover and shrinks it to at most `pixels` on its longest side.
fn decode(bytes: Vec<u8>, pixels: u32) -> Option<gdk::Texture> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()?;
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let Some((to_width, to_height)) = fit(width, height, pixels as usize) else {
        return Some(texture);
    };
    // Asked for in the layout it already has, GTK hands over the decoded
    // pixels themselves: a JPEG decodes to three bytes a pixel, and is shrunk
    // from there without a four-byte copy of the whole picture first.
    let (format, channels) = working_format(texture.format());
    let (data, stride) = {
        let mut downloader = gdk::TextureDownloader::new(&texture);
        downloader.set_format(format);
        downloader.download_bytes()
    };
    // When a copy was made (covers with alpha), the decode it came from goes
    // before shrinking.
    drop(texture);
    let small = shrink(
        &data,
        stride,
        channels,
        (width, height),
        (to_width, to_height),
    );
    let small = gdk::MemoryTexture::new(
        to_width as i32,
        to_height as i32,
        format,
        &glib::Bytes::from_owned(small),
        to_width * channels,
    );
    Some(small.upcast())
}

/// The layout a cover of `source` layout is shrunk and kept in, and its
/// bytes a pixel: grey stays one channel, colour without alpha three, and
/// anything with alpha four, premultiplied so averaging does not bleed the
/// colour of transparent pixels.
fn working_format(source: gdk::MemoryFormat) -> (gdk::MemoryFormat, usize) {
    use gdk::MemoryFormat as F;
    match source {
        F::G8 | F::G16 => (F::G8, 1),
        F::R8g8b8
        | F::B8g8r8
        | F::R8g8b8x8
        | F::B8g8r8x8
        | F::X8r8g8b8
        | F::X8b8g8r8
        | F::R16g16b16
        | F::R16g16b16Float
        | F::R32g32b32Float => (F::R8g8b8, 3),
        _ => (F::R8g8b8a8Premultiplied, 4),
    }
}

/// What a pixel of `format` takes in memory, for the cache's budget.
fn bytes_per_pixel(format: gdk::MemoryFormat) -> usize {
    use gdk::MemoryFormat as F;
    match format {
        F::G8 | F::A8 => 1,
        F::G8a8 | F::G8a8Premultiplied | F::G16 | F::A16 | F::A16Float => 2,
        F::R8g8b8 | F::B8g8r8 => 3,
        F::R16g16b16 | F::R16g16b16Float => 6,
        F::R16g16b16a16 | F::R16g16b16a16Premultiplied => 8,
        F::R16g16b16a16Float | F::R16g16b16a16FloatPremultiplied => 8,
        F::R32g32b32Float => 12,
        F::R32g32b32a32Float | F::R32g32b32a32FloatPremultiplied => 16,
        _ => 4,
    }
}

/// The size that fits `pixels` on the longest side, or `None` when the image
/// already fits (or no size was asked for).
fn fit(width: usize, height: usize, pixels: usize) -> Option<(usize, usize)> {
    let longest = width.max(height);
    if pixels == 0 || longest <= pixels {
        return None;
    }
    let scaled = |side: usize| (side * pixels).div_ceil(longest).max(1);
    Some((scaled(width), scaled(height)))
}

/// For each target index along one axis, the source indices it covers and
/// the share each one has.
fn spans(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let step = from as f64 / to as f64;
    (0..to)
        .map(|i| {
            let (start, end) = (i as f64 * step, (i + 1) as f64 * step);
            (start.floor() as usize..(end.ceil() as usize).min(from))
                .filter_map(|s| {
                    let covered = end.min(s as f64 + 1.0) - start.max(s as f64);
                    (covered > 0.0).then_some((s, (covered / step) as f32))
                })
                .collect()
        })
        .collect()
}

/// Shrinks pixels of `channels` bytes (one to four) by averaging the area
/// each target pixel covers.
fn shrink(
    data: &[u8],
    stride: usize,
    channels: usize,
    from: (usize, usize),
    to: (usize, usize),
) -> Vec<u8> {
    let (columns, rows) = (spans(from.0, to.0), spans(from.1, to.1));
    let mut out = Vec::with_capacity(to.0 * to.1 * channels);
    for row in &rows {
        for column in &columns {
            let mut sum = [0f32; 4];
            let sum = &mut sum[..channels];
            for &(y, wy) in row {
                for &(x, wx) in column {
                    let at = y * stride + x * channels;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += f32::from(data[at + channel]) * wy * wx;
                    }
                }
            }
            out.extend(sum.iter().map(|v| v.round().clamp(0.0, 255.0) as u8));
        }
    }
    out
}

async fn download(url: &str) -> Option<Vec<u8>> {
    let response = descant_spotify::http().get(url).send().await.ok()?;
    let bytes = response.error_for_status().ok()?.bytes().await.ok()?;
    Some(bytes.to_vec())
}

/// A cover from the disk cache, marked as just used.
fn read_touched(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
    Some(bytes)
}

fn keep(path: &Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, bytes);
}

/// Where cover files are kept.
pub fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join("descant").join("covers")
}

/// The bytes the cover files take on disk.
pub async fn disk_usage() -> u64 {
    let dir = cache_dir();
    let counted = runtime().spawn_blocking(move || {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    });
    counted.await.unwrap_or(0)
}

/// Forgets every cover, in memory and on disk. Covers on screen stay until
/// their page is left.
pub async fn clear() {
    IMAGES.with(|i| i.memory.borrow_mut().clear());
    let dir = cache_dir();
    let _ = runtime()
        .spawn_blocking(move || {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        })
        .await;
}

/// Trims the disk cache to its budget, the covers used longest ago first,
/// and clears out temporary track files a crash left behind. Run once at
/// start, off the GTK thread.
pub fn prune_disk_cache() {
    let dir = cache_dir();
    runtime().spawn_blocking(remove_stale_track_files);
    runtime().spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let files = entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some((entry.path(), meta.len(), meta.modified().ok()?))
            })
            .collect();
        for path in over_budget(files, DISK_BUDGET) {
            let _ = std::fs::remove_file(path);
        }
    });
}

/// Where librespot keeps the playing track and the next one while they
/// download. On disk in the cache, not in TMPDIR: `/tmp` is tmpfs on most
/// systems, where two tracks of about 10 MB each would pin RAM; in the cache
/// they are page cache the kernel can take back.
pub fn track_dir() -> PathBuf {
    let dir = glib::user_cache_dir().join("descant").join("tracks");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// librespot's temporary track files (`.tmp…`) normally go when a track
/// ends; a crash leaves them. Only old ones, in case one is being written.
fn remove_stale_track_files() {
    let Ok(entries) = std::fs::read_dir(track_dir()) else {
        return;
    };
    let hour_ago = SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().starts_with(".tmp")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < hour_ago);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Which files to remove so the rest fit `budget`: the oldest first.
fn over_budget(mut files: Vec<(PathBuf, u64, SystemTime)>, budget: u64) -> Vec<PathBuf> {
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort_by_key(|(_, _, used)| *used);
    files
        .into_iter()
        .take_while(|(_, size, _)| {
            let over = total > budget;
            total -= size;
            over
        })
        .map(|(path, _, _)| path)
        .collect()
}

/// A file name for a cover URL; stable for a given Rust release, which is
/// enough for a cache.
fn file_name(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_the_least_recently_used_once_over_budget() {
        let mut lru = Lru::new(10);
        lru.insert("a".into(), 'a', 4);
        lru.insert("b".into(), 'b', 4);
        assert_eq!(lru.get("a"), Some('a')); // a is now fresher than b
        lru.insert("c".into(), 'c', 4); // 12 > 10: b goes
        assert_eq!(lru.get("b"), None);
        assert_eq!(lru.get("a"), Some('a'));
        assert_eq!(lru.get("c"), Some('c'));
        assert_eq!(lru.total(), 8);
    }

    #[test]
    fn replacing_an_entry_does_not_count_it_twice() {
        let mut lru = Lru::new(10);
        lru.insert("a".into(), 1, 6);
        lru.insert("a".into(), 2, 6);
        assert_eq!(lru.total(), 6);
        assert_eq!(lru.get("a"), Some(2));
    }

    #[test]
    fn a_single_entry_over_budget_is_still_kept() {
        let mut lru = Lru::new(10);
        lru.insert("big".into(), 'x', 50);
        assert_eq!(lru.get("big"), Some('x'));
    }

    #[test]
    fn file_names_are_stable_and_distinct() {
        assert_eq!(
            file_name("https://i.scdn.co/image/a"),
            file_name("https://i.scdn.co/image/a")
        );
        assert_ne!(
            file_name("https://i.scdn.co/image/a"),
            file_name("https://i.scdn.co/image/b")
        );
    }

    #[test]
    fn the_disk_cache_drops_the_covers_used_longest_ago() {
        let at = |secs| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let files = vec![
            (PathBuf::from("new"), 40, at(300)),
            (PathBuf::from("old"), 40, at(100)),
            (PathBuf::from("mid"), 40, at(200)),
        ];
        assert_eq!(over_budget(files.clone(), 200), Vec::<PathBuf>::new());
        assert_eq!(over_budget(files.clone(), 80), vec![PathBuf::from("old")]);
        assert_eq!(
            over_budget(files, 50),
            vec![PathBuf::from("old"), PathBuf::from("mid")]
        );
    }

    #[test]
    fn covers_fit_the_pixels_they_fill() {
        assert_eq!(fit(300, 300, 240), Some((240, 240)));
        assert_eq!(fit(640, 320, 100), Some((100, 50)));
        assert_eq!(fit(64, 64, 72), None); // never enlarged
        assert_eq!(fit(300, 300, 0), None);
    }

    #[test]
    fn shrinking_averages_the_area_each_pixel_covers() {
        // a 2×2 image: black, white / white, black → one mid-grey pixel
        let (b, w) = ([0, 0, 0, 255], [255, 255, 255, 255]);
        let data: Vec<u8> = [b, w, w, b].concat();
        assert_eq!(
            shrink(&data, 8, 4, (2, 2), (1, 1)),
            vec![128, 128, 128, 255]
        );
    }

    #[test]
    fn opaque_pixels_shrink_in_three_bytes_and_grey_in_one() {
        // black, white / white, black, each row padded to a stride of 8
        // bytes, as a texture's rows may be; the padding (9) is skipped
        let rgb: Vec<u8> = [
            &[0u8, 0, 0, 255, 255, 255, 9, 9][..],
            &[255, 255, 255, 0, 0, 0, 9, 9][..],
        ]
        .concat();
        assert_eq!(shrink(&rgb, 8, 3, (2, 2), (1, 1)), vec![128, 128, 128]);
        let grey = [0u8, 255, 255, 0];
        assert_eq!(shrink(&grey, 2, 1, (2, 2), (1, 1)), vec![128]);
    }

    #[test]
    fn covers_without_alpha_keep_three_bytes_a_pixel() {
        use gdk::MemoryFormat as F;
        assert_eq!(working_format(F::R8g8b8), (F::R8g8b8, 3));
        assert_eq!(working_format(F::B8g8r8x8), (F::R8g8b8, 3));
        assert_eq!(working_format(F::G8), (F::G8, 1));
        assert_eq!(working_format(F::R8g8b8a8), (F::R8g8b8a8Premultiplied, 4));
        // what is kept is what is counted
        for source in [F::R8g8b8, F::G8, F::R8g8b8a8, F::R16g16b16a16] {
            let (kept, channels) = working_format(source);
            assert_eq!(bytes_per_pixel(kept), channels);
        }
        assert_eq!(bytes_per_pixel(F::R16g16b16a16), 8);
    }

    #[test]
    fn a_flat_colour_stays_flat_at_any_ratio() {
        let pixel = [10u8, 120, 200, 255];
        let data: Vec<u8> = pixel.repeat(300 * 300);
        let out = shrink(&data, 300 * 4, 4, (300, 300), (240, 240));
        assert_eq!(out.len(), 240 * 240 * 4);
        assert!(out.chunks(4).all(|p| p == pixel));
    }

    #[test]
    fn every_source_pixel_counts_once() {
        for (from, to) in [(300, 240), (640, 72), (64, 60)] {
            let total: f32 = spans(from, to).iter().flatten().map(|(_, w)| w).sum();
            assert!((total - to as f32).abs() < 1e-3, "{from}→{to}: {total}");
        }
    }
}
