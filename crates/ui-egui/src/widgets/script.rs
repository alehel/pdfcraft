//! The JavaScript editor: a code box that colours the script as it is typed, suggests names
//! (field names in `getField("…")`, an object's names after a `.`), keeps the indentation, numbers
//! its lines, outlines the bracket matching the one at the caret and, once typing pauses, checks
//! the script: errors and likely mistakes are underlined, and listed under the box with their
//! place. Nothing here runs the script ([`pdfcraft_engine::js::assist::lint`] only parses it).

mod edit;
mod suggest;

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, Stroke};
use pdfcraft_engine::js::assist::{self, Knowledge};
use pdfcraft_engine::js::{Diagnostic, Severity, Token, TokenKind};

use crate::theme::{ThemeKind, Tokens};

/// How long typing must pause before the script is checked, in seconds.
const CHECK_AFTER: f64 = 0.4;

/// Longest script coloured; a longer one (past the engine's own limit, which the check reports)
/// is shown plain, so a huge paste costs no more than plain text to lay out.
const MAX_COLOURED_BYTES: usize = 256 * 1024;

/// Most problems listed under the box; the rest are counted.
const LISTED_PROBLEMS: usize = 3;

/// The tallest the box can be dragged, in points.
const MAX_HEIGHT: f32 = 900.0;

/// What a script runs with in its document: the form's field names and the document-level
/// scripts, whose declarations every script can use. `fields` is `None` with no document open,
/// which turns the field name check off.
#[derive(Clone, Debug, Default)]
pub(crate) struct ScriptEnv {
    pub fields: Option<Vec<String>>,
    pub doc_scripts: Vec<String>,
}

impl ScriptEnv {
    fn key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.fields.hash(&mut h);
        self.doc_scripts.hash(&mut h);
        h.finish()
    }
}

/// The editor's check: the text it last saw (by hash), when that text changed, and the
/// diagnostics for the text (and environment) last checked.
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

/// The names the document-level scripts declare, worked out again only when they change.
#[derive(Clone, Default)]
struct Shared {
    key: u64,
    names: Arc<Vec<String>>,
}

/// A JavaScript box `rows` lines high and `width` wide (`f32::INFINITY` for all the room there
/// is), with `hint` shown while it is empty, for a script that runs in `env`. `id_salt` names the
/// text box as [`egui::TextEdit::id_salt`] does. Returns the text box's response.
///
/// The check runs on the UI thread, once per pause in typing: a form script of a few KiB takes
/// about a millisecond, one at the engine's 256 KiB limit some 60 ms (measured in a test build).
pub(crate) fn script_editor(
    ui: &mut egui::Ui,
    text: &mut String,
    id_salt: &str,
    rows: usize,
    width: f32,
    hint: Option<&str>,
    env: &ScriptEnv,
) -> egui::Response {
    // Named in the caller's `ui`, as `id_salt` alone would; the status line goes under the box,
    // also in a grid cell or a horizontal row.
    let id = ui.make_persistent_id(id_salt);
    ui.vertical(|ui| editor_and_status(ui, text, id, rows, width, hint, env)).inner
}

#[allow(clippy::too_many_lines)] // One frame of the editor, in order: keys, box, popup, check, problems.
fn editor_and_status(
    ui: &mut egui::Ui,
    text: &mut String,
    id: egui::Id,
    rows: usize,
    width: f32,
    hint: Option<&str>,
    env: &ScriptEnv,
) -> egui::Response {
    let (check_id, colours_id, shared_id, suggest_id) =
        (id.with("script-check"), id.with("script-colours"), id.with("script-shared"), id.with("script-suggest"));
    let now = ui.input(|i| i.time);
    // The environment's hash, once a frame: it covers every field name and document script.
    let doc_key = env.key();
    // A script shown for the first time is checked at once (a saved script with an error says so on opening).
    let mut check: Check =
        ui.data(|d| d.get_temp(check_id)).unwrap_or_else(|| Check { seen: hash(text) ^ doc_key, edited_at: now - CHECK_AFTER, checked: None });
    let mut colours: Colours = ui.data(|d| d.get_temp(colours_id)).unwrap_or_else(|| Colours { hash: hash(""), tokens: Arc::default() });
    let mut suggest: suggest::Suggest = ui.data(|d| d.get_temp(suggest_id)).unwrap_or_default();
    let palette = Palette::for_kind(Tokens::get(ui.ctx()).kind);
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let fields: &[String] = env.fields.as_deref().unwrap_or_default();

    // Keys the editor handles itself, before the text box sees them.
    let focused = ui.memory(|m| m.has_focus(id));
    let mut requested = false;
    if focused && let Some(selection) = selection(ui.ctx(), id) {
        let mut done: Option<edit::Edit> = None;
        match suggest.keys(ui) {
            Some(suggest::Key::Accept) => {
                if let Some((bytes, insert)) = suggest.chosen() {
                    let range = edit::char_of(text, bytes.start)..edit::char_of(text, bytes.end);
                    done = Some(edit::replace(text, range, &insert));
                }
                suggest.open = None;
            }
            Some(suggest::Key::Close) => suggest.open = None,
            None => {
                let (tab, shift_tab, enter, space) = ui.input_mut(|i| {
                    let none = egui::Modifiers::NONE;
                    // Shift+Tab first: a plain key also matches when Shift is held.
                    let shift_tab = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
                    (
                        i.consume_key(none, egui::Key::Tab),
                        shift_tab,
                        i.consume_key(none, egui::Key::Enter),
                        i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space),
                    )
                });
                requested = space;
                let brace = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "}")));
                if tab || shift_tab {
                    let unit = edit::indent_unit(text);
                    done = Some(if shift_tab || edit::spans_lines(text, &selection) {
                        edit::indent_lines(text, selection.clone(), shift_tab, &unit)
                    } else {
                        edit::replace(text, selection.clone(), &unit)
                    });
                } else if enter {
                    done = Some(edit::newline(text, selection.clone(), &edit::indent_unit(text)));
                } else if brace && let Some(e) = edit::close_brace(text, selection.clone(), &edit::indent_unit(text)) {
                    ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == "}")));
                    done = Some(e);
                }
            }
        }
        if let Some(e) = done {
            *text = e.text;
            set_selection(ui.ctx(), id, e.selection.clone());
            // An edit the editor made is not typing: it opens no suggestions.
            suggest.for_text = hash(text);
            suggest.for_caret = e.selection.end;
        }
    }

    // The box: coloured, with problems underlined, in a frame that can be dragged taller. While
    // suggestions are open, a warning about the word being completed ("app has no al") is noise.
    let typing = suggest.open.as_ref().map(|c| c.replace.clone());
    let checked = check.checked.clone().map(|(key, found)| (key, Arc::new(unfinished(&found, typing.as_ref()))));
    let digits = text.lines().count().max(1).to_string().len().max(2);
    let digit_width = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let gutter = digit_width * digits as f32 + 12.0;
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let source = buffer.as_str();
        let h = hash(source);
        if colours.hash != h {
            let tokens = if source.len() <= MAX_COLOURED_BYTES { pdfcraft_engine::js::highlight(source) } else { Vec::new() };
            colours = Colours { hash: h, tokens: Arc::new(tokens) };
        }
        // Only a check of this very text is underlined: after an edit its places may have moved.
        let marks: Vec<(Range<usize>, Severity)> = checked
            .as_ref()
            .filter(|(of, _)| *of == h ^ doc_key)
            .map(|(_, found)| found.iter().filter_map(|d| d.at.as_ref().map(|at| (visible(source, at.bytes.clone()), d.severity))).collect())
            .unwrap_or_default();
        let mut job = layout_job(source, &colours.tokens, &marks, &palette, &egui::TextStyle::Monospace.resolve(ui.style()));
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let box_width = if width.is_finite() { width } else { ui.available_width() };
    let min_height = row_height * 2.0 + 14.0;
    // In a dialog, no taller than the dialog can grow and still fit in the window: a box dragged
    // past that would push the dialog's own buttons off the screen. The dialog's size is last
    // frame's, so it is used only if the box was in it then (a box just opened could be measured
    // against another dialog, and the box keeps a height it is clamped to).
    let height_id = id.with("script-height");
    let frame = ui.ctx().cumulative_frame_nr();
    let last = ui.data(|d| d.get_temp::<(u64, f32)>(height_id)).filter(|(at, _)| at.saturating_add(1) >= frame).map(|(_, h)| h);
    let max_height = match (crate::widgets::dialog_room(ui), last) {
        (Some(room), Some(last)) if (last + room).is_finite() => (last + room).clamp(min_height, MAX_HEIGHT.max(min_height)),
        _ => MAX_HEIGHT,
    };
    let mut output: Option<egui::text_edit::TextEditOutput> = None;
    // The part of the box that shows: the gutter and bracket outlines are clipped to it.
    let mut viewport = egui::Rect::NOTHING;
    egui::Resize::default()
        .id_salt(id.with("size"))
        .resizable([false, true])
        .default_width(box_width)
        .min_width(box_width)
        .default_height(row_height * rows.max(1) as f32 + 14.0)
        .min_height(min_height)
        .max_height(max_height)
        .with_stroke(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(id.with("scroll")).auto_shrink([false, false]).show(ui, |ui| {
                viewport = ui.clip_rect();
                let margin = egui::Margin { left: (10.0 + gutter).round().clamp(0.0, 120.0) as i8, right: 10, top: 6, bottom: 6 };
                let mut edit = crate::widgets::block(text)
                    .code_editor()
                    .event_filter(keys_kept(suggest.open.is_some()))
                    .margin(margin)
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, ui.available_height()))
                    .id(id)
                    .layouter(&mut layouter);
                if let Some(hint) = hint {
                    edit = edit.hint_text(hint);
                }
                output = Some(edit.show(ui));
            });
        });
    let Some(output) = output else { return ui.label("") };
    if viewport.height().is_finite() {
        ui.data_mut(|d| d.insert_temp(height_id, (frame, viewport.height())));
    }
    let response = output.response.response.clone().accessible_name(tl!("JavaScript"));
    paint_gutter(ui, &output, gutter, &font, viewport);
    let caret = output.cursor_range.map(|r| r.primary.index.0);
    if let Some(caret) = caret.filter(|_| response.has_focus()) {
        paint_brackets(ui, &output, text, &colours.tokens, caret, palette.bracket, viewport);
    }

    // Suggestions: opened by typing a `.`, inside getField("…"), two letters of a name, or Ctrl+Space.
    let text_hash = hash(text);
    if let Some(caret) = caret.filter(|_| response.has_focus()) {
        let typed = text_hash != suggest.for_text;
        if typed || requested {
            let at = edit::byte_of(text, caret);
            let found = assist::complete(text, at, fields);
            let wanted = requested
                || suggest.open.is_some()
                || found.as_ref().is_some_and(|c| {
                    let before = text.get(..c.replace.start).unwrap_or_default();
                    let typed_len = at.saturating_sub(c.replace.start);
                    before.ends_with('.') || c.items.first().is_some_and(|i| i.kind == assist::SuggestionKind::Field) || typed_len >= 2
                });
            let keep = suggest.open.as_ref().zip(found.as_ref()).is_some_and(|(a, b)| a.items == b.items);
            suggest.open = found.filter(|_| wanted).map(Arc::new);
            if !keep {
                suggest.selected = 0;
            }
        } else if caret != suggest.for_caret {
            suggest.open = None;
        }
        suggest.for_text = text_hash;
        suggest.for_caret = caret;
    } else if !response.has_focus() {
        suggest.open = None;
        suggest.for_text = text_hash;
    }
    if let (Some(caret), Some(_)) = (caret, suggest.open.as_ref()) {
        let rect = output.galley.pos_from_cursor(CCursor::new(caret)).translate(output.galley_pos.to_vec2());
        if let Some(i) = suggest.show(ui.ctx(), suggest_id, rect.left_bottom() + egui::vec2(0.0, 2.0), &font) {
            suggest.selected = i;
            if let Some((bytes, insert)) = suggest.chosen() {
                let range = edit::char_of(text, bytes.start)..edit::char_of(text, bytes.end);
                let e = edit::replace(text, range, &insert);
                *text = e.text;
                set_selection(ui.ctx(), id, e.selection);
                ui.memory_mut(|m| m.request_focus(id));
            }
            suggest.open = None;
            suggest.for_text = hash(text);
        }
    }
    // Escape is the box's while the list is open, from the next frame on (the box was drawn
    // before the list opened or closed this frame). egui keeps the filter only while the box has
    // focus, whether or not the window has.
    ui.memory_mut(|m| m.set_focus_lock_filter(id, keys_kept(suggest.open.is_some())));

    // The check, once typing pauses.
    let mut shared: Shared = ui.data(|d| d.get_temp(shared_id)).unwrap_or_default();
    if shared.key != doc_key {
        let names: Vec<String> = env.doc_scripts.iter().flat_map(|s| assist::declared_names(s)).collect();
        shared = Shared { key: doc_key, names: Arc::new(names) };
    }
    let key = hash(text) ^ doc_key;
    if key != check.seen {
        check.seen = key;
        check.edited_at = now;
    }
    if check.checked.as_ref().is_none_or(|(of, _)| *of != key) {
        let waited = now - check.edited_at;
        if waited >= CHECK_AFTER {
            let knowledge = Knowledge { fields: env.fields.as_deref(), shared: &shared.names };
            check.checked = Some((key, Arc::new(assist::lint(text, knowledge))));
        } else {
            // `try_` and a fallback: a clock that is not finite must not panic here.
            let wait = std::time::Duration::try_from_secs_f64((CHECK_AFTER - waited).clamp(0.0, CHECK_AFTER)).unwrap_or_default();
            ui.ctx().request_repaint_after(wait);
        }
    }
    let typing = suggest.open.as_ref().map(|c| c.replace.clone());
    let shown = Check { checked: check.checked.clone().map(|(k, found)| (k, Arc::new(unfinished(&found, typing.as_ref())))), ..check.clone() };
    problems(ui, text, &shown, key, id, &palette);
    ui.data_mut(|d| {
        d.insert_temp(check_id, check);
        d.insert_temp(colours_id, colours);
        d.insert_temp(shared_id, shared);
        d.insert_temp(suggest_id, suggest);
    });
    response
}

/// The keys the box keeps while it has focus: the arrows, Tab (it indents), and Escape while
/// the suggestion list is open. Without Escape, egui takes the box's focus away on Escape before
/// the box sees the key, and a dialog the box is in closes instead of the list.
fn keys_kept(escape: bool) -> egui::EventFilter {
    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape }
}

/// `found` without warnings about exactly the bytes `typing`: the word suggestions are open for.
fn unfinished(found: &[Diagnostic], typing: Option<&Range<usize>>) -> Vec<Diagnostic> {
    found.iter().filter(|d| d.severity == Severity::Error || typing.is_none_or(|t| d.at.as_ref().is_none_or(|at| at.bytes != *t))).cloned().collect()
}

/// The selection of text box `id`, in characters, from its stored state.
fn selection(ctx: &egui::Context, id: egui::Id) -> Option<Range<usize>> {
    let state = egui::text_edit::TextEditState::load(ctx, id)?;
    let range = state.cursor.char_range()?;
    let [a, b] = range.sorted_cursors();
    Some(a.index.0..b.index.0)
}

fn set_selection(ctx: &egui::Context, id: egui::Id, selection: Range<usize>) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(selection.start), CCursor::new(selection.end))));
    state.store(ctx, id);
}

/// Line numbers in the box's left margin, by the first row of each line (a wrapped line has one).
/// Only the rows in `viewport`, the part of the scrolled box that shows, are numbered.
fn paint_gutter(ui: &egui::Ui, output: &egui::text_edit::TextEditOutput, gutter: f32, font: &egui::FontId, viewport: egui::Rect) {
    let t = Tokens::get(ui.ctx());
    let rect = output.response.response.rect.intersect(viewport);
    let painter = ui.painter_at(rect);
    let right = rect.left() + 4.0 + gutter;
    let mut line = 1usize;
    let mut starts_line = true;
    for row in &output.galley.rows {
        let y = output.galley_pos.y + row.rect().min.y;
        if starts_line && y + row.rect().height() > rect.top() && y < rect.bottom() {
            painter.text(egui::pos2(right - 8.0, y), egui::Align2::RIGHT_TOP, line.to_string(), font.clone(), t.text_muted);
        }
        if starts_line {
            line = line.saturating_add(1);
        }
        starts_line = row.ends_with_newline;
    }
    painter.vline(right, rect.y_range(), Stroke::new(1.0, t.divider));
}

/// An outline round the bracket at the caret and the one that matches it, where they show in
/// `viewport`.
fn paint_brackets(
    ui: &egui::Ui,
    output: &egui::text_edit::TextEditOutput,
    text: &str,
    tokens: &[Token],
    caret: usize,
    colour: Color32,
    viewport: egui::Rect,
) {
    let Some((a, b)) = edit::matching_bracket(text, tokens, caret) else { return };
    let painter = ui.painter_at(output.text_clip_rect.intersect(viewport));
    for byte in [a, b] {
        let i = edit::char_of(text, byte);
        let start = output.galley.pos_from_cursor(CCursor::new(i));
        let end = output.galley.pos_from_cursor(CCursor::new(i.saturating_add(1)));
        let rect = egui::Rect::from_min_max(start.min, egui::pos2(end.min.x.max(start.min.x + 2.0), start.max.y));
        painter.rect_stroke(rect.translate(output.galley_pos.to_vec2()).expand(0.5), 2.0, Stroke::new(1.0, colour), egui::StrokeKind::Outside);
    }
}

/// The lines under the box: the problems found (the first few, then how many more), or that there
/// are none. While a check is pending the previous result stays, muted, so the lines do not
/// flicker on every keystroke. Clicking a problem puts the caret on it.
fn problems(ui: &mut egui::Ui, text: &str, check: &Check, key: u64, editor: egui::Id, palette: &Palette) {
    // The line keeps its height when it has nothing to say, so the buttons below never move
    // under the pointer when the first character is typed or the box is cleared.
    let checked = check.checked.as_ref().filter(|_| !text.trim().is_empty());
    let Some((of, found)) = checked else {
        let height = ui.text_style_height(&egui::TextStyle::Small);
        ui.allocate_space(egui::vec2(0.0, height));
        return;
    };
    let fresh = *of == key;
    let muted = Tokens::get(ui.ctx()).text_muted;
    if found.is_empty() {
        ui.label(egui::RichText::new(tl!("No problems found")).small().color(muted));
        return;
    }
    for problem in found.iter().take(LISTED_PROBLEMS) {
        let colour = match problem.severity {
            _ if !fresh => muted,
            Severity::Error => palette.error,
            Severity::Warning => palette.warning,
        };
        let label = egui::RichText::new(describe(problem)).small().color(colour);
        match problem.at.as_ref().filter(|_| fresh) {
            Some(at) => {
                let clicked = ui.add(egui::Label::new(label).sense(egui::Sense::click())).on_hover_text(tl!("Go to the problem")).clicked();
                if clicked {
                    let index = edit::char_of(text, at.bytes.start);
                    set_selection(ui.ctx(), editor, index..index);
                    ui.ctx().memory_mut(|m| m.request_focus(editor));
                }
            }
            None => {
                ui.label(label);
            }
        }
    }
    let more = found.len().saturating_sub(LISTED_PROBLEMS);
    if more > 0 {
        ui.label(egui::RichText::new(crate::i18n::fmt(tl!("and {n} more"), &[("n", &more.to_string())])).small().color(muted));
    }
}

/// What the problem list says about `problem`.
fn describe(problem: &Diagnostic) -> String {
    match (&problem.at, problem.severity) {
        (Some(at), Severity::Error) => crate::i18n::fmt(
            tl!("Line {line}, column {column}: {message}"),
            &[("line", &at.line.to_string()), ("column", &at.column.to_string()), ("message", &problem.message)],
        ),
        (Some(at), Severity::Warning) => crate::i18n::fmt(
            tl!("Line {line}, column {column}, warning: {message}"),
            &[("line", &at.line.to_string()), ("column", &at.column.to_string()), ("message", &problem.message)],
        ),
        (None, _) => crate::i18n::fmt(tl!("Syntax error: {message}"), &[("message", &problem.message)]),
    }
}

/// `bytes` of `source`, moved back onto the last visible character when it covers only a line
/// end or white space (an underline there would not show).
fn visible(source: &str, bytes: Range<usize>) -> Range<usize> {
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

/// The colours of the script's tokens, its problems and its bracket outline.
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
    warning: Color32,
    bracket: Color32,
}

impl Palette {
    /// Each colour keeps at least 4.5:1 contrast on the theme's field background, and the error's
    /// and warning's on the dialog card the problem list sits on as well (egui's own error red
    /// does not, on either).
    fn for_kind(kind: ThemeKind) -> Self {
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
                error: Color32::from_rgb(0xC6, 0x28, 0x28),
                warning: Color32::from_rgb(0x8A, 0x5A, 0x00),
                bracket: Color32::from_rgb(0x6B, 0x70, 0x7A),
            },
            ThemeKind::Dark => Self {
                text: t.text,
                keyword: Color32::from_rgb(0xC7, 0x92, 0xEA),
                literal: Color32::from_rgb(0x7F, 0xD1, 0xF0),
                string: Color32::from_rgb(0x9C, 0xCC, 0x65),
                regex: Color32::from_rgb(0xF0, 0xA3, 0x5C),
                comment: Color32::from_rgb(0x9A, 0xA1, 0xAD),
                api: Color32::from_rgb(0x82, 0xAA, 0xFF),
                error: Color32::from_rgb(0xFF, 0x7B, 0x72),
                warning: Color32::from_rgb(0xE3, 0xB3, 0x41),
                bracket: Color32::from_rgb(0x9A, 0xA1, 0xAD),
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

/// `source` laid out in `font`, its `tokens` coloured and its problems (`marks`) underlined, an
/// error in red over a tint, a warning in amber.
fn layout_job(source: &str, tokens: &[Token], marks: &[(Range<usize>, Severity)], palette: &Palette, font: &egui::FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    // Cut points: where a token or a mark starts or ends. Every token and mark sits on
    // character boundaries, so every cut does.
    let mut cuts: Vec<usize> = Vec::with_capacity(tokens.len().saturating_add(marks.len()).saturating_mul(2).saturating_add(2));
    cuts.extend([0, source.len()]);
    for t in tokens {
        cuts.extend([t.bytes.start, t.bytes.end]);
    }
    for (m, _) in marks {
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
        // The most serious mark covering the piece.
        let mark = marks.iter().filter(|(m, _)| m.start <= *from && *to <= m.end).map(|(_, s)| *s).min();
        match mark {
            Some(Severity::Error) => {
                format.underline = Stroke::new(1.5, palette.error);
                format.background = palette.error.gamma_multiply(0.18);
            }
            Some(Severity::Warning) => format.underline = Stroke::new(1.5, palette.warning),
            None => {}
        }
        job.append(piece, 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests;
