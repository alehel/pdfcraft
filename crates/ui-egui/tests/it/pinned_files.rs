//! Files pinned to Home: the Pinned files box right under the community card, pinning from
//! Recent, File ▸ Pin file to Home… and a tab's menu, opening and unpinning, and the setting
//! surviving a restart and Clear Recent Files.

use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_ui_egui::pinned_files::MAX_PINNED_FILES;
use pdfcraft_ui_egui::{PdfCraftApp, RecentFile};

/// A one-page PDF.
const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

/// A fresh, empty folder under the temp dir, removed when dropped.
struct TempFolder(PathBuf);

impl TempFolder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pdfcraft-pinned-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// Write the fixture as `name` and return its path.
    fn pdf(&self, name: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, FIXTURE).unwrap();
        path.to_string_lossy().into_owned()
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn harness(setup: impl FnOnce(&mut PdfCraftApp) + 'static) -> Harness<'static, PdfCraftApp> {
    // Tall enough that Recent is on screen without scrolling.
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 1600.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        // Open files on the frame that asks, not on a worker thread.
        app.run_inline = true;
        setup(&mut app);
        app
    });
    // Fonts install on frame 1 and apply on frame 2.
    h.run_steps(4);
    h
}

fn recent(name: &str, path: &str) -> RecentFile {
    RecentFile { name: name.into(), path: path.into(), pages: 1, size: FIXTURE.len() }
}

/// The top of the first piece of text painted as exactly `text` on the last frame.
fn text_top(h: &Harness<'static, PdfCraftApp>, text: &str) -> f32 {
    h.output()
        .shapes
        .iter()
        .find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text() == text => Some(t.visual_bounding_rect().top()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{text:?} is painted"))
}

#[test]
fn the_pinned_box_sits_right_under_the_community_card() {
    let h = harness(|_| {});
    h.get_by_label_contains("Keep the PDFs you use most one click away");
    let community = text_top(&h, "Join the ArtCraft community");
    let pinned = text_top(&h, "Pinned files");
    let tools = text_top(&h, "Recommended tools");
    assert!(community < pinned && pinned < tools, "community card {community}, pinned files {pinned}, recommended tools {tools}");
}

#[test]
fn pinning_a_recent_file_shows_it_in_the_box() {
    let dir = TempFolder::new("from-recent");
    let path = dir.pdf("contract.pdf");
    let row = path.clone();
    let mut h = harness(move |app| app.recent.push(recent("contract.pdf", &row)));
    assert!(h.query_by_label("Pin contract.pdf to Home").is_none(), "the pin shows only over its row");
    assert_eq!(h.query_all_by_label("contract.pdf").count(), 1, "only the Recent row");
    h.get_by_label("contract.pdf").hover();
    h.run_steps(2);
    h.get_by_label("Pin contract.pdf to Home").click();
    h.run_steps(2);
    assert!(h.state().pinned_files.contains(&path));
    assert_eq!(h.query_all_by_label("contract.pdf").count(), 2, "the Recent row and its tile in the box");
    assert!(h.query_by_label_contains("Keep the PDFs you use most").is_none(), "the hint goes once a file is pinned");
    // A pinned file's row shows its pin without hovering, and the pin unpins it.
    h.hover_at(egui::pos2(1.0, 1.0));
    h.run_steps(2);
    h.get_by_label("Unpin contract.pdf from Home").click();
    h.run_steps(2);
    assert!(!h.state().pinned_files.contains(&path));
    assert_eq!(h.state().recent.len(), 1, "unpinning leaves Recent alone");
}

#[test]
fn clicking_a_pinned_file_opens_it() {
    let dir = TempFolder::new("open");
    let path = dir.pdf("handbook.pdf");
    let pin = path.clone();
    let mut h = harness(move |app| assert!(app.pin_file(&pin)));
    h.get_by_label("handbook.pdf").click();
    h.run_steps(3);
    let app = h.state();
    assert_eq!(app.views.len(), 1, "the PDF opens in a tab");
    assert_eq!(app.recent.first().map(|r| r.path.clone()), Some(path));
}

#[test]
fn unpinning_a_tile_keeps_the_file() {
    let dir = TempFolder::new("unpin");
    let path = dir.pdf("keep.pdf");
    let pin = path.clone();
    let mut h = harness(move |app| assert!(app.pin_file(&pin)));
    assert!(h.query_by_label("Unpin keep.pdf").is_none(), "the unpin button shows only over its tile");
    h.get_by_label("keep.pdf").hover();
    h.run_steps(2);
    h.get_by_label("Unpin keep.pdf").click();
    h.run_steps(2);
    assert!(h.state().pinned_files.is_empty());
    assert!(h.state().views.is_empty(), "unpinning doesn't open the file");
    assert!(h.query_by_label("keep.pdf").is_none());
    h.get_by_label_contains("Keep the PDFs you use most one click away");
    assert!(std::path::Path::new(&path).exists(), "unpinning never touches the file");
}

#[test]
fn file_menu_pins_the_chosen_files() {
    let dir = TempFolder::new("menu");
    let picked = vec![dir.pdf("a.pdf"), dir.pdf("b.pdf")];
    let pick = picked.clone();
    let mut h = harness(move |app| app.pick_override = Some(pick));
    h.get_by_label("Pin a file…");
    assert!(h.state_mut().execute("file.pin_file"));
    h.run_steps(3);
    assert_eq!(h.state().pinned_files.paths(), picked.as_slice());
    assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Pinned 2 files to Home"));
    h.get_by_label("a.pdf");
    h.get_by_label("b.pdf");
}

#[test]
fn a_tab_menu_pins_and_unpins_its_file() {
    let dir = TempFolder::new("tab");
    let path = dir.pdf("draft.pdf");
    let open = path.clone();
    let mut h = harness(move |app| {
        app.open_bytes("draft.pdf", Some(open), FIXTURE.to_vec()).unwrap();
        app.open_bytes("unsaved.pdf", None, FIXTURE.to_vec()).unwrap();
    });
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "unsaved.pdf").click_secondary();
    h.run_steps(2);
    assert!(h.get_by_label("Pin to Home").accesskit_node().is_disabled(), "a document without a file has nothing to pin");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "draft.pdf").click_secondary();
    h.run_steps(2);
    h.get_by_label("Pin to Home").click();
    h.run_steps(2);
    assert!(h.state().pinned_files.contains(&path));
    assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Pinned draft.pdf to Home"));
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "draft.pdf").click_secondary();
    h.run_steps(2);
    h.get_by_label("Unpin from Home").click();
    h.run_steps(2);
    assert!(h.state().pinned_files.is_empty());
}

#[test]
fn pinned_files_survive_a_restart_and_clear_recent() {
    let dir = TempFolder::new("restart");
    let path = dir.pdf("report.pdf");
    let mut a = PdfCraftApp::new();
    a.set_option("language", "en").unwrap();
    a.recent.push(recent("report.pdf", &path));
    assert!(a.pin_file(&path));
    assert!(a.execute("file.clear_recent"));
    assert!(a.recent.is_empty());
    assert!(a.pinned_files.contains(&path), "Clear Recent Files keeps pinned files");
    let mut b = PdfCraftApp::new();
    b.set_option("language", "en").unwrap();
    b.restore(&a.persist());
    assert_eq!(b.pinned_files.paths(), std::slice::from_ref(&path));
    // A pinned file that is missing right now stays pinned: its drive may be offline.
    drop(dir);
    let mut c = PdfCraftApp::new();
    c.restore(&a.persist());
    assert_eq!(c.pinned_files.paths(), [path]);
    c.restore("{\"pinned_files\": [5, \"relative/a.pdf\", null]}");
    assert!(c.pinned_files.is_empty(), "only absolute file paths are restored");
    c.restore("{not json");
    assert!(c.pinned_files.is_empty());
}

#[test]
fn pinning_is_limited_and_never_duplicates() {
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    let files: Vec<String> =
        (0..MAX_PINNED_FILES).map(|i| std::path::Path::new(root).join(format!("pinned-{i}.pdf")).to_string_lossy().into_owned()).collect();
    for f in &files {
        assert!(app.pin_file(f));
    }
    assert!(app.pin_file(&files[0]), "pinning again is fine");
    assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("pinned-0.pdf is already pinned to Home"));
    assert_eq!(app.pinned_files.paths().len(), MAX_PINNED_FILES, "and adds nothing");
    assert!(!app.pin_file(&std::path::Path::new(root).join("one-too-many.pdf").to_string_lossy()));
    assert!(app.toast.as_ref().is_some_and(|(m, _)| m.contains("Unpin one first")), "the user is told");
    assert!(!app.pin_file(""), "no file, nothing pinned");
    app.unpin_file(&files[0]);
    assert!(app.pin_file(&std::path::Path::new(root).join("one-too-many.pdf").to_string_lossy()), "room again after an unpin");
}
