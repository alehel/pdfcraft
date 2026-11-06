//! Files pinned to Home: the PDFs the user keeps coming back to, in a box under the community
//! card, one click away whatever else they open and whether or not Recent is cleared.
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

/// The file name in `path`, for tiles and messages.
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

/// A file's tile is at least this wide where the row has room, and widens to fill the row.
#[cfg(not(target_arch = "wasm32"))]
const TILE_MIN_WIDTH: f32 = 230.0;
/// ...but no wider than this, on a very wide Home.
#[cfg(not(target_arch = "wasm32"))]
const TILE_MAX_WIDTH: f32 = 340.0;
#[cfg(not(target_arch = "wasm32"))]
const TILE_HEIGHT: f32 = 56.0;
#[cfg(not(target_arch = "wasm32"))]
const TILE_GAP: f32 = 10.0;

/// The width of each tile in a row `room` wide: as many as fit at [`TILE_MIN_WIDTH`], widened
/// to share the row. A row narrower than one tile gets one tile as wide as the row.
#[cfg(not(target_arch = "wasm32"))]
fn tile_width(room: f32) -> f32 {
    let room = if room.is_finite() { room.max(1.0) } else { TILE_MIN_WIDTH };
    let columns = ((room + TILE_GAP) / (TILE_MIN_WIDTH + TILE_GAP)).floor().max(1.0);
    ((room - TILE_GAP * (columns - 1.0)) / columns).clamp(1.0, TILE_MAX_WIDTH)
}

/// The Home view's Pinned files box, right under the community card (desktop only: the web build
/// opens files from the browser, without paths to keep).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn section(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    use egui::{CornerRadius, Rect, Sense, Stroke, vec2};

    use crate::theme::{self, Tokens};
    use crate::{icons, widgets};

    let t = Tokens::get(ui.ctx());
    let mut pick = false;
    let mut open = None;
    let mut unpin = None;
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // As tall as the button from the start, so the heading centres on it.
                ui.set_min_height(30.0);
                let (icon, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                icons::paint(ui, icon, "pin", 17.0, t.icon);
                ui.label(egui::RichText::new(tl!("Pinned files")).font(theme::semibold(15.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    pick = widgets::ghost_button(ui, "pin", tl!("Pin a file…")).clicked();
                });
            });
            if app.pinned_files.is_empty() {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(tl!("Keep the PDFs you use most one click away: pin a file here, or hover a recent file and click its pin."))
                        .color(t.text_muted),
                );
                return;
            }
            ui.add_space(8.0);
            let width = tile_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(TILE_GAP, TILE_GAP);
                for path in app.pinned_files.paths() {
                    let name = file_name(path);
                    let (rect, resp) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
                    let press = theme::Press::track(ui, &resp);
                    super::card_lift(ui, &t, rect, press);
                    let fill = press.fill(t.card, t.hover, t.pressed);
                    ui.painter().rect(rect, CornerRadius::same(10), fill, Stroke::new(1.0, t.divider), egui::StrokeKind::Inside);
                    let body = rect.translate(press.offset());
                    icons::paint(
                        ui,
                        Rect::from_min_size(body.min + vec2(12.0, 16.0), vec2(24.0, 24.0)),
                        "file-text",
                        22.0,
                        egui::Color32::from_rgb(0xE0, 0x3E, 0x3E),
                    );
                    // Room on the right for the unpin button; hovering shows the whole path.
                    let text_width = body.width() - 44.0 - 40.0;
                    widgets::row_text(ui, body.min + vec2(44.0, 20.0), name.as_str(), theme::medium(13.0), t.text, text_width);
                    let place = super::folders_ui::location(path);
                    widgets::row_text(ui, body.min + vec2(44.0, 38.0), place, theme::regular(11.5), t.text_faint, text_width);
                    // The tile, not its response: the pointer stays "in" the tile over its button.
                    if ui.rect_contains_pointer(rect) {
                        let x_rect = Rect::from_center_size(rect.right_top() + vec2(-18.0, 18.0), vec2(24.0, 24.0));
                        let x = ui.interact(x_rect, ui.scope_id().with(("unpin-file", path)), Sense::click());
                        let label = crate::i18n::fmt(tl!("Unpin {name}"), &[("name", &name)]);
                        x.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
                        if x.hovered() {
                            ui.painter().rect_filled(x_rect, CornerRadius::same(6), t.pressed);
                        }
                        icons::paint(ui, x_rect.shrink(5.0), "x", 14.0, t.icon);
                        if theme::hand(x).on_hover_text(tl!("Unpin from Home")).clicked() {
                            unpin = Some(path.clone());
                        }
                    }
                    let resp = theme::hand(resp).on_hover_text(path);
                    resp.context_menu(|ui| {
                        if ui.button(tl!("Unpin from Home")).clicked() {
                            unpin = Some(path.clone());
                            ui.close();
                        }
                    });
                    if resp.clicked() && unpin.is_none() {
                        open = Some(path.clone());
                    }
                }
            });
        });
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

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tiles_fill_the_row() {
        for room in [0.0, 100.0, 229.0, 230.0, 469.0, 470.0, 1066.0, 5000.0] {
            let w = tile_width(room);
            let columns = ((room + TILE_GAP) / (w + TILE_GAP)).floor().max(1.0);
            assert!((1.0..=TILE_MAX_WIDTH).contains(&w), "{room}: {w}");
            assert!(columns * w + (columns - 1.0) * TILE_GAP <= room.max(1.0) + 0.01, "{room}: {columns} tiles of {w} fit");
            if room >= TILE_MIN_WIDTH {
                assert!(w >= TILE_MIN_WIDTH, "{room}: {w}");
            }
        }
        assert_eq!(tile_width(f32::NAN), TILE_MIN_WIDTH);
        assert_eq!(tile_width(f32::INFINITY), TILE_MIN_WIDTH);
        // Four tiles share the Home of a 1440-point window with the tools panel open.
        assert!((tile_width(1066.0) - 259.0).abs() < 0.01);
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
