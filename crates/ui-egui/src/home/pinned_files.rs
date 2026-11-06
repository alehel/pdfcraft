//! Files pinned to Home: the PDFs the user keeps coming back to, listed above the pinned folders
//! and laid out like them, one click away whatever else they open and whether or not Recent is
//! cleared.
//!
//! Local only: a pinned file is a path on this computer, kept in the settings and read only when
//! it is opened. Nothing is read to draw the box, so a file on an offline drive or in a cloud
//! folder that is still syncing never blocks the frame; opening it says what went wrong, and the
//! file stays pinned until the user unpins it.

use crate::PdfCraftApp;

/// At most this many files can be pinned.
pub const MAX_PINNED_FILES: usize = 24;
/// Longest file path accepted, from the app or from settings.
const MAX_PATH_CHARS: usize = 4096;
/// Settings entries read when restoring, so a huge hand-edited list can't stall the launch.
const MAX_RESTORED_ENTRIES: usize = 1_000;

/// The files pinned to Home: absolute paths, in the order they were pinned, without duplicates
/// and at most [`MAX_PINNED_FILES`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PinnedFiles {
    files: Vec<String>,
}

/// What pinning one file did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pinned {
    Added,
    Already,
    Full,
    Refused,
}

impl PinnedFiles {
    /// The pinned paths, in the order they were pinned.
    pub fn paths(&self) -> &[String] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether `path` names a pinned file (the same path, or one that names the same file once
    /// made absolute).
    pub fn contains(&self, path: &str) -> bool {
        self.files.iter().any(|f| crate::same_file(f, path))
    }

    /// Pin `path`, which must already be [`clean`].
    fn add(&mut self, path: String) -> Pinned {
        if self.contains(&path) {
            Pinned::Already
        } else if self.files.len() >= MAX_PINNED_FILES {
            Pinned::Full
        } else {
            self.files.push(path);
            Pinned::Added
        }
    }

    /// Unpin `path`. Returns whether it was pinned.
    fn remove(&mut self, path: &str) -> bool {
        let before = self.files.len();
        self.files.retain(|f| !crate::same_file(f, path));
        self.files.len() != before
    }

    /// The pinned paths for the settings file.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!(self.files)
    }

    /// Restore pinned files from settings: absolute paths only, without duplicates, at most
    /// [`MAX_PINNED_FILES`], from the first [`MAX_RESTORED_ENTRIES`] entries. Settings are
    /// untrusted, so anything else is ignored, and a value that isn't a list keeps what is there. A file that is missing right now is kept (its
    /// drive or cloud sync may be offline).
    pub(crate) fn restore(&mut self, value: &serde_json::Value) {
        let Some(items) = value.as_array() else { return };
        let mut restored = PinnedFiles::default();
        for path in items.iter().take(MAX_RESTORED_ENTRIES).filter_map(serde_json::Value::as_str).filter_map(clean) {
            if restored.add(path) == Pinned::Full {
                break;
            }
        }
        *self = restored;
    }
}

/// `path` as a file to pin: absolute, with a file name, and of a sane length; `None` if it can't
/// be one. Not trimmed: a file name may begin or end with a space.
fn clean(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    let fits = !path.is_empty() && path.chars().count() <= MAX_PATH_CHARS && !path.contains('\0');
    (fits && p.is_absolute() && p.file_name().is_some()).then(|| path.to_string())
}

/// The file name in `path`, for rows and messages.
pub fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

impl PdfCraftApp {
    /// File ▸ Pin file to Home… (and "Pin a file…" on Home): choose PDFs to pin.
    pub fn pin_file_dialog(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf"]).set_title(tl!("Pin files to Home").to_string());
            self.ask(crate::pickers::Ask::Files(dialog), None, |app, paths| {
                let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                app.pin_files(&paths);
            });
        }
        #[cfg(target_arch = "wasm32")]
        self.notify_tr("Pinned files need the desktop app");
    }

    /// Pin `path` to Home and say so. A relative path is taken from the current folder. Returns
    /// false, after telling the user, when it can't be pinned. Pinning a file again changes
    /// nothing.
    pub fn pin_file(&mut self, path: &str) -> bool {
        self.pin_files(std::slice::from_ref(&path.to_string()))
    }

    /// Pin each of `paths` to Home, in order, and tell the user what happened in one message.
    /// Returns false when any of them could not be pinned.
    pub fn pin_files(&mut self, paths: &[String]) -> bool {
        let mut added = Vec::new();
        let mut already = Vec::new();
        let (mut full, mut refused) = (false, false);
        for path in paths {
            let absolute = std::path::absolute(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let outcome = match clean(&absolute) {
                Some(path) => self.pinned_files.add(path),
                None => Pinned::Refused,
            };
            match outcome {
                Pinned::Added => added.push(file_name(&absolute)),
                Pinned::Already => already.push(file_name(&absolute)),
                Pinned::Full => full = true,
                Pinned::Refused => refused = true,
            }
        }
        // A file left out matters more than the ones that went in.
        if full {
            self.notify_fmt("You can pin up to {n} files. Unpin one first.", &[("n", &MAX_PINNED_FILES.to_string())]);
        } else if refused {
            self.notify_tr("Choose a file on this computer to pin");
        } else if let [name] = added.as_slice() {
            self.notify_fmt("Pinned {name} to Home", &[("name", name)]);
        } else if !added.is_empty() {
            let n = u64::try_from(added.len()).unwrap_or(u64::MAX);
            self.notify(crate::i18n::trn(crate::i18n::current(), n, "Pinned {n} file to Home", "Pinned {n} files to Home"));
        } else if let [name] = already.as_slice() {
            self.notify_fmt("{name} is already pinned to Home", &[("name", name)]);
        } else if !already.is_empty() {
            self.notify_tr("These files are already pinned to Home");
        }
        !(full || refused)
    }

    /// Remove `path` from Home. The file itself is not touched.
    pub fn unpin_file(&mut self, path: &str) {
        self.pinned_files.remove(path);
    }

    /// Pin `path` to Home, or unpin it if it is pinned, and say which (a tab's menu and the
    /// Recent list share this).
    pub fn toggle_pinned_file(&mut self, path: &str) {
        if self.pinned_files.remove(path) {
            self.notify_fmt("Unpinned {name} from Home", &[("name", &file_name(path))]);
        } else {
            self.pin_file(path);
        }
    }
}

/// A pinned file's row: as tall as a pinned folder's heading row, whose layout it follows.
#[cfg(not(target_arch = "wasm32"))]
const ROW_HEIGHT: f32 = 30.0;

/// The Home view's Pinned files section, right above Folders and laid out like it: a heading
/// with Pin a file… beside it, then one row per file with its name, where it is and Unpin.
/// Desktop only: the web build opens files from the browser, without paths to keep.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn section(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    use egui::{Align2, CornerRadius, Rect, Sense, vec2};

    use crate::theme::{self, Tokens};
    use crate::{icons, widgets};

    let t = Tokens::get(ui.ctx());
    let mut pick = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Pinned files")).font(theme::semibold(17.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            pick = widgets::ghost_button(ui, "pin", tl!("Pin a file…")).clicked();
        });
    });
    ui.add_space(4.0);
    if app.pinned_files.is_empty() {
        ui.label(
            egui::RichText::new(tl!(
                "Pin the PDFs you use most to open them here in one click: hover a recent file and click its pin, or choose a file."
            ))
            .color(t.text_muted),
        );
    }
    let mut open = None;
    let mut unpin = None;
    for path in app.pinned_files.paths() {
        let name = file_name(path);
        ui.add_space(6.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
        // The row, not its response: the pointer stays "in" the row over its Unpin button. The
        // wash reaches a little past the icon and the button, which line up with the headings.
        if ui.rect_contains_pointer(rect) {
            ui.painter().rect_filled(rect.expand2(vec2(6.0, 0.0)), CornerRadius::same(8), t.hover);
        }
        // Unpin, as on a pinned folder; laid out first so the name and location stop short of it.
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
        let button = widgets::ghost_button(&mut right, "x", tl!("Unpin"));
        // Every file has an Unpin button: say which one it is.
        let label = crate::i18n::fmt(tl!("Unpin {name}"), &[("name", &name)]);
        button.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        if button.clicked() {
            unpin = Some(path.clone());
        }
        let end = right.min_rect().left() - 12.0;
        icons::paint(
            ui,
            Rect::from_center_size(rect.left_center() + vec2(11.0, 0.0), vec2(22.0, 22.0)),
            "file-text",
            20.0,
            egui::Color32::from_rgb(0xE0, 0x3E, 0x3E),
        );
        // File names and paths are file/path chrome: explicitly left to right, so an Arabic name
        // keeps its `.pdf` at the visual end. A long name or location is cut short, never Unpin.
        let name_x = rect.left() + 30.0;
        let name_galley = ltr_galley(ui, name.clone(), theme::semibold(14.0), t.text, end - name_x);
        let place_x = name_x + name_galley.size().x + 10.0;
        ui.painter().galley(Align2::LEFT_CENTER.anchor_size(egui::pos2(name_x, rect.center().y), name_galley.size()).min, name_galley, t.text);
        if end - place_x >= 24.0 {
            let place = ltr_galley(ui, super::folders_ui::location(path), theme::regular(12.0), t.text_faint, end - place_x);
            ui.painter().galley(Align2::LEFT_CENTER.anchor_size(egui::pos2(place_x, rect.center().y), place.size()).min, place, t.text_faint);
        }
        if theme::hand(resp).on_hover_text(path).clicked() && unpin.is_none() {
            open = Some(path.clone());
        }
    }
    if pick {
        app.pin_file_dialog();
    }
    if let Some(path) = unpin {
        app.unpin_file(&path);
    }
    if let Some(path) = open {
        app.open_recent(&path);
    }
}

/// `text` on one line, left to right, cut with "…" at `max_width`.
#[cfg(not(target_arch = "wasm32"))]
fn ltr_galley(ui: &egui::Ui, text: String, font: egui::FontId, color: egui::Color32, max_width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.direction = egui::epaint::text::TextDirection::LeftToRight;
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    ui.painter().layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static str {
        if cfg!(windows) { "C:\\" } else { "/" }
    }

    #[test]
    fn files_from_settings_must_be_absolute_unique_and_few() {
        let a = format!("{}scans{}a.pdf", root(), std::path::MAIN_SEPARATOR);
        let mut items = vec![
            serde_json::json!(5),
            serde_json::json!(null),
            serde_json::json!("relative/a.pdf"),
            serde_json::json!(""),
            serde_json::json!(root()),
            serde_json::json!(format!("{}a\0b.pdf", root())),
            serde_json::json!(format!("{}{}.pdf", root(), "x".repeat(MAX_PATH_CHARS))),
            serde_json::json!(a.clone()),
            serde_json::json!(a.clone()),
        ];
        items.extend((0..100).map(|i| serde_json::json!(format!("{}f{i}.pdf", root()))));
        let mut p = PinnedFiles::default();
        p.restore(&serde_json::Value::Array(items));
        assert_eq!(p.paths().len(), MAX_PINNED_FILES);
        assert_eq!(p.paths().first(), Some(&a), "garbage skipped, the duplicate dropped");
        assert_eq!(p.paths().iter().filter(|f| **f == a).count(), 1);
        assert!(p.paths().iter().all(|f| std::path::Path::new(f).is_absolute()));
        p.restore(&serde_json::json!({ "not": "a list" }));
        assert_eq!(p.paths().len(), MAX_PINNED_FILES, "garbage keeps what is there");
        p.restore(&serde_json::json!([]));
        assert!(p.is_empty(), "an empty list empties it");
    }

    #[test]
    fn a_huge_list_from_settings_is_read_only_so_far() {
        let mut items = vec![serde_json::json!(5); MAX_RESTORED_ENTRIES];
        items.push(serde_json::json!(format!("{}late.pdf", root())));
        let mut p = PinnedFiles::default();
        p.restore(&serde_json::Value::Array(items));
        assert!(p.is_empty(), "entries past the first {MAX_RESTORED_ENTRIES} are not read");
    }

    #[test]
    fn a_file_needs_a_name_and_keeps_its_spaces() {
        assert_eq!(clean(root()), None, "the root is no file");
        assert_eq!(clean(""), None);
        let spaced = format!("{} spaced .pdf ", root());
        assert_eq!(clean(&spaced).as_deref(), Some(spaced.as_str()));
        assert_eq!(file_name(&format!("{}docs{}report.pdf", root(), std::path::MAIN_SEPARATOR)), "report.pdf");
    }

    #[test]
    fn pinning_is_limited_never_duplicates_and_unpins() {
        let mut p = PinnedFiles::default();
        let path = |i: usize| format!("{}f{i}.pdf", root());
        for i in 0..MAX_PINNED_FILES {
            assert_eq!(p.add(path(i)), Pinned::Added);
        }
        assert_eq!(p.add(path(0)), Pinned::Already);
        assert_eq!(p.add(path(MAX_PINNED_FILES)), Pinned::Full);
        assert!(p.remove(&path(3)));
        assert!(!p.remove(&path(3)), "already gone");
        assert!(!p.contains(&path(3)));
        assert_eq!(p.add(path(MAX_PINNED_FILES)), Pinned::Added, "room again after an unpin");
    }
}
