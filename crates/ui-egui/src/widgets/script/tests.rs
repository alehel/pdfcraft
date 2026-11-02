use super::*;

/// Shows the editor over `text` at `time` and returns what it holds afterwards.
fn frame(ctx: &egui::Context, text: &mut String, time: f64) -> (Check, Vec<String>) {
    let mut id = egui::Id::NULL;
    let output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
        id = ui.make_persistent_id("probe");
        script_editor(ui, text, "probe", 4, 300.0, None);
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
    assert!(said.iter().any(|s| s == "No syntax errors"), "{said:?}");
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
            h = ui.vertical(|ui| script_editor(ui, &mut text, "probe", 4, 300.0, None)).response.rect.height();
        });
        output.drop_without_applying_deltas();
        h
    };
    let empty = height("", 1.0);
    let typed = height("v", 1.1);
    let error = height("var (", 2.0);
    let cleared = height("", 3.0);
    assert!((empty - typed).abs() < 0.5 && (empty - error).abs() < 0.5 && (empty - cleared).abs() < 0.5, "{empty} {typed} {error} {cleared}");
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
    let marked = Some(source.find('{').expect("brace")..source.len());
    let job = layout_job(source, &tokens, marked, &palette(), &egui::FontId::monospace(12.0));
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
    let d = Diagnostic { message: "the script is nested too deeply to run safely".into(), at: None };
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
        let checks =
            on_field.map(|(name, c)| (name, c, field)).into_iter().chain([("error in the box", p.error, field), ("error message", p.error, card)]);
        for (name, c, background) in checks {
            let (a, b) = (luminance(c), luminance(background));
            let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(ratio >= 4.5, "{kind:?} {name}: {ratio:.2}:1");
        }
    }
}
