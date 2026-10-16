//! Background jobs bound to one document (Recognize Text, Optimize PDF).
//!
//! A [`Job`] runs its work on a worker thread (inline in tests and on the web) and remembers
//! which document it started from and that document's edit generation. Its result is handed
//! back by [`Job::poll`] as [`Polled::Done`] only while the document is still open and unedited;
//! otherwise it comes back as [`Polled::Stale`], and the caller decides what an older state's
//! result is still good for.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use pdfcraft_engine::{DocId, Session};

/// What the worker shares with the UI while it runs: the latest progress and a cancel request.
struct Status<P> {
    progress: P,
    cancel: bool,
}

/// How the worker ended.
enum Slot<T> {
    Running,
    Finished(T),
    /// It panicked or could not be started: no result will come.
    Lost,
}

/// The worker's side of a job: report progress, and learn whether to stop.
pub struct Reporter<P> {
    status: Arc<Mutex<Status<P>>>,
}

impl<P> Reporter<P> {
    /// Record `progress`; returns `false` once the job has been asked to stop.
    pub fn report(&self, progress: P) -> bool {
        // The status is plain data, valid even if another holder panicked.
        let mut s = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        s.progress = progress;
        !s.cancel
    }
}

/// What [`Job::poll`] found.
pub enum Polled<T, P> {
    /// Still running: the latest progress, and whether a cancel was asked for.
    Running(P, bool),
    /// Finished, and the document is open at the generation the job started from.
    Done(T),
    /// Finished, but the document was edited or closed meanwhile: `T` describes an older state.
    Stale(T),
    /// The worker stopped without a result (it panicked or could not be started).
    Lost,
}

/// A worker bound to one document; see the module docs.
pub struct Job<T, P> {
    doc: DocId,
    generation: u64,
    status: Arc<Mutex<Status<P>>>,
    slot: Arc<Mutex<Slot<T>>>,
}

impl<T: Send + 'static, P: Clone + Send + 'static> Job<T, P> {
    /// Start `work` on document `doc` with `progress` as its first report, on a thread named
    /// `name` (inline when `inline`, and always on the web). `None` if `doc` isn't open.
    pub fn start(
        session: &Session,
        doc: DocId,
        name: &str,
        inline: bool,
        progress: P,
        work: impl FnOnce(&Reporter<P>) -> T + Send + 'static,
    ) -> Option<Self> {
        let generation = session.get(doc)?.edit_generation();
        let status = Arc::new(Mutex::new(Status { progress, cancel: false }));
        let slot = Arc::new(Mutex::new(Slot::Running));
        let (reporter, out) = (Reporter { status: status.clone() }, slot.clone());
        let run = move || {
            // A panic is reported as a lost job instead of one that never finishes.
            let ended = catch_unwind(AssertUnwindSafe(|| work(&reporter))).map_or(Slot::Lost, Slot::Finished);
            *out.lock().unwrap_or_else(PoisonError::into_inner) = ended;
        };
        #[cfg(not(target_arch = "wasm32"))]
        if inline {
            run();
        } else if std::thread::Builder::new().name(name.into()).spawn(run).is_err() {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Slot::Lost;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (name, inline);
            run();
        }
        Some(Job { doc, generation, status, slot })
    }

    /// The document the job works on.
    pub fn doc(&self) -> DocId {
        self.doc
    }

    /// Ask the worker to stop (it sees this at its next report).
    pub fn cancel(&self) {
        self.status.lock().unwrap_or_else(PoisonError::into_inner).cancel = true;
    }

    /// The progress so far, or the result once the worker is done (taken: poll again and a
    /// finished job reads as [`Polled::Lost`]). `session` says whether the document is unchanged.
    pub fn poll(&self, session: &Session) -> Polled<T, P> {
        let ended = {
            let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*slot, Slot::Running) {
                drop(slot);
                let s = self.status.lock().unwrap_or_else(PoisonError::into_inner);
                return Polled::Running(s.progress.clone(), s.cancel);
            }
            std::mem::replace(&mut *slot, Slot::Lost)
        };
        match ended {
            Slot::Running => Polled::Lost, // checked above
            Slot::Finished(t) if session.get(self.doc).is_some_and(|d| d.edit_generation() == self.generation) => Polled::Done(t),
            Slot::Finished(t) => Polled::Stale(t),
            Slot::Lost => Polled::Lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use pdfcraft_engine::Edit;

    use super::*;

    fn session() -> (Session, DocId) {
        let mut s = Session::new();
        let bytes = s.create_from_text("t", "hello").unwrap();
        let id = s.open("t.pdf", None, bytes, None).unwrap();
        (s, id)
    }

    #[test]
    fn a_result_for_the_unchanged_document_is_done() {
        let (s, id) = session();
        let job = Job::start(&s, id, "test", true, 0, |r: &Reporter<i32>| r.report(5)).unwrap();
        assert_eq!(job.doc(), id);
        assert!(matches!(job.poll(&s), Polled::Done(true)));
        assert!(matches!(job.poll(&s), Polled::Lost), "a result is handed out once");
    }

    #[test]
    fn a_result_for_an_edited_or_closed_document_is_stale() {
        let (mut s, id) = session();
        let job = Job::start(&s, id, "test", true, (), |_: &Reporter<()>| 7).unwrap();
        s.apply(id, Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
        assert!(matches!(job.poll(&s), Polled::Stale(7)));

        let job = Job::start(&s, id, "test", true, (), |_: &Reporter<()>| 8).unwrap();
        s.close(id);
        assert!(matches!(job.poll(&s), Polled::Stale(8)));
        assert!(Job::start(&s, id, "test", true, (), |_: &Reporter<()>| 9).is_none(), "no job for a closed document");
    }

    #[test]
    fn a_worker_reports_progress_and_sees_a_cancel() {
        let (s, id) = session();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let job = Job::start(&s, id, "pdfcraft-test-job", false, 0, move |r: &Reporter<i32>| {
            r.report(3);
            let _ = wait.recv();
            r.report(4)
        })
        .unwrap();
        // The worker has reported 3 once the progress reads it; a cancel is seen at the next report.
        let start = std::time::Instant::now();
        while !matches!(job.poll(&s), Polled::Running(3, false)) {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "the worker reports");
            std::thread::yield_now();
        }
        job.cancel();
        assert!(matches!(job.poll(&s), Polled::Running(3, true)));
        go.send(()).unwrap();
        loop {
            match job.poll(&s) {
                Polled::Running(..) => std::thread::yield_now(),
                Polled::Done(keep_going) => break assert!(!keep_going, "the worker was told to stop"),
                _ => panic!("the job finishes on the same document"),
            }
        }
    }

    #[test]
    fn a_panicking_worker_is_lost_not_stuck() {
        let (s, id) = session();
        let job = Job::start(&s, id, "test", true, (), |_: &Reporter<()>| -> i32 { panic!("boom") }).unwrap();
        assert!(matches!(job.poll(&s), Polled::Lost));
    }
}
