//! The suggestion list under the caret: field names in `getField("…")`, an object's names after a
//! `.`, the document's names for a word. Up and Down choose, Enter or Tab take the choice, Escape
//! or moving the caret away closes it, Ctrl+Space (⌘Space) opens it anywhere.

use std::sync::Arc;

use egui::{Color32, Stroke};
use pdfcraft_engine::js::assist::{Completion, SuggestionKind};

use crate::theme::Tokens;

/// The list's width, in points.
const LIST_WIDTH: f32 = 320.0;

/// Suggestions shown before the list scrolls.
pub(super) const VISIBLE_ROWS: usize = 8;

/// The list's layer: above every dialog, which egui raises to the top of the foreground on each
/// press inside it.
pub(super) const ORDER: egui::Order = egui::Order::Tooltip;

/// The list's state, kept between frames.
#[derive(Clone, Default)]
pub(super) struct Suggest {
    /// The suggestions while the list is open.
    pub open: Option<Arc<Completion>>,
    pub selected: usize,
    /// The text and caret the list was made for: a caret that moves without typing closes it.
    pub for_text: u64,
    pub for_caret: usize,
    /// From one row's top to the next's, as last drawn (the theme pads the rows); 0 until then.
    row_pitch: f32,
    /// The row last scrolled into view: the list follows the choice, and the wheel otherwise.
    pub scrolled_to: Option<usize>,
}

/// What a key did to an open list.
pub(super) enum Key {
    /// Take the selected suggestion.
    Accept,
    /// Close the list.
    Close,
}

impl Suggest {
    /// Up, Down, Enter, Tab and Escape while the list is open, taken before the text box sees
    /// them. Only without modifiers: Shift+Tab still outdents, Shift+Down still selects.
    pub fn keys(&mut self, ui: &egui::Ui) -> Option<Key> {
        let count = self.open.as_ref().map_or(0, |c| c.items.len());
        if count == 0 {
            return None;
        }
        ui.input_mut(|i| {
            if take(i, egui::Key::ArrowDown) {
                self.selected = (self.selected.saturating_add(1)) % count;
            }
            if take(i, egui::Key::ArrowUp) {
                self.selected = self.selected.checked_sub(1).unwrap_or(count.saturating_sub(1));
            }
            if take(i, egui::Key::Enter) || take(i, egui::Key::Tab) {
                return Some(Key::Accept);
            }
            // Escape with any modifier: the dialog around the box would take Shift+Escape too.
            if i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Escape, pressed: true, .. })) {
                i.events.retain(|e| !matches!(e, egui::Event::Key { key: egui::Key::Escape, .. }));
                return Some(Key::Close);
            }
            None
        })
    }

    /// The suggestion to insert, if one is chosen.
    pub fn chosen(&self) -> Option<(std::ops::Range<usize>, String)> {
        let c = self.open.as_ref()?;
        let item = c.items.get(self.selected)?;
        Some((c.replace.clone(), item.insert.clone()))
    }

    /// Draw the open list with its top left corner at `at`. Returns the index of a clicked suggestion.
    pub fn show(&mut self, ctx: &egui::Context, id: egui::Id, at: egui::Pos2, font: &egui::FontId) -> Option<usize> {
        let completion = self.open.clone()?;
        let t = Tokens::get(ctx);
        let mut clicked = None;
        let mut measured = None;
        egui::Area::new(id).order(ORDER).fixed_pos(at).constrain(true).show(ctx, |ui| {
            egui::Frame::new()
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.border))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(4))
                .shadow(ui.visuals().popup_shadow)
                .show(ui, |ui| {
                    let row = ui.text_style_height(&egui::TextStyle::Body).max(font.size) + 6.0;
                    let gap = ui.spacing().item_spacing.y;
                    let pitch = if self.row_pitch > 0.0 { self.row_pitch } else { row + gap };
                    let list = pitch * VISIBLE_ROWS as f32 - gap;
                    // A fixed width: rows that fill the room there is would stretch an Area across the window.
                    ui.set_width(LIST_WIDTH);
                    // At least `list` as well as at most: an Area lays its contents out in its size
                    // last frame, which would keep a list as short as the one shown before it.
                    egui::ScrollArea::vertical().max_height(list).min_scrolled_height(list).show(ui, |ui| {
                        ui.set_width(LIST_WIDTH);
                        for (i, item) in completion.items.iter().enumerate() {
                            let selected = i == self.selected;
                            let mut label = egui::text::LayoutJob::default();
                            let kind = match item.kind {
                                SuggestionKind::Field => tl!("field"),
                                SuggestionKind::Function => tl!("function"),
                                SuggestionKind::Property => tl!("property"),
                            };
                            let fmt = |colour: Color32, font: egui::FontId| egui::TextFormat { font_id: font, color: colour, ..Default::default() };
                            label.append(&item.label, 0.0, fmt(t.text, font.clone()));
                            label.append(kind, 12.0, fmt(t.text_muted, egui::TextStyle::Small.resolve(ui.style())));
                            let response = ui.add(egui::Button::selectable(selected, label).min_size(egui::vec2(ui.available_width(), row)));
                            let height = response.rect.height();
                            if i == 0 && height.is_finite() && height > 0.0 {
                                measured = Some(height + gap);
                            }
                            let response = match item.detail {
                                Some(detail) => response.on_hover_text(crate::i18n::t(detail)),
                                None => response,
                            };
                            if selected && self.scrolled_to != Some(i) {
                                response.scroll_to_me(None);
                                self.scrolled_to = Some(i);
                            }
                            if response.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                    // The chosen name's description, under the list.
                    if let Some(detail) = completion.items.get(self.selected).and_then(|i| i.detail) {
                        ui.separator();
                        ui.add(egui::Label::new(egui::RichText::new(crate::i18n::t(detail)).small().color(t.text_muted)).wrap());
                    }
                });
        });
        if let Some(pitch) = measured {
            self.row_pitch = pitch;
        }
        clicked
    }
}

/// Takes a press of `key` with no modifier held, if there is one. egui's own `consume_key` also
/// takes it with Shift or Alt held.
fn take(input: &mut egui::InputState, key: egui::Key) -> bool {
    let before = input.events.len();
    input.events.retain(|e| !matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none()));
    input.events.len() < before
}
