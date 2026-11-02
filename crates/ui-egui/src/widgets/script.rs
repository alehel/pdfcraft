//! The JavaScript editor: a code box that colours the script as it is typed and, once typing
//! pauses, checks its syntax, underlines the error and says what and where it is under the box.
//! Nothing here runs the script ([`pdfcraft_engine::js::check`] only parses it).

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, Stroke};
use pdfcraft_engine::js::{Diagnostic, Token, TokenKind};

use crate::theme::{ThemeKind, Tokens};

/// How long typing must pause before the script is checked, in seconds.
const CHECK_AFTER: f64 = 0.4;

/// Longest script coloured; a longer one (past the engine's own limit, which the check reports)
/// is shown plain, so a huge paste costs no more than plain text to lay out.
const MAX_COLOURED_BYTES: usize = 256 * 1024;

/// The editor's check: the text it last saw (by hash), when that text changed, and the
/// diagnostics for the text last checked.
#[derive(Clone, Default)]
struct Check {
    seen: u64,
    edited_at: f64,
    checked: Option<(u64, Arc<Vec<Diagnostic>>)>,
}

/// The tokens of the text last coloured, so an unchanged script is not tokenized every frame.
#[derive(Clone, Default)]
struct Colours {
    hash: u64,
    tokens: Arc<Vec<Token>>,
}

/// A JavaScript box `rows` lines high and `width` wide (`f32::INFINITY` for all the room there
/// is), with `hint` shown while it is empty. `id_salt` names the text box as
/// [`egui::TextEdit::id_salt`] does. Returns the text box's response.
///
/// The check runs on the UI thread, once per pause in typing: a form script of a few KiB takes
/// about a millisecond, one at the engine's 256 KiB limit some 60 ms (measured in a test build).
pub(crate) fn script_editor(ui: &mut egui::Ui, text: &mut String, id_salt: &str, rows: usize, width: f32, hint: Option<&str>) -> egui::Response {
    // Named in the caller's `ui`, as `id_salt` alone would; the status line goes under the box,
    // also in a grid cell or a horizontal row.
    let id = ui.make_persistent_id(id_salt);
    ui.vertical(|ui| editor_and_status(ui, text, id, rows, width, hint)).inner
}

fn editor_and_status(ui: &mut egui::Ui, text: &mut String, id: egui::Id, rows: usize, width: f32, hint: Option<&str>) -> egui::Response {
    let (check_id, colours_id) = (id.with("script-check"), id.with("script-colours"));
    let now = ui.input(|i| i.time);
    // A script shown for the first time is checked at once (a saved script with an error says so on opening).
    let mut check: Check =
        ui.data(|d| d.get_temp(check_id)).unwrap_or_else(|| Check { seen: hash(text), edited_at: now - CHECK_AFTER, checked: None });
    let mut colours: Colours = ui.data(|d| d.get_temp(colours_id)).unwrap_or_else(|| Colours { hash: hash(""), tokens: Arc::default() });
    let palette = Palette::for_kind(Tokens::get(ui.ctx()).kind, ui.visuals().error_fg_color);
    let checked = check.checked.clone();
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let source = buffer.as_str();
        let h = hash(source);
        if colours.hash != h {
            let tokens = if source.len() <= MAX_COLOURED_BYTES { pdfcraft_engine::js::highlight(source) } else { Vec::new() };
            colours = Colours { hash: h, tokens: Arc::new(tokens) };
        }
        // Only a check of this very text is underlined: after an edit its place may have moved.
        let marked = checked
            .as_ref()
            .filter(|(of, _)| *of == h)
            .and_then(|(_, found)| found.iter().find_map(|d| d.at.as_ref()))
            .map(|at| visible(source, at.bytes.clone()));
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let mut job = layout_job(source, &colours.tokens, marked, &palette, &font);
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut edit = crate::widgets::block(text).code_editor().desired_rows(rows).desired_width(width).id(id).layouter(&mut layouter);
    if let Some(hint) = hint {
        edit = edit.hint_text(hint);
    }
    let response = ui.add(edit).accessible_name(tl!("JavaScript"));

    let h = hash(text);
    if h != check.seen {
        check.seen = h;
        check.edited_at = now;
    }
    if check.checked.as_ref().is_none_or(|(of, _)| *of != h) {
        let waited = now - check.edited_at;
        if waited >= CHECK_AFTER {
            check.checked = Some((h, Arc::new(pdfcraft_engine::js::check(text))));
        } else {
            // `try_` and a fallback: a clock that is not finite must not panic here.
            let wait = std::time::Duration::try_from_secs_f64((CHECK_AFTER - waited).clamp(0.0, CHECK_AFTER)).unwrap_or_default();
            ui.ctx().request_repaint_after(wait);
        }
    }
    status(ui, text, &check, h, response.id, &palette);
    ui.data_mut(|d| {
        d.insert_temp(check_id, check);
        d.insert_temp(colours_id, colours);
    });
    response
}

/// The line under the box: the first problem found, or that there are none. While a check is
/// pending the previous result stays, muted, so the line does not flicker on every keystroke.
fn status(ui: &mut egui::Ui, text: &str, check: &Check, hash: u64, editor: egui::Id, palette: &Palette) {
    // The line keeps its height when it has nothing to say, so the buttons below never move
    // under the pointer when the first character is typed or the box is cleared.
    let checked = check.checked.as_ref().filter(|_| !text.trim().is_empty());
    let Some((of, found)) = checked else {
        let height = ui.text_style_height(&egui::TextStyle::Small);
        ui.allocate_space(egui::vec2(0.0, height));
        return;
    };
    let fresh = *of == hash;
    let muted = Tokens::get(ui.ctx()).text_muted;
    let Some(problem) = found.first() else {
        ui.label(egui::RichText::new(tl!("No syntax errors")).small().color(muted));
        return;
    };
    let said = describe(problem);
    let colour = if fresh { palette.error } else { muted };
    let label = egui::RichText::new(said).small().color(colour);
    match problem.at.as_ref().filter(|_| fresh) {
        // Clicking the message puts the caret on the error.
        Some(at) => {
            let clicked = ui.add(egui::Label::new(label).sense(egui::Sense::click())).on_hover_text(tl!("Go to the error")).clicked();
            if clicked {
                let index = text.get(..at.bytes.start).map_or(0, |before| before.chars().count());
                if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), editor) {
                    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(index))));
                    state.store(ui.ctx(), editor);
                }
                ui.ctx().memory_mut(|m| m.request_focus(editor));
            }
        }
        None => {
            ui.label(label);
        }
    }
}

/// What the status line says about `problem`.
fn describe(problem: &Diagnostic) -> String {
    match &problem.at {
        Some(at) => crate::i18n::fmt(
            tl!("Line {line}, column {column}: {message}"),
            &[("line", &at.line.to_string()), ("column", &at.column.to_string()), ("message", &problem.message)],
        ),
        None => crate::i18n::fmt(tl!("Syntax error: {message}"), &[("message", &problem.message)]),
    }
}

/// `bytes` of `source`, moved back onto the last visible character when it covers only a line
/// end or white space (an underline there would not show).
fn visible(source: &str, bytes: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let shown = source.get(bytes.clone()).is_some_and(|s| !s.trim().is_empty());
    if shown {
        return bytes;
    }
    let before = source.get(..bytes.start).unwrap_or_default().trim_end();
    match before.char_indices().next_back() {
        Some((i, c)) => i..i.saturating_add(c.len_utf8()),
        None => bytes,
    }
}

fn hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The colours of the script's tokens and of its error.
#[derive(Clone, Copy)]
struct Palette {
    text: Color32,
    keyword: Color32,
    literal: Color32,
    string: Color32,
    regex: Color32,
    comment: Color32,
    api: Color32,
    error: Color32,
}

impl Palette {
    /// Each colour keeps at least 4.5:1 contrast on the theme's field background.
    fn for_kind(kind: ThemeKind, error: Color32) -> Self {
        let t = Tokens::for_kind(kind);
        match kind {
            ThemeKind::Light => Self {
                text: t.text,
                keyword: Color32::from_rgb(0x7A, 0x3E, 0x9D),
                literal: Color32::from_rgb(0x0B, 0x6E, 0x99),
                string: Color32::from_rgb(0x2E, 0x7D, 0x32),
                regex: Color32::from_rgb(0xA8, 0x51, 0x00),
                comment: Color32::from_rgb(0x6B, 0x70, 0x7A),
                api: Color32::from_rgb(0x1F, 0x5F, 0xBF),
                error,
            },
            ThemeKind::Dark => Self {
                text: t.text,
                keyword: Color32::from_rgb(0xC7, 0x92, 0xEA),
                literal: Color32::from_rgb(0x7F, 0xD1, 0xF0),
                string: Color32::from_rgb(0x9C, 0xCC, 0x65),
                regex: Color32::from_rgb(0xF0, 0xA3, 0x5C),
                comment: Color32::from_rgb(0x9A, 0xA1, 0xAD),
                api: Color32::from_rgb(0x82, 0xAA, 0xFF),
                error,
            },
        }
    }

    fn of(&self, kind: TokenKind) -> Color32 {
        match kind {
            TokenKind::Keyword => self.keyword,
            TokenKind::Literal | TokenKind::Number => self.literal,
            TokenKind::String | TokenKind::Template => self.string,
            TokenKind::Regex => self.regex,
            TokenKind::Comment => self.comment,
            TokenKind::Api => self.api,
        }
    }
}

/// `source` laid out in `font`, its `tokens` coloured and `marked` (the error) underlined.
fn layout_job(source: &str, tokens: &[Token], marked: Option<std::ops::Range<usize>>, palette: &Palette, font: &egui::FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    // Cut points: where a token or the mark starts or ends. Every token and the mark sit on
    // character boundaries, so every cut does.
    let mut cuts: Vec<usize> = Vec::with_capacity(tokens.len().saturating_mul(2).saturating_add(4));
    cuts.extend([0, source.len()]);
    for t in tokens {
        cuts.extend([t.bytes.start, t.bytes.end]);
    }
    if let Some(m) = &marked {
        cuts.extend([m.start, m.end]);
    }
    cuts.retain(|c| *c <= source.len() && source.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut token = tokens.iter().peekable();
    for pair in cuts.windows(2) {
        let [from, to] = pair else { continue };
        let Some(piece) = source.get(*from..*to) else { continue };
        while token.next_if(|t| t.bytes.end <= *from).is_some() {}
        let kind = token.peek().filter(|t| t.bytes.start <= *from).map(|t| t.kind);
        let mut format = TextFormat { font_id: font.clone(), color: kind.map_or(palette.text, |k| palette.of(k)), ..Default::default() };
        format.italics = kind == Some(TokenKind::Comment);
        if marked.as_ref().is_some_and(|m| m.start <= *from && *to <= m.end) {
            format.underline = Stroke::new(1.5, palette.error);
            format.background = palette.error.gamma_multiply(0.18);
        }
        job.append(piece, 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests;
