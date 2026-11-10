//! Thumbnail textures: decoded on worker threads, uploaded into one egui
//! context, kept in a small LRU. Server pictures come through the
//! [`ImageCache`]. Requests wait in a short queue, newest first, and only
//! those still on screen are kept.

use crate::image_cache::ImageCache;
use crate::services::Opener;
use crossbeam_channel::{Receiver, unbounded};
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

const MAX_TEXTURES: usize = 240;
const MAX_WIDTH: u32 = 480;
/// Width of the decodes banners across the page use.
const WIDE_WIDTH: u32 = 1280;
/// The most requests waiting for a decoder thread; older ones are dropped
/// and asked for again when they are next on screen.
const MAX_QUEUED: usize = 64;

/// A request: texture key, path or URL, width.
type Request = (String, String, u32);

/// A decoded texture and when it was last used.
struct Slot {
    tex: TextureHandle,
    used: u64,
}

/// What the decoder threads work through: the newest request first.
struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<Request>,
    closed: bool,
}

impl Queue {
    fn new() -> Queue {
        Queue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `req` ahead of the rest. When that overfills the queue the
    /// oldest request is dropped, and its key returned.
    fn push(&self, req: Request) -> Option<String> {
        let mut state = self.lock();
        state.items.push_front(req);
        let dropped = if state.items.len() > MAX_QUEUED {
            state.items.pop_back().map(|r| r.0)
        } else {
            None
        };
        drop(state);
        self.ready.notify_one();
        dropped
    }

    /// The next request, waiting for one; `None` once the queue is closed.
    fn pop(&self) -> Option<Request> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(req) = state.items.pop_front() {
                return Some(req);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Keeps the requests whose key `keep` accepts; returns the other keys.
    fn retain(&self, keep: impl Fn(&str) -> bool) -> Vec<String> {
        let mut dropped = Vec::new();
        self.lock().items.retain(|(key, ..)| {
            let kept = keep(key.as_str());
            if !kept {
                dropped.push(key.clone());
            }
            kept
        });
        dropped
    }

    /// Wakes the decoder threads for good, so they end.
    fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }

    #[cfg(test)]
    fn keys(&self) -> Vec<String> {
        self.lock().items.iter().map(|r| r.0.clone()).collect()
    }
}

pub struct Thumbs {
    textures: HashMap<String, Slot>,
    /// Counts lookups; stamps a texture's last use.
    clock: u64,
    pending: HashSet<String>,
    failed: HashSet<String>,
    /// Keys asked for since the last `poll`: what is on screen.
    seen: HashSet<String>,
    queue: Arc<Queue>,
    done: Receiver<(String, Option<ColorImage>)>,
}

/// Whether `key` is a server's picture (not a local file).
fn is_remote(key: &str) -> bool {
    fp_sources::urlutil::is_http_url(key) || fp_sources::is_source_location(key)
}

/// Decodes an image file into an egui image no wider than `max_width`.
pub fn decode(bytes: &[u8], max_width: u32) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max_width {
        let h = (img.height() as u64 * max_width as u64 / img.width() as u64).max(1) as u32;
        img.resize_exact(max_width, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

impl Thumbs {
    /// `key`s are local paths or URLs.
    pub fn new(opener: Arc<Opener>, images: Arc<ImageCache>) -> Thumbs {
        let queue = Arc::new(Queue::new());
        let (tx, done) = unbounded();
        for i in 0..3 {
            let queue = queue.clone();
            let tx = tx.clone();
            let opener = opener.clone();
            let images = images.clone();
            let _ = std::thread::Builder::new()
                .name(format!("fp-thumbs-{i}"))
                .spawn(move || {
                    while let Some((key, url, width)) = queue.pop() {
                        if !is_remote(&url) {
                            let img = opener
                                .fetch(&url, 16 << 20)
                                .ok()
                                .and_then(|b| decode(&b, width));
                            if tx.send((key, img)).is_err() {
                                break;
                            }
                            continue;
                        }
                        let (img, due) = match images.get(&url) {
                            Ok((bytes, due)) => (decode(&bytes, width), due),
                            Err(_) => (None, false),
                        };
                        if tx.send((key.clone(), img)).is_err() {
                            break;
                        }
                        // Shown from the cache first; a changed picture
                        // replaces it.
                        if due
                            && let Ok(Some(bytes)) = images.refresh(&url)
                            && let Some(img) = decode(&bytes, width)
                            && tx.send((key, Some(img))).is_err()
                        {
                            break;
                        }
                    }
                });
        }
        Thumbs::with(queue, done)
    }

    fn with(queue: Arc<Queue>, done: Receiver<(String, Option<ColorImage>)>) -> Thumbs {
        Thumbs {
            textures: HashMap::new(),
            clock: 0,
            pending: HashSet::new(),
            failed: HashSet::new(),
            seen: HashSet::new(),
            queue,
            done,
        }
    }

    /// Uploads finished decodes and drops queued requests for pictures that
    /// left the screen; true when something new is available.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut any = false;
        for (key, img) in self.done.try_iter() {
            self.pending.remove(&key);
            match img {
                Some(img) => {
                    let tex = ctx.load_texture(&key, img, TextureOptions::LINEAR);
                    self.clock += 1;
                    let used = self.clock;
                    self.textures.insert(key, Slot { tex, used });
                    any = true;
                }
                None => {
                    self.failed.insert(key);
                }
            }
        }
        self.forget_unseen();
        let excess = self.textures.len().saturating_sub(MAX_TEXTURES);
        if excess > 0 {
            let mut by_use: Vec<(u64, String)> = self
                .textures
                .iter()
                .map(|(key, slot)| (slot.used, key.clone()))
                .collect();
            by_use.sort_unstable();
            for (_, key) in by_use.into_iter().take(excess) {
                self.textures.remove(&key);
            }
        }
        any
    }

    /// Drops the queued requests nothing asked for since the last call, so
    /// a fast scroll does not decode what has already gone by. They are
    /// asked for again if they come back on screen. A frame that painted
    /// nothing asked for nothing, and says nothing about the screen.
    fn forget_unseen(&mut self) {
        if self.seen.is_empty() {
            return;
        }
        let seen = std::mem::take(&mut self.seen);
        for key in self.queue.retain(|key| seen.contains(key)) {
            self.pending.remove(&key);
        }
    }

    /// The texture for `key`, requesting it when missing.
    pub fn get(&mut self, key: &str) -> Option<TextureHandle> {
        self.lookup(key, key, MAX_WIDTH)
    }

    /// A larger decode of `key` for a banner across the page; the small
    /// one (when there is one) until it is ready.
    pub fn get_wide(&mut self, key: &str) -> Option<TextureHandle> {
        self.lookup(&format!("{key}#wide"), key, WIDE_WIDTH)
            .or_else(|| self.touch(key))
    }

    /// The texture under `key`, marked as just used.
    fn touch(&mut self, key: &str) -> Option<TextureHandle> {
        self.clock += 1;
        let slot = self.textures.get_mut(key)?;
        slot.used = self.clock;
        Some(slot.tex.clone())
    }

    /// The texture under `key`, decoding `url` at `width` when missing.
    fn lookup(&mut self, key: &str, url: &str, width: u32) -> Option<TextureHandle> {
        if let Some(tex) = self.touch(key) {
            return Some(tex);
        }
        if !self.seen.contains(key) {
            self.seen.insert(key.to_string());
        }
        if !self.pending.contains(key) && !self.failed.contains(key) {
            self.pending.insert(key.to_string());
            let req = (key.to_string(), url.to_string(), width);
            if let Some(dropped) = self.queue.push(req) {
                self.pending.remove(&dropped);
            }
        }
        None
    }

    /// Forget failures (e.g. after the library re-thumbnailed).
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    /// Forgets the failed server pictures only, keeping every texture and
    /// the failed local ones: a server that was asleep may answer now.
    pub fn retry_failed_remote(&mut self) {
        self.failed.retain(|k| !is_remote(k));
    }

    /// Drops what was decoded of `key`, so it is read again: the file was
    /// written anew under the same name.
    pub fn forget(&mut self, key: &str) {
        let wide = format!("{key}#wide");
        for k in [key, wide.as_str()] {
            self.textures.remove(k);
            self.failed.remove(k);
        }
    }

    /// Drops every server picture, so each is fetched again.
    pub fn forget_remote(&mut self) {
        self.textures.retain(|k, _| !is_remote(k));
        self.failed.retain(|k| !is_remote(k));
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        self.queue.close();
    }
}

/// Paints `tex` filling `rect` (cropped to its aspect) with rounded corners.
pub fn paint_cover_rounded(
    painter: &egui::Painter,
    rect: egui::Rect,
    tex: &TextureHandle,
    radius: u8,
) {
    painter.add(
        egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE)
            .with_texture(tex.id(), cover_uv(rect, tex)),
    );
}

/// The part of the texture that fills `rect`, cropped to its aspect.
fn cover_uv(rect: egui::Rect, tex: &TextureHandle) -> egui::Rect {
    let [w, h] = tex.size();
    let r_aspect = rect.width() / rect.height().max(1.0);
    let t_aspect = w as f32 / (h as f32).max(1.0);
    if t_aspect > r_aspect {
        let f = r_aspect / t_aspect;
        egui::Rect::from_min_max(
            egui::pos2(0.5 - f / 2.0, 0.0),
            egui::pos2(0.5 + f / 2.0, 1.0),
        )
    } else {
        let f = t_aspect / r_aspect;
        egui::Rect::from_min_max(
            egui::pos2(0.0, 0.5 - f / 2.0),
            egui::pos2(1.0, 0.5 + f / 2.0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::Sender;

    type Done = Sender<(String, Option<ColorImage>)>;

    /// Thumbs with no decoder threads: requests stay in the queue.
    fn thumbs() -> (Thumbs, Done) {
        let (tx, rx) = unbounded();
        (Thumbs::with(Arc::new(Queue::new()), rx), tx)
    }

    fn pixel() -> ColorImage {
        ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255])
    }

    fn strings(keys: &[&str]) -> Vec<String> {
        keys.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn newest_requests_go_first() {
        let (mut t, _tx) = thumbs();
        for key in ["/a.jpg", "/b.jpg", "/c.jpg"] {
            assert!(t.get(key).is_none());
        }
        assert_eq!(t.queue.keys(), strings(&["/c.jpg", "/b.jpg", "/a.jpg"]));
        assert_eq!(t.queue.pop().map(|r| r.0).as_deref(), Some("/c.jpg"));
        // Asking again while it waits does not queue it twice.
        t.get("/a.jpg");
        assert_eq!(t.queue.keys(), strings(&["/b.jpg", "/a.jpg"]));
    }

    #[test]
    fn the_queue_keeps_only_what_is_on_screen() {
        let ctx = egui::Context::default();
        let (mut t, _tx) = thumbs();
        for key in ["/a.jpg", "/b.jpg", "/c.jpg"] {
            t.get(key);
        }
        // All three were on screen: they stay.
        t.poll(&ctx);
        assert_eq!(t.queue.keys().len(), 3);
        // A frame that painted nothing says nothing about the screen.
        t.poll(&ctx);
        assert_eq!(t.queue.keys().len(), 3);
        // The next painted frame shows only /b.jpg, and a banner.
        t.get("/b.jpg");
        t.get_wide("/d.jpg");
        t.poll(&ctx);
        assert_eq!(t.queue.keys(), strings(&["/d.jpg#wide", "/b.jpg"]));
        assert!(!t.pending.contains("/a.jpg") && !t.pending.contains("/c.jpg"));
        // Back on screen, /a.jpg is asked for again.
        t.get("/a.jpg");
        assert_eq!(t.queue.keys()[0], "/a.jpg");
    }

    #[test]
    fn a_full_queue_forgets_its_oldest_and_asks_again_later() {
        let (mut t, _tx) = thumbs();
        for i in 0..=MAX_QUEUED {
            t.get(&format!("/{i}.jpg"));
        }
        let keys = t.queue.keys();
        assert_eq!(keys.len(), MAX_QUEUED);
        assert_eq!(keys[0], format!("/{MAX_QUEUED}.jpg"));
        assert!(!keys.contains(&"/0.jpg".to_string()));
        assert!(!t.pending.contains("/0.jpg"));
        // Seen again, it is queued first, and the next oldest makes room.
        t.get("/0.jpg");
        let keys = t.queue.keys();
        assert_eq!(keys.len(), MAX_QUEUED);
        assert_eq!(keys[0], "/0.jpg");
        assert!(!keys.contains(&"/1.jpg".to_string()));
    }

    #[test]
    fn failed_server_pictures_retry_without_dropping_textures() {
        let ctx = egui::Context::default();
        let (mut t, tx) = thumbs();
        let shown = "http://server/shown.jpg";
        let lost = "http://server/lost.jpg";
        let local = "/pictures/lost.jpg";
        tx.send((shown.into(), Some(pixel()))).unwrap();
        tx.send((lost.into(), None)).unwrap();
        tx.send((local.into(), None)).unwrap();
        assert!(t.poll(&ctx));
        assert!(t.get(lost).is_none() && t.get(local).is_none());
        assert!(t.queue.keys().is_empty());

        t.retry_failed_remote();
        // The picture on screen stays, with no new request for it.
        assert!(t.get(shown).is_some());
        assert!(t.queue.keys().is_empty());
        // The server's lost picture is asked for again; the local one is not.
        assert!(t.get(lost).is_none() && t.get(local).is_none());
        assert_eq!(t.queue.keys(), strings(&[lost]));
    }

    #[test]
    fn the_least_recently_used_texture_goes_first() {
        let ctx = egui::Context::default();
        let (mut t, tx) = thumbs();
        for i in 0..MAX_TEXTURES {
            tx.send((format!("/{i}.jpg"), Some(pixel()))).unwrap();
        }
        t.poll(&ctx);
        assert_eq!(t.textures.len(), MAX_TEXTURES);
        // /0.jpg is the oldest, until it is used.
        assert!(t.get("/0.jpg").is_some());
        tx.send(("/new.jpg".into(), Some(pixel()))).unwrap();
        t.poll(&ctx);
        assert_eq!(t.textures.len(), MAX_TEXTURES);
        assert!(t.textures.contains_key("/0.jpg"));
        assert!(!t.textures.contains_key("/1.jpg"));
        assert!(t.textures.contains_key("/new.jpg"));
    }
}
