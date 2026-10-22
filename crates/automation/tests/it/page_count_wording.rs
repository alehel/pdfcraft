//! Out-of-range errors give the count in the right number: a one-page document "has 1 page", not
//! "1 pages", and a three-page one still "has 3 pages".

use pdfcraft_automation::{Automation, Content, ToolError};
use pdfcraft_engine::{Edit, Session};
use serde_json::{Value, json};

/// A PDF with `n` pages: one line of text on the first, the rest blank.
fn pdf(n: usize) -> Vec<u8> {
    let mut session = Session::new();
    let first = session.create_from_text("Pages", "Page one").unwrap();
    let id = session.open("pages.pdf", None, first, None).unwrap();
    for at in 1..n {
        session.apply(id, Edit::InsertBlankPage { at, width: 612.0, height: 792.0 }).unwrap();
    }
    session.save_bytes(id).unwrap().as_ref().clone()
}

/// The message of a call that must fail.
fn refusal(a: &mut Automation, tool: &str, args: &Value) -> String {
    match a.call(tool, args) {
        Err(ToolError::InvalidArgs(m) | ToolError::Failed(m)) => m,
        Err(e) => panic!("{tool} {args}: {e:?}"),
        Ok(_) => panic!("{tool} {args}: expected an error"),
    }
}

#[test]
fn out_of_range_errors_say_1_page_and_3_pages() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.pdf"), pdf(1)).unwrap();
    std::fs::write(dir.path().join("three.pdf"), pdf(3)).unwrap();
    let mut a = Automation::new().with_root(dir.path()).unwrap();
    let mut open = |path: &str| match a.call("doc_open", &json!({ "path": path })).unwrap().remove(0) {
        Content::Json(v) => v["doc"].as_u64().unwrap(),
        other => panic!("doc_open: {other:?}"),
    };
    let (one, three) = (open("one.pdf"), open("three.pdf"));

    // Every wrong message, so a failure names all the places at once.
    let mut wrong = Vec::new();
    let mut expect = |what: String, got: String, want: String| {
        if got != want {
            wrong.push(format!("{what}: got {got:?}, want {want:?}"));
        }
    };
    // One call per validator: `page`, `pages`, an insertion position, `start_page` and link_open's own.
    for (doc, past, has) in [(one, 2, "1 page"), (three, 4, "3 pages")] {
        let cases = [
            ("page_render", json!({ "doc": doc, "page": past }), format!("page {past} is out of range: the document has {has}")),
            ("text_extract", json!({ "doc": doc, "pages": [past] }), format!("page {past} is out of range: the document has {has}")),
            ("page_move", json!({ "doc": doc, "pages": [1], "to": past + 1 }), format!("to must be between 1 and {past} (the document has {has})")),
            (
                "text_find",
                json!({ "doc": doc, "query": "Page", "start_page": past }),
                format!("start_page {past} is out of range: the document has {has}"),
            ),
            ("link_open", json!({ "doc": doc, "page": past, "index": 1 }), format!("page {past} is out of range: the document has {has}")),
        ];
        for (tool, args, want) in cases {
            let got = refusal(&mut a, tool, &args);
            expect(format!("{tool} {args}"), got, want);
        }
        let got = match a.render_pixels(doc, past, 1.0) {
            Err(ToolError::InvalidArgs(m)) => m,
            other => panic!("render_pixels: {:?}", other.map(|(w, h, _)| (w, h))),
        };
        expect(format!("render_pixels page {past}"), got, format!("page {past} is out of range: the document has {has}"));
    }

    // The first page holds one line in one paragraph.
    let args = json!({ "doc": one, "page": 1, "line": 2, "text": "x" });
    expect(format!("text_edit {args}"), refusal(&mut a, "text_edit", &args), "line 2 is out of range: page 1 has 1 line".into());
    let args = json!({ "doc": one, "page": 1, "paragraph": 2 });
    expect(
        format!("text_edit_layout {args}"),
        refusal(&mut a, "text_edit_layout", &args),
        "paragraph 2 is out of range: page 1 has 1 paragraph".into(),
    );

    // Replace Pages needs as many source pages as targets.
    let args = json!({ "doc": three, "pages": [1, 2], "path": "one.pdf", "from_pages": [1] });
    let got = refusal(&mut a, "page_replace", &args);
    if !got.ends_with("1 page can't replace 2") {
        wrong.push(format!("page_replace {args}: got {got:?}, want it to end in \"1 page can't replace 2\""));
    }

    assert!(wrong.is_empty(), "{} wrong:\n{}", wrong.len(), wrong.join("\n"));
}
