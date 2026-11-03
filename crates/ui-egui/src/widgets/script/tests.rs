use super::*;

/// Shows the editor over `text` at `time` and returns what it holds afterwards.
fn frame(ctx: &egui::Context, text: &mut String, time: f64) -> (Check, Vec<String>) {
    let mut id = egui::Id::NULL;
    let output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
        id = ui.make_persistent_id("probe");
        script_editor(ui, text, "probe", 4, 300.0, None, &ScriptEnv::default());
    });
    let check = ctx.data(|d| d.get_temp::<Check>(id.with("script-check"))).expect("the editor keeps its check");
    let mut said = Vec::new();
    for clipped in &output.shapes {
        if let egui::Shape::Text(t) = &clipped.shape {
            said.push(t.galley.text().to_owned());
        }
    }
    output.drop_without_applying_deltas();
    (check, said)
}

fn found(check: &Check) -> Option<Vec<Diagnostic>> {
    check.checked.as_ref().map(|(_, d)| d.to_vec())
}

#[test]
fn a_script_is_checked_on_opening_and_again_once_typing_pauses() {
    let ctx = egui::Context::default();
    let mut text = String::from("var a = 1;\nif (a { }");
    let (check, said) = frame(&ctx, &mut text, 1.0);
    let problem = found(&check).and_then(|d| d.into_iter().next()).expect("opened with an error, checked at once");
    assert_eq!(problem.at.as_ref().map(|at| (at.line, at.column)), Some((2, 7)));
    assert!(said.iter().any(|s| s.starts_with("Line 2, column 7: ")), "{said:?}");

    // Fixed, but typing has not paused: the old result stays (muted) and nothing is re-checked.
    text = String::from("var a = 1;\nif (a) { }");
    let (check, said) = frame(&ctx, &mut text, 1.1);
    assert_eq!(found(&check).map(|d| d.len()), Some(1), "not re-checked mid-typing");
    assert!(said.iter().any(|s| s.starts_with("Line 2")), "the previous result stays while typing: {said:?}");
    let (check, _) = frame(&ctx, &mut text, 1.3);
    assert_eq!(found(&check).map(|d| d.len()), Some(1), "still within the pause");
    let (check, said) = frame(&ctx, &mut text, 1.6);
    assert_eq!(found(&check), Some(Vec::new()), "checked once typing paused");
    assert!(said.iter().any(|s| s == "No problems found"), "{said:?}");
}

#[test]
fn the_editor_keeps_its_height_as_the_box_fills_and_empties() {
    // A status line that appeared with the first character would move the buttons below the
    // box out from under the pointer.
    let ctx = egui::Context::default();
    let height = |text: &str, time: f64| {
        let mut text = text.to_owned();
        let mut h = 0.0;
        let output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
            h = ui.vertical(|ui| script_editor(ui, &mut text, "probe", 4, 300.0, None, &ScriptEnv::default())).response.rect.height();
        });
        let said: Vec<String> =
            output.shapes.iter().filter_map(|c| if let egui::Shape::Text(t) = &c.shape { Some(t.galley.text().to_owned()) } else { None }).collect();
        output.drop_without_applying_deltas();
        (h, said)
    };
    let (empty, _) = height("", 1.0);
    let (typed, _) = height("v", 1.1);
    height("var (", 2.0);
    // Once typing has paused, so the line under the box says what is wrong.
    let (error, said) = height("var (", 2.5);
    assert!(said.iter().any(|s| s.starts_with("Line 1")), "the error is shown: {said:?}");
    let (cleared, _) = height("", 3.0);
    assert!((empty - typed).abs() < 0.5 && (empty - error).abs() < 0.5 && (empty - cleared).abs() < 0.5, "{empty} {typed} {error} {cleared}");
}

#[test]
fn undo_never_brings_back_the_script_shown_before() {
    // One box shows one field's script, then another's (the same box id): Cmd+Z after typing
    // in the second goes back to the second script as it came, not to the first.
    let ctx = egui::Context::default();
    let mut id = egui::Id::NULL;
    let mut show = |text: &mut String, time: f64, events: Vec<egui::Event>| {
        let input = egui::RawInput { time: Some(time), events, focused: true, ..Default::default() };
        let output = ctx.run_ui(input, |ui| {
            id = ui.make_persistent_id("probe");
            ui.memory_mut(|m| m.request_focus(id));
            script_editor(ui, text, "probe", 4, 300.0, None, &ScriptEnv::default());
        });
        output.drop_without_applying_deltas();
    };
    let typed = |s: &str| vec![egui::Event::Text(s.into())];
    let mut first = String::from("first");
    show(&mut first, 1.0, Vec::new());
    show(&mut first, 1.1, Vec::new());
    show(&mut first, 2.0, typed("x"));
    show(&mut first, 5.0, Vec::new());
    assert_eq!(first, "firstx");

    let mut second = String::from("second");
    show(&mut second, 6.0, Vec::new());
    show(&mut second, 6.1, typed("y"));
    assert_eq!(second, "secondy");
    let undo = egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
    show(&mut second, 6.2, vec![undo]);
    assert_eq!(second, "second");
}

#[test]
fn an_empty_box_says_nothing() {
    let ctx = egui::Context::default();
    let mut text = String::from("  \n");
    let (_, said) = frame(&ctx, &mut text, 1.0);
    assert!(!said.iter().any(|s| s.contains("syntax") || s.starts_with("Line")), "{said:?}");
}

fn palette() -> Palette {
    Palette::for_kind(ThemeKind::Light)
}

#[test]
fn the_layout_keeps_every_character_and_colours_and_marks_the_right_ones() {
    let source = "var s = 'é'; // ok\nif (x {";
    let tokens = pdfcraft_engine::js::highlight(source);
    let marked = [(source.find('{').expect("brace")..source.len(), Severity::Error)];
    let job = layout_job(source, &tokens, &marked, &palette(), &egui::FontId::monospace(12.0));
    assert_eq!(job.text, source, "nothing lost or doubled");
    let colour_of = |piece: &str| {
        let at = source.find(piece).expect("piece");
        job.sections
            .iter()
            .find(|s| s.byte_range.start.0 <= at && at < s.byte_range.end.0)
            .map(|s| (s.format.color, s.format.underline.width > 0.0, s.format.italics))
    };
    let p = palette();
    assert_eq!(colour_of("var"), Some((p.keyword, false, false)));
    assert_eq!(colour_of("'é'"), Some((p.string, false, false)));
    assert_eq!(colour_of("// ok"), Some((p.comment, false, true)));
    assert_eq!(colour_of(" = "), Some((p.text, false, false)));
    assert_eq!(colour_of("{"), Some((p.text, true, false)), "the error is underlined");
    assert_eq!(colour_of("x"), Some((p.text, false, false)));
}

#[test]
fn a_mark_on_a_line_end_moves_onto_the_last_character_before_it() {
    let source = "f(a,\n";
    assert_eq!(visible(source, 4..5), 3..4, "a newline: the comma before it");
    assert_eq!(visible(source, 5..5), 3..4, "the end of the source: the comma");
    assert_eq!(visible("é \n", 2..3), 0..2, "a multi-byte character, whole");
    assert_eq!(visible("  ", 1..2), 1..2, "nothing visible before: unchanged");
    assert_eq!(visible(source, 0..1), 0..1, "a visible mark is kept");
}

#[test]
fn problems_without_a_place_say_so() {
    let d = Diagnostic { severity: Severity::Error, message: "the script is nested too deeply to run safely".into(), at: None };
    assert_eq!(describe(&d), "Syntax error: the script is nested too deeply to run safely");
}

#[test]
fn every_colour_reads_on_its_background() {
    // WCAG relative luminance contrast.
    fn luminance(c: Color32) -> f64 {
        let channel = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let tokens = Tokens::for_kind(kind);
        let (field, card) = (tokens.field, tokens.card);
        let p = Palette::for_kind(kind);
        let on_field =
            [("keyword", p.keyword), ("literal", p.literal), ("string", p.string), ("regex", p.regex), ("comment", p.comment), ("api", p.api)];
        // The error is underlined in the box and written in the status line on the dialog card.
        let checks = on_field.map(|(name, c)| (name, c, field)).into_iter().chain([
            ("error in the box", p.error, field),
            ("error message", p.error, card),
            ("warning in the box", p.warning, field),
            ("warning message", p.warning, card),
        ]);
        for (name, c, background) in checks {
            let (a, b) = (luminance(c), luminance(background));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(ratio >= 4.5, "{kind:?} {name}: {ratio:.2}:1");
        }
    }
}

#[test]
fn a_check_that_finds_something_asks_for_a_frame_to_underline_it() {
    // The box is laid out before the check runs: without another frame the underline would wait
    // for the next input.
    let ctx = egui::Context::default();
    let mut text = String::from("var a;");
    // egui itself asks for the first two frames.
    for time in [1.0, 5.0, 6.0] {
        frame(&ctx, &mut text, time);
    }
    assert!(!ctx.has_requested_repaint(), "nothing to do: no frame asked for");
    text = String::from("if (a {");
    frame(&ctx, &mut text, 7.0);
    let (check, _) = frame(&ctx, &mut text, 7.5);
    assert_eq!(found(&check).map(|d| d.len()), Some(1), "checked once typing paused");
    assert!(ctx.has_requested_repaint(), "the next frame underlines what the check found");
}

/// An editor driven by key events, as a person types into it.
struct Typing {
    ctx: egui::Context,
    text: String,
    env: ScriptEnv,
    time: f64,
    id: egui::Id,
    said: Vec<String>,
    /// Each text painted: what it says, where, and the rectangle it is clipped to.
    shapes: Vec<(String, egui::Rect, egui::Rect)>,
    /// Whether the last frame left an Escape press for whatever is around the box (a dialog).
    escape_left: bool,
    /// Room above the box, as a dialog has (the top of the screen would hide what is painted above it).
    above: f32,
}

impl Typing {
    fn new(text: &str, env: ScriptEnv) -> Self {
        let mut t = Typing {
            ctx: egui::Context::default(),
            text: text.into(),
            env,
            time: 1.0,
            id: egui::Id::NULL,
            said: Vec::new(),
            shapes: Vec::new(),
            escape_left: false,
            above: 0.0,
        };
        t.frame(Vec::new());
        t.ctx.memory_mut(|m| m.request_focus(t.id));
        t.frame(Vec::new());
        t
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.time += 0.05;
        let (mut id, mut escape_left, text, env, above) = (egui::Id::NULL, false, &mut self.text, &self.env, self.above);
        let output = self.ctx.run_ui(egui::RawInput { time: Some(self.time), events, ..Default::default() }, |ui| {
            ui.add_space(above);
            id = ui.make_persistent_id("probe");
            script_editor(ui, text, "probe", 6, 400.0, None, env);
            escape_left = ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        (self.id, self.escape_left) = (id, escape_left);
        self.shapes = output
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), egui::Rect::from_min_size(t.pos, t.galley.size()), c.clip_rect)),
                _ => None,
            })
            .collect();
        self.said = self.shapes.iter().map(|(s, _, _)| s.clone()).collect();
        output.drop_without_applying_deltas();
    }

    /// Put the selection on characters `range`, as clicking or dragging would.
    fn select(&mut self, range: Range<usize>) {
        set_selection(&self.ctx, self.id, range);
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
    }

    fn typed(&mut self, s: &str) {
        self.frame(vec![egui::Event::Text(s.into())]);
    }

    fn caret(&self) -> Option<Range<usize>> {
        selection(&self.ctx, self.id)
    }

    fn suggest(&self) -> Vec<String> {
        let s = self.ctx.data(|d| d.get_temp::<suggest::Suggest>(self.id.with("script-suggest"))).unwrap_or_default();
        s.open.map(|c| c.items.iter().map(|i| i.label.clone()).collect()).unwrap_or_default()
    }

    /// Wait for the check (typing paused).
    fn pause(&mut self) {
        self.time += CHECK_AFTER;
        self.frame(Vec::new());
    }
}

#[test]
fn enter_keeps_the_indentation_tab_indents_lines_and_a_brace_outdents() {
    let mut t = Typing::new("if (a) {", ScriptEnv::default());
    t.select(8..8);
    t.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(t.text, "if (a) {\n  ");
    assert_eq!(t.caret(), Some(11..11));
    t.typed("x;");
    t.key(egui::Key::Enter, egui::Modifiers::NONE);
    t.typed("}");
    assert_eq!(t.text, "if (a) {\n  x;\n}", "the brace goes back to the block's level");
    // Tab on two selected lines indents both; Shift+Tab takes it back.
    t.select(0..13);
    t.key(egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(t.text, "  if (a) {\n    x;\n}");
    t.select(0..17);
    t.key(egui::Key::Tab, egui::Modifiers::SHIFT);
    assert_eq!(t.text, "if (a) {\n  x;\n}");
    // Tab at a caret types the script's indentation, not a tab character.
    t.select(0..0);
    t.key(egui::Key::Tab, egui::Modifiers::NONE);
    assert_eq!(t.text, "  if (a) {\n  x;\n}");
}

#[test]
fn a_dot_opens_suggestions_that_enter_takes_and_escape_closes() {
    let mut t = Typing::new("app", ScriptEnv::default());
    t.select(3..3);
    t.typed(".");
    let offered = t.suggest();
    assert_eq!(offered.first().map(String::as_str), Some("alert"), "{offered:?}");
    t.key(egui::Key::ArrowDown, egui::Modifiers::NONE);
    t.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(t.text, "app.beep", "the second suggestion, and Enter did not add a line");
    assert!(t.suggest().is_empty(), "closed after choosing");
    // Escape closes the list and leaves the text alone. The key is the box's: it keeps its focus,
    // and a dialog around it does not close (egui drops a box's focus on Escape unless the box
    // claims the key, and the dialog then takes it).
    t.typed("; app.");
    assert!(!t.suggest().is_empty(), "the list is open");
    t.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(t.suggest().is_empty());
    assert_eq!(t.text, "app.beep; app.");
    assert!(t.ctx.memory(|m| m.has_focus(t.id)), "the box keeps its focus");
    assert!(!t.escape_left, "the dialog does not see the Escape");
    // With the list closed, Escape is no longer the box's.
    t.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(t.escape_left, "a second Escape is the dialog's");
}

/// The list shows its eight rows however short the list open before it was: an egui Area lays its
/// contents out in its last size, which kept a list that grew as short as the one before it.
#[test]
fn the_list_shows_eight_rows_after_a_shorter_list() {
    let fields: Vec<String> = (1..=12).map(|i| format!("Field{i:02}")).collect();
    let mut t = Typing::new("getField(\"Field0", ScriptEnv { fields: Some(fields), doc_scripts: Vec::new() });
    // Rows padded as the app's theme pads buttons, taller than the text alone.
    t.ctx.global_style_mut(|s| s.spacing.button_padding.y = 8.0);
    t.select(16..16);
    t.typed("1");
    t.frame(Vec::new());
    assert_eq!(t.suggest(), ["Field01"], "a list of one");
    t.key(egui::Key::Backspace, egui::Modifiers::NONE);
    t.frame(Vec::new());
    t.frame(Vec::new());
    assert_eq!(t.suggest().len(), 9);
    let shown = t.shapes.iter().filter(|(s, rect, clip)| s.starts_with("Field") && clip.contains_rect(*rect)).count();
    assert_eq!(shown, suggest::VISIBLE_ROWS, "{:?}", t.shapes.iter().filter(|(s, _, _)| s.starts_with("Field")).collect::<Vec<_>>());
}

/// Line numbers show only beside the lines in view: a scrolled box painted the numbers of the
/// lines scrolled out over whatever was above it.
#[test]
fn line_numbers_stay_inside_a_scrolled_box() {
    let text: String = (1..=40).map(|i| format!("var a{i} = {i};\n")).collect();
    let end = text.chars().count();
    let mut t = Typing::new(&text, ScriptEnv::default());
    t.above = 300.0;
    t.select(end..end);
    t.typed("x");
    // The box scrolls to the caret over a few frames.
    for _ in 0..20 {
        t.frame(Vec::new());
    }
    // The script shows between these heights (its clip, inside the box's margins).
    let shown = t.shapes.iter().find(|(s, _, _)| s.starts_with("var a1 = 1;")).map(|(_, _, clip)| clip.y_range()).expect("the script is painted");
    let numbers: Vec<_> = t.shapes.iter().filter(|(s, _, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).collect();
    assert!(numbers.iter().any(|(s, rect, clip)| s == "41" && clip.intersects(*rect)), "the caret's line is numbered: {numbers:?}");
    for (s, rect, clip) in numbers {
        if clip.intersects(*rect) {
            let visible = clip.intersect(*rect);
            // The box's 6-point margins: a number may show in them, beside a line cut by the edge.
            assert!(
                visible.top() >= shown.min - 6.5 && visible.bottom() <= shown.max + 6.5,
                "line {s} shows outside the box ({shown:?}): {rect:?} clipped to {clip:?}"
            );
        }
    }
}

/// In a dialog, the box is never taller than the dialog can be and still fit in the window: a box
/// dragged taller pushed the dialog's buttons off the screen, and kept that height when reopened.
#[test]
fn the_box_keeps_its_dialog_inside_the_window() {
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
    let mut text = String::new();
    for frame in 0..6 {
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(1.0 + f64::from(frame) * 0.05), ..Default::default() };
        let output = ctx.run_ui(input, |ui| {
            crate::widgets::show_modal(ui.ctx(), "probe-dialog", |ui| {
                ui.label("Title");
                // 80 rows: far more than the window has room for.
                script_editor(ui, &mut text, "probe", 80, 300.0, None, &ScriptEnv::default());
                ui.label("Buttons");
            });
        });
        output.drop_without_applying_deltas();
    }
    let card: egui::Rect = ctx.data(|d| d.get_temp(egui::Id::unique("probe-dialog").with("card"))).expect("the dialog was shown");
    assert!(card.top() >= 0.0 && card.bottom() <= screen.bottom(), "the dialog leaves the window: {card:?}");
}

#[test]
fn field_names_are_suggested_and_checked_against_the_document() {
    let env = ScriptEnv { fields: Some(vec!["Total".into(), "Qty".into()]), doc_scripts: vec!["var rate = 2;".into()] };
    let mut t = Typing::new("getField(\"", env);
    t.select(10..10);
    t.typed("T");
    assert_eq!(t.suggest(), ["Total"]);
    t.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(t.text, "getField(\"Total\"", "an unclosed string gets its quote");
    // A misspelt name is a warning under the box; a document script's variable is known.
    t.text = "rate = getField(\"Totl\").value;".into();
    t.frame(Vec::new());
    t.pause();
    let warning = t.said.iter().find(|s| s.contains("warning")).cloned().unwrap_or_default();
    assert!(warning.starts_with("Line 1, column 17, warning: ") && warning.contains("did you mean \"Total\""), "{:?}", t.said);
    assert!(!t.said.iter().any(|s| s.contains("not declared")), "rate is declared by a document script: {:?}", t.said);
    // Without that document script, it is not.
    let mut alone = Typing::new("rate = getField(\"Total\").value;", ScriptEnv { fields: Some(vec!["Total".into()]), doc_scripts: Vec::new() });
    alone.pause();
    assert!(alone.said.iter().any(|s| s.contains("\"rate\" is not declared")), "{:?}", alone.said);
}

#[test]
fn a_warning_about_the_word_being_completed_waits_for_it() {
    let warning = |bytes: Range<usize>| Diagnostic {
        severity: Severity::Warning,
        message: "app has no \"al\"".into(),
        at: Some(pdfcraft_engine::js::Location { line: 1, column: 5, bytes }),
    };
    let found = vec![warning(4..6), warning(10..12)];
    assert_eq!(unfinished(&found, Some(&(4..6))), vec![warning(10..12)]);
    assert_eq!(unfinished(&found, None), found);
}
