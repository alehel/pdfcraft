use super::model::*;

#[test]
fn every_name_the_engine_provides_is_described_and_nothing_else() {
    let m = model();
    assert_eq!(m.members.len(), Object::ALL.len(), "every object was read: {:?}", m.members.keys());
    for object in Object::ALL {
        let provided = m.members.get(&object).cloned().unwrap_or_default();
        assert!(!provided.is_empty(), "{object:?} has names");
        let described: std::collections::BTreeSet<String> =
            DESCRIPTIONS.iter().filter(|(o, _, _)| *o == object).map(|(_, n, _)| n.to_string()).collect();
        let missing: Vec<_> = provided.difference(&described).collect();
        let stale: Vec<_> = described.difference(&provided).collect();
        assert!(missing.is_empty() && stale.is_empty(), "{object:?}: describe {missing:?}; no longer provided {stale:?}");
    }
}

#[test]
fn globals_include_javascript_and_the_document() {
    let m = model();
    for name in ["Math", "Number", "parseInt", "getField", "app", "event", "util"] {
        assert!(m.globals.contains(name), "{name}");
    }
    assert!(m.has(Object::App, "alert") && m.has(Object::Field, "toString") && !m.has(Object::App, "alret"));
    // Functions are told from values by what they are, not by their description.
    for (object, name, function) in [
        (Object::App, "alert", true),
        (Object::Document, "getField", true),
        (Object::Field, "isBoxChecked", true),
        (Object::Event, "change", false),
        (Object::Field, "value", false),
        (Object::App, "viewerVersion", false),
    ] {
        assert_eq!(m.functions.contains(&(object, name.to_string())), function, "{object:?}.{name}");
    }
}

use super::*;
use crate::check::Severity;
use proptest::prelude::*;

fn fields() -> Vec<String> {
    ["Total", "Qty", "Price", "name", "address.street", "address.city"].iter().map(|s| s.to_string()).collect()
}

/// The warnings for `source` in a form with [`fields`], each with the text it points at.
fn warnings(source: &str) -> Vec<(String, String)> {
    let fields = fields();
    let found = lint(source, Knowledge { fields: Some(&fields), shared: &["shared".to_string()] });
    found
        .into_iter()
        .map(|d| {
            assert_eq!(d.severity, Severity::Warning, "{d:?}");
            let at = d.at.as_ref().and_then(|at| source.get(at.bytes.clone())).unwrap_or("<no place>").to_string();
            (at, d.message)
        })
        .collect()
}

#[test]
fn a_clean_script_has_no_warnings() {
    let script = "var f = getField('Total');\nvar q = this.getField(\"Qty\").value;\nf.value = q * getField('Price').value;\nevent.rc = event.value !== '';\nevent.target.textColor = color.red;\nvar a = getField('address');\nshared = 1;\nfunction money(v) { return util.printf('%.2f', v); }\nif (q > 0) { app.alert(money(q), 3); } else { console.println(f.valueAsString); }\nfor (var i = 0; i < numFields; i++) { getNthFieldName(i); }\nMath.round(1).toString();";
    assert_eq!(warnings(script), Vec::<(String, String)>::new());
}

#[test]
fn an_unknown_field_name_is_flagged_with_the_nearest_one() {
    let w = warnings("var t = getField(\"Totl\").value;\nthis.getField('total');\ngetField('Nothing like it');");
    assert_eq!(w.len(), 3, "{w:?}");
    assert_eq!(w[0].0, "\"Totl\"");
    assert!(w[0].1.contains("did you mean \"Total\""), "{}", w[0].1);
    assert!(w[1].1.contains("case-sensitive") && w[1].1.contains("\"Total\""), "{}", w[1].1);
    assert!(!w[2].1.contains("did you mean"), "{}", w[2].1);
}

#[test]
fn field_names_are_not_checked_without_a_document() {
    let found = lint("getField('anything');", Knowledge::default());
    assert_eq!(found, Vec::new());
}

#[test]
fn unknown_names_on_acrobat_objects_and_fields_are_flagged() {
    let w = warnings(
        "app.alret('x');\nevent.vaule = 1;\ngetField('Total').vaule;\nvar f = getField('Qty');\nf.borderStyle = 'solid';\nevent.target.valu;\ncolor.convert(color.red, 'G');",
    );
    let at: Vec<&str> = w.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(at, ["alret", "vaule", "vaule", "borderStyle", "valu", "convert"], "{w:?}");
    assert!(w[0].1.contains("app has no \"alret\"; did you mean \"alert\""), "{}", w[0].1);
    assert!(w[2].1.starts_with("Field objects have no \"vaule\""), "{}", w[2].1);
    assert!(w[3].1.contains("Acrobat feature PdfCraft does not provide"), "{}", w[3].1);
}

#[test]
fn a_script_that_declares_an_acrobat_name_means_its_own() {
    assert_eq!(warnings("var app = { mine: 1 };\napp.mine = 2;"), Vec::<(String, String)>::new());
    // Reassigned: no longer known to be a field.
    assert_eq!(warnings("var f = getField('Qty');\nf = { x: 1 };\nf.x;"), Vec::<(String, String)>::new());
}

#[test]
fn undeclared_assignments_conditions_and_dead_code_are_flagged() {
    let w = warnings(
        "total = 0;\nif (total = 1) { }\nwhile ((total = 2)) { break; }\nfunction f() {\n  return 1;\n  total++;\n}\nfunction g() { return 2; function h() {} }",
    );
    let at: Vec<&str> = w.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(at, ["total", "total", "total = 1", "total", "total++"], "{w:?}");
    assert!(w[0].1.contains("not declared"), "{}", w[0].1);
    assert!(w[2].1.contains("=="), "{}", w[2].1);
    assert!(w[4].1.contains("never runs") && w[4].1.contains("return"), "{}", w[4].1);
}

#[test]
fn syntax_errors_come_back_as_errors_and_stop_the_warnings() {
    let found = lint("app.alret(;", Knowledge::default());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].severity, Severity::Error);
}

#[test]
fn document_scripts_share_their_declarations() {
    let mut names = declared_names("var total = 0;\nfunction money(v) { var inner; }\nconst { a, b: [c] } = x;");
    names.sort();
    assert_eq!(names, ["a", "c", "inner", "money", "total", "v"]);
    assert_eq!(declared_names("var (;"), Vec::<String>::new());
}

#[test]
fn suggestions_are_near_misses_only() {
    use super::lint::suggest;
    assert_eq!(suggest("alret", ["alert", "beep"]), Some("alert"));
    assert_eq!(suggest("TOTAL", ["Total", "Totals"]), Some("Total"));
    assert_eq!(suggest("x", ["alert"]), None);
    assert_eq!(suggest("borderStyle", ["display", "value"]), None);
    assert_eq!(suggest("", ["a"]), None);
}

fn labels(c: &Option<Completion>) -> Vec<String> {
    c.as_ref().map(|c| c.items.iter().map(|i| i.label.clone()).collect()).unwrap_or_default()
}

/// Completion at the `|` in `marked`.
fn at(marked: &str) -> (String, Option<Completion>) {
    let cursor = marked.find('|').expect("a caret");
    let source = marked.replacen('|', "", 1);
    let c = complete(&source, cursor, &fields());
    (source, c)
}

#[test]
fn field_names_are_suggested_inside_get_field() {
    let (source, c) = at("var t = getField(\"to|\").value;");
    assert_eq!(labels(&c), ["Total"]);
    let c = c.expect("suggestions");
    assert_eq!(source.get(c.replace.clone()), Some("to"), "the string's text is replaced, not its quotes");
    assert_eq!(c.items[0].insert, "Total");
    // Unclosed: the closing quote comes with the name.
    let (_, c) = at("this.getField('addr|");
    assert_eq!(labels(&c), ["address.city", "address.street"]);
    assert_eq!(c.expect("suggestions").items[0].insert, "address.city'");
    // Every field for an empty string, but not in other strings or after the closing quote.
    assert_eq!(labels(&at("getField(\"|\")").1).len(), 6);
    assert_eq!(at("app.alert(\"to|\")").1, None);
    assert_eq!(at("getField(\"Qty\"|)").1, None);
}

#[test]
fn members_are_suggested_after_a_dot() {
    assert_eq!(labels(&at("app.al|").1), ["alert"]);
    let (_, c) = at("event.|");
    assert!(labels(&c).contains(&"value".to_string()) && labels(&c).contains(&"rc".to_string()));
    assert_eq!(labels(&at("getField('Total').valueAs|").1), ["valueAsString"]);
    assert_eq!(labels(&at("event.target.fill|").1), ["fillColor"]);
    // checkTh*isBox* contains it too, after the name that starts with it.
    assert_eq!(labels(&at("var f = getField('Qty');\nf.isBox|").1), ["isBoxChecked", "checkThisBox"]);
    assert_eq!(labels(&at("app.al|").1), ["alert"], "two letters only match the start, not setInterval");
    assert_eq!(labels(&at("this.getNth|").1), ["getNthFieldName"]);
    // `ue` is in valueAsString too: names starting with the prefix come first.
    assert_eq!(labels(&at("event.va|").1).first().map(String::as_str), Some("value"));
    let alert = at("app.al|").1.expect("suggestions").items.remove(0);
    assert_eq!((alert.kind, alert.detail.is_some()), (SuggestionKind::Function, true));
    // Unknown objects get nothing.
    assert_eq!(at("myThing.|").1, None);
}

#[test]
fn bare_words_suggest_the_documents_names() {
    assert_eq!(labels(&at("var f = getF|").1), ["getField"]);
    assert_eq!(labels(&at("ev|").1), ["event"]);
    assert_eq!(at("getField|").1, None, "already complete");
    assert_eq!(at("|").1, None);
    assert_eq!(at("// app.al|").1, None, "not in a comment");
    assert_eq!(at("var x = 12|").1, None);
    // The whole word under the caret is replaced.
    let (source, c) = at("app.al|rt('x')");
    assert_eq!(c.and_then(|c| source.get(c.replace).map(str::to_string)).as_deref(), Some("alrt"));
    // A name already written out in full gets no list, after a dot too.
    assert_eq!(at("app.al|ert('x')").1, None);
}

proptest! {
    /// Any text and caret: no panic, suggestions replace text on character boundaries around the
    /// caret, and the linter places what it finds inside the source.
    #[test]
    fn any_text_and_caret_is_handled(source in "[a-z.( )'\"`$/*\n{}é=;]{0,60}", caret in 0usize..64) {
        let fields = fields();
        if let Some(c) = complete(&source, caret, &fields) {
            prop_assert!(c.replace.start <= caret && caret <= c.replace.end && c.replace.end <= source.len());
            prop_assert!(source.is_char_boundary(c.replace.start) && source.is_char_boundary(c.replace.end));
            prop_assert!(!c.items.is_empty() && c.items.len() <= MAX_SUGGESTIONS_FOR_TEST);
        }
        for d in lint(&source, Knowledge { fields: Some(&fields), shared: &[] }) {
            if let Some(at) = d.at {
                prop_assert!(at.bytes.end <= source.len() && source.is_char_boundary(at.bytes.start) && source.is_char_boundary(at.bytes.end));
            }
        }
    }
}

const MAX_SUGGESTIONS_FOR_TEST: usize = complete::MAX_SUGGESTIONS;
