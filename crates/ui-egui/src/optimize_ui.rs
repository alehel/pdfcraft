//! Optimize PDF ▸ Advanced optimization (Acrobat's PDF Optimizer): Images, Discard Objects,
//! Discard User Data and Clean Up panels. The result is saved as a copy, like Reduce File Size.
//!
//! The optimization (this dialog's, or Reduce File Size's fixed choices) runs on a worker thread
//! and shows its progress in a notice with a bar and a Cancel button; the copy is saved once it
//! is done.

use egui::{Align, Layout};
use pdfcraft_engine::optimize::{Compression, ImageSettings, QUALITIES, Settings};
use pdfcraft_engine::optimizer::{OptimizeStage, Optimized};
use pdfcraft_engine::{EditError, Hidden, OptimizeReport};

use crate::jobs::{Job, Polled};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizeTab {
    Images,
    DiscardObjects,
    DiscardUserData,
    CleanUp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptimizeDraft {
    pub tab: OptimizeTab,
    pub settings: Settings,
    /// Remove Hidden Information categories to discard.
    pub discard: Vec<Hidden>,
    /// "Audit space usage…" was pressed.
    pub audit: bool,
}

impl Default for OptimizeDraft {
    fn default() -> Self {
        Self { tab: OptimizeTab::Images, settings: Settings::default(), discard: Vec::new(), audit: false }
    }
}

/// Which command started an optimization: they save under different names, and only the
/// Optimizer's notice lists what it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptimizeKind {
    /// Reduce File Size (Acrobat's defaults).
    Reduce,
    /// Optimize PDF ▸ Advanced optimization (the dialog's choices).
    Advanced,
}

impl OptimizeKind {
    /// The suffix of the saved copy's name ("notes (reduced).pdf").
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            OptimizeKind::Reduce => "reduced",
            OptimizeKind::Advanced => "optimized",
        }
    }
}

pub struct OptimizeRun {
    pub kind: OptimizeKind,
    /// The size of the document the optimization started from: the saving is measured against
    /// it, not against the document as edited since.
    pub before: usize,
    /// The optimization, with the stage it is at as its progress.
    pub job: Job<Result<Optimized, EditError>, OptimizeStage>,
}

/// The notice's text for a stage.
fn stage_label(stage: OptimizeStage) -> String {
    match stage {
        OptimizeStage::Discarding => tl!("Optimizing… removing user data").to_string(),
        OptimizeStage::Images { done, total } => crate::i18n::fmt(
            tl!("Optimizing… image {d} of {t}"),
            &[("d", &(done + 1).min(total.max(1)).to_string()), ("t", &total.max(1).to_string())],
        ),
        OptimizeStage::CleaningUp => tl!("Optimizing… cleaning up").to_string(),
        OptimizeStage::Merging => tl!("Optimizing… merging identical objects").to_string(),
        OptimizeStage::Writing => tl!("Optimizing… writing the copy").to_string(),
    }
}

/// What the saved notice adds after the size: images optimized and items discarded.
fn summary(r: &OptimizeReport) -> String {
    let o = &r.optimize;
    let lang = crate::i18n::current();
    let mut parts = Vec::new();
    let images = o.images_resampled + o.images_recompressed;
    if images > 0 {
        parts.push(crate::i18n::trn(lang, images as u64, "{n} image optimized", "{n} images optimized"));
    }
    let discarded: usize = r.discarded.iter().map(|(_, n)| n).sum();
    if discarded > 0 {
        parts.push(crate::i18n::trn(lang, discarded as u64, "{n} item discarded", "{n} items discarded"));
    }
    if parts.is_empty() { String::new() } else { format!("; {}", parts.join(", ")) }
}

fn image_row(ui: &mut egui::Ui, id: &str, title: &str, s: &mut ImageSettings) {
    ui.label(egui::RichText::new(tl!(title)).font(theme::semibold(13.0)));
    ui.horizontal(|ui| {
        ui.add(widgets::checkbox(&mut s.downsample, tl!("Bicubic downsampling to")));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.target_ppi).range(9.0..=2400.0).suffix(" ppi"));
        ui.label(tl!("for images above"));
        ui.add_enabled(s.downsample, egui::DragValue::new(&mut s.above_ppi).range(9.0..=2400.0).suffix(" ppi"));
    });
    s.above_ppi = s.above_ppi.max(s.target_ppi);
    ui.horizontal(|ui| {
        ui.label(tl!("Compression"));
        let label = match s.compression {
            Compression::Jpeg(_) => "JPEG",
            Compression::Flate => tl!("ZIP"),
            Compression::Retain => tl!("Retain existing"),
        };
        egui::ComboBox::from_id_salt((id, "compression")).selected_text(label).show_ui(ui, |ui| {
            let q = match s.compression {
                Compression::Jpeg(q) => q,
                _ => 60,
            };
            ui.selectable_value(&mut s.compression, Compression::Jpeg(q), "JPEG");
            ui.selectable_value(&mut s.compression, Compression::Flate, tl!("ZIP"));
            ui.selectable_value(&mut s.compression, Compression::Retain, tl!("Retain existing"));
        });
        if let Compression::Jpeg(q) = &mut s.compression {
            ui.label(tl!("Quality"));
            let name = QUALITIES.iter().min_by_key(|(_, v)| (*v as i32 - *q as i32).abs()).map_or(tl!("Medium"), |(n, _)| tl!(n));
            egui::ComboBox::from_id_salt((id, "quality")).selected_text(name).show_ui(ui, |ui| {
                for (n, v) in QUALITIES {
                    ui.selectable_value(q, v, tl!(n));
                }
            });
        }
    });
    ui.add_space(8.0);
}

fn discard_box(ui: &mut egui::Ui, list: &mut Vec<Hidden>, h: Hidden, label: &str) {
    let mut on = list.contains(&h);
    if ui.add(widgets::checkbox(&mut on, tl!(label))).changed() {
        if on {
            list.push(h);
        } else {
            list.retain(|x| *x != h);
        }
    }
}

/// Draw the dialog; returns (ok, cancel).
pub(crate) fn body(ui: &mut egui::Ui, d: &mut OptimizeDraft, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("PDF Optimizer")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for (tab, label) in [
            (OptimizeTab::Images, tl!("Images")),
            (OptimizeTab::DiscardObjects, tl!("Discard Objects")),
            (OptimizeTab::DiscardUserData, tl!("Discard User Data")),
            (OptimizeTab::CleanUp, tl!("Clean Up")),
        ] {
            if widgets::mode_tab(ui, label, d.tab == tab).clicked() {
                d.tab = tab;
            }
        }
    });
    ui.separator();
    ui.add_space(6.0);
    let s = &mut d.settings;
    match d.tab {
        OptimizeTab::Images => {
            image_row(ui, "color", "Color Images", &mut s.color);
            image_row(ui, "gray", "Grayscale Images", &mut s.gray);
            ui.label(
                egui::RichText::new(tl!("Each image is measured where pages draw it; an image is replaced only if the result is smaller."))
                    .small()
                    .color(t.text_muted),
            );
        }
        OptimizeTab::DiscardObjects => {
            discard_box(ui, &mut d.discard, Hidden::LinksActionsScripts, "Discard all links, actions and JavaScript");
            ui.add(widgets::checkbox(&mut s.discard_alternate_images, tl!("Discard alternate images")));
            ui.add(widgets::checkbox(&mut s.discard_thumbnails, tl!("Discard embedded page thumbnails")));
            ui.add(widgets::checkbox(&mut s.discard_tags, tl!("Discard document tags")));
            ui.add(widgets::checkbox(&mut s.discard_print_settings, tl!("Discard embedded print settings")));
            discard_box(ui, &mut d.discard, Hidden::Bookmarks, "Discard bookmarks");
            discard_box(ui, &mut d.discard, Hidden::FormFields, "Flatten form fields");
            discard_box(ui, &mut d.discard, Hidden::HiddenLayers, "Discard hidden layer content");
        }
        OptimizeTab::DiscardUserData => {
            discard_box(ui, &mut d.discard, Hidden::Comments, "Discard all comments, forms and multimedia");
            discard_box(ui, &mut d.discard, Hidden::Metadata, "Discard document information and metadata");
            discard_box(ui, &mut d.discard, Hidden::Attachments, "Discard all object data (file attachments)");
            discard_box(ui, &mut d.discard, Hidden::PrivateData, "Discard private data of other applications");
            discard_box(ui, &mut d.discard, Hidden::HiddenText, "Discard hidden text");
        }
        OptimizeTab::CleanUp => {
            ui.add(widgets::checkbox(&mut s.flate_unencoded, tl!("Use Flate to encode streams that are not encoded")));
            ui.add(widgets::checkbox(&mut s.remove_invalid_links, tl!("Remove invalid links and bookmarks")));
            ui.add(widgets::checkbox(&mut s.remove_unreferenced_dests, tl!("Remove unreferenced named destinations")));
            ui.add_enabled(false, widgets::checkbox(&mut true, tl!("Compress document structure (object streams)")));
            ui.add_enabled(false, widgets::checkbox(&mut true, tl!("Remove unused objects and merge identical ones")));
        }
    }
    ui.add_space(12.0);
    let (mut ok, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Audit space usage…"), false).clicked() {
            d.audit = true;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("OK"), true).clicked() {
                ok = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        });
    });
    (ok, cancel)
}

/// Audit Space Usage: bytes and share of the file per kind of content.
pub(crate) fn audit_body(ui: &mut egui::Ui, rows: &[pdfcraft_engine::optimize::SpaceUse], t: &Tokens) -> bool {
    ui.label(egui::RichText::new(tl!("Space Audit")).font(crate::theme::semibold(18.0)));
    ui.add_space(8.0);
    egui::Grid::new("space-audit").num_columns(3).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
        for h in [tl!("Description"), tl!("Bytes"), tl!("Percentage")] {
            ui.label(egui::RichText::new(h).color(t.text_muted));
        }
        ui.end_row();
        let total: u64 = rows.iter().map(|r| r.bytes).sum();
        for r in rows.iter().filter(|r| r.bytes > 0) {
            // "Patterns" also names the redaction search patterns ("模式"); the audit's PDF
            // graphics objects need their own key.
            let category = if r.category == pdfcraft_engine::optimize::SpaceCategory::Patterns {
                tl!("Patterns (graphics objects)").to_string()
            } else {
                tl!(r.category.label()).to_string()
            };
            ui.label(category);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(r.bytes.to_string()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(format!("{:.2}%", r.percent)));
            ui.end_row();
        }
        ui.label(egui::RichText::new(tl!("Total")).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(egui::RichText::new(total.to_string()).strong()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(egui::RichText::new("100.00%").strong()));
        ui.end_row();
    });
    ui.add_space(12.0);
    let mut ok = false;
    ui.horizontal(|ui| ui.with_layout(Layout::right_to_left(Align::Center), |ui| ok = widgets::pill_button(ui, tl!("OK"), true).clicked()));
    ok
}

impl PdfCraftApp {
    /// Optimize PDF with the dialog's choices on a worker thread; the copy is saved when done.
    pub fn optimize_with_draft(&mut self) {
        let (settings, discard) = (self.optimize_draft.settings.clone(), self.optimize_draft.discard.clone());
        self.start_optimize(OptimizeKind::Advanced, &settings, &discard);
    }

    /// Run an optimization of the active document on a worker thread (inline in tests and on
    /// the web), showing its progress; the copy is saved when it is done.
    pub(crate) fn start_optimize(&mut self, kind: OptimizeKind, settings: &Settings, discard: &[Hidden]) {
        // What's typed in a form field is part of the document (#166).
        if !self.commit_form_typing() {
            return;
        }
        let Some((_, id)) = self.active_ids() else { return };
        if self.optimize_run.is_some() {
            self.notify_tr("Optimization is already running");
            return;
        }
        let before = self.session.get(id).map_or(0, |d| d.bytes.len());
        let job = match self.session.optimize_job(id, settings, discard) {
            Ok(job) => job,
            Err(e) => return self.save_optimized(id, kind, before, Err(e)),
        };
        let job =
            Job::start(&self.session, id, "pdfcraft-optimize", self.run_inline, OptimizeStage::default(), move |r| job.run(|stage| r.report(stage)));
        self.optimize_run = job.map(|job| OptimizeRun { kind, before, job });
        self.poll_optimize();
    }

    /// Stop a running optimization (nothing is saved).
    pub fn cancel_optimize(&mut self) {
        if let Some(r) = &self.optimize_run {
            r.job.cancel();
        }
    }

    /// Show progress; save the copy once the worker is done.
    pub(crate) fn poll_optimize(&mut self) {
        let Some(run) = self.optimize_run.as_ref() else { return };
        let (doc, kind, before) = (run.job.doc(), run.kind, run.before);
        let result = match run.job.poll(&self.session) {
            Polled::Running(stage, cancelling) => {
                let label = if cancelling { tl!("Cancelling…").to_string() } else { stage_label(stage) };
                self.progress_notice = Some(crate::widgets::ProgressNotice { label, fraction: stage.fraction(), cancellable: !cancelling });
                if let Some(ctx) = &self.ctx {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                return;
            }
            // The copy is of the document as it was when the run started, which is what was asked
            // for; the saving is measured against that state (`before`).
            Polled::Done(result) | Polled::Stale(result) => result,
            Polled::Lost => {
                // The worker stopped without a result: it will never report back.
                self.optimize_run = None;
                self.progress_notice = None;
                self.notify_tr("Couldn't optimize the file");
                return;
            }
        };
        self.optimize_run = None;
        self.progress_notice = None;
        match result {
            Err(EditError::Cancelled) => self.notify_tr("Optimization cancelled"),
            result => {
                let result = result.map(|(b, r)| {
                    let detail = if kind == OptimizeKind::Advanced { summary(&r) } else { String::new() };
                    (b, detail)
                });
                self.save_optimized(doc, kind, before, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn the_progress_card_shows_the_stage_and_cancels() {
        // A worker at image 4 of 10 that goes on when told to, and stops if it was cancelled.
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_cc| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.open_bytes("notes.txt", None, b"hello".to_vec()).unwrap();
            let (_, doc) = app.active_ids().unwrap();
            let stage = OptimizeStage::Images { done: 3, total: 10 };
            let job = Job::start(&app.session, doc, "pdfcraft-test-optimize", false, stage, move |r| {
                let _ = wait.recv();
                if r.report(OptimizeStage::Writing) { Err(EditError::Optimize("not cancelled".into())) } else { Err(EditError::Cancelled) }
            });
            app.optimize_run = job.map(|job| OptimizeRun { kind: OptimizeKind::Advanced, before: 0, job });
            app.notify("Saved notes.pdf");
            app
        });
        // 1.5 s (kittest steps a quarter second): the bar has eased in, the toast still shows.
        h.run_steps(6);
        if let Ok(path) = std::env::var("PDFCRAFT_OPTIMIZE_PROGRESS_SHOT") {
            h.render().unwrap().save(path).unwrap();
        }
        h.get_by_label("Optimizing… image 4 of 10");
        let shown = h.state().progress_notice.clone().unwrap();
        assert!((shown.fraction - 0.275).abs() < 1e-4, "{shown:?}");
        assert!(shown.cancellable);

        h.get_by_label("Cancel").click();
        h.run_steps(2);
        h.get_by_label("Cancelling…");
        assert!(h.query_by_label("Cancel").is_none(), "Cancel is not offered twice");

        // The worker sees the request at its next report, stops and reports back.
        go.send(()).unwrap();
        let start = std::time::Instant::now();
        while h.state().optimize_run.is_some() && start.elapsed() < std::time::Duration::from_secs(10) {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(h.state().optimize_run.is_none() && h.state().progress_notice.is_none());
        assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Optimization cancelled"));
    }

    #[test]
    fn a_second_run_is_refused_while_one_is_going() {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("notes.txt", None, b"hello".to_vec()).unwrap();
        let (_, doc) = app.active_ids().unwrap();
        let job = Job::start(&app.session, doc, "test", true, OptimizeStage::default(), |_| Err(EditError::Cancelled));
        app.optimize_run = job.map(|job| OptimizeRun { kind: OptimizeKind::Reduce, before: 0, job });
        app.optimize_with_draft();
        assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("Optimization is already running"));
    }

    /// The saving is measured against the document the optimization started from. An
    /// edit made meanwhile (which grows the file) must not make the copy look smaller.
    #[test]
    fn the_saving_is_measured_against_the_document_as_it_was_when_the_run_started() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-optimize-before-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("optimized.pdf");
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("notes.txt", None, "lorem ipsum ".repeat(500).into_bytes()).unwrap();
        app.save_override = Some(out.to_string_lossy().into_owned());
        let (_, doc) = app.active_ids().unwrap();
        let start = app.session.get(doc).unwrap().bytes.clone();
        // The copy is exactly as large as the document it was made from.
        let copy = start.clone();
        let job = Job::start(&app.session, doc, "test", true, OptimizeStage::default(), move |_| Ok((copy, OptimizeReport::default())));
        app.optimize_run = job.map(|job| OptimizeRun { kind: OptimizeKind::Advanced, before: start.len(), job });
        // The user edits the document while the optimization runs.
        app.session.apply(doc, pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 }).unwrap();
        assert!(app.session.get(doc).unwrap().bytes.len() > start.len(), "the edit grew the file");

        app.poll_optimize();
        assert!(app.optimize_run.is_none());
        assert_eq!(std::fs::read(&out).unwrap(), *start);
        let size = crate::panels::human_size(start.len());
        let expected = format!("Saved {}: {size} → {size} (not smaller)", out.display());
        assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some(expected.as_str()));
    }
}
