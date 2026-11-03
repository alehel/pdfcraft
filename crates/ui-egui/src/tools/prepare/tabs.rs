use super::draft::{ACTION_KINDS, ActionDraft, FieldDraft};
use super::{CalcOp, Calculate, DATE_PRESETS, Format, TIME_PRESETS, Validate, format_value, theme, widgets};

/// The categories of the Format tab.
#[derive(Clone, Copy, PartialEq)]
enum Category {
    None,
    Number,
    Percent,
    Date,
    Time,
    Special,
    /// Custom format and keystroke scripts.
    Custom,
}

fn category(f: &Format) -> Category {
    match f {
        Format::None => Category::None,
        Format::Number { .. } => Category::Number,
        Format::Percent { .. } => Category::Percent,
        Format::Date(_) => Category::Date,
        Format::Time(_) => Category::Time,
        Format::Special(_) | Format::Mask(_) => Category::Special,
    }
}

const SEPARATORS: [&str; 5] = ["1,234.56", "1234.56", "1.234,56", "1234,56", "1'234.56"];

const NEGATIVES: [&str; 4] = ["-1,234.01", "1,234.01 (red)", "(1,234.01)", "(1,234.01) (red)"];

const SPECIALS: [&str; 5] = ["Zip Code", "Zip Code + 4", "Phone Number", "Social Security Number", "Arbitrary Mask"];

/// Draw the field-format editor for the selected category and its options. `env` is what a
/// custom script runs with.
pub(super) fn format_tab(ui: &mut egui::Ui, d: &mut FieldDraft, t: &crate::theme::Tokens, env: &crate::widgets::ScriptEnv) {
    let cat = if d.custom_format.is_some() { Category::Custom } else { category(&d.format) };
    let mut picked = cat;
    ui.horizontal(|ui| {
        ui.label(tl!("Select format category:"));
        craft_ui::ComboBox::from_id_salt("format-cat")
            .selected_text(tl!(match cat {
                Category::None => "None",
                Category::Number => "Number",
                Category::Percent => "Percentage",
                Category::Date => "Date",
                Category::Time => "Time",
                Category::Special => "Special",
                Category::Custom => "Custom",
            }))
            .show_ui(ui, |ui| {
                for (c, l) in [
                    (Category::None, tl!("None")),
                    (Category::Number, tl!("Number")),
                    (Category::Percent, tl!("Percentage")),
                    (Category::Date, tl!("Date")),
                    (Category::Time, tl!("Time")),
                    (Category::Special, tl!("Special")),
                    (Category::Custom, tl!("Custom")),
                ] {
                    ui.selectable_value(&mut picked, c, l);
                }
            });
    });
    if picked != cat {
        // Custom brings back the field's own scripts, if it had some; a built-in format replaces them.
        d.custom_format =
            (picked == Category::Custom).then(|| d.original.as_ref().as_ref().and_then(|o| o.custom_format.clone()).unwrap_or_default());
        d.format = match picked {
            Category::None | Category::Custom => Format::None,
            Category::Number => Format::Number { decimals: 2, sep: 0, neg: 0, currency: String::new(), prepend: true },
            Category::Percent => Format::Percent { decimals: 2, sep: 0 },
            Category::Date => Format::Date("mm/dd/yyyy".into()),
            Category::Time => Format::Time("HH:MM".into()),
            Category::Special => Format::Special(0),
        };
    }
    ui.add_space(8.0);
    if let Some((format, keystroke)) = d.custom_format.as_mut() {
        ui.label(tl!("Custom format script:"));
        crate::widgets::script_editor(ui, format, "custom-format", 5, f32::INFINITY, Some("event.value = …"), env);
        ui.add_space(6.0);
        ui.label(tl!("Custom keystroke script:"));
        crate::widgets::script_editor(ui, keystroke, "custom-keystroke", 5, f32::INFINITY, Some("event.rc = …"), env);
        return;
    }
    let sample = match &d.format {
        Format::Number { .. } | Format::Percent { .. } => "-1234.5",
        Format::Date(_) => "10/1/2026",
        Format::Time(_) => "14:05",
        Format::Special(2) => "5551234567",
        Format::Special(3) => "123456789",
        Format::Special(_) => "123456789",
        _ => "",
    };
    match &mut d.format {
        Format::None => {
            ui.label(egui::RichText::new(tl!("The value is shown as typed.")).color(t.text_muted));
        }
        Format::Number { decimals, sep, neg, currency, prepend } => {
            egui::Grid::new("fmt-number").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tl!("Decimal places:"));
                ui.add(egui::DragValue::new(decimals).range(0..=10));
                ui.end_row();
                ui.label(tl!("Separator style:"));
                craft_ui::ComboBox::from_id_salt("fmt-sep").selected_text(SEPARATORS[(*sep).min(4) as usize]).show_ui(ui, |ui| {
                    for (i, s) in SEPARATORS.iter().enumerate() {
                        ui.selectable_value(sep, i as u8, *s);
                    }
                });
                ui.end_row();
                ui.label(tl!("Currency symbol:"));
                ui.horizontal(|ui| {
                    craft_ui::ComboBox::from_id_salt("fmt-cur")
                        .selected_text(if currency.is_empty() { tl!("None") } else { currency.as_str() })
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            for c in ["", "$", "€", "£", "¥", "CHF "] {
                                ui.selectable_value(currency, c.to_string(), if c.is_empty() { tl!("None") } else { c });
                            }
                        });
                    ui.add(widgets::checkbox(prepend, tl!("Before the number")));
                });
                ui.end_row();
                ui.label(tl!("Negative number style:"));
                craft_ui::ComboBox::from_id_salt("fmt-neg").selected_text(NEGATIVES[(*neg).min(3) as usize]).show_ui(ui, |ui| {
                    for (i, s) in NEGATIVES.iter().enumerate() {
                        ui.selectable_value(neg, i as u8, *s);
                    }
                });
                ui.end_row();
            });
        }
        Format::Percent { decimals, sep } => {
            egui::Grid::new("fmt-pct").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tl!("Decimal places:"));
                ui.add(egui::DragValue::new(decimals).range(0..=10));
                ui.end_row();
                ui.label(tl!("Separator style:"));
                craft_ui::ComboBox::from_id_salt("fmt-psep").selected_text(SEPARATORS[(*sep).min(4) as usize]).show_ui(ui, |ui| {
                    for (i, s) in SEPARATORS.iter().enumerate() {
                        ui.selectable_value(sep, i as u8, *s);
                    }
                });
                ui.end_row();
            });
        }
        Format::Date(p) | Format::Time(p) => {
            let presets: Vec<&str> = if matches!(cat, Category::Time) || picked == Category::Time {
                TIME_PRESETS.to_vec()
            } else {
                DATE_PRESETS.iter().copied().chain(["m/d/yyyy", "mm/dd/yyyy", "yyyy-mm-dd", "dd/mm/yyyy"]).collect()
            };
            ui.horizontal(|ui| {
                ui.label(tl!("Format:"));
                craft_ui::ComboBox::from_id_salt("fmt-date").selected_text(p.as_str()).width(170.0).show_ui(ui, |ui| {
                    for pr in presets {
                        ui.selectable_value(p, pr.to_string(), pr);
                    }
                });
                let custom = ui.label(tl!("Custom:"));
                ui.add(crate::widgets::line(p).desired_width(120.0)).labelled_by(custom.id);
            });
        }
        Format::Special(n) => {
            let mut idx = *n as usize;
            craft_ui::ComboBox::from_id_salt("fmt-special").selected_text(tl!(SPECIALS[idx.min(4)])).show_ui(ui, |ui| {
                for (i, s) in SPECIALS.iter().enumerate() {
                    ui.selectable_value(&mut idx, i, tl!(s));
                }
            });
            if idx == 4 {
                d.format = Format::Mask("999-999".into());
            } else {
                *n = idx as u8;
            }
        }
        Format::Mask(m) => {
            let mut idx = 4usize;
            craft_ui::ComboBox::from_id_salt("fmt-special").selected_text(tl!(SPECIALS[4])).show_ui(ui, |ui| {
                for (i, s) in SPECIALS.iter().enumerate() {
                    ui.selectable_value(&mut idx, i, tl!(s));
                }
            });
            ui.horizontal(|ui| {
                let mask = ui.label(tl!("Mask:"));
                ui.add(crate::widgets::line(m).desired_width(160.0)).labelled_by(mask.id);
            });
            ui.label(egui::RichText::new(tl!("9 digit, A letter, O letter or digit, X any character")).small().color(t.text_faint));
            if idx < 4 {
                d.format = Format::Special(idx as u8);
            }
        }
    }
    if !sample.is_empty() && d.format != Format::None {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(crate::i18n::fmt(tl!("Example: {sample}"), &[("sample", &format_value(&d.format, sample))])).color(t.text_muted),
        );
    }
}

/// `env` is what a custom script runs with.
pub(super) fn validate_tab(ui: &mut egui::Ui, d: &mut FieldDraft, env: &crate::widgets::ScriptEnv) {
    #[derive(PartialEq, Clone, Copy)]
    enum Mode {
        None,
        Range,
        Custom,
    }
    let mode = match (&d.custom_validate, &d.validate) {
        (Some(_), _) => Mode::Custom,
        (None, Validate::Range { .. }) => Mode::Range,
        (None, Validate::None) => Mode::None,
    };
    let mut m = mode;
    widgets::radio_value(ui, &mut m, Mode::None, tl!("Field value is not validated"));
    widgets::radio_value(ui, &mut m, Mode::Range, tl!("Field value is in range:"));
    if let (Mode::Range, Validate::Range { min, max }) = (mode, &mut d.validate) {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            let mut has = min.is_some();
            ui.add(widgets::checkbox(&mut has, tl!("From:")));
            let mut v = min.unwrap_or(0.0);
            ui.add_enabled(has, egui::DragValue::new(&mut v).speed(1.0));
            *min = has.then_some(v);
            let mut has = max.is_some();
            ui.add(widgets::checkbox(&mut has, tl!("To:")));
            let mut v = max.unwrap_or(100.0);
            ui.add_enabled(has, egui::DragValue::new(&mut v).speed(1.0));
            *max = has.then_some(v);
        });
    }
    widgets::radio_value(ui, &mut m, Mode::Custom, tl!("Run custom validation script:"));
    if let Some(script) = d.custom_validate.as_mut() {
        crate::widgets::script_editor(ui, script, "custom-validate", 8, f32::INFINITY, Some("event.rc = event.value > 0;"), env);
    }
    if m != mode {
        // Custom brings back the field's own script, if it had one; another choice replaces it.
        d.custom_validate = (m == Mode::Custom).then(|| d.original.as_ref().as_ref().and_then(|o| o.custom_validate.clone()).unwrap_or_default());
        d.validate = match m {
            Mode::Range => Validate::Range { min: Some(0.0), max: Some(100.0) },
            Mode::None | Mode::Custom => Validate::None,
        };
    }
}

/// `env` is what an action's script runs with (the document's fields and document-level scripts).
pub(super) fn actions_tab(ui: &mut egui::Ui, d: &mut FieldDraft, t: &crate::theme::Tokens, env: &crate::widgets::ScriptEnv) {
    use pdfcraft_engine::FieldTrigger as T;
    let editing = d.editing.is_some();
    ui.label(egui::RichText::new(if editing { tl!("Edit Action") } else { tl!("Add an Action") }).font(theme::semibold(13.0)));
    let a = &mut d.new_action;
    egui::Grid::new("field-actions-add").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tl!("Select Trigger:"));
        // Editing changes the script of one trigger's action: its trigger and kind stay.
        ui.add_enabled_ui(!editing, |ui| {
            craft_ui::ComboBox::from_id_salt("action-trigger").selected_text(tl!(a.trigger.label())).width(220.0).show_ui(ui, |ui| {
                for tr in T::ALL {
                    ui.selectable_value(&mut a.trigger, tr, tl!(tr.label()));
                }
            });
        });
        ui.end_row();
        ui.label(tl!("Select Action:"));
        ui.add_enabled_ui(!editing, |ui| {
            craft_ui::ComboBox::from_id_salt("action-kind").selected_text(tl!(ACTION_KINDS[a.kind])).width(220.0).show_ui(ui, |ui| {
                for (i, k) in ACTION_KINDS.iter().enumerate() {
                    ui.selectable_value(&mut a.kind, i, tl!(k));
                }
            });
        });
        ui.end_row();
        let hint = match a.kind {
            0 => "JavaScript",
            1 => "https://…",
            2 => tl!("Fields to reset (comma-separated; empty: all)"),
            3 => tl!("Print, NextPage, PrevPage, FirstPage or LastPage"),
            4 => tl!("Page number"),
            5 | 6 => tl!("Field names (comma-separated)"),
            _ => tl!("URL to submit to"),
        };
        // A script gets the dialog's whole width, under the grid; a value stays in it.
        if a.kind != 0 {
            ui.label(tl!("Value:"));
            ui.add(crate::widgets::line(&mut a.text).desired_width(340.0).hint_text(hint).id_salt("action-text"));
            ui.end_row();
        }
    });
    if a.kind == 0 {
        ui.add_space(4.0);
        ui.label(tl!("Script:"));
        crate::widgets::script_editor(ui, &mut a.text, "action-text", 8, f32::INFINITY, Some("JavaScript"), env);
    }
    let mut error = None;
    let (mut commit, mut discard) = (false, false);
    ui.horizontal(|ui| {
        commit = ui.button(if editing { tl!("Update") } else { tl!("Add") }).clicked();
        discard = editing && ui.button(tl!("Discard")).clicked();
    });
    if commit {
        match d.new_action.action() {
            Ok(act) => {
                let tr = d.new_action.trigger;
                d.actions.retain(|(x, _)| *x != tr);
                d.actions.push((tr, act));
                d.actions.sort_by_key(|(x, _)| T::ALL.iter().position(|y| y == x));
                d.new_action.text.clear();
                d.editing = None;
            }
            Err(e) => error = Some(e),
        }
    }
    if discard {
        d.new_action = ActionDraft::default();
        d.editing = None;
    }
    if let Some(e) = error {
        ui.label(egui::RichText::new(tl!(&e)).small().color(t.text_muted));
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Actions")).font(theme::semibold(13.0)));
    egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if d.actions.is_empty() {
            ui.label(egui::RichText::new(tl!("No actions")).color(t.text_muted));
        }
        let (mut remove, mut edit) = (None, None);
        for (i, (tr, act)) in d.actions.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!(tr.label())).strong());
                let more = d.chained.iter().find(|(x, _)| x == tr).map_or(0, |(_, n)| *n);
                if more == 0 {
                    ui.label(act.describe());
                } else {
                    let then = crate::i18n::trn(crate::i18n::current(), more as u64, "then {n} more action", "then {n} more actions");
                    ui.label(format!("{}, {then}", act.describe()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.push_id(i, |ui| {
                        if ui.button(tl!("Delete")).clicked() {
                            remove = Some(i);
                        }
                        if let pdfcraft_engine::FieldAction::JavaScript(js) = act
                            && ui.add_enabled(d.editing.is_none(), egui::Button::new(tl!("Edit"))).clicked()
                        {
                            edit = Some((*tr, js.clone()));
                        }
                    });
                });
            });
        }
        if let Some((trigger, text)) = edit {
            d.new_action = ActionDraft { trigger, kind: 0, text };
            d.editing = Some(trigger);
        }
        if let Some(i) = remove {
            if d.actions.get(i).map(|(x, _)| Some(*x)) == Some(d.editing) {
                d.new_action = ActionDraft::default();
                d.editing = None;
            }
            d.actions.remove(i);
        }
    });
}

/// `env` is what a custom script runs with.
pub(super) fn calculate_tab(ui: &mut egui::Ui, d: &mut FieldDraft, t: &crate::theme::Tokens, env: &crate::widgets::ScriptEnv) {
    #[derive(PartialEq, Clone, Copy)]
    enum Mode {
        None,
        Simple,
        Notation,
        Custom,
    }
    let mode = match (&d.custom_calculate, &d.calculate) {
        (Some(_), _) => Mode::Custom,
        (None, Calculate::None) => Mode::None,
        (None, Calculate::Simple { .. }) => Mode::Simple,
        (None, Calculate::Notation(_)) => Mode::Notation,
    };
    let mut m = mode;
    widgets::radio_value(ui, &mut m, Mode::None, tl!("Value is not calculated"));
    widgets::radio_value(ui, &mut m, Mode::Simple, tl!("Value is the"));
    if let (Mode::Simple, Calculate::Simple { op, fields }) = (mode, &mut d.calculate) {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            craft_ui::ComboBox::from_id_salt("calc-op").selected_text(tl!(op.label())).show_ui(ui, |ui| {
                for o in CalcOp::ALL {
                    ui.selectable_value(op, o, tl!(o.label()));
                }
            });
            ui.label(tl!("of the following fields:"));
        });
        egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
            for name in &d.others {
                let mut on = fields.contains(name);
                if ui.add(widgets::checkbox(&mut on, name)).changed() {
                    if on {
                        fields.push(name.clone());
                    } else {
                        fields.retain(|f| f != name);
                    }
                }
            }
        });
    }
    widgets::radio_value(ui, &mut m, Mode::Notation, tl!("Simplified field notation:"));
    if let (Mode::Notation, Calculate::Notation(expr)) = (mode, &mut d.calculate) {
        ui.add(crate::widgets::block(expr).hint_text("Price * Quantity").desired_rows(2).desired_width(f32::INFINITY));
        ui.label(egui::RichText::new(tl!("Field names with + - * / and parentheses; put \\ before spaces in names.")).small().color(t.text_faint));
    }
    widgets::radio_value(ui, &mut m, Mode::Custom, tl!("Custom calculation script:"));
    if let Some(script) = d.custom_calculate.as_mut() {
        crate::widgets::script_editor(ui, script, "custom-calculate", 8, f32::INFINITY, Some("event.value = …"), env);
    }
    if m != mode {
        // Custom brings back the field's own script, if it had one, and leaves the built-in
        // calculation as it was: the script replaces it in the calculation order's place.
        let original = d.original.as_ref().as_ref();
        d.custom_calculate = (m == Mode::Custom).then(|| original.and_then(|o| o.custom_calculate.clone()).unwrap_or_default());
        let kept = |want: Mode| {
            original.map(|o| o.calculate.clone()).filter(|c| match want {
                Mode::Simple => matches!(c, Calculate::Simple { .. }),
                Mode::Notation => matches!(c, Calculate::Notation(_)),
                _ => false,
            })
        };
        match m {
            Mode::None => d.calculate = Calculate::None,
            Mode::Simple => d.calculate = kept(Mode::Simple).unwrap_or(Calculate::Simple { op: CalcOp::Sum, fields: Vec::new() }),
            Mode::Notation => d.calculate = kept(Mode::Notation).unwrap_or(Calculate::Notation(String::new())),
            Mode::Custom => {}
        }
    }
}
