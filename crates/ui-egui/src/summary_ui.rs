//! Summarize Comments: the options dialog and its live preview.
//!
//! Two sections: **Include** (which comment types, optionally only what the Comments panel's filter
//! shows, and whether to quote marked text) and **Appearance** (order, theme and each author's
//! colour). A stylesheet the user loads is one more entry in the theme menu, and the preview beside
//! the options shows the summary a page at a time, made again whenever an option changes.

use std::collections::{BTreeMap, HashSet};

use egui::{Align, Layout};
use pdfcraft_engine::{DocId, SummaryOptions, SummarySort};
use pdfcraft_render::Annotation;

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons, widgets};

/// The theme menu's entry for the loaded stylesheet (`SummaryDraft::theme`).
pub const CUSTOM: &str = "custom";
/// The date the Date format menu shows its choices with.
const SAMPLE_DATE: &str = "2026-10-21 15:28";

/// The most authors' colours kept between sessions.
pub const MAX_SAVED_AUTHOR_COLORS: usize = 500;

/// `#rrggbb` as sRGB bytes.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#').filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))?;
    // Six ASCII hex digits: every slice below is on a character boundary.
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// The dialog's choices, kept for the session; the theme and stylesheet are also remembered.
#[derive(Default)]
pub struct SummaryDraft {
    pub sort: SummarySort,
    /// Quote the text highlights, underlines and strikethroughs mark.
    pub marked_text: bool,
    /// Comment types (as the Comments panel names them) left out.
    pub hidden_types: Vec<String>,
    /// List only what the Comments panel's filter and search show.
    pub panel_filter: bool,
    /// A built-in theme id, or [`CUSTOM`] for `stylesheet`.
    pub theme: String,
    /// A stylesheet the user loaded: (file name, CSS).
    pub stylesheet: Option<(String, String)>,
    /// What the loaded stylesheet says that can't be used.
    pub warnings: Vec<String>,
    /// Colours chosen for authors, by name as the summary prints it; the others are automatic.
    pub author_colors: BTreeMap<String, [u8; 3]>,
    /// Write dates as ISO 8601 does rather than the way the interface language does.
    pub iso_dates: bool,
    /// The summary page the preview shows (0-based).
    pub preview_page: usize,
    preview: Option<Preview>,
    /// Whether the chosen look draws authors' colours, for the look it was worked out for.
    shows_author_colors: Option<(u64, bool)>,
}

impl SummaryDraft {
    pub fn new() -> SummaryDraft {
        SummaryDraft { marked_text: true, theme: "modern".into(), ..Default::default() }
    }

    /// The engine's options for these choices; `only` is the comments to list.
    pub fn options(&self, only: Option<HashSet<(usize, usize)>>) -> SummaryOptions {
        let custom = self.theme == CUSTOM;
        SummaryOptions {
            sort: self.sort,
            // A stylesheet starts from the theme it imports, Modern when it imports none.
            theme: if custom { "modern".into() } else { self.theme.clone() },
            stylesheet: if custom { self.stylesheet.as_ref().map(|(_, css)| css.clone()) } else { None },
            only,
            marked_text: self.marked_text,
            author_colors: self.author_colors.clone(),
            dates: (!self.iso_dates).then(|| crate::i18n::current().code().to_string()),
        }
    }

    /// Whether the chosen theme or stylesheet draws anything in authors' colours. Worked out again
    /// only when the look changes.
    fn shows_author_colors(&mut self) -> bool {
        use std::hash::{Hash, Hasher};
        let opts = self.options(None);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&opts.theme, &opts.stylesheet).hash(&mut h);
        let key = h.finish();
        if let Some((k, shows)) = self.shows_author_colors
            && k == key
        {
            return shows;
        }
        let shows = pdfcraft_engine::summary_style(&opts).map_or(true, |(style, _)| style.uses_author_color());
        self.shows_author_colors = Some((key, shows));
        shows
    }
}

/// A thread's first comment that a summary can list (not a reply or a status change).
fn listable(a: &Annotation) -> bool {
    a.in_reply_to.is_none() && a.state.is_none()
}

/// What the summary of document `id` lists with these choices: the comments (`None`: all of
/// them), how many that is, and how many there are.
pub fn selection(app: &PdfCraftApp, id: DocId) -> (Option<HashSet<(usize, usize)>>, usize, usize) {
    let Some(doc) = app.session.get(id) else { return (None, 0, 0) };
    let info = &doc.info;
    let d = &app.summary;
    let total = info.annotations.iter().filter(|a| listable(a)).count();
    let panel: Option<HashSet<(usize, usize)>> = d.panel_filter.then(|| {
        app.views
            .iter()
            .find(|v| v.id == id)
            .map(|v| crate::comments_panel::shown(info, &v.comments).into_iter().map(|a| (a.page, a.index)).collect())
            .unwrap_or_default()
    });
    if d.hidden_types.is_empty() && panel.is_none() {
        return (None, total, total);
    }
    let only: HashSet<(usize, usize)> = info
        .annotations
        .iter()
        .filter(|a| listable(a))
        .filter(|a| !d.hidden_types.iter().any(|t| t == crate::comments_panel::kind_label(a)))
        .filter(|a| panel.as_ref().is_none_or(|p| p.contains(&(a.page, a.index))))
        .map(|a| (a.page, a.index))
        .collect();
    let n = only.len();
    (Some(only), n, total)
}

/// Whether the Comments panel's filter or search hides anything in the view of `id`.
fn panel_filter_active(app: &PdfCraftApp, id: DocId) -> bool {
    app.views.iter().find(|v| v.id == id).is_some_and(|v| {
        let cv = &v.comments;
        !(cv.hidden_types.is_empty()
            && cv.hidden_authors.is_empty()
            && cv.hidden_statuses.is_empty()
            && cv.hidden_colors.is_empty()
            && cv.hidden_checks.is_empty())
            || cv.search.as_deref().is_some_and(|q| !q.trim().is_empty())
    })
}

/// Page `SummaryDraft::preview_page` of the summary (moved to its last page when the summary is
/// shorter) as a texture, with that page and the number of pages. The summary is made again only
/// when the choices or the document change, and the page drawn again only when it changes.
fn preview(app: &mut PdfCraftApp, ctx: &egui::Context, id: DocId) -> Option<(egui::TextureHandle, usize, usize)> {
    let (only, _, _) = selection(app, id);
    let opts = app.summary.options(only);
    let doc = app.session.get(id)?;
    // The document's bytes and comments identify its state; the options identify the choices.
    let key = format!("{id:?}/{:p}/{}/{}/{opts:?}", doc.bytes.as_ptr(), doc.bytes.len(), doc.info.annotations.len());
    let wanted = app.summary.preview_page;
    if let Some(p) = &app.summary.preview
        && p.key == key
        && p.page == wanted.min(p.pages.saturating_sub(1))
    {
        app.summary.preview_page = p.page;
        return Some((p.texture.clone(), p.page, p.pages));
    }
    let made = app.summary.preview.as_ref().filter(|p| p.key == key).map(|p| p.pdf.clone());
    let pdf = match made {
        Some(pdf) => pdf,
        None => app.session.summarize_comments(id, &opts).ok()?.0,
    };
    let mut r = pdfcraft_render::PageRenderer::new(pdf.clone(), pdfcraft_render::RenderConfig::default());
    let pages = r.page_count();
    let page = wanted.min(pages.saturating_sub(1));
    app.summary.preview_page = page;
    let ppp = ctx.pixels_per_point().max(1.0);
    // 260 points wide on screen; a Letter page is 612 points wide.
    let scale = (260.0 * ppp / 612.0).clamp(0.1, 4.0);
    let out = r.render(pdfcraft_render::RenderRequest { page, kind: pdfcraft_render::RequestKind::Pixels, scale, ..Default::default() });
    if out.error.is_some() || out.width == 0 || out.height == 0 {
        return None;
    }
    let img = egui::ColorImage::from_rgba_premultiplied([out.width as usize, out.height as usize], &out.rgba);
    let texture = ctx.load_texture("summary-preview", img, egui::TextureOptions::LINEAR);
    app.summary.preview = Some(Preview { key, pdf, pages, page, texture: texture.clone() });
    Some((texture, page, pages))
}

/// The preview: the summary made for one set of choices, and the page of it on screen.
struct Preview {
    /// The choices and the document state it was made for.
    key: String,
    pdf: std::sync::Arc<Vec<u8>>,
    pages: usize,
    page: usize,
    texture: egui::TextureHandle,
}

/// What the dialog asks for when it closes.
#[derive(Default)]
pub struct Outcome {
    pub create: bool,
    pub close: bool,
    pub load_css: bool,
    pub export_css: bool,
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).font(theme::semibold(14.0)));
    ui.add_space(2.0);
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Outcome {
    let mut out = Outcome::default();
    let ctx = ui.ctx().clone();
    ui.label(egui::RichText::new(tl!("Summarize Options")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let Some((_, id)) = app.active_ids() else {
        out.close = true;
        return out;
    };
    let (_, listed, total) = selection(app, id);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(430.0);
            include_section(ui, app, id, t);
            ui.add_space(10.0);
            appearance_section(ui, app, t, &mut out);
            author_colors(ui, app, id, t);
        });
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(tl!("Preview")).small().color(t.text_muted));
            match preview(app, &ctx, id) {
                Some((tex, page, pages)) => {
                    let size = tex.size_vec2() / ctx.pixels_per_point().max(1.0);
                    let r = ui.add(egui::Image::new(&tex).fit_to_exact_size(size));
                    ui.painter().rect_stroke(r.rect, 0.0, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
                    if pages > 1 {
                        ui.horizontal(|ui| {
                            if ui.add_enabled_ui(page > 0, |ui| icons::button(ui, "chevron-left", 24.0, false, tl!("Previous page"))).inner.clicked()
                            {
                                app.summary.preview_page = page.saturating_sub(1);
                            }
                            let of =
                                crate::i18n::fmt(tl!("Page {p} of {n}"), &[("p", &page.saturating_add(1).to_string()), ("n", &pages.to_string())]);
                            ui.label(egui::RichText::new(of).small().color(t.text_muted));
                            if ui
                                .add_enabled_ui(page + 1 < pages, |ui| icons::button(ui, "chevron-right", 24.0, false, tl!("Next page")))
                                .inner
                                .clicked()
                            {
                                app.summary.preview_page = page.saturating_add(1);
                            }
                        });
                    }
                }
                None => {
                    ui.label(egui::RichText::new("—").color(t.text_faint));
                }
            }
        });
    });
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(
                tl!("{listed} of {total} comments"),
                &[("listed", &listed.to_string()), ("total", &total.to_string())],
            ))
            .color(t.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_enabled_ui(listed > 0, |ui| {
                if widgets::pill_button(ui, tl!("Create Summary"), true).clicked() {
                    out.create = true;
                    out.close = true;
                }
            });
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                out.close = true;
            }
        });
    });
    out
}

fn include_section(ui: &mut egui::Ui, app: &mut PdfCraftApp, id: DocId, t: &Tokens) {
    section(ui, tl!("Include"));
    let Some(doc) = app.session.get(id) else { return };
    let mut counts: Vec<(String, usize)> = Vec::new();
    for a in doc.info.annotations.iter().filter(|a| listable(a)) {
        let kind = crate::comments_panel::kind_label(a);
        match counts.iter_mut().find(|(k, _)| k == kind) {
            Some((_, n)) => *n += 1,
            None => counts.push((kind.to_string(), 1)),
        }
    }
    counts.sort();
    let panel_active = panel_filter_active(app, id);
    let d = &mut app.summary;
    if counts.is_empty() {
        ui.label(egui::RichText::new(tl!("No comments yet.")).color(t.text_muted));
    } else {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Comment types")).small().color(t.text_muted));
            if ui.small_button(tl!("All")).clicked() {
                d.hidden_types.clear();
            }
            if ui.small_button(tl!("None")).clicked() {
                d.hidden_types = counts.iter().map(|(k, _)| k.clone()).collect();
            }
        });
        egui::Grid::new("summary-types").num_columns(2).spacing([18.0, 2.0]).show(ui, |ui| {
            for (i, (kind, n)) in counts.iter().enumerate() {
                let mut on = !d.hidden_types.contains(kind);
                if ui.add(widgets::checkbox(&mut on, format!("{} ({n})", tl!(kind)))).changed() {
                    if on {
                        d.hidden_types.retain(|x| x != kind);
                    } else {
                        d.hidden_types.push(kind.clone());
                    }
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    }
    // The panel's filter matters only when it hides something.
    if panel_active || d.panel_filter {
        ui.add(widgets::checkbox(&mut d.panel_filter, tl!("Only comments the Comments panel's filter shows")));
    }
    ui.add(widgets::checkbox(&mut d.marked_text, tl!("Quote the text that highlights, underlines and strikethroughs mark")));
}

/// Each author the summary can name, with their colour: automatic until one is picked, and back
/// to automatic with the button beside it.
fn author_colors(ui: &mut egui::Ui, app: &mut PdfCraftApp, id: DocId, t: &Tokens) {
    let Some(doc) = app.session.get(id) else { return };
    let authors = pdfcraft_engine::summary_author_colors(&doc.info.annotations, &app.summary.author_colors);
    if authors.is_empty() {
        return;
    }
    let d = &mut app.summary;
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Author colours")).small().color(t.text_muted));
    if !d.shows_author_colors() {
        ui.label(egui::RichText::new(tl!("This theme doesn't colour authors' names.")).small().color(t.text_faint));
    }
    egui::ScrollArea::vertical().id_salt("summary-authors").max_height(110.0).auto_shrink([false, true]).show(ui, |ui| {
        egui::Grid::new("summary-author-colors").num_columns(2).spacing([18.0, 4.0]).show(ui, |ui| {
            for (i, (name, color)) in authors.iter().enumerate() {
                ui.horizontal(|ui| {
                    let mut c = *color;
                    if ui.color_edit_button_srgb(&mut c).changed() {
                        d.author_colors.insert(name.clone(), c);
                    }
                    ui.add(egui::Label::new(name.as_str()).truncate());
                    if d.author_colors.contains_key(name) && ui.small_button(tl!("Auto")).clicked() {
                        d.author_colors.remove(name);
                    }
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    });
}

fn appearance_section(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, out: &mut Outcome) {
    section(ui, tl!("Appearance"));
    let d = &mut app.summary;
    egui::Grid::new("summary-appearance").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label(tl!("Sort by"));
        egui::ComboBox::from_id_salt("summary-sort").width(160.0).selected_text(tl!(d.sort.name())).show_ui(ui, |ui| {
            for s in SummarySort::ALL {
                ui.selectable_value(&mut d.sort, s, tl!(s.name()));
            }
        });
        ui.end_row();
        ui.label(tl!("Theme"));
        ui.horizontal(|ui| {
            let current = match (&d.stylesheet, d.theme.as_str()) {
                (Some((name, _)), CUSTOM) => name.clone(),
                (_, theme) => tl!(pdfcraft_engine::SUMMARY_THEMES.iter().find(|t| t.0 == theme).map_or("Modern", |t| t.1)).to_string(),
            };
            egui::ComboBox::from_id_salt("summary-theme").width(160.0).selected_text(current).show_ui(ui, |ui| {
                for (id, name, _) in pdfcraft_engine::SUMMARY_THEMES {
                    ui.selectable_value(&mut d.theme, id.to_string(), tl!(name));
                }
                if let Some((name, _)) = &d.stylesheet {
                    ui.separator();
                    ui.selectable_value(&mut d.theme, CUSTOM.to_string(), name.as_str());
                }
            });
            if ui.button(tl!("Load CSS…")).clicked() {
                out.load_css = true;
            }
            if ui.button(tl!("Export CSS…")).clicked() {
                out.export_css = true;
            }
        });
        ui.end_row();
        ui.label(tl!("Date format"));
        let local = format!("{} \u{2014} {}", pdfcraft_engine::summary_date(SAMPLE_DATE, crate::i18n::current().code()), tl!("Interface language"));
        let iso = format!("{SAMPLE_DATE} \u{2014} ISO 8601");
        let selected = if d.iso_dates { iso.clone() } else { local.clone() };
        egui::ComboBox::from_id_salt("summary-dates").width(260.0).selected_text(selected).show_ui(ui, |ui| {
            ui.selectable_value(&mut d.iso_dates, false, local);
            ui.selectable_value(&mut d.iso_dates, true, iso);
        });
        ui.end_row();
    });
    if d.theme == CUSTOM && !d.warnings.is_empty() {
        let n = d.warnings.len();
        let title = crate::i18n::fmt(tl!("{n} parts of the stylesheet can't be used"), &[("n", &n.to_string())]);
        egui::CollapsingHeader::new(egui::RichText::new(title).small().color(t.text_muted)).id_salt("summary-warnings").show(ui, |ui| {
            for w in &d.warnings {
                ui.label(egui::RichText::new(w).small().weak());
            }
        });
    }
}
