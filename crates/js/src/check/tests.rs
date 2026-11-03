use super::*;
use proptest::prelude::*;

/// The one diagnostic `source` gets, with the text it points at.
fn only(source: &str) -> (Diagnostic, &str) {
    let found = check(source);
    assert_eq!(found.len(), 1, "{source:?}: {found:?}");
    let d = found.into_iter().next().expect("one diagnostic");
    let text = d.at.as_ref().and_then(|at| source.get(at.bytes.clone())).unwrap_or_default();
    (d, text)
}

fn at(d: &Diagnostic) -> (u32, u32) {
    d.at.as_ref().map(|at| (at.line, at.column)).unwrap_or_default()
}

#[test]
fn scripts_the_engine_accepts_pass() {
    for ok in [
        "",
        "   \n// only a comment",
        "event.value = getField(\"a\").value + getField(\"b\").value;",
        "var s = `total: ${util.printf(\"%.2f\", 3)} of ${ {a: 1}.a }`;",
        "if (/^\\d+$/.test(event.change)) { event.rc = true; } else { app.alert('digits only'); }",
        "function f(x) { return x / 2 / 3; }",
    ] {
        assert_eq!(check(ok), Vec::new(), "{ok:?}");
    }
}

#[test]
fn a_missing_bracket_is_placed_on_the_token_after_it() {
    let (d, text) = only("var a = 1;\nif (a { app.alert(a); }");
    assert_eq!(at(&d), (2, 7));
    assert_eq!(text, "{");
    assert!(d.message.contains("')'"), "{}", d.message);
    assert!(!d.message.contains("line"), "the place is not repeated in the message: {}", d.message);
}

#[test]
fn columns_count_characters_and_lines_count_every_javascript_line_end() {
    // A multi-byte character before the error, and each kind of line end before it.
    let source = "var å = 1;\r\nvar b = 2;\rvar c = 3;\u{2028}var d = 'ø' + ;";
    let (d, text) = only(source);
    assert_eq!(at(&d), (4, 15));
    assert_eq!(text, ";");
}

#[test]
fn an_unclosed_string_is_placed() {
    // boa gives no place for these; the opening quote or backtick is the place.
    let (d, text) = only("var x = 1;\nvar s = \"abc\nvar t = 2;");
    assert_eq!((at(&d), text), ((2, 9), "\""), "{d:?}");
    let (d, text) = only("var x = 1;\nvar s = 'a\\'");
    assert_eq!((at(&d), text), ((2, 9), "'"), "an escaped quote does not close it: {d:?}");
    let (d, text) = only("var t = `${a} and ${ {b: 1}.b } and more");
    assert_eq!((at(&d), text), ((1, 9), "`"), "{d:?}");
}

#[test]
fn a_script_that_stops_early_points_at_its_last_character() {
    let (d, text) = only("function f() {\n  return 1;\n\n");
    assert_eq!(at(&d), (2, 11));
    assert_eq!(text, ";");
}

#[test]
fn a_top_level_return_is_an_error_as_when_running() {
    let (d, text) = only("return 1;");
    assert_eq!(text, "return");
    assert_eq!(at(&d), (1, 1));
    // And running it fails the same way.
    let out = crate::run("return 1;", &crate::Event::doc("Open"), &crate::DocInfo::default(), &[], &[], crate::Limits::default());
    assert!(out.error.is_some());
}

#[test]
fn a_script_too_deep_to_parse_safely_is_reported_without_a_place() {
    let deep = format!("var x = {}1{};", "(".repeat(5000), ")".repeat(5000));
    let (d, _) = only(&deep);
    assert_eq!(d.at, None);
    assert!(d.message.contains("nested too deeply"), "{}", d.message);
}

#[test]
fn deeply_nested_conditionals_are_checked_without_overflowing() {
    // The bracket guard lets 4096 conditionals through; they nest in the parser without brackets.
    let mut s = String::from("var x = ");
    for _ in 0..4000 {
        s.push_str("a ? b : ");
    }
    s.push_str("c;");
    assert_eq!(check(&s), Vec::new());
}

fn kinds(source: &str) -> Vec<(TokenKind, &str)> {
    highlight(source).into_iter().map(|t| (t.kind, source.get(t.bytes).unwrap_or("<bad range>"))).collect()
}

#[test]
fn tokens_are_coloured_by_kind() {
    use TokenKind::*;
    assert_eq!(
        kinds("var n = getField(\"total\").value; // sum\nif (n > 1.5e+3) event.rc = false; /* x */"),
        vec![
            (Keyword, "var"),
            (Api, "getField"),
            (String, "\"total\""),
            (Comment, "// sum"),
            (Keyword, "if"),
            (Number, "1.5e+3"),
            (Api, "event"),
            (Literal, "false"),
            (Comment, "/* x */")
        ]
    );
}

#[test]
fn a_slash_divides_after_a_value_and_starts_a_regex_elsewhere() {
    use TokenKind::*;
    assert_eq!(kinds("a = b / c / d;"), vec![]);
    assert_eq!(kinds("x = (y) / 2;"), vec![(Number, "2")]);
    assert_eq!(kinds("ok = /[/]\\/x/gi.test(s);"), vec![(Regex, "/[/]\\/x/gi")]);
    assert_eq!(kinds("return /a/;"), vec![(Keyword, "return"), (Regex, "/a/")]);
}

#[test]
fn template_code_is_coloured_as_code() {
    use TokenKind::*;
    assert_eq!(
        kinds("`a ${ {k: 'v'}.k } b ${event.value}`"),
        vec![(Template, "`a ${"), (String, "'v'"), (Template, "} b ${"), (Api, "event"), (Template, "}`")]
    );
}

#[test]
fn properties_are_never_keywords_or_api_names() {
    assert_eq!(kinds("x.default = this.event;"), vec![(TokenKind::Keyword, "this")]);
}

#[test]
fn unclosed_strings_and_comments_stop_where_the_parser_would() {
    use TokenKind::*;
    assert_eq!(kinds("s = 'open\nvar t;"), vec![(String, "'open"), (Keyword, "var")]);
    assert_eq!(kinds("/* never closed\nvar t;"), vec![(Comment, "/* never closed\nvar t;")]);
    assert_eq!(kinds("`never closed ${"), vec![(Template, "`never closed ${")]);
}

proptest! {
    /// Any text, however malformed: tokens are in order, do not overlap and sit on character
    /// boundaries; and the checker places what it finds inside the source.
    #[test]
    fn any_text_is_handled(source in "[a-z0-9 `'\"/*\\\\${}()\\[\\]\n\r.+?:;éø\u{2028}]{0,80}") {
        let mut end = 0;
        for t in highlight(&source) {
            prop_assert!(t.bytes.start >= end && t.bytes.start < t.bytes.end && t.bytes.end <= source.len());
            prop_assert!(source.is_char_boundary(t.bytes.start) && source.is_char_boundary(t.bytes.end));
            end = t.bytes.end;
        }
        for d in check(&source) {
            if let Some(at) = d.at {
                prop_assert!(at.bytes.start <= at.bytes.end && at.bytes.end <= source.len());
                prop_assert!(source.is_char_boundary(at.bytes.start) && source.is_char_boundary(at.bytes.end));
                prop_assert!(at.line >= 1 && at.column >= 1);
            }
        }
    }
}

fn byte_at(source: &str, line: u32, column: u32) -> usize {
    Lines::new(source).byte_at(source, line, column)
}

#[test]
fn places_past_a_line_or_the_source_end_clamp_to_it() {
    assert_eq!(byte_at("ab\ncd", 1, 9), 2, "past the line end: its line terminator");
    assert_eq!(byte_at("ab\r\ncd", 1, 9), 3, "inside a \\r\\n, still a character boundary");
    assert_eq!(byte_at("ab\ncd", 2, 2), 4);
    assert_eq!(byte_at("ab", 5, 1), 2, "past the last line: the end");
    assert_eq!(byte_at("", 1, 1), 0);
}

/// The places as they were found before [`Lines`]: by walking the script from its start.
mod walked {
    fn walk(source: &str, mut visit: impl FnMut(usize, u32, u32) -> bool) {
        let (mut line, mut column) = (1u32, 1u32);
        let mut chars = source.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if visit(i, line, column) {
                return;
            }
            match c {
                '\r' => {
                    if chars.peek().is_some_and(|(_, n)| *n == '\n') {
                        chars.next();
                    }
                    (line, column) = (line.saturating_add(1), 1);
                }
                '\n' | '\u{2028}' | '\u{2029}' => (line, column) = (line.saturating_add(1), 1),
                _ => column = column.saturating_add(1),
            }
        }
        visit(source.len(), line, column);
    }

    pub fn byte_at(source: &str, line: u32, column: u32) -> usize {
        let mut found = source.len();
        walk(source, |i, l, c| {
            if l > line || (l == line && c >= column) {
                found = i;
                return true;
            }
            false
        });
        if line_column(source, found).0 > line {
            found = source.get(..found).and_then(|s| s.char_indices().next_back()).map_or(0, |(i, _)| i);
        }
        found
    }

    pub fn line_column(source: &str, at: usize) -> (u32, u32) {
        let mut found = (1, 1);
        walk(source, |i, l, c| {
            found = (l, c);
            i >= at
        });
        found
    }
}

proptest! {
    /// Lines and columns placed through [`Lines`] are those walking the script finds.
    #[test]
    fn places_match_walking_the_script(source in "[ab\n\ré\u{2028}]{0,24}", line in 0u32..8, column in 0u32..8, at in 0usize..40) {
        let lines = Lines::new(&source);
        prop_assert_eq!(lines.byte_at(&source, line, column), walked::byte_at(&source, line, column), "{:?} {} {}", source, line, column);
        prop_assert_eq!(lines.line_column(&source, at), walked::line_column(&source, at), "{:?} {}", source, at);
    }
}

#[test]
fn a_line_separator_inside_a_string_does_not_end_it() {
    // Allowed in string literals since ECMAScript 2019, and the parser accepts it.
    let source = "var s = \"a\u{2028}b\"; var t = 'c';";
    assert_eq!(check(source), Vec::new());
    let strings: Vec<&str> = highlight(source).iter().filter(|t| t.kind == TokenKind::String).filter_map(|t| source.get(t.bytes.clone())).collect();
    assert_eq!(strings, ["\"a\u{2028}b\"", "'c'"]);
}

#[test]
fn every_name_coloured_as_acrobat_s_is_one_the_engine_provides() {
    for name in API {
        let script = format!("if (typeof {name} === 'undefined') throw new Error('missing');");
        let outcome = crate::run(&script, &crate::Event::doc("Open"), &crate::DocInfo::default(), &[], &[], crate::Limits::default());
        assert_eq!(outcome.error, None, "{name}");
    }
}

#[test]
fn a_slash_after_a_keyword_s_brackets_or_a_block_starts_a_regex() {
    let kinds = |s: &str| highlight(s).into_iter().map(|t| (t.kind, s.get(t.bytes).unwrap_or_default().to_string())).collect::<Vec<_>>();
    assert!(kinds("if (x) /'/.test(s);").contains(&(TokenKind::Regex, "/'/".into())), "after a keyword's brackets");
    assert!(kinds("{}\n/a/.test(s);").contains(&(TokenKind::Regex, "/a/".into())), "after a block");
    assert!(!kinds("x = (a + b) / 2 / c;").iter().any(|(k, _)| *k == TokenKind::Regex), "division after a value's bracket");
}

#[test]
fn a_wording_is_said_with_its_values_whole() {
    let w = Wording { text: "no \"{name}\"; did you mean \"{near}\"?", holes: vec![("name", "{near}".into()), ("near", "Total".into())] };
    assert_eq!(w.said(w.text), "no \"{near}\"; did you mean \"Total\"?", "a value is not read again");
    assert_eq!(w.said("«{near}» ou «{name}»"), "«Total» ou «{near}»", "a translation may reorder the holes");
    assert_eq!(w.said("{other} {name"), "{other} {name", "holes it does not fill, and an unclosed brace, stay as written");
}
