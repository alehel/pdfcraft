//! Scoring a converted Word file against its source, through pandoc's reading of both.
//!
//! pandoc is the reader because it is independent of us and of LibreOffice, and what it sees is
//! roughly what any program consuming a `.docx` sees: Word's heading styles, real list numbering,
//! tables, emphasis and footnotes. It does not see layout (alignment, fonts, sizes, positions),
//! which the pixel comparison in the parent module covers instead. Nor does it read text boxes,
//! so the words and their order come from the two PDFs instead (`pdftotext`), and text we put in
//! a positioned box scores as present there but as missing structure here — which it is, since a
//! box is not part of the document's flow.
//!
//! Every structural score is an F1 over a multiset of keys taken from each side, so extra
//! structure in our output costs precision just as missing structure costs recall. A kind the
//! source does not have and the output does not invent scores `None` (not applicable), so a
//! fixture without tables does not get a free 1.000 for them.

use std::collections::BTreeMap;

use serde_json::Value;

/// Nesting deeper than this in pandoc's JSON is not followed (pandoc's own output for our
/// fixtures nests a handful of levels; the cap keeps a hostile file from recursing without end).
const MAX_DEPTH: usize = 64;

/// What one document contains, as pandoc reads it.
#[derive(Debug, Default, PartialEq)]
pub struct Outline {
    /// `level:text` of every heading.
    pub headings: Vec<String>,
    /// The text of every body paragraph outside lists and tables.
    pub paragraphs: Vec<String>,
    /// `kind:depth:text` of every list item, kind `b` (bulleted) or `n` (numbered).
    pub items: Vec<String>,
    /// `col:colspan×rowspan:text` of every table cell; `col` counts the spans before it. The row
    /// is left out on purpose: with it, one spurious row (a paragraph read into the table) shifts
    /// every row after it and scores the whole table 0, hiding how much of it survived. Rows that
    /// merge still score low, because their cells' text changes.
    pub cells: Vec<String>,
    /// Words set in bold, in italic, and underlined, outside headings (a heading style is bold
    /// already, so whether its runs say so again changes nothing a reader sees).
    pub bold: Vec<String>,
    pub italic: Vec<String>,
    pub underline: Vec<String>,
    /// The text of every footnote.
    pub notes: Vec<String>,
    /// How many pictures there are.
    pub images: usize,
}

#[derive(Clone, Copy, Default)]
struct Marks {
    bold: bool,
    italic: bool,
    underline: bool,
}

/// Where a run of blocks sits: inside a list item (its kind and depth) or a table cell.
#[derive(Clone, Copy)]
enum Place {
    Body,
    Item(char, usize),
    Cell,
    Heading,
    Note,
}

impl Outline {
    /// Read pandoc's JSON (`pandoc -t json`).
    pub fn from_pandoc(json: &Value) -> Outline {
        let mut o = Outline::default();
        if let Some(blocks) = json.get("blocks").and_then(Value::as_array) {
            o.blocks(blocks, Place::Body, 0);
        }
        o
    }

    fn blocks(&mut self, blocks: &[Value], place: Place, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for b in blocks {
            self.block(b, place, depth);
        }
    }

    fn block(&mut self, b: &Value, place: Place, depth: usize) {
        let c = b.get("c");
        let at = |i: usize| c.and_then(|c| c.get(i));
        let kind = b.get("t").and_then(Value::as_str).unwrap_or("");
        match kind {
            "Para" | "Plain" => {
                let text = self.text_of(c, place, depth);
                if text.is_empty() {
                    return;
                }
                match place {
                    Place::Body => self.paragraphs.push(text),
                    Place::Item(kind, d) => self.items.push(format!("{kind}:{d}:{text}")),
                    Place::Cell | Place::Heading | Place::Note => {}
                }
            }
            "Header" => {
                let level = at(0).and_then(Value::as_u64).unwrap_or(0);
                let text = self.text_of(at(2), Place::Heading, depth);
                self.headings.push(format!("{level}:{text}"));
            }
            "BulletList" => self.list(c, 'b', place, depth),
            "OrderedList" => self.list(at(1), 'n', place, depth),
            "Table" => self.table(b, depth),
            "BlockQuote" => self.blocks(arr(c), place, depth + 1),
            // A figure's blocks follow its attributes and caption; a div's follow its attributes.
            "Figure" => self.blocks(arr(at(2)), place, depth + 1),
            "Div" => self.blocks(arr(at(1)), place, depth + 1),
            "LineBlock" => {
                for line in arr(c) {
                    let text = self.text_of(Some(line), place, depth);
                    if !text.is_empty() && matches!(place, Place::Body) {
                        self.paragraphs.push(text);
                    }
                }
            }
            _ => {}
        }
    }

    fn list(&mut self, items: Option<&Value>, kind: char, place: Place, depth: usize) {
        let level = match place {
            Place::Item(_, d) => d.saturating_add(1),
            _ => 0,
        };
        for item in arr(items) {
            self.blocks(arr(Some(item)), Place::Item(kind, level), depth + 1);
        }
    }

    /// pandoc 3 tables: `[attr, caption, colspecs, head, bodies, foot]`, each part holding rows
    /// of `[attr, cells]` and each cell `[attr, align, rowspan, colspan, blocks]`.
    fn table(&mut self, b: &Value, depth: usize) {
        let Some(c) = b.get("c") else { return };
        let mut rows: Vec<&Value> = Vec::new();
        let part_rows = |part: Option<&Value>, i: usize| part.and_then(|p| p.get(i)).and_then(Value::as_array).cloned().unwrap_or_default();
        let head = part_rows(c.get(3), 1);
        let mut bodies = Vec::new();
        for body in c.get(4).and_then(Value::as_array).into_iter().flatten() {
            bodies.extend(part_rows(Some(body), 2));
            bodies.extend(part_rows(Some(body), 3));
        }
        let foot = part_rows(c.get(5), 1);
        rows.extend(head.iter());
        rows.extend(bodies.iter());
        rows.extend(foot.iter());
        for row in &rows {
            let mut col = 0usize;
            for cell in row.get(1).and_then(Value::as_array).into_iter().flatten() {
                let rowspan = cell.get(2).and_then(Value::as_u64).unwrap_or(1);
                let colspan = cell.get(3).and_then(Value::as_u64).unwrap_or(1);
                let mut texts = Vec::new();
                for blk in arr(cell.get(4)) {
                    texts.push(self.text_of(blk.get("c"), Place::Cell, depth + 1));
                }
                let text = texts.into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
                self.cells.push(format!("{col}:{colspan}×{rowspan}:{text}"));
                col = col.saturating_add(usize::try_from(colspan).unwrap_or(1));
            }
        }
    }

    /// The plain text of an inline list, recording its words and marks on the way.
    fn text_of(&mut self, inlines: Option<&Value>, place: Place, depth: usize) -> String {
        let mut text = String::new();
        if let Some(list) = inlines.and_then(Value::as_array) {
            self.inlines(list, Marks::default(), place, depth, &mut text);
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn inlines(&mut self, list: &[Value], marks: Marks, place: Place, depth: usize, text: &mut String) {
        if depth > MAX_DEPTH {
            return;
        }
        for i in list {
            let c = i.get("c");
            match i.get("t").and_then(Value::as_str).unwrap_or("") {
                "Str" => {
                    let s = c.and_then(Value::as_str).unwrap_or("");
                    text.push_str(s);
                    if !matches!(place, Place::Note | Place::Heading) {
                        let words = crate::gates::word_sequence(s);
                        for (on, into) in [(marks.bold, &mut self.bold), (marks.italic, &mut self.italic), (marks.underline, &mut self.underline)] {
                            if on {
                                into.extend(words.iter().cloned());
                            }
                        }
                    }
                }
                "Space" | "SoftBreak" | "LineBreak" => text.push(' '),
                "Strong" => self.inlines(arr(c), Marks { bold: true, ..marks }, place, depth + 1, text),
                "Emph" => self.inlines(arr(c), Marks { italic: true, ..marks }, place, depth + 1, text),
                "Underline" => self.inlines(arr(c), Marks { underline: true, ..marks }, place, depth + 1, text),
                "Strikeout" | "Superscript" | "Subscript" | "SmallCaps" => self.inlines(arr(c), marks, place, depth + 1, text),
                "Span" | "Quoted" | "Cite" => self.inlines(arr(c.and_then(|c| c.get(1))), marks, place, depth + 1, text),
                "Link" => self.inlines(arr(c.and_then(|c| c.get(1))), marks, place, depth + 1, text),
                "Code" => text.push_str(c.and_then(|c| c.get(1)).and_then(Value::as_str).unwrap_or("")),
                "Image" => self.images = self.images.saturating_add(1),
                "Note" => {
                    let mut note = Outline::default();
                    let mut parts = Vec::new();
                    for blk in arr(c) {
                        parts.push(note.text_of(blk.get("c"), Place::Note, depth + 1));
                    }
                    self.notes.push(parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" "));
                }
                _ => {}
            }
        }
    }
}

/// The array `v` holds, or nothing.
fn arr(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// How far a converted document agrees with its source. Every score is in `0.0..=1.0`, higher
/// is better; `None` where neither side has anything of that kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Scores {
    pub text: Option<f64>,
    pub order: Option<f64>,
    pub headings: Option<f64>,
    pub paragraphs: Option<f64>,
    pub lists: Option<f64>,
    pub tables: Option<f64>,
    pub bold: Option<f64>,
    pub italic: Option<f64>,
    pub underline: Option<f64>,
    pub notes: Option<f64>,
    pub images: Option<f64>,
}

impl Scores {
    /// Column headings for the report, in the order [`Scores::values`] returns them.
    pub const NAMES: [&'static str; 11] = ["text", "order", "head", "para", "list", "table", "bold", "ital", "undl", "note", "img"];

    /// Score `ours` against `source`: their outlines, and the words of each PDF in reading order.
    pub fn compare(source: &Outline, ours: &Outline, source_words: &[String], our_words: &[String]) -> Scores {
        let order = if source_words.is_empty() && our_words.is_empty() { None } else { Some(crate::gates::order_score(our_words, source_words).0) };
        let images = match (source.images, ours.images) {
            (0, 0) => None,
            (a, b) => Some(a.min(b) as f64 / a.max(b) as f64),
        };
        Scores {
            text: f1(source_words, our_words),
            order,
            headings: f1(&source.headings, &ours.headings),
            paragraphs: f1(&source.paragraphs, &ours.paragraphs),
            lists: f1(&source.items, &ours.items),
            tables: f1(&source.cells, &ours.cells),
            bold: f1(&source.bold, &ours.bold),
            italic: f1(&source.italic, &ours.italic),
            underline: f1(&source.underline, &ours.underline),
            notes: f1(&source.notes, &ours.notes),
            images,
        }
    }

    pub fn values(&self) -> [Option<f64>; 11] {
        [
            self.text,
            self.order,
            self.headings,
            self.paragraphs,
            self.lists,
            self.tables,
            self.bold,
            self.italic,
            self.underline,
            self.notes,
            self.images,
        ]
    }

    /// The mean of the structural scores that apply (everything but text and order, which say
    /// little about structure), or `None` when none applies.
    pub fn structure(&self) -> Option<f64> {
        let v: Vec<f64> = self.values().into_iter().skip(2).flatten().collect();
        if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) }
    }
}

/// F1 of two multisets; `None` when both are empty.
fn f1(source: &[String], ours: &[String]) -> Option<f64> {
    if source.is_empty() && ours.is_empty() {
        return None;
    }
    let count = |v: &[String]| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for k in v {
            let n = m.entry(k.clone()).or_insert(0);
            *n = n.saturating_add(1);
        }
        m
    };
    Some(crate::gates::f1(&count(ours), &count(source)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(t: &str) -> Value {
        json!({ "t": "Str", "c": t })
    }
    fn sp() -> Value {
        json!({ "t": "Space" })
    }

    /// A small document with every kind the outline records.
    fn sample() -> Value {
        let cell = |rs: u64, cs: u64, t: &str| json!([["", [], []], { "t": "AlignDefault" }, rs, cs, [{ "t": "Plain", "c": [s(t)] }]]);
        json!({ "blocks": [
            { "t": "Header", "c": [1, ["id", [], []], [{ "t": "Strong", "c": [s("Big")] }, sp(), s("title")]] },
            { "t": "Para", "c": [s("Plain"), sp(), { "t": "Strong", "c": [s("bold")] }, sp(), { "t": "Emph", "c": [s("tilt")] },
                { "t": "Note", "c": [{ "t": "Para", "c": [s("footnote"), sp(), s("text")] }] }] },
            { "t": "BulletList", "c": [
                [{ "t": "Plain", "c": [s("one")] }, { "t": "OrderedList", "c": [[1, { "t": "Decimal" }, { "t": "Period" }], [[{ "t": "Plain", "c": [s("inner")] }]]] }],
                [{ "t": "Plain", "c": [s("two")] }]
            ] },
            { "t": "Table", "c": [["", [], []], [null, []], [], [["", [], []], [[["", [], []], [cell(1, 2, "Head")]]]],
                [[["", [], []], 0, [], [[["", [], []], [cell(1, 1, "a"), cell(1, 1, "b")]]]]], [["", [], []], []]] },
            { "t": "Para", "c": [{ "t": "Image", "c": [["", [], []], [], ["x.png", ""]] }] }
        ] })
    }

    #[test]
    fn the_outline_records_every_kind() {
        let o = Outline::from_pandoc(&sample());
        assert_eq!(o.headings, ["1:Big title"]);
        assert_eq!(o.bold, ["bold"], "a heading's runs are not counted as bold text");
        assert_eq!(o.paragraphs, ["Plain bold tilt"]);
        assert_eq!(o.items, ["b:0:one", "n:1:inner", "b:0:two"]);
        assert_eq!(o.cells, ["0:2×1:Head", "0:1×1:a", "1:1×1:b"]);
        assert_eq!(o.italic, ["tilt"]);
        assert_eq!(o.notes, ["footnote text"]);
        assert_eq!(o.images, 1);
    }

    #[test]
    fn a_document_scores_perfectly_against_itself() {
        let o = Outline::from_pandoc(&sample());
        let words = crate::gates::word_sequence("Big title Plain bold tilt");
        let scores = Scores::compare(&o, &o, &words, &words);
        assert!(scores.values().iter().flatten().all(|v| (*v - 1.0).abs() < 1e-9), "{scores:?}");
        assert_eq!(scores.underline, None, "neither side has underlining");
        assert_eq!(scores.structure(), Some(1.0));
    }

    #[test]
    fn flattened_structure_scores_low_but_keeps_its_text() {
        let source = Outline::from_pandoc(&sample());
        // What a converter that loses all structure produces: every line a plain paragraph.
        let flat: Vec<Value> = ["Big title", "Plain bold tilt", "one", "inner", "two", "Head", "a b"]
            .iter()
            .map(|line| json!({ "t": "Para", "c": line.split(' ').flat_map(|w| [s(w), sp()]).collect::<Vec<_>>() }))
            .collect();
        let ours = Outline::from_pandoc(&json!({ "blocks": flat }));
        let words = crate::gates::word_sequence("Big title");
        let scores = Scores::compare(&source, &ours, &words, &words);
        assert_eq!(scores.text, Some(1.0), "same words");
        assert!(scores.paragraphs.is_some_and(|p| (p - 0.25).abs() < 1e-9), "one of seven lines was a paragraph: {scores:?}");
        assert_eq!(scores.headings, Some(0.0));
        assert_eq!(scores.lists, Some(0.0));
        assert_eq!(scores.tables, Some(0.0));
        assert_eq!(scores.bold, Some(0.0));
        assert_eq!(scores.images, Some(0.0));
        assert!(scores.structure().is_some_and(|s| s < 0.3), "{scores:?}");
    }

    #[test]
    fn invented_structure_costs_precision() {
        let source = Outline { headings: vec!["1:Title".into()], ..Outline::default() };
        let ours = Outline { headings: vec!["1:Title".into(), "2:Not a heading".into()], ..Outline::default() };
        let h = Scores::compare(&source, &ours, &[], &[]).headings.unwrap_or(0.0);
        assert!((h - 2.0 / 3.0).abs() < 1e-9, "{h}");
    }

    #[test]
    fn malformed_and_deep_json_is_read_without_panicking() {
        for v in [json!(null), json!({ "blocks": 3 }), json!({ "blocks": [{ "t": "Table", "c": "x" }, { "t": "Header" }, 7] })] {
            let _ = Outline::from_pandoc(&v);
        }
        let mut deep = json!([s("x")]);
        for _ in 0..500 {
            deep = json!([{ "t": "Strong", "c": deep }]);
        }
        let o = Outline::from_pandoc(&json!({ "blocks": [{ "t": "Para", "c": deep }] }));
        assert!(o.paragraphs.is_empty() && o.bold.is_empty(), "text past the depth cap is not followed");
    }
}
