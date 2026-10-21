//! Formatted reports PdfCraft makes from documents (a comment summary): a title, group headings
//! and one card per entry, set in the standard Helvetica faces with weight, colour and rules, so
//! a reader can scan it. [`from_text_escaped`](crate::from_text_escaped) remains for plain text.
//!
//! Every string is written in WinAnsiEncoding; a character Helvetica can't draw is written as
//! `[U+XXXX]` and the subtitle says so, as [`crate::from_text_escaped`] does.

use pdfcraft_cos::{Dict, Document, Object};
use pdfcraft_fonts::{Std14, literal, win_ansi, wrap_with};

use crate::{CreateError, ESCAPED_NOTE, add_page, set_title};

/// A report: what [`report`] lays out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub title: String,
    /// Grey lines under the title (what the report covers).
    pub subtitle: Vec<String>,
    pub blocks: Vec<Block>,
    /// Left-hand footer text on every page (the page number is on the right).
    pub footer: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// A group heading ("Page 3", an author, a comment type).
    Heading(String),
    /// A line of grey italic text ("No comments match").
    Note(String),
    Entry(Entry),
}

/// One card: a numbered, typed item with what it quotes, its text and its replies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub number: Option<usize>,
    /// The bold label ("Highlight").
    pub kind: String,
    /// Grey details after the label ("Ada Lin · 2026-10-21 15:28").
    pub meta: String,
    /// The stripe down the card's left edge and the tint behind the quote (RGB 0–1).
    pub color: Option<[f32; 3]>,
    /// Text the item marks, set in italics on a tint of `color`.
    pub quote: Option<String>,
    pub body: Option<String>,
    pub replies: Vec<Reply>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    /// 0 for a reply to the entry, 1 for a reply to that reply…
    pub depth: usize,
    pub author: String,
    pub meta: String,
    pub body: String,
}

impl Report {
    /// The report as plain text: the same content in reading order, replies indented four spaces
    /// a level. Tests and text exports read this.
    pub fn plain_text(&self) -> String {
        let mut out = format!("{}\n", self.title);
        for s in &self.subtitle {
            out.push_str(&format!("{s}\n"));
        }
        for b in &self.blocks {
            out.push('\n');
            match b {
                Block::Heading(h) => out.push_str(&format!("{h}\n")),
                Block::Note(n) => out.push_str(&format!("{n}\n")),
                Block::Entry(e) => {
                    out.push_str(&header_text(e));
                    out.push('\n');
                    if let Some(q) = &e.quote {
                        out.push_str(&format!("\u{201c}{q}\u{201d}\n"));
                    }
                    if let Some(b) = &e.body {
                        out.push_str(&format!("{b}\n"));
                    }
                    for r in &e.replies {
                        let indent = "    ".repeat(r.depth.saturating_add(1).min(MAX_DEPTH));
                        out.push_str(&format!("{indent}{}  {}\n", r.author, r.meta));
                        for line in r.body.lines() {
                            out.push_str(&format!("{indent}{line}\n"));
                        }
                    }
                }
            }
        }
        out
    }
}

fn header_text(e: &Entry) -> String {
    let mut s = String::new();
    if let Some(n) = e.number {
        s.push_str(&format!("#{n}  "));
    }
    s.push_str(&e.kind);
    if !e.meta.is_empty() {
        s.push_str(&format!("  {}", e.meta));
    }
    s
}

/// Replies deeper than this are drawn at this depth (the page has to keep some width).
const MAX_DEPTH: usize = 6;

const INK: [f32; 3] = [0.11, 0.12, 0.15];
const BODY: [f32; 3] = [0.2, 0.22, 0.26];
const MUTED: [f32; 3] = [0.42, 0.45, 0.5];
const RULE: [f32; 3] = [0.84, 0.85, 0.88];
const CARD: [f32; 3] = [0.968, 0.972, 0.98];
const STRIPE: [f32; 3] = [0.72, 0.74, 0.78];
const THREAD: [f32; 3] = [0.8, 0.82, 0.86];

#[derive(Clone, Copy, PartialEq)]
enum Face {
    Regular,
    Bold,
    Oblique,
}

impl Face {
    fn std14(self) -> Std14 {
        match self {
            Face::Regular => Std14::Helvetica,
            Face::Bold => Std14::HelveticaBold,
            Face::Oblique => Std14::HelveticaOblique,
        }
    }
    fn resource(self) -> &'static str {
        match self {
            Face::Regular => "F1",
            Face::Bold => "F2",
            Face::Oblique => "F3",
        }
    }
    fn width(self, s: &str, size: f64) -> f64 {
        self.std14().text_width(s, size)
    }
}

/// Something drawn, with `y` measured down from the top of the row it belongs to.
#[derive(Clone)]
enum Op {
    Text { face: Face, size: f64, color: [f32; 3], x: f64, y: f64, text: String },
    Fill { color: [f32; 3], x: f64, y: f64, w: f64, h: f64 },
}

/// A strip of the page that is never split across pages.
#[derive(Clone, Default)]
struct Row {
    h: f64,
    ops: Vec<Op>,
}

impl Row {
    fn text(face: Face, size: f64, color: [f32; 3], x: f64, text: &str, leading: f64) -> Row {
        // The baseline sits so that the cap height is centred in the row.
        let y = leading / 2.0 + size * 0.35;
        Row { h: leading, ops: vec![Op::Text { face, size, color, x, y, text: text.to_string() }] }
    }
    fn gap(h: f64) -> Row {
        Row { h, ops: Vec::new() }
    }
}

/// Pages being laid out: what each holds, behind (fills) and in front (text).
struct Pages {
    height: f64,
    top: f64,
    bottom: f64,
    pages: Vec<(Vec<u8>, Vec<u8>)>,
    /// Distance down from the top edge of the current page.
    y: f64,
}

impl Pages {
    fn new(height: f64) -> Pages {
        let mut p = Pages { height, top: 54.0, bottom: 60.0, pages: Vec::new(), y: 0.0 };
        p.new_page();
        p
    }
    fn new_page(&mut self) {
        self.pages.push((Vec::new(), Vec::new()));
        self.y = self.top;
    }
    fn room(&self) -> f64 {
        self.height - self.bottom - self.y
    }
    fn body_height(&self) -> f64 {
        self.height - self.bottom - self.top
    }
    fn at_top(&self) -> bool {
        (self.y - self.top).abs() < 0.01
    }
    /// Draw `op` with its row's top at `row_top` (down from the page top).
    fn draw(&mut self, op: &Op, row_top: f64, dx: f64) {
        let height = self.height;
        let Some((back, front)) = self.pages.last_mut() else { return };
        match op {
            Op::Text { face, size, color, x, y, text } => {
                let [r, g, b] = color.map(f64::from);
                front.extend(
                    format!("BT /{} {size:.2} Tf {r:.3} {g:.3} {b:.3} rg {:.2} {:.2} Td ", face.resource(), dx + x, height - row_top - y)
                        .into_bytes(),
                );
                front.extend(literal(&win_ansi(text)));
                front.extend_from_slice(b" Tj ET\n");
            }
            Op::Fill { color, x, y, w, h } => {
                let [r, g, b] = color.map(f64::from);
                back.extend(format!("{r:.3} {g:.3} {b:.3} rg {:.2} {:.2} {w:.2} {h:.2} re f\n", dx + x, height - row_top - y - h).into_bytes());
            }
        }
    }
    /// Place a row at the current position (the caller has made room).
    fn place(&mut self, row: &Row, dx: f64) {
        let top = self.y;
        for op in &row.ops {
            self.draw(op, top, dx);
        }
        self.y += row.h;
    }
}

/// `color`, finite and within 0–1, or `fallback`.
fn clean(color: Option<[f32; 3]>, fallback: [f32; 3]) -> [f32; 3] {
    match color {
        Some(c) if c.iter().all(|v| v.is_finite()) => c.map(|v| v.clamp(0.0, 1.0)),
        _ => fallback,
    }
}

/// `c` mixed towards white, keeping `keep` of it.
fn tint(c: [f32; 3], keep: f32) -> [f32; 3] {
    c.map(|v| 1.0 - (1.0 - v) * keep)
}

/// Each character Helvetica can't draw as `[U+XXXX]`, counting them.
fn escape(s: &str, count: &mut usize) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\n' || pdfcraft_fonts::first_undrawable(c.encode_utf8(&mut [0; 4])).is_none() {
            out.push(c);
        } else {
            *count = count.saturating_add(1);
            out.push_str(&format!("[U+{:04X}]", u32::from(c)));
        }
    }
    out
}

fn wrapped(face: Face, size: f64, width: f64, text: &str) -> Vec<String> {
    wrap_with(&text.replace("\r\n", "\n").replace('\t', "    "), size, width.max(size * 2.0), |s, z| face.width(s, z))
}

/// The rows of one entry's card, inside its padding, `inner` points wide.
fn entry_rows(e: &Entry, inner: f64) -> Vec<Row> {
    let mut rows = Vec::new();
    // Header: "#2" in grey, the kind in bold, then the details in grey; the details move to lines
    // of their own when the header would run past the card.
    let (num, kind_size, meta_size, lead) = (9.5, 10.5, 9.5, 16.0);
    let number = e.number.map(|n| format!("#{n}")).unwrap_or_default();
    let mut x = 0.0;
    let mut header = Row::text(Face::Bold, num, MUTED, 0.0, &number, lead);
    if !number.is_empty() {
        x += Face::Bold.width(&number, num) + 6.0;
    }
    let base = lead / 2.0 + kind_size * 0.35;
    header.ops.push(Op::Text { face: Face::Bold, size: kind_size, color: INK, x, y: base, text: e.kind.clone() });
    x += Face::Bold.width(&e.kind, kind_size) + 8.0;
    let meta_fits = x + Face::Regular.width(&e.meta, meta_size) <= inner;
    if meta_fits && !e.meta.is_empty() {
        header.ops.push(Op::Text { face: Face::Regular, size: meta_size, color: MUTED, x, y: base, text: e.meta.clone() });
    }
    rows.push(header);
    if !meta_fits {
        for l in wrapped(Face::Regular, meta_size, inner, &e.meta) {
            rows.push(Row::text(Face::Regular, meta_size, MUTED, 0.0, &l, 13.0));
        }
    }
    let color = clean(e.color, [1.0, 0.85, 0.2]);
    if let Some(q) = e.quote.as_deref().filter(|q| !q.is_empty()) {
        rows.push(Row::gap(4.0));
        let (size, lead, pad) = (10.5, 15.0, 6.0);
        let lines = wrapped(Face::Oblique, size, inner - 2.0 * pad, &format!("\u{201c}{q}\u{201d}"));
        let n = lines.len();
        for (i, l) in lines.into_iter().enumerate() {
            let mut row = Row::text(Face::Oblique, size, BODY, pad, &l, lead);
            // The tint runs under the whole quote, with a little air above and below it.
            let (top, extra) = (if i == 0 { -2.0 } else { 0.0 }, if i + 1 == n { 2.0 } else { 0.0 });
            row.ops.insert(0, Op::Fill { color: tint(color, 0.25), x: 0.0, y: top, w: inner, h: lead - top + extra });
            rows.push(row);
        }
        rows.push(Row::gap(2.0));
    }
    if let Some(b) = e.body.as_deref().filter(|b| !b.is_empty()) {
        rows.push(Row::gap(4.0));
        for l in wrapped(Face::Regular, 10.5, inner, b) {
            rows.push(Row::text(Face::Regular, 10.5, INK, 0.0, &l, 14.5));
        }
    }
    for r in &e.replies {
        let indent = 14.0 * r.depth.saturating_add(1).min(MAX_DEPTH) as f64;
        let width = inner - indent;
        let mut reply = vec![Row::gap(6.0)];
        let mut head = Row::text(Face::Bold, 9.5, INK, indent + 8.0, &r.author, 14.0);
        let meta_x = indent + 8.0 + Face::Bold.width(&r.author, 9.5) + 6.0;
        head.ops.push(Op::Text { face: Face::Regular, size: 9.0, color: MUTED, x: meta_x, y: 7.0 + 9.0 * 0.35, text: r.meta.clone() });
        reply.push(head);
        for l in wrapped(Face::Regular, 10.0, width - 8.0, &r.body) {
            reply.push(Row::text(Face::Regular, 10.0, BODY, indent + 8.0, &l, 13.5));
        }
        // A thread line down the reply's left edge.
        for row in reply.iter_mut().skip(1) {
            let h = row.h;
            row.ops.insert(0, Op::Fill { color: THREAD, x: indent, y: 0.0, w: 1.5, h });
        }
        rows.extend(reply);
    }
    rows
}

/// Lay out `r` on pages of `page` size. Returns the document and how many characters were written
/// as `[U+XXXX]`.
pub fn report(r: &Report, page: (f64, f64)) -> Result<(Document, usize), CreateError> {
    let (w, h) = page;
    if !(w.is_finite() && h.is_finite() && (144.0..=crate::MAX_SIDE).contains(&w) && (144.0..=crate::MAX_SIDE).contains(&h)) {
        return Err(CreateError::Invalid(format!("a report page of {w} × {h} points")));
    }
    // Escape everything first, so the subtitle can say whether anything was.
    let mut escaped = 0usize;
    let mut esc = |s: &str| escape(s, &mut escaped);
    let title = esc(&r.title);
    let mut subtitle: Vec<String> = r.subtitle.iter().map(|s| esc(s)).collect();
    let footer = esc(&r.footer);
    let blocks: Vec<Block> = r
        .blocks
        .iter()
        .map(|b| match b {
            Block::Heading(s) => Block::Heading(esc(s)),
            Block::Note(s) => Block::Note(esc(s)),
            Block::Entry(e) => Block::Entry(Entry {
                number: e.number,
                kind: esc(&e.kind),
                meta: esc(&e.meta),
                color: e.color,
                quote: e.quote.as_deref().map(&mut esc),
                body: e.body.as_deref().map(&mut esc),
                replies: e.replies.iter().map(|x| Reply { depth: x.depth, author: esc(&x.author), meta: esc(&x.meta), body: esc(&x.body) }).collect(),
            }),
        })
        .collect();
    if escaped > 0 {
        subtitle.push(ESCAPED_NOTE.to_string());
    }

    let margin = 54.0;
    let width = w - 2.0 * margin;
    let mut p = Pages::new(h);
    for l in wrapped(Face::Bold, 20.0, width, &title) {
        p.place(&Row::text(Face::Bold, 20.0, INK, 0.0, &l, 26.0), margin);
    }
    p.place(&Row::gap(2.0), margin);
    for s in &subtitle {
        for l in wrapped(Face::Regular, 10.0, width, s) {
            p.place(&Row::text(Face::Regular, 10.0, MUTED, 0.0, &l, 14.0), margin);
        }
    }
    p.place(&Row { h: 14.0, ops: vec![Op::Fill { color: RULE, x: 0.0, y: 12.0, w: width, h: 0.75 }] }, margin);

    let (stripe, pad_l, pad_r, pad_v) = (3.0, 14.0, 12.0, 10.0);
    let inner = width - stripe - pad_l - pad_r;
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Heading(text) => {
                let lines = wrapped(Face::Bold, 12.5, width, text);
                let needed = 14.0 + lines.len() as f64 * 18.0 + 8.0 + 60.0;
                if p.room() < needed && !p.at_top() {
                    p.new_page();
                }
                if !p.at_top() && i > 0 {
                    p.place(&Row::gap(10.0), margin);
                }
                for l in lines {
                    p.place(&Row::text(Face::Bold, 12.5, INK, 0.0, &l, 18.0), margin);
                }
                p.place(&Row { h: 9.0, ops: vec![Op::Fill { color: RULE, x: 0.0, y: 2.0, w: width, h: 0.75 }] }, margin);
            }
            Block::Note(text) => {
                for l in wrapped(Face::Oblique, 10.5, width, text) {
                    if p.room() < 15.0 {
                        p.new_page();
                    }
                    p.place(&Row::text(Face::Oblique, 10.5, MUTED, 0.0, &l, 15.0), margin);
                }
            }
            Block::Entry(e) => {
                let rows = entry_rows(e, inner);
                let total: f64 = rows.iter().map(|r| r.h).sum::<f64>() + 2.0 * pad_v;
                // A card that fits on a page is not split; a longer one is, row by row.
                if total > p.room() && total <= p.body_height() {
                    p.new_page();
                }
                let stripe_color = clean(e.color, STRIPE);
                let mut rows = rows.into_iter().peekable();
                while rows.peek().is_some() {
                    if p.room() < pad_v + 20.0 + pad_v {
                        p.new_page();
                    }
                    // One segment of the card: as many rows as fit on this page (at least one).
                    // Its background goes down first, so the quote's tint shows on top of it.
                    let mut segment = Vec::new();
                    let mut used = 2.0 * pad_v;
                    while let Some(row) = rows.peek() {
                        if !segment.is_empty() && used + row.h > p.room() {
                            break;
                        }
                        used += row.h;
                        segment.push(rows.next().unwrap_or_default());
                    }
                    let top = p.y;
                    p.draw(&Op::Fill { color: CARD, x: 0.0, y: 0.0, w: width, h: used }, top, margin);
                    p.draw(&Op::Fill { color: stripe_color, x: 0.0, y: 0.0, w: stripe, h: used }, top, margin);
                    p.y += pad_v;
                    for row in &segment {
                        p.place(row, margin + stripe + pad_l);
                    }
                    p.y += pad_v;
                    if rows.peek().is_some() {
                        p.new_page();
                    }
                }
                p.place(&Row::gap(10.0), margin);
            }
        }
    }

    let mut doc = Document::new_empty();
    let mut fonts = Dict::new();
    for face in [Face::Regular, Face::Bold, Face::Oblique] {
        let mut font = Dict::new();
        font.set(b"Type".to_vec(), Object::name("Font"));
        font.set(b"Subtype".to_vec(), Object::name("Type1"));
        font.set(b"BaseFont".to_vec(), Object::name(face.std14().base_font()));
        font.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
        let r = doc.add(font);
        fonts.set(face.resource().as_bytes().to_vec(), Object::Ref(r));
    }
    let count = p.pages.len();
    let footer_size = 8.0;
    for (n, (back, front)) in p.pages.into_iter().enumerate() {
        let mut c = back;
        c.extend(front);
        let [r, g, b] = MUTED.map(f64::from);
        let label = format!("Page {} of {count}", n + 1);
        let right = w - margin - Face::Regular.width(&label, footer_size);
        let footer_text = wrapped(Face::Regular, footer_size, (right - margin - 12.0).max(36.0), &footer).into_iter().next().unwrap_or_default();
        for (x, text) in [(margin, footer_text.as_str()), (right, label.as_str())] {
            c.extend(format!("BT /F1 {footer_size} Tf {r:.3} {g:.3} {b:.3} rg {x:.2} 32 Td ").into_bytes());
            c.extend(literal(&win_ansi(text)));
            c.extend_from_slice(b" Tj ET\n");
        }
        let [r, g, b] = RULE.map(f64::from);
        c.extend(format!("{r:.3} {g:.3} {b:.3} rg {margin:.2} 44 {width:.2} 0.6 re f\n").into_bytes());
        let mut res = Dict::new();
        res.set(b"Font".to_vec(), Object::Dict(fonts.clone()));
        add_page(&mut doc, w, h, res, Some(c))?;
    }
    set_title(&mut doc, &r.title);
    Ok((doc, escaped))
}
