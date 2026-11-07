//! First-page previews of the pinned files, for Home's grid.
//!
//! Read the way the pinned folders are listed (`folders_ui`): while Home shows, at most every
//! [`RESCAN_SECS`], one worker thread goes through the pinned files in order. A file unchanged
//! since its preview was drawn (same size and modification time) is skipped; any other is read (a
//! file over [`MAX_FILE_BYTES`] gets no preview) and its first page drawn through a one-thread
//! [`RenderPool`] (its panic guard, size caps and watchdog) at [`DENSITY`] pixels per point.
//! Previews arrive one by one as the worker goes. Nothing is read on the UI thread: a file on a
//! drive that doesn't answer holds up the previews after it until it does, never the app.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use egui::{TextureHandle, Vec2};
use pdfcraft_render::{Pixels, RenderConfig, RenderPool, RenderRequest};

/// Files larger than this get no preview: it would mean reading all of it.
pub const MAX_FILE_BYTES: u64 = 128 << 20;
/// While Home shows, the files are checked again at most this often (seconds).
pub const RESCAN_SECS: f64 = 5.0;
/// Pixels per point a preview is drawn at: sharp on most screens, scaled down on the others.
const DENSITY: f32 = 2.0;
/// The longest side of a preview the worker may hand over, in pixels.
const MAX_SIDE: usize = 1024;
/// A page not drawn by then gets no preview (the pool's watchdog gives up on a stuck render
/// sooner; this only bounds the wait if no answer ever comes).
const RENDER_WAIT: Duration = Duration::from_secs(120);

/// What a preview was made from: a file changed on disk is drawn again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

/// What a pinned file's card shows (also for tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewState {
    /// Not read yet.
    Pending,
    /// Its first page, `width` × `height` pixels.
    Ready { width: u32, height: u32 },
    /// The file is there, but no page can be drawn: too large, not a PDF, password-protected, or
    /// its first page wouldn't render.
    None,
    /// The file can't be read: moved or deleted, no permission, or its drive or cloud sync is
    /// offline.
    Unavailable,
}

struct Entry {
    stamp: Option<Stamp>,
    tex: Option<TextureHandle>,
    state: PreviewState,
}

/// What the worker found for a file.
enum Found {
    /// As it was (or not drawn this time): the card stays as it is.
    Unchanged,
    Unavailable,
    /// There, but no page can be drawn.
    NoPreview(Stamp),
    /// Read: its bytes and its first page's size in points (the worker draws it).
    Read {
        bytes: Arc<Vec<u8>>,
        size: (f32, f32),
        stamp: Stamp,
    },
    /// Its first page, drawn.
    Drawn {
        stamp: Stamp,
        size: [usize; 2],
        pixels: Pixels,
    },
}

/// What a scan has found so far, file by file; the worker holds the other reference until it is
/// done.
type Results = Arc<Mutex<Vec<(String, Found)>>>;

/// Reads a file for its preview, unless it is still as the stamp says (tests stand in for it).
type Loader = Arc<dyn Fn(&str, Option<Stamp>) -> Found + Send + Sync>;

/// The pinned files' previews and the scan under way.
#[derive(Default)]
pub struct Previews {
    entries: HashMap<String, Entry>,
    scan: Option<Results>,
    /// When (egui time) the last scan started.
    last_scan: Option<f64>,
    /// Check every file on the next frame.
    stale: bool,
    next_tag: u64,
    /// Stands in for [`load`] (tests).
    loader: Option<Loader>,
}

impl Previews {
    /// What `path`'s card shows now.
    pub fn state(&self, path: &str) -> PreviewState {
        self.entries.get(path).map_or(PreviewState::Pending, |e| e.state.clone())
    }

    /// `path`'s preview: its texture and size in pixels, when it has one.
    pub(crate) fn texture(&self, path: &str) -> Option<(egui::TextureId, Vec2)> {
        let tex = self.entries.get(path)?.tex.as_ref()?;
        let [w, h] = tex.size();
        Some((tex.id(), egui::vec2(w as f32, h as f32)))
    }

    /// Check every file again on the next frame (tests).
    pub fn recheck(&mut self) {
        self.stale = true;
    }

    /// Once per frame while Home shows: forget files no longer pinned, take what the worker found,
    /// and start a scan when one is due. `now` is egui time. With `inline`, the scan runs on this
    /// thread (tests).
    pub(crate) fn frame(&mut self, ctx: &egui::Context, paths: &[String], now: f64, inline: bool) {
        self.entries.retain(|path, _| paths.contains(path));
        self.take_results(ctx, paths);
        if self.scan.is_some() || paths.is_empty() {
            return;
        }
        // `now < last` means the clock restarted (a new context in tests): scan again.
        let due = self.stale || self.last_scan.is_none_or(|last| now < last || now - last >= RESCAN_SECS);
        if !due {
            return;
        }
        self.stale = false;
        self.last_scan = Some(now);
        let files: Vec<(String, Option<Stamp>)> = paths.iter().map(|p| (p.clone(), self.entries.get(p).and_then(|e| e.stamp))).collect();
        let results: Results = Arc::default();
        let out = results.clone();
        let wake = ctx.clone();
        let loader = self.loader.clone();
        let work = move || {
            for (path, known) in files {
                let found = match &loader {
                    Some(load) => load(&path, known),
                    None => load(&path, known, MAX_FILE_BYTES),
                };
                let found = match found {
                    Found::Read { bytes, size, stamp } => draw(bytes, size, stamp, inline),
                    other => other,
                };
                out.lock().unwrap_or_else(PoisonError::into_inner).push((path, found));
                wake.request_repaint();
            }
        };
        if inline {
            work();
        } else if std::thread::Builder::new().name("pdfcraft-pinned-previews".into()).spawn(work).is_err() {
            // No worker: tried again when the next scan is due.
            return;
        }
        self.scan = Some(results);
        self.take_results(ctx, paths);
    }

    /// Take what the scan found so far, and let it go once its worker is done.
    fn take_results(&mut self, ctx: &egui::Context, paths: &[String]) {
        let Some(results) = &self.scan else { return };
        // Whether the worker is done first, then its results: it leaves each before it lets go,
        // so the last one is never missed. A worker that panicked is done too.
        let done = Arc::strong_count(results) == 1;
        let found = std::mem::take(&mut *results.lock().unwrap_or_else(PoisonError::into_inner));
        if done {
            self.scan = None;
        }
        for (path, found) in found {
            // Unpinned while it was read.
            if !paths.contains(&path) {
                continue;
            }
            let entry = match found {
                Found::Unchanged => continue,
                Found::Unavailable => Entry { stamp: None, tex: None, state: PreviewState::Unavailable },
                Found::NoPreview(stamp) | Found::Read { stamp, .. } => Entry { stamp: Some(stamp), tex: None, state: PreviewState::None },
                Found::Drawn { stamp, size, pixels } => {
                    self.next_tag = self.next_tag.wrapping_add(1);
                    let tex = texture(ctx, self.next_tag, size, pixels);
                    let state = tex.as_ref().map_or(PreviewState::None, |t| {
                        let [width, height] = t.size().map(|v| u32::try_from(v).unwrap_or(u32::MAX));
                        PreviewState::Ready { width, height }
                    });
                    Entry { stamp: Some(stamp), tex, state }
                }
            };
            self.entries.insert(path, entry);
        }
    }
}

/// Draw the first page of `bytes`, `size` points, fitted in a card's page box (on the worker).
fn draw(bytes: Arc<Vec<u8>>, (w, h): (f32, f32), stamp: Stamp, inline: bool) -> Found {
    let fit = super::PAPER * DENSITY;
    let scale = (fit.x / w).min(fit.y / h);
    if !(scale.is_finite() && scale > 0.0) {
        return Found::NoPreview(stamp);
    }
    // Only ever shown on screen, like the Combine thumbnails (AGENTS.md §1.4).
    let config = RenderConfig::default().on_screen();
    let pool = if inline {
        Some(RenderPool::new_inline(bytes, config))
    } else {
        RenderPool::threaded(bytes, 1, config).map(RenderPool::without_replacements)
    };
    // No thread to draw on: tried again on the next scan.
    let Some(pool) = pool else { return Found::Unchanged };
    pool.set_queue(vec![RenderRequest { page: 0, scale, tag: 1, preview: true, ..Default::default() }]);
    let deadline = Instant::now() + RENDER_WAIT;
    loop {
        // The pool's watchdog runs in here, and answers a stuck render with an error.
        if let Some(out) = pool.try_recv() {
            let size = [out.width, out.height].map(|v| usize::try_from(v).unwrap_or(usize::MAX));
            return match out.error {
                None => Found::Drawn { stamp, size, pixels: out.rgba },
                Some(_) => Found::NoPreview(stamp),
            };
        }
        if pool.is_inline() || Instant::now() >= deadline {
            return Found::NoPreview(stamp);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A drawn preview as a texture. A raster that doesn't add up, or is larger than a preview can
/// be, is refused, never uploaded.
fn texture(ctx: &egui::Context, tag: u64, [w, h]: [usize; 2], pixels: Pixels) -> Option<TextureHandle> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || w.checked_mul(h).and_then(|p| p.checked_mul(4)) != Some(pixels.len()) {
        return None;
    }
    let image = crate::canvas::texture_image([w, h], pixels);
    Some(ctx.load_texture(format!("pinned-file-preview-{tag}"), image, egui::TextureOptions::LINEAR))
}

/// Read `path` for its preview, unless it is still as `known` says. Files over `max_bytes` aren't
/// read.
fn load(path: &str, known: Option<Stamp>, max_bytes: u64) -> Found {
    use std::io::Read;

    let Ok(meta) = std::fs::metadata(path) else { return Found::Unavailable };
    if !meta.is_file() {
        return Found::Unavailable;
    }
    let stamp = Stamp { len: meta.len(), modified: meta.modified().ok() };
    if known == Some(stamp) {
        return Found::Unchanged;
    }
    if stamp.len > max_bytes {
        return Found::NoPreview(stamp);
    }
    let Ok(file) = std::fs::File::open(path) else { return Found::Unavailable };
    // The size can change after it was looked at: no more than allowed is read, whatever it says.
    let mut bytes = Vec::with_capacity(usize::try_from(stamp.len).unwrap_or(0));
    if file.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes).is_err() {
        return Found::Unavailable;
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Found::NoPreview(stamp);
    }
    let bytes = Arc::new(bytes);
    match pdfcraft_render::page_size(&bytes, None, 0) {
        Some(size) => Found::Read { bytes, size, stamp },
        None => Found::NoPreview(stamp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDF: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

    fn temp(name: &str, bytes: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!("pdfcraft-preview-{name}-{}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Run frames (on this thread, `now` moving on a little each time) until `done`, or fail.
    fn frames_until(p: &mut Previews, ctx: &egui::Context, paths: &[String], done: impl Fn(&Previews) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut now = 0.0;
        while !done(p) {
            assert!(Instant::now() < deadline, "timed out");
            p.frame(ctx, paths, now, false);
            std::thread::sleep(Duration::from_millis(5));
            now += 0.005;
        }
    }

    #[test]
    fn a_file_is_read_once_until_it_changes() {
        let file = temp("load.pdf", PDF);
        let Found::Read { size, stamp, .. } = load(&file, None, MAX_FILE_BYTES) else { panic!("the file is read") };
        assert_eq!(size, (300.0, 400.0));
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES), Found::Unchanged));
        std::fs::write(&file, b"%PDF-1.7 not really").unwrap();
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES), Found::NoPreview(_)), "changed, and no page to draw");
        let _ = std::fs::remove_file(&file);
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES), Found::Unavailable));
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let file = temp("large.pdf", PDF);
        let len = PDF.len() as u64;
        assert!(matches!(load(&file, None, len), Found::Read { .. }), "a file at the limit is read");
        // One byte over: not read at all (its size says so before any read).
        assert!(matches!(load(&file, None, len - 1), Found::NoPreview(_)));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_folder_is_no_file() {
        assert!(matches!(load(&std::env::temp_dir().to_string_lossy(), None, MAX_FILE_BYTES), Found::Unavailable));
    }

    #[test]
    fn files_are_read_and_drawn_off_the_ui_thread() {
        let file = temp("threaded.pdf", PDF);
        let paths = vec![file.clone()];
        let readers = Arc::new(Mutex::new(Vec::new()));
        let seen = readers.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known| {
                seen.lock().unwrap().push(std::thread::current().id());
                load(f, known, MAX_FILE_BYTES)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        frames_until(&mut p, &ctx, &paths, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let PreviewState::Ready { width, height } = p.state(&file) else { panic!("drawn") };
        assert_eq!((width, height), (219, 292), "the 300 × 400 point page, fitted in a card's page box at {DENSITY} pixels per point");
        let readers = readers.lock().unwrap().clone();
        assert!(!readers.is_empty() && readers.iter().all(|id| *id != std::thread::current().id()), "not on the UI thread");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn previews_arrive_one_by_one_and_a_slow_file_holds_up_only_those_after_it() {
        let (first, last) = (temp("first.pdf", PDF), temp("last.pdf", PDF));
        let slow = "/nowhere/slow.pdf".to_owned();
        let paths = vec![first.clone(), slow.clone(), last.clone()];
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let wait = gate.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known| {
                if f == "/nowhere/slow.pdf" {
                    let mut open = wait.0.lock().unwrap();
                    while !*open {
                        open = wait.1.wait(open).unwrap();
                    }
                    return Found::Unavailable;
                }
                load(f, known, MAX_FILE_BYTES)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // The frames go on while the worker waits on the slow file; the file before it is in.
        frames_until(&mut p, &ctx, &paths, |p| matches!(p.state(&first), PreviewState::Ready { .. }));
        assert_eq!(p.state(&last), PreviewState::Pending);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        frames_until(&mut p, &ctx, &paths, |p| matches!(p.state(&last), PreviewState::Ready { .. }));
        assert_eq!(p.state(&slow), PreviewState::Unavailable);
        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&last);
    }

    #[test]
    fn an_unchanged_file_isnt_read_or_drawn_again() {
        let file = temp("unchanged.pdf", PDF);
        let paths = vec![file.clone()];
        let reads = Arc::new(Mutex::new(0usize));
        let count = reads.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known| {
                let found = load(f, known, MAX_FILE_BYTES);
                if matches!(found, Found::Read { .. }) {
                    *count.lock().unwrap() += 1;
                }
                found
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        frames_until(&mut p, &ctx, &paths, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        for _ in 0..3 {
            p.recheck();
            frames_until(&mut p, &ctx, &paths, |p| p.scan.is_none() && !p.stale);
        }
        assert_eq!(*reads.lock().unwrap(), 1);
        assert!(matches!(p.state(&file), PreviewState::Ready { .. }));
        let _ = std::fs::remove_file(&file);
    }
}
