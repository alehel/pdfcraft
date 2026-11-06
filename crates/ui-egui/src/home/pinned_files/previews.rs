//! First-page previews of the pinned files, for Home's grid.
//!
//! Nothing is read on the UI thread, so an offline drive or a cloud folder still syncing never
//! stalls the frame. One file at a time is read on a loader thread, and only while Home shows:
//! its size first (a file over [`MAX_FILE_BYTES`] gets no preview), then its first page's size,
//! and the page renders through a one-thread [`RenderPool`] (its panic guard, size caps and
//! watchdog) at the size a card shows it. At most [`MAX_THREADS`] render threads exist at once,
//! counted until they have actually exited. A read that takes longer than [`LOAD_TIMEOUT`]
//! (a hung network drive) is given up on, so the other files still get their previews; at most
//! [`MAX_ABANDONED`] such reads may still be stuck before no new one starts. While Home shows,
//! each file is checked again every [`RECHECK_SECS`]: one changed on disk renders again, one that
//! can't be read shows as unavailable.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use egui::{TextureHandle, Vec2};
use pdfcraft_render::{RenderConfig, RenderPool, RenderRequest, Retiring};

/// Files larger than this get no preview: it would mean reading all of it.
pub const MAX_FILE_BYTES: u64 = 128 << 20;
/// While Home shows, a file's preview is checked against the file this often (seconds).
pub const RECHECK_SECS: f64 = 30.0;
/// Render threads at once, counting those of finished or abandoned renders until they exit.
const MAX_THREADS: usize = 2;
/// The longest side of a preview, in pixels: a card's page at a high-density screen's scale.
const MAX_SIDE: f32 = 512.0;
/// A file read that takes longer than this (seconds) is given up on, and the file shown as
/// unavailable until it is next due.
const LOAD_TIMEOUT: f64 = 20.0;
/// Reads given up on that may still be stuck before no new read starts (threads can't be killed).
const MAX_ABANDONED: usize = 2;

/// What a preview was made from: a file changed on disk is drawn again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

/// What a pinned file's card shows (also for tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewState {
    /// Not read yet, or being read or drawn.
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
    /// When (egui time) the file was last checked.
    checked: f64,
}

/// What the loader found.
enum Found {
    /// As it was: the preview stands.
    Unchanged,
    /// Read: its bytes and its first page's size in points.
    Read {
        bytes: Arc<Vec<u8>>,
        size: (f32, f32),
        stamp: Stamp,
    },
    /// There, but no page can be drawn.
    NoPreview(Stamp),
    Unavailable,
}

struct Loading {
    path: String,
    slot: Arc<Mutex<Option<Found>>>,
    /// When (egui time) the read started.
    since: f64,
}

struct Job {
    path: String,
    pool: RenderPool,
    tag: u64,
    stamp: Stamp,
}

/// The pinned files' previews and the work under way.
#[derive(Default)]
pub struct Previews {
    entries: HashMap<String, Entry>,
    loading: Option<Loading>,
    jobs: Vec<Job>,
    retiring: Vec<Retiring>,
    /// Reads given up on, until their threads let go of their slot.
    abandoned: Vec<Arc<Mutex<Option<Found>>>>,
    next_tag: u64,
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
        for e in self.entries.values_mut() {
            e.checked = f64::NEG_INFINITY;
        }
    }

    /// Render threads that exist now: those of renders under way and of pools let go of.
    pub fn threads(&mut self) -> usize {
        self.retiring.retain_mut(|r| r.running() > 0);
        let retiring: usize = self.retiring.iter_mut().map(Retiring::running).sum();
        let working: usize = self.jobs.iter().map(|j| j.pool.threads()).sum();
        working.saturating_add(retiring)
    }

    /// Once per frame while Home shows: forget files no longer pinned, take what the loader and
    /// the renders finished, and start the next file due. `px` is the box a page is fitted in, in
    /// pixels; `now` is egui time. With `inline`, files are read and drawn on this thread (tests).
    pub(crate) fn frame(&mut self, ctx: &egui::Context, paths: &[String], px: Vec2, now: f64, inline: bool) {
        self.entries.retain(|path, _| paths.contains(path));
        let (keep, gone): (Vec<Job>, Vec<Job>) = self.jobs.drain(..).partition(|j| paths.contains(&j.path));
        self.jobs = keep;
        self.retiring.extend(gone.into_iter().map(|j| j.pool.retire()));

        self.take_loaded(px, now, inline);
        let drew = self.take_rendered(ctx, now);
        self.give_up_slow_read(now);

        self.abandoned.retain(|slot| Arc::strong_count(slot) > 1);
        if self.loading.is_none() && self.abandoned.len() < MAX_ABANDONED && self.threads() < MAX_THREADS {
            let due = paths.iter().find(|path| {
                !self.jobs.iter().any(|j| j.path == **path)
                    && self.entries.get(*path).is_none_or(|e| now < e.checked || now - e.checked >= RECHECK_SECS)
            });
            if let Some(path) = due.cloned() {
                self.start_loading(ctx, path, now, inline);
                // Read inline: draw it in this frame.
                if inline {
                    self.take_loaded(px, now, inline);
                    self.take_rendered(ctx, now);
                }
            }
        }
        // Results arrive between frames: look again soon while anything is under way.
        if drew {
            ctx.request_repaint();
        } else if self.loading.is_some() || !self.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn start_loading(&mut self, ctx: &egui::Context, path: String, now: f64, inline: bool) {
        let known = self.entries.get(&path).and_then(|e| e.stamp);
        let entry = self.entries.entry(path.clone()).or_insert(Entry { stamp: None, tex: None, state: PreviewState::Pending, checked: now });
        // Not again until it is due, whatever happens to this read.
        entry.checked = now;
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let file = path.clone();
        let ctx = ctx.clone();
        let work = move || {
            let found = load(&file, known, MAX_FILE_BYTES);
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
            ctx.request_repaint();
        };
        if inline {
            work();
        } else if std::thread::Builder::new().name("pdfcraft-pinned-preview".into()).spawn(work).is_err() {
            // No thread now: tried again when it is next due.
            return;
        }
        self.loading = Some(Loading { path, slot, since: now });
    }

    /// A read under way for longer than [`LOAD_TIMEOUT`] is given up on: the file shows as
    /// unavailable until it is next due, and the next file may start.
    fn give_up_slow_read(&mut self, now: f64) {
        let Some(loading) = &self.loading else { return };
        // `now < since`: the clock restarted (a new context in tests).
        if now >= loading.since && now - loading.since < LOAD_TIMEOUT {
            return;
        }
        let Some(Loading { path, slot, .. }) = self.loading.take() else { return };
        self.abandoned.push(slot);
        if let Some(entry) = self.entries.get_mut(&path) {
            *entry = Entry { stamp: None, tex: None, state: PreviewState::Unavailable, checked: now };
        }
    }

    /// Take the loader's answer, if it has one, and start drawing a file it read.
    fn take_loaded(&mut self, px: Vec2, now: f64, inline: bool) {
        let Some(loading) = &self.loading else { return };
        let found = loading.slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        let found = match found {
            Some(found) => found,
            // The loader ended without an answer (it panicked): the file is tried again when due.
            None if Arc::strong_count(&loading.slot) == 1 => {
                self.loading = None;
                return;
            }
            None => return,
        };
        let Some(Loading { path, .. }) = self.loading.take() else { return };
        // Unpinned while it was read.
        let Some(entry) = self.entries.get_mut(&path) else { return };
        entry.checked = now;
        match found {
            Found::Unchanged => {}
            Found::Unavailable => {
                *entry = Entry { stamp: None, tex: None, state: PreviewState::Unavailable, checked: now };
            }
            Found::NoPreview(stamp) => {
                *entry = Entry { stamp: Some(stamp), tex: None, state: PreviewState::None, checked: now };
            }
            Found::Read { bytes, size: (w, h), stamp } => {
                // Fitted in the card's box, never longer than a preview's side.
                let scale = (px.x / w).min(px.y / h).min(MAX_SIDE / w.max(h));
                if !(scale.is_finite() && scale > 0.0) {
                    *entry = Entry { stamp: Some(stamp), tex: None, state: PreviewState::None, checked: now };
                    return;
                }
                // Only ever shown on screen, like the Combine thumbnails (AGENTS.md §1.4).
                let config = RenderConfig::default().on_screen();
                let pool = if inline { RenderPool::new_inline(bytes, config) } else { RenderPool::new(bytes, 1, config).without_replacements() };
                self.next_tag = self.next_tag.wrapping_add(1);
                let tag = self.next_tag;
                // The request stays in the pool's queue (it is never replaced) until its answer.
                pool.set_queue(vec![RenderRequest { page: 0, scale, tag, preview: true, ..Default::default() }]);
                self.jobs.push(Job { path, pool, tag, stamp });
            }
        }
    }

    /// Take finished renders: each becomes its card's preview. Returns whether any did.
    fn take_rendered(&mut self, ctx: &egui::Context, now: f64) -> bool {
        let mut drew = false;
        let mut i = 0;
        while i < self.jobs.len() {
            let Some(job) = self.jobs.get(i) else { break };
            let answer = job.pool.try_recv().filter(|out| out.request.tag == job.tag);
            // A pool without threads answers on the call that renders: no answer is none to come.
            if answer.is_none() && !job.pool.is_inline() {
                i += 1;
                continue;
            }
            let job = self.jobs.swap_remove(i);
            drew = true;
            let tex = answer.filter(|out| out.error.is_none()).and_then(|out| texture(ctx, job.tag, [out.width, out.height], out.rgba));
            if let Some(entry) = self.entries.get_mut(&job.path) {
                let state = match &tex {
                    Some(t) => {
                        let [w, h] = t.size().map(|v| u32::try_from(v).unwrap_or(u32::MAX));
                        PreviewState::Ready { width: w, height: h }
                    }
                    None => PreviewState::None,
                };
                *entry = Entry { stamp: Some(job.stamp), tex, state, checked: now };
            }
            // Its thread counts until it exits (at once when idle).
            self.retiring.push(job.pool.retire());
        }
        drew
    }
}

/// A rendered preview as a texture. A raster that doesn't add up, or is larger than a preview
/// can be, is refused, never uploaded.
fn texture(ctx: &egui::Context, tag: u64, size: [u32; 2], pixels: pdfcraft_render::Pixels) -> Option<TextureHandle> {
    let [w, h] = size.map(|v| v as usize);
    let side = (MAX_SIDE as usize).saturating_add(2);
    if w == 0 || h == 0 || w > side || h > side || w.checked_mul(h).and_then(|p| p.checked_mul(4)) != Some(pixels.len()) {
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
    // The size can change after it was looked at: read no more than allowed, whatever it says.
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

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("pdfcraft-preview-{name}-{}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_file_is_read_once_until_it_changes() {
        let path = temp("load.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let Found::Read { size, stamp, .. } = load(&file, None, MAX_FILE_BYTES) else { panic!("the file is read") };
        assert_eq!(size, (300.0, 400.0));
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES), Found::Unchanged));
        std::fs::write(&path, b"%PDF-1.7 not really").unwrap();
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES), Found::NoPreview(_)), "changed, and no page to draw");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES), Found::Unavailable));
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let path = temp("large.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let len = PDF.len() as u64;
        assert!(matches!(load(&file, None, len), Found::Read { .. }), "a file at the limit is read");
        // One byte over: not read (a read of a byte more than the limit would get the whole file).
        assert!(matches!(load(&file, None, len - 1), Found::NoPreview(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_folder_is_no_file() {
        assert!(matches!(load(&std::env::temp_dir().to_string_lossy(), None, MAX_FILE_BYTES), Found::Unavailable));
    }
}
