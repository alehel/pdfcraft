//! First-page previews of the pinned files, for Home's grid.
//!
//! Nothing is read on the UI thread, so an offline drive or a cloud folder still syncing never
//! stalls the frame. One file at a time is read on a loader thread, and only while Home shows:
//! its size first (a file over [`MAX_FILE_BYTES`] gets no preview), then its first page's size,
//! and the page renders through a one-thread [`RenderPool`] (its panic guard, size caps and
//! watchdog) at the size a card shows it. At most [`MAX_THREADS`] render threads exist at once,
//! counted until they have actually exited. A read that takes longer than [`LOAD_TIMEOUT`]
//! (a hung network drive) stops holding up the other files: it is still waited for, and its file
//! isn't read again meanwhile; while [`MAX_ABANDONED`] such reads are stuck, no new one starts.
//! While Home shows, each file is checked again every [`RECHECK_SECS`]: one changed on disk
//! renders again, one that can't be read shows as unavailable.

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
/// A file read that takes longer than this (seconds) stops holding up the other files.
const LOAD_TIMEOUT: f64 = 20.0;
/// Reads given up on that may still be stuck before no new read starts (threads can't be
/// killed): each is a different file, so this many unreadable files stop the reading.
const MAX_ABANDONED: usize = 4;

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

/// A file being read on a loader thread.
struct Reading {
    path: String,
    slot: Arc<Mutex<Option<Found>>>,
    /// When (egui time) the read started.
    since: f64,
    /// Given up on after [`LOAD_TIMEOUT`]: still polled, so a slow read that finishes is used,
    /// but no longer holding up the other files.
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
    fn answer(&self) -> Answer {
        // Whether the thread has let go first, then the slot: a thread fills the slot before it
        // lets go, so an answer given just before is never mistaken for none.
        let ended = Arc::strong_count(&self.slot) == 1;
        match self.slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
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
}

/// Reads a file for its preview, unless it is still as the stamp says (tests stand in for it).
type Loader = Arc<dyn Fn(&str, Option<Stamp>) -> Found + Send + Sync>;

/// The pinned files' previews and the work under way.
#[derive(Default)]
pub struct Previews {
    entries: HashMap<String, Entry>,
    /// The read under way, and those given up on whose threads haven't finished.
    reads: Vec<Reading>,
    jobs: Vec<Job>,
    retiring: Vec<Retiring>,
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
        self.entries.retain(|path, _| paths.contains(path));
        let (keep, gone): (Vec<Job>, Vec<Job>) = self.jobs.drain(..).partition(|j| paths.contains(&j.path));
        self.jobs = keep;
        self.retiring.extend(gone.into_iter().map(|j| j.pool.retire()));

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
            && stuck < MAX_ABANDONED
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
    /// exit, and when the next file is due to be checked. A read wakes the app when it finishes.
    fn schedule(&self, ctx: &egui::Context, now: f64, drew: bool, waiting: bool) {
        use std::time::Duration;
        let after = |secs: f64| ctx.request_repaint_after(Duration::from_secs_f64(secs.clamp(0.0, RECHECK_SECS)));
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
        if !self.entries.is_empty() {
            let next = self.entries.values().map(|e| e.checked + RECHECK_SECS - now).filter(|s| s.is_finite()).fold(RECHECK_SECS, f64::min);
            after(next.max(1.0));
        }
    }

    fn start_reading(&mut self, ctx: &egui::Context, path: String, now: f64, inline: bool) {
        let known = self.entries.get(&path).and_then(|e| e.stamp);
        let entry = self.entries.entry(path.clone()).or_insert(Entry { stamp: None, tex: None, state: PreviewState::Pending, checked: now });
        // Not again until it is due, whatever happens to this read.
        entry.checked = now;
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let file = path.clone();
        let ctx = ctx.clone();
        let loader = self.loader.clone();
        let work = move || {
            let found = match &loader {
                Some(load) => load(&file, known),
                None => load(&file, known, MAX_FILE_BYTES),
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
        self.reads.push(Reading { path, slot, since: now, abandoned: false });
    }

    /// The read under way for longer than [`LOAD_TIMEOUT`] stops holding up the other files. A
    /// file with no preview yet shows as unavailable; one with a preview keeps it (a slow read of
    /// a file being synced, say). Either way the read's answer is still used if it comes.
    fn give_up_slow_read(&mut self, now: f64) {
        for r in self.reads.iter_mut().filter(|r| !r.abandoned) {
            // `now < since`: the clock restarted (a new context in tests).
            if now >= r.since && now - r.since < LOAD_TIMEOUT {
                continue;
            }
            r.abandoned = true;
            if let Some(entry) = self.entries.get_mut(&r.path).filter(|e| e.tex.is_none()) {
                entry.state = PreviewState::Unavailable;
            }
        }
    }

    /// Take the reads' answers, given-up ones included, and start drawing each file read.
    fn take_reads(&mut self, px: Vec2, now: f64, inline: bool) {
        let mut i = 0;
        while i < self.reads.len() {
            let answer = self.reads.get(i).map_or(Answer::Waiting, Reading::answer);
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
            // As it was: the preview (or why there is none) stands, even after a slow read.
            Found::Unchanged => {
                if entry.state == PreviewState::Unavailable && entry.tex.is_some() {
                    entry.state = entry.tex.as_ref().map_or(PreviewState::None, ready);
                }
            }
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
                // A pool that couldn't start its thread would render here, on the UI thread: not
                // for a preview. Tried again when it is next due.
                if pool.is_inline() && !inline {
                    return;
                }
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
                let state = tex.as_ref().map_or(PreviewState::None, ready);
                *entry = Entry { stamp: Some(job.stamp), tex, state, checked: now };
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
            loader: Some(Arc::new(move |f: &str, known| {
                seen.lock().unwrap().push(std::thread::current().id());
                load(f, known, MAX_FILE_BYTES)
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
            loader: Some(Arc::new(move |f: &str, known| {
                if f.ends_with("stuck.pdf") {
                    *count.lock().unwrap() += 1;
                    wait.wait();
                    return Found::NoPreview(Stamp { len: 0, modified: None });
                }
                load(f, known, MAX_FILE_BYTES)
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
            loader: Some(Arc::new(move |f: &str, known| {
                let first = {
                    let mut n = count.lock().unwrap();
                    *n += 1;
                    *n == 1
                };
                if !first {
                    wait.wait();
                }
                load(f, known, MAX_FILE_BYTES)
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
    fn a_folder_is_no_file() {
        assert!(matches!(load(&std::env::temp_dir().to_string_lossy(), None, MAX_FILE_BYTES), Found::Unavailable));
    }
}
