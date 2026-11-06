//! First-page previews of the pinned files, for Home's grid.
//!
//! Nothing is read on the UI thread, so an offline drive or a cloud folder still syncing never
//! stalls the frame. One file at a time is read on a loader thread (so at most one download runs,
//! and a large file downloading slowly holds back the files after it), and only while Home shows:
//! its size first (a file over [`MAX_FILE_BYTES`] gets no preview), then its first page's size,
//! and the page renders through a one-thread [`RenderPool`] (its panic guard, size caps and
//! watchdog) at the size a card shows it. At most [`MAX_THREADS`] render threads exist at once,
//! counted until they have actually exited: a file read while none is free waits for one. A read
//! that makes no progress for [`LOAD_TIMEOUT`] (a hung network drive; a slow download still
//! progressing is waited for) stops holding up the other files: it is still waited for and its
//! answer used, and its file isn't read again meanwhile, even unpinned and pinned again; only
//! while [`MAX_STUCK`] such reads are stuck does no new one start. While Home shows, each file is checked again every [`RECHECK_SECS`]: one
//! changed on disk renders again, one that can't be read shows as unavailable, and a preview
//! is drawn again when the screen's density rises well past the one it was drawn for (a smaller
//! one only scales it down, so zooming the interface doesn't read every file again). While Home doesn't show, the work stops
//! ([`Previews::park_unless_drawn`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
/// A file read that makes no progress for this long (seconds) stops holding up the other files.
const LOAD_TIMEOUT: f64 = 20.0;
/// A file is read this many bytes at a time, each one counting as progress.
const CHUNK: usize = 256 << 10;
/// A read with no progress for this long (seconds) may be waiting on a drive that doesn't answer:
/// its file isn't opened on a click meanwhile (opening asks the drive on the UI thread).
const STALL_HINT: f64 = 2.0;
/// A preview is drawn again when the box it is shown in grows past this factor (in pixels).
const SHARPER: f32 = 1.5;
/// Reads given up on that may still be stuck before no new read starts (threads can't be
/// killed). Each is a different file, pinned now or earlier: more than can be pinned at once.
const MAX_STUCK: usize = super::MAX_PINNED_FILES + 8;

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
    /// The box (pixels) its preview was drawn for.
    px: Vec2,
}

impl Entry {
    fn new(state: PreviewState, stamp: Option<Stamp>, now: f64) -> Self {
        Self { stamp, tex: None, state, checked: now, px: Vec2::ZERO }
    }
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

/// A file being read on a loader thread.
struct Reading {
    path: String,
    slot: Arc<Mutex<Option<Found>>>,
    /// Bytes read so far, counted by the loader.
    progress: Arc<AtomicU64>,
    /// `progress` when last looked at.
    seen: u64,
    /// When (egui time) the read started or last made progress.
    since: f64,
    /// No progress for [`LOAD_TIMEOUT`]: still polled, so a read that finishes is used, but no
    /// longer holding up the other files.
    abandoned: bool,
}

/// What a finished read left in its slot.
enum Answer {
    Found(Found),
    /// The loader ended without one (it panicked).
    Lost,
    /// Not yet.
    Waiting,
}

impl Reading {
    /// The read's answer, if it has one. With `hold_read`, a file read (its bytes) stays in the
    /// slot: no render thread is free for it yet.
    fn answer(&self, hold_read: bool) -> Answer {
        // Whether the thread has let go first, then the slot: a thread fills the slot before it
        // lets go, so an answer given just before is never mistaken for none.
        let ended = Arc::strong_count(&self.slot) == 1;
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if hold_read && matches!(*slot, Some(Found::Read { .. })) {
            return Answer::Waiting;
        }
        match slot.take() {
            Some(found) => Answer::Found(found),
            None if ended => Answer::Lost,
            None => Answer::Waiting,
        }
    }
}

struct Job {
    path: String,
    pool: RenderPool,
    tag: u64,
    stamp: Stamp,
    px: Vec2,
}

/// Reads a file for its preview, unless it is still as the stamp says, counting the bytes read
/// (tests stand in for it).
type Loader = Arc<dyn Fn(&str, Option<Stamp>, &AtomicU64) -> Found + Send + Sync>;

/// The pinned files' previews and the work under way.
#[derive(Default)]
pub struct Previews {
    entries: HashMap<String, Entry>,
    /// The read under way, and those given up on whose threads haven't finished.
    reads: Vec<Reading>,
    jobs: Vec<Job>,
    retiring: Vec<Retiring>,
    next_tag: u64,
    /// The pass (egui's count) Home last asked for previews in.
    drawn_in: Option<u64>,
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

    /// Whether opening `path` now could wait on a drive that doesn't answer: it was found
    /// unavailable, or a read of it has made no progress for [`STALL_HINT`] (`now` is egui time).
    pub(crate) fn unresponsive(&self, path: &str, now: f64) -> bool {
        self.state(path) == PreviewState::Unavailable
            || self.reads.iter().any(|r| r.path == path && Arc::strong_count(&r.slot) > 1 && (r.abandoned || now - r.since > STALL_HINT))
    }

    /// Check `path` again on the next frame, unless a read of it is under way.
    pub(crate) fn recheck_now(&mut self, path: &str) {
        if let Some(e) = self.entries.get_mut(path) {
            e.checked = f64::NEG_INFINITY;
        }
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

    /// Reads under way, those given up on included (tests).
    pub fn reads(&self) -> usize {
        self.reads.len()
    }

    /// Once per frame while Home shows: forget files no longer pinned, take what the loader and
    /// the renders finished, and start the next file due. `px` is the box a page is fitted in, in
    /// pixels; `now` is egui time. With `inline`, files are read and drawn on this thread (tests).
    pub(crate) fn frame(&mut self, ctx: &egui::Context, paths: &[String], px: Vec2, now: f64, inline: bool) {
        self.drawn_in = Some(ctx.cumulative_pass_nr());
        // Reads of files unpinned stay listed until they finish: such a file isn't read again
        // while one is stuck, even pinned again, and its answer is dropped.
        self.entries.retain(|path, _| paths.contains(path));
        // A file pinned again while its earlier read is still under way waits for that read (its
        // answer is used), and its card says so meanwhile.
        for r in &self.reads {
            if paths.contains(&r.path) && !self.entries.contains_key(&r.path) {
                let state = if r.abandoned { PreviewState::Unavailable } else { PreviewState::Pending };
                self.entries.insert(r.path.clone(), Entry::new(state, None, now));
            }
        }
        let (keep, gone): (Vec<Job>, Vec<Job>) = self.jobs.drain(..).partition(|j| paths.contains(&j.path));
        self.jobs = keep;
        self.retiring.extend(gone.into_iter().map(|j| j.pool.retire()));
        // A preview drawn for a box well smaller than now (a denser screen) is drawn again; the old
        // one stands in meanwhile. Asked for every frame until a sharp enough one is in, so a check
        // under way that finds the file unchanged doesn't end it.
        let off = |drawn: f32, wanted: f32| wanted > drawn * SHARPER;
        let reads = &self.reads;
        for (path, e) in self.entries.iter_mut() {
            if e.tex.is_some() && (off(e.px.x, px.x) || off(e.px.y, px.y)) && !reads.iter().any(|r| r.path == *path) {
                e.stamp = None;
                e.checked = f64::NEG_INFINITY;
            }
        }

        self.take_reads(px, now, inline);
        let mut drew = self.take_rendered(ctx, now);
        self.give_up_slow_read(now);

        let reading = self.reads.iter().any(|r| !r.abandoned);
        let stuck = self.reads.iter().filter(|r| r.abandoned).count();
        // A file whose read is under way (or stuck) isn't read again meanwhile.
        let due = paths
            .iter()
            .find(|path| {
                !self.jobs.iter().any(|j| j.path == **path)
                    && !self.reads.iter().any(|r| r.path == **path)
                    && self.entries.get(*path).is_none_or(|e| now < e.checked || now - e.checked >= RECHECK_SECS)
            })
            .cloned();
        // Waiting for render threads to exit, which wakes nothing: look again soon. (A read
        // under way or stuck wakes the app itself when it finishes.)
        let mut waiting = false;
        if let Some(path) = due
            && !reading
            && stuck < MAX_STUCK
        {
            if self.threads() < MAX_THREADS {
                self.start_reading(ctx, path, now, inline);
                // Read inline: draw it in this frame.
                if inline {
                    self.take_reads(px, now, inline);
                    drew |= self.take_rendered(ctx, now);
                }
            } else {
                waiting = true;
            }
        }
        self.schedule(ctx, now, drew, waiting);
    }

    /// Ask for the frames the work needs: soon while renders are under way (their pools don't
    /// wake the app), when a read is due to be given up on, while a file waits for a thread to
    /// exit, and when the next file is due to be checked. A read wakes the app when it finishes,
    /// so a file due but waiting for a read (its own, or another's) asks for nothing.
    fn schedule(&self, ctx: &egui::Context, now: f64, drew: bool, waiting: bool) {
        use std::time::Duration;
        let after = |secs: f64| {
            if secs.is_finite() {
                ctx.request_repaint_after(Duration::from_secs_f64(secs.clamp(0.0, RECHECK_SECS)));
            }
        };
        if drew {
            ctx.request_repaint();
        }
        if !self.jobs.is_empty() {
            after(0.03);
        }
        if let Some(r) = self.reads.iter().find(|r| !r.abandoned) {
            after(r.since + LOAD_TIMEOUT - now);
        }
        if waiting {
            after(0.25);
        }
        let next = self
            .entries
            .iter()
            .filter(|(path, _)| !self.reads.iter().any(|r| r.path == **path) && !self.jobs.iter().any(|j| j.path == **path))
            .map(|(_, e)| e.checked + RECHECK_SECS - now)
            .filter(|s| s.is_finite() && *s > 0.0)
            .fold(f64::INFINITY, f64::min);
        if next.is_finite() {
            after(next);
        }
    }

    fn start_reading(&mut self, ctx: &egui::Context, path: String, now: f64, inline: bool) {
        let known = self.entries.get(&path).and_then(|e| e.stamp);
        let entry = self.entries.entry(path.clone()).or_insert_with(|| Entry::new(PreviewState::Pending, None, now));
        // Not again until it is due, whatever happens to this read.
        entry.checked = now;
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let progress = Arc::new(AtomicU64::new(0));
        let counted = progress.clone();
        let file = path.clone();
        let ctx = ctx.clone();
        let loader = self.loader.clone();
        let work = move || {
            let found = match &loader {
                Some(load) => load(&file, known, &counted),
                None => load(&file, known, MAX_FILE_BYTES, &counted),
            };
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
            ctx.request_repaint();
        };
        if inline {
            work();
        } else if std::thread::Builder::new().name("pdfcraft-pinned-preview".into()).spawn(work).is_err() {
            // No thread now: tried again when it is next due.
            return;
        }
        self.reads.push(Reading { path, slot, progress, seen: 0, since: now, abandoned: false });
    }

    /// The read under way with no progress for [`LOAD_TIMEOUT`] stops holding up the other files.
    /// A file never read yet shows as unavailable; one read before keeps its card as it was. Either
    /// way the read's answer is still used if it comes.
    fn give_up_slow_read(&mut self, now: f64) {
        // A read whose thread is done (its answer waits for a render thread) isn't slow.
        for r in self.reads.iter_mut().filter(|r| !r.abandoned && Arc::strong_count(&r.slot) > 1) {
            // Bytes arrived since the last look: still going (a slow download, say).
            let read = r.progress.load(Ordering::Relaxed);
            if read != r.seen {
                r.seen = read;
                r.since = now;
                continue;
            }
            // `now < since`: the clock restarted (a new context in tests).
            if now >= r.since && now - r.since < LOAD_TIMEOUT {
                continue;
            }
            r.abandoned = true;
            if let Some(entry) = self.entries.get_mut(&r.path).filter(|e| e.tex.is_none() && e.stamp.is_none()) {
                entry.state = PreviewState::Unavailable;
            }
        }
    }

    /// Unless Home asked for previews in this pass (it doesn't show), let go of the work only its
    /// frames move on: renders under way are stopped and files read but not yet drawn are let go
    /// of, so neither holds a file's bytes meanwhile. Both are done again when Home shows.
    pub(crate) fn park_unless_drawn(&mut self, ctx: &egui::Context) {
        if self.drawn_in != Some(ctx.cumulative_pass_nr()) {
            self.park();
        }
    }

    fn park(&mut self) {
        for job in std::mem::take(&mut self.jobs) {
            if let Some(e) = self.entries.get_mut(&job.path) {
                e.checked = f64::NEG_INFINITY;
            }
            self.retiring.push(job.pool.retire());
        }
        for r in &self.reads {
            let mut slot = r.slot.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*slot, Some(Found::Read { .. })) {
                // The read counts as lost (its thread is done): read again when due, now.
                *slot = None;
                if let Some(e) = self.entries.get_mut(&r.path) {
                    e.checked = f64::NEG_INFINITY;
                }
            }
        }
    }

    /// Take the reads' answers, given-up ones included, and start drawing each file read.
    fn take_reads(&mut self, px: Vec2, now: f64, inline: bool) {
        let mut i = 0;
        while i < self.reads.len() {
            // A file read waits in its slot for a free render thread (answers of reads given up
            // on can arrive together).
            let hold = !inline && self.threads() >= MAX_THREADS;
            let answer = self.reads.get(i).map_or(Answer::Waiting, |r| r.answer(hold));
            match answer {
                Answer::Waiting => i += 1,
                // Tried again when it is next due.
                Answer::Lost => {
                    self.reads.swap_remove(i);
                }
                Answer::Found(found) => {
                    let read = self.reads.swap_remove(i);
                    self.take_found(read.path, found, px, now, inline);
                }
            }
        }
    }

    /// Use what a read found for `path`.
    fn take_found(&mut self, path: String, found: Found, px: Vec2, now: f64, inline: bool) {
        // Unpinned while it was read.
        let Some(entry) = self.entries.get_mut(&path) else { return };
        entry.checked = now;
        match found {
            // As it was when the read started; but a card made since (the file unpinned and pinned
            // again meanwhile) knows nothing of that: read again now.
            Found::Unchanged if entry.stamp.is_none() => entry.checked = f64::NEG_INFINITY,
            // As it was: the preview (or why there is none) stands, even after a slow read.
            Found::Unchanged => {}
            Found::Unavailable => *entry = Entry::new(PreviewState::Unavailable, None, now),
            Found::NoPreview(stamp) => *entry = Entry::new(PreviewState::None, Some(stamp), now),
            Found::Read { bytes, size: (w, h), stamp } => {
                // Fitted in the card's box, never longer than a preview's side.
                let scale = (px.x / w).min(px.y / h).min(MAX_SIDE / w.max(h));
                if !(scale.is_finite() && scale > 0.0) {
                    *entry = Entry::new(PreviewState::None, Some(stamp), now);
                    return;
                }
                // Only ever shown on screen, like the Combine thumbnails (AGENTS.md §1.4).
                let config = RenderConfig::default().on_screen();
                let pool = if inline { RenderPool::new_inline(bytes, config) } else { RenderPool::new(bytes, 1, config).without_replacements() };
                // A pool that couldn't start its thread would render here, on the UI thread: not
                // for a preview. Tried again when it is next due.
                if pool.is_inline() && !inline {
                    return;
                }
                self.next_tag = self.next_tag.wrapping_add(1);
                let tag = self.next_tag;
                // The request stays in the pool's queue (it is never replaced) until its answer.
                pool.set_queue(vec![RenderRequest { page: 0, scale, tag, preview: true, ..Default::default() }]);
                self.jobs.push(Job { path, pool, tag, stamp, px });
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
                let state = tex.as_ref().map_or(PreviewState::None, ready);
                *entry = Entry { stamp: Some(job.stamp), tex, state, checked: now, px: job.px };
            }
            // Its thread counts until it exits (at once when idle).
            self.retiring.push(job.pool.retire());
        }
        drew
    }
}

/// The state of a card showing `tex`.
fn ready(tex: &TextureHandle) -> PreviewState {
    let [width, height] = tex.size().map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    PreviewState::Ready { width, height }
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

/// Read `path` for its preview, unless it is still as `known` says, counting the bytes read in
/// `progress`. Files over `max_bytes` aren't read.
fn load(path: &str, known: Option<Stamp>, max_bytes: u64, progress: &AtomicU64) -> Found {
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
    let Ok(mut file) = std::fs::File::open(path) else { return Found::Unavailable };
    // A chunk at a time, so the reader can tell a slow download from a hung drive, and memory
    // grows only as the file arrives. The size can change after it was looked at: no more than
    // allowed is read, whatever it says.
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let n = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Found::Unavailable,
        };
        // Room for the whole file once its data flows (not for a read that stalls before), so
        // the buffer doesn't grow by doubling to twice the file's size. Within the limit checked
        // above; a file grown since only grows the buffer as usual.
        if bytes.capacity() == 0 {
            bytes.reserve_exact(usize::try_from(stamp.len).unwrap_or(0).max(n));
        }
        bytes.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        progress.store(read, Ordering::Relaxed);
        if read > max_bytes {
            return Found::NoPreview(stamp);
        }
    }
    bytes.shrink_to_fit();
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
        let Found::Read { size, stamp, .. } = load(&file, None, MAX_FILE_BYTES, &AtomicU64::new(0)) else { panic!("the file is read") };
        assert_eq!(size, (300.0, 400.0));
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES, &AtomicU64::new(0)), Found::Unchanged));
        std::fs::write(&path, b"%PDF-1.7 not really").unwrap();
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES, &AtomicU64::new(0)), Found::NoPreview(_)), "changed, and no page to draw");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(load(&file, Some(stamp), MAX_FILE_BYTES, &AtomicU64::new(0)), Found::Unavailable));
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let path = temp("large.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let len = PDF.len() as u64;
        assert!(matches!(load(&file, None, len, &AtomicU64::new(0)), Found::Read { .. }), "a file at the limit is read");
        // One byte over: not read at all (its size says so before any read).
        assert!(matches!(load(&file, None, len - 1, &AtomicU64::new(0)), Found::NoPreview(_)));
        let _ = std::fs::remove_file(&path);
    }

    /// A gate a stand-in loader waits at until the test opens it.
    #[derive(Default)]
    struct Gate(Mutex<bool>, std::sync::Condvar);

    impl Gate {
        fn wait(&self) {
            let mut open = self.0.lock().unwrap();
            while !*open {
                open = self.1.wait(open).unwrap();
            }
        }

        fn open(&self) {
            *self.0.lock().unwrap() = true;
            self.1.notify_all();
        }
    }

    const PX: Vec2 = egui::vec2(112.0, 146.0);

    /// Run frames (on this thread, `now` moving on a little each time) until `done`, or fail.
    fn frames_until(p: &mut Previews, ctx: &egui::Context, paths: &[String], now: &mut f64, done: impl Fn(&mut Previews) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !done(p) {
            assert!(std::time::Instant::now() < deadline, "timed out");
            p.frame(ctx, paths, PX, *now, false);
            std::thread::sleep(std::time::Duration::from_millis(5));
            *now += 0.005;
        }
    }

    #[test]
    fn files_are_read_and_drawn_off_the_ui_thread() {
        let path = temp("threaded.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let readers = Arc::new(Mutex::new(Vec::new()));
        let seen = readers.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                seen.lock().unwrap().push(std::thread::current().id());
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let readers = readers.lock().unwrap().clone();
        assert_eq!(readers.len(), 1, "read once");
        assert!(readers.iter().all(|id| *id != std::thread::current().id()), "not on the UI thread");
        // The render's thread goes once its preview is in.
        frames_until(&mut p, &ctx, &paths, &mut now, |p| p.threads() == 0);
        assert_eq!(p.reads(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stuck_read_holds_up_neither_the_others_nor_itself() {
        let ok = temp("unstuck.pdf", PDF).to_string_lossy().into_owned();
        let stuck = "/nowhere/stuck.pdf".to_owned();
        let paths = vec![stuck.clone(), ok.clone()];
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                if f.ends_with("stuck.pdf") {
                    *count.lock().unwrap() += 1;
                    wait.wait();
                    return Found::NoPreview(Stamp { len: 0, modified: None });
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        p.frame(&ctx, &paths, PX, 0.0, false);
        p.frame(&ctx, &paths, PX, 1.0, false);
        assert_eq!(p.state(&ok), PreviewState::Pending, "one read at a time");
        // Past the timeout: the stuck file is marked, and the next one is read.
        let mut now = LOAD_TIMEOUT + 1.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&ok), PreviewState::Ready { .. }));
        assert_eq!(p.state(&stuck), PreviewState::Unavailable);
        // Due again, but its read is still under way: not started a second time.
        let mut later = 10.0 * RECHECK_SECS;
        for _ in 0..3 {
            p.frame(&ctx, &paths, PX, later, false);
            assert_eq!(p.reads.iter().filter(|r| r.path == stuck).count(), 1);
        }
        // When it does finish, its answer is used: no second read was needed.
        gate.open();
        frames_until(&mut p, &ctx, &paths, &mut later, |p| p.reads() == 0);
        assert_eq!(p.state(&stuck), PreviewState::None);
        assert_eq!(*calls.lock().unwrap(), 1);
        let _ = std::fs::remove_file(&ok);
    }

    #[test]
    fn a_slow_check_keeps_the_preview() {
        let path = temp("slow.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                let first = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n == 1
                };
                if !first {
                    wait.wait();
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let ready = p.state(&file);
        // Checked again, slowly (a file being synced): past the timeout the preview stays.
        p.recheck();
        p.frame(&ctx, &paths, PX, now, false);
        assert_eq!(p.reads(), 1);
        p.frame(&ctx, &paths, PX, now + LOAD_TIMEOUT + 1.0, false);
        assert_eq!(p.state(&file), ready);
        assert!(p.texture(&file).is_some());
        gate.open();
        let mut later = now + LOAD_TIMEOUT + 1.0;
        frames_until(&mut p, &ctx, &paths, &mut later, |p| p.reads() == 0);
        assert_eq!(p.state(&file), ready, "unchanged, and still drawn");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_slow_check_keeps_a_card_without_a_preview_as_it_was() {
        let path = temp("slow-broken.pdf", b"%PDF-1.7 not really");
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                let first = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n == 1
                };
                if !first {
                    wait.wait();
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| p.state(&file) == PreviewState::None);
        p.recheck();
        p.frame(&ctx, &paths, PX, now, false);
        p.frame(&ctx, &paths, PX, now + LOAD_TIMEOUT + 1.0, false);
        assert_eq!(p.state(&file), PreviewState::None, "known to be there: not called unavailable");
        gate.open();
        let mut later = now + LOAD_TIMEOUT + 1.0;
        frames_until(&mut p, &ctx, &paths, &mut later, |p| p.reads() == 0);
        assert_eq!(p.state(&file), PreviewState::None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn late_answers_wait_for_a_render_thread() {
        let files: Vec<String> = (0..4).map(|i| temp(&format!("late{i}.pdf"), PDF).to_string_lossy().into_owned()).collect();
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                *count.lock().unwrap() += 1;
                wait.wait();
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // Each read is given up on in turn, until all four are under way.
        for k in 0..4 {
            p.frame(&ctx, &files, PX, f64::from(k) * (LOAD_TIMEOUT + 1.0), false);
        }
        assert_eq!(p.reads(), 4);
        // They all answer at once.
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !p.reads.iter().all(|r| Arc::strong_count(&r.slot) == 1) {
            assert!(std::time::Instant::now() < deadline, "the reads finish");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut now = 4.0 * (LOAD_TIMEOUT + 1.0);
        frames_until(&mut p, &ctx, &files, &mut now, |p| {
            assert!(p.threads() <= MAX_THREADS, "{} render threads", p.threads());
            files.iter().all(|f| matches!(p.state(f), PreviewState::Ready { .. }))
        });
        assert_eq!(*calls.lock().unwrap(), 4, "the answers waited: no file was read twice");
        for f in &files {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn hung_files_dont_stop_the_others() {
        let ok = temp("after-hung.pdf", PDF).to_string_lossy().into_owned();
        let hung: Vec<String> = (0..6).map(|i| format!("/nowhere/hung{i}.pdf")).collect();
        let mut paths = hung.clone();
        paths.push(ok.clone());
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (wait, seen) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                if f.starts_with("/nowhere/") {
                    seen.lock().unwrap().push(f.to_owned());
                    wait.wait();
                    return Found::NoPreview(Stamp { len: 0, modified: None });
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        for _ in 0..hung.len() {
            p.frame(&ctx, &paths, PX, now, false);
            now += LOAD_TIMEOUT + 1.0;
        }
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&ok), PreviewState::Ready { .. }));
        // A stuck file unpinned and pinned again isn't read again while its read is stuck, and its
        // card says so; the stuck read's answer is used when it comes.
        let without: Vec<String> = paths.iter().filter(|f| **f != hung[0]).cloned().collect();
        p.frame(&ctx, &without, PX, now, false);
        assert_eq!(p.state(&hung[0]), PreviewState::Pending, "unpinned: nothing known");
        p.frame(&ctx, &paths, PX, now + 10.0 * RECHECK_SECS, false);
        assert_eq!(p.state(&hung[0]), PreviewState::Unavailable);
        assert_eq!(calls.lock().unwrap().iter().filter(|f| **f == hung[0]).count(), 1);
        gate.open();
        let mut later = now + 10.0 * RECHECK_SECS;
        frames_until(&mut p, &ctx, &paths, &mut later, |p| p.reads() == 0);
        assert_eq!(p.state(&hung[0]), PreviewState::None, "its answer, not a second read");
        assert_eq!(calls.lock().unwrap().iter().filter(|f| **f == hung[0]).count(), 1);
        let _ = std::fs::remove_file(&ok);
    }

    #[test]
    fn work_stops_while_home_is_hidden() {
        let path = temp("hidden.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let wait = gate.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                wait.wait();
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        p.frame(&ctx, &paths, PX, 0.0, false);
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !p.reads.iter().all(|r| Arc::strong_count(&r.slot) == 1) {
            assert!(std::time::Instant::now() < deadline, "the read finishes");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Home went away with the file read but not drawn: its bytes are let go of.
        p.park();
        assert!(p.reads.iter().all(|r| r.slot.lock().unwrap().is_none()));
        // Back on Home, it is read and drawn again straight away.
        let mut now = 1.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_render_under_way_stops_while_home_is_hidden() {
        let path = temp("hidden-render.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let wait = gate.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                wait.wait();
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        p.frame(&ctx, &paths, PX, 0.0, false);
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !p.reads.iter().all(|r| Arc::strong_count(&r.slot) == 1) {
            assert!(std::time::Instant::now() < deadline, "the read finishes");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Its render starts, and Home goes away before its answer is taken.
        p.take_reads(PX, 1.0, false);
        assert_eq!(p.jobs.len(), 1);
        p.park();
        assert!(p.jobs.is_empty(), "the render is let go of");
        frames_until(&mut p, &ctx, &[], &mut 2.0, |p| p.threads() == 0);
        // Back on Home, it is read and drawn again.
        let mut now = 3.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn work_is_let_go_of_only_in_passes_without_home() {
        let path = temp("passes.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let wait = gate.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                wait.wait();
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let pass = |p: &mut Previews, home: bool| {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                if home {
                    p.frame(ui.ctx(), &paths, PX, 0.0, false);
                }
                p.park_unless_drawn(ui.ctx());
            })
            .drop_without_applying_deltas();
        };
        let read_done = |p: &Previews| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !p.reads.iter().all(|r| Arc::strong_count(&r.slot) == 1) {
                assert!(std::time::Instant::now() < deadline, "the read finishes");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        pass(&mut p, true);
        gate.open();
        read_done(&p);
        // Home shows: its render starts, and stays (or is already drawn).
        pass(&mut p, true);
        assert!(p.jobs.len() == 1 || matches!(p.state(&file), PreviewState::Ready { .. }), "not let go of while Home shows");
        // A pass without Home: a render under way is let go of.
        pass(&mut p, false);
        assert!(p.jobs.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_whose_drive_doesnt_answer_isnt_opened() {
        let path = temp("answer.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                let first = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n == 1
                };
                if !first {
                    wait.wait();
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        assert!(!p.unresponsive(&file, now), "drawn and nothing under way: opens");
        // A check that gets no answer: briefly, it may still answer; soon, it isn't opened, though
        // its card keeps the preview.
        p.recheck();
        p.frame(&ctx, &paths, PX, now, false);
        assert!(!p.unresponsive(&file, now + 1.0));
        assert!(p.unresponsive(&file, now + STALL_HINT + 1.0));
        p.frame(&ctx, &paths, PX, now + LOAD_TIMEOUT + 1.0, false);
        assert!(matches!(p.state(&file), PreviewState::Ready { .. }));
        assert!(p.unresponsive(&file, now + LOAD_TIMEOUT + 1.0));
        gate.open();
        let mut later = now + LOAD_TIMEOUT + 1.0;
        frames_until(&mut p, &ctx, &paths, &mut later, |p| p.reads() == 0);
        assert!(!p.unresponsive(&file, later), "it answered");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_pinned_again_during_a_check_gets_its_preview_back() {
        let path = temp("repin.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                let n = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n
                };
                if n == 2 {
                    wait.wait();
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        // A check starts; the file is unpinned and pinned again meanwhile; the check finds it as
        // it was, which its new card knows nothing of: it is read and drawn again.
        p.recheck();
        p.frame(&ctx, &paths, PX, now, false);
        p.frame(&ctx, &[], PX, now, false);
        p.frame(&ctx, &paths, PX, now, false);
        assert_eq!(p.state(&file), PreviewState::Pending);
        gate.open();
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        assert_eq!(*calls.lock().unwrap(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_read_waiting_for_a_render_thread_isnt_called_slow() {
        let mut p = Previews::default();
        let path = "/nowhere/read.pdf".to_owned();
        p.entries.insert(path.clone(), Entry::new(PreviewState::Pending, None, 0.0));
        // Its thread is done (the slot has no other owner) and its answer waits in the slot.
        let slot = Arc::new(Mutex::new(Some(Found::NoPreview(Stamp { len: 0, modified: None }))));
        p.reads.push(Reading { path: path.clone(), slot, progress: Arc::new(AtomicU64::new(0)), seen: 0, since: 0.0, abandoned: false });
        p.give_up_slow_read(10.0 * LOAD_TIMEOUT);
        assert!(p.reads.iter().all(|r| !r.abandoned));
        assert_eq!(p.state(&path), PreviewState::Pending);
    }

    #[test]
    fn a_small_density_change_doesnt_read_the_files_again() {
        let path = temp("zoom.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let calls = Arc::new(Mutex::new(0usize));
        let count = calls.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                *count.lock().unwrap() += 1;
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        // Interface zoom steps up and down: the preview scales, the file isn't read again.
        for zoom in [1.2, 0.8, 0.5, 1.4] {
            for _ in 0..5 {
                p.frame(&ctx, &paths, PX * zoom, now, false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        assert_eq!(*calls.lock().unwrap(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_slow_read_still_progressing_is_waited_for() {
        let ok = temp("waits.pdf", PDF).to_string_lossy().into_owned();
        let slow = "/nowhere/downloading.pdf".to_owned();
        let paths = vec![slow.clone(), ok.clone()];
        let gate = Arc::new(Gate::default());
        let wait = gate.clone();
        let step = Arc::new(Mutex::new(0u64));
        let ticks = step.clone();
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                if f == "/nowhere/downloading.pdf" {
                    // Bytes arrive whenever the test says so, until it opens the gate.
                    while !*wait.0.lock().unwrap() {
                        progress.store(*ticks.lock().unwrap(), Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    return Found::NoPreview(Stamp { len: 0, modified: None });
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        p.frame(&ctx, &paths, PX, 0.0, false);
        // Far past the timeout in all, but never that long without progress: still waited for,
        // and the next file waits its turn.
        for k in 1..=10u32 {
            *step.lock().unwrap() = u64::from(k);
            std::thread::sleep(std::time::Duration::from_millis(20));
            p.frame(&ctx, &paths, PX, f64::from(k) * LOAD_TIMEOUT * 0.6, false);
            assert!(p.reads.iter().all(|r| !r.abandoned), "step {k}");
            assert_eq!(p.state(&ok), PreviewState::Pending);
        }
        // No more progress: given up on after the timeout, and the next file goes.
        p.frame(&ctx, &paths, PX, 6.0 * LOAD_TIMEOUT + LOAD_TIMEOUT + 1.0, false);
        assert!(p.reads.iter().any(|r| r.path == slow && r.abandoned));
        gate.open();
        let mut now = 8.0 * LOAD_TIMEOUT;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&ok), PreviewState::Ready { .. }) && p.reads() == 0);
        let _ = std::fs::remove_file(&ok);
    }

    #[test]
    fn a_new_screen_density_draws_the_preview_again() {
        let path = temp("density.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let mut p = Previews::default();
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let PreviewState::Ready { width, .. } = p.state(&file) else { panic!("drawn") };
        let sharper = PX * 2.0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            p.frame(&ctx, &paths, sharper, now, false);
            if matches!(p.state(&file), PreviewState::Ready { width: w, .. } if w > width) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "drawn again at the new size");
            std::thread::sleep(std::time::Duration::from_millis(5));
            now += 0.005;
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_density_change_during_a_check_still_draws_again() {
        let path = temp("density-check.pdf", PDF);
        let file = path.to_string_lossy().into_owned();
        let paths = vec![file.clone()];
        let gate = Arc::new(Gate::default());
        let calls = Arc::new(Mutex::new(0usize));
        let (wait, count) = (gate.clone(), calls.clone());
        let mut p = Previews {
            loader: Some(Arc::new(move |f: &str, known, progress: &AtomicU64| {
                let n = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n
                };
                if n == 2 {
                    wait.wait();
                }
                load(f, known, MAX_FILE_BYTES, progress)
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut now = 0.0;
        frames_until(&mut p, &ctx, &paths, &mut now, |p| matches!(p.state(&file), PreviewState::Ready { .. }));
        let PreviewState::Ready { width, .. } = p.state(&file) else { panic!("drawn") };
        // A check starts (it will find the file unchanged), and the density changes meanwhile.
        p.recheck();
        p.frame(&ctx, &paths, PX, now, false);
        let sharper = PX * 2.0;
        p.frame(&ctx, &paths, sharper, now, false);
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            p.frame(&ctx, &paths, sharper, now, false);
            if matches!(p.state(&file), PreviewState::Ready { width: w, .. } if w > width) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "drawn again at the new size");
            std::thread::sleep(std::time::Duration::from_millis(5));
            now += 0.005;
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_folder_is_no_file() {
        assert!(matches!(load(&std::env::temp_dir().to_string_lossy(), None, MAX_FILE_BYTES, &AtomicU64::new(0)), Found::Unavailable));
    }
}
