//! Formatted reports PdfCraft makes from documents (a comment summary): a title, group headings
//! and one card per entry, laid out in the standard fonts with the weights, colours, rules and
//! spacing a [`ReportStyle`] gives them (a built-in theme or a user's stylesheet, see
//! [`crate::report_style`]). [`from_text_escaped`](crate::from_text_escaped) remains for plain text.
//!
//! Every string is written in WinAnsiEncoding; a character Helvetica can't draw is written as
//! `[U+XXXX]` and the subtitle says so, as [`crate::from_text_escaped`] does.

use pdfcraft_cos::{Dict, Document, Object};
use pdfcraft_fonts::{Std14, literal, win_ansi, wrap_with};

use crate::report_style::{Part, PartStyle, ReportStyle};
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

/// The colour `var(--comment)` stands for when a comment has none.
const NO_COLOR: [f32; 3] = [0.72, 0.74, 0.78];

/// Something drawn, with `y` measured down from the top of the row it belongs to.
#[derive(Clone)]
enum Op {
    Text { font: Std14, size: f64, color: [f32; 3], x: f64, y: f64, text: String },
    Fill { color: [f32; 3], x: f64, y: f64, w: f64, h: f64 },
}

/// A strip of the page that is never split across pages.
#[derive(Clone, Default)]
struct Row {
    h: f64,
    ops: Vec<Op>,
}

impl Row {
    fn gap(h: f64) -> Row {
        Row { h: h.max(0.0), ops: Vec::new() }
    }
    /// A filled band (a rule), `h` high, with `above` and `below` points around it.
    fn rule(above: f64, h: f64, below: f64, color: Option<[f32; 3]>, x: f64, w: f64) -> Row {
        let (above, h, below) = (above.max(0.0), h.max(0.0), below.max(0.0));
        let ops = match color {
            Some(color) if h > 0.0 => vec![Op::Fill { color, x, y: above, w, h }],
            _ => Vec::new(),
        };
        Row { h: above + h + below, ops }
    }
}

/// One part's text as it is drawn: its font, size, colour and line spacing.
struct Ink<'a> {
    part: &'a PartStyle,
    color: [f32; 3],
}

impl<'a> Ink<'a> {
    fn new(part: &'a PartStyle, comment: [f32; 3]) -> Ink<'a> {
        Ink { part, color: part.color.resolve(comment).unwrap_or([0.0; 3]) }
    }
    fn width(&self, s: &str) -> f64 {
        self.part.font().text_width(s, self.part.size)
    }
    fn text(&self, s: &str) -> String {
        if self.part.uppercase { s.to_uppercase() } else { s.to_string() }
    }
    /// `s` wrapped to `width` points.
    fn lines(&self, width: f64, s: &str) -> Vec<String> {
        let size = self.part.size;
        let font = self.part.font();
        wrap_with(&self.text(s).replace("\r\n", "\n").replace('\t', "    "), size, width.max(size * 2.0), |t, z| font.text_width(t, z))
    }
    /// A text op on a row of this part's leading, at `x`.
    fn op(&self, x: f64, s: &str, leading: f64) -> Op {
        // The baseline sits so that the cap height is centred in the line.
        Op::Text { font: self.part.font(), size: self.part.size, color: self.color, x, y: leading / 2.0 + self.part.size * 0.35, text: s.to_string() }
    }
    fn rows(&self, x: f64, width: f64, s: &str) -> Vec<Row> {
        let lead = self.part.leading();
        self.lines(width, s).into_iter().map(|l| Row { h: lead, ops: vec![self.op(x, &l, lead)] }).collect()
    }
}

/// Pages being laid out: what each holds, behind (fills) and in front (text).
struct Pages {
    height: f64,
    top: f64,
    bottom: f64,
    pages: Vec<(Vec<u8>, Vec<u8>)>,
    fonts: Vec<Std14>,
    /// Distance down from the top edge of the current page.
    y: f64,
}

impl Pages {
    fn new(height: f64, top: f64, bottom: f64) -> Pages {
        let mut p = Pages { height, top, bottom, pages: Vec::new(), fonts: Vec::new(), y: 0.0 };
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
        if let Op::Text { font, .. } = op
            && !self.fonts.contains(font)
        {
            self.fonts.push(*font);
        }
        let Some((back, front)) = self.pages.last_mut() else { return };
        match op {
            Op::Text { font, size, color, x, y, text } => {
                let [r, g, b] = color.map(f64::from);
                front.extend(
                    format!("BT /F{} {size:.2} Tf {r:.3} {g:.3} {b:.3} rg {:.2} {:.2} Td ", *font as usize, dx + x, height - row_top - y)
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
    /// Place rows that may run onto a new page, one at a time.
    fn flow(&mut self, rows: &[Row], dx: f64) {
        for row in rows {
            if row.h > self.room() && !self.at_top() {
                self.new_page();
            }
            self.place(row, dx);
        }
    }
}

/// `color`, finite and within 0–1, or `fallback`.
fn clean(color: Option<[f32; 3]>, fallback: [f32; 3]) -> [f32; 3] {
    match color {
        Some(c) if c.iter().all(|v| v.is_finite()) => c.map(|v| v.clamp(0.0, 1.0)),
        _ => fallback,
    }
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

/// A band of rows with a background and a left border (a quote, a reply): `rows` are laid out
/// inside the part's padding and border, and the band starts `x` points in, `w` wide.
fn banded(part: &PartStyle, comment: [f32; 3], x: f64, w: f64, rows: Vec<Row>) -> Vec<Row> {
    let fill = part.background.resolve(comment);
    let (bw, bc) = (part.border_left.0, part.border_left.1.resolve(comment));
    let [pt, _, pb, _] = part.padding;
    let mut out = Vec::with_capacity(rows.len() + 2);
    let band = |mut row: Row, out: &mut Vec<Row>| {
        let h = row.h;
        let mut under = Vec::new();
        if let Some(color) = fill {
            under.push(Op::Fill { color, x, y: 0.0, w, h });
        }
        if let Some(color) = bc.filter(|_| bw > 0.0) {
            under.push(Op::Fill { color, x, y: 0.0, w: bw, h });
        }
        under.append(&mut row.ops);
        row.ops = under;
        out.push(row);
    };
    if pt > 0.0 {
        band(Row::gap(pt), &mut out);
    }
    for r in rows {
        band(r, &mut out);
    }
    if pb > 0.0 {
        band(Row::gap(pb), &mut out);
    }
    out
}

/// The rows of one entry's card, inside its padding, `inner` points wide.
fn entry_rows(e: &Entry, inner: f64, style: &ReportStyle) -> Vec<Row> {
    let comment = clean(e.color, NO_COLOR);
    let (number, kind, meta) = (style.part(Part::Number), style.part(Part::Kind), style.part(Part::Meta));
    let (ni, ki, mi) = (Ink::new(number, comment), Ink::new(kind, comment), Ink::new(meta, comment));
    let mut rows = Vec::new();
    // Header: the number, the kind and then the details on one baseline; the details move to
    // lines of their own when the header would run past the card.
    let lead = [(number, !number.hidden), (kind, !kind.hidden), (meta, !meta.hidden)]
        .iter()
        .filter(|(_, shown)| *shown)
        .map(|(p, _)| p.leading())
        .fold(0.0, f64::max);
    let big = [number, kind, meta].iter().filter(|p| !p.hidden).map(|p| p.size).fold(0.0, f64::max);
    let base = lead / 2.0 + big * 0.35;
    let mut header = Row { h: lead, ops: Vec::new() };
    let mut x = 0.0;
    let put = |ink: &Ink, text: &str, header: &mut Row, x: &mut f64, gap: f64| {
        if ink.part.hidden || text.is_empty() {
            return;
        }
        let t = ink.text(text);
        let mut op = ink.op(*x, &t, lead);
        if let Op::Text { y, .. } = &mut op {
            *y = base;
        }
        header.ops.push(op);
        *x += ink.width(&t) + gap;
    };
    let number_text = e.number.map(|n| format!("#{n}")).unwrap_or_default();
    put(&ni, &number_text, &mut header, &mut x, 6.0);
    put(&ki, &e.kind, &mut header, &mut x, 8.0);
    let meta_fits = x + mi.width(&mi.text(&e.meta)) <= inner;
    if meta_fits {
        put(&mi, &e.meta, &mut header, &mut x, 0.0);
    }
    if !header.ops.is_empty() {
        rows.push(header);
    }
    if !meta_fits && !meta.hidden {
        rows.extend(mi.rows(0.0, inner, &e.meta));
    }
    let quote = style.part(Part::Quote);
    if let Some(q) = e.quote.as_deref().filter(|q| !q.is_empty() && !quote.hidden) {
        let qi = Ink::new(quote, comment);
        let [_, mr, _, ml] = quote.margin;
        let (w, bw) = ((inner - ml - mr).max(36.0), quote.border_left.0.max(0.0));
        let text_x = ml + bw + quote.padding[3];
        let text_w = w - bw - quote.padding[3] - quote.padding[1];
        rows.push(Row::gap(quote.margin[0]));
        rows.extend(banded(quote, comment, ml, w, qi.rows(text_x, text_w, &format!("\u{201c}{q}\u{201d}"))));
        rows.push(Row::gap(quote.margin[2]));
    }
    let body = style.part(Part::Body);
    if let Some(b) = e.body.as_deref().filter(|b| !b.is_empty() && !body.hidden) {
        rows.push(Row::gap(body.margin[0]));
        rows.extend(Ink::new(body, comment).rows(body.margin[3], inner - body.margin[3] - body.margin[1], b));
        rows.push(Row::gap(body.margin[2]));
    }
    let reply = style.part(Part::Reply);
    if !reply.hidden {
        let (ra, rm, rb) = (style.part(Part::ReplyAuthor), style.part(Part::ReplyMeta), style.part(Part::ReplyBody));
        let (ai, mi, bi) = (Ink::new(ra, comment), Ink::new(rm, comment), Ink::new(rb, comment));
        for r in &e.replies {
            let indent = (reply.margin[3] * r.depth.saturating_add(1).min(MAX_DEPTH) as f64).clamp(0.0, inner / 2.0);
            let w = (inner - indent - reply.margin[1]).max(36.0);
            let text_x = indent + reply.border_left.0.max(0.0) + reply.padding[3];
            let text_w = (w - reply.border_left.0.max(0.0) - reply.padding[3] - reply.padding[1]).max(36.0);
            let mut inside = Vec::new();
            let lead = if ra.hidden { rm.leading() } else { ra.leading().max(if rm.hidden { 0.0 } else { rm.leading() }) };
            let mut head = Row { h: lead, ops: Vec::new() };
            let mut x = text_x;
            let base = lead / 2.0 + ra.size.max(rm.size) * 0.35;
            for (ink, text, gap) in [(&ai, r.author.as_str(), 6.0), (&mi, r.meta.as_str(), 0.0)] {
                if ink.part.hidden || text.is_empty() {
                    continue;
                }
                let t = ink.text(text);
                let mut op = ink.op(x, &t, lead);
                if let Op::Text { y, .. } = &mut op {
                    *y = base;
                }
                head.ops.push(op);
                x += ink.width(&t) + gap;
            }
            if !head.ops.is_empty() {
                inside.push(head);
            }
            if !rb.hidden {
                inside.extend(bi.rows(text_x, text_w, &r.body));
            }
            rows.push(Row::gap(reply.margin[0]));
            // The band starts at the indent; its rows are already placed relative to the card.
            let mut band = banded(reply, comment, indent, w, inside);
            rows.append(&mut band);
            rows.push(Row::gap(reply.margin[2]));
        }
    }
    rows
}

/// Lay out `r` on pages of `page` size (unless the style's `@page` sets one) in `style`.
/// Returns the document and how many characters were written as `[U+XXXX]`.
pub fn report(r: &Report, page: (f64, f64), style: &ReportStyle) -> Result<(Document, usize), CreateError> {
    let (w, h) = style.page_size.unwrap_or(page);
    if !(w.is_finite() && h.is_finite() && (144.0..=crate::MAX_SIDE).contains(&w) && (144.0..=crate::MAX_SIDE).contains(&h)) {
        return Err(CreateError::Invalid(format!("a report page of {w} × {h} points")));
    }
    let [mt, mr, mb, ml] = style.margin.map(|m| if m.is_finite() { m.max(0.0) } else { 54.0 });
    let width = w - ml - mr;
    if width < 108.0 || h - mt - mb < 108.0 {
        return Err(CreateError::Invalid("the page margins leave less than 1½ inches for the report".into()));
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

    let none = NO_COLOR;
    let rule = |p: &PartStyle, side: (f64, &crate::report_style::Paint)| (side.0, side.1.resolve(none).filter(|_| !p.hidden));
    let mut p = Pages::new(h, mt, mb);
    let (title_s, sub_s, header_s) = (style.part(Part::Title), style.part(Part::Subtitle), style.part(Part::Header));
    if !title_s.hidden {
        p.flow(&Ink::new(title_s, none).rows(0.0, width, &title), ml);
    }
    if !sub_s.hidden {
        for s in &subtitle {
            p.flow(&Ink::new(sub_s, none).rows(0.0, width, s), ml);
        }
    }
    if !header_s.hidden {
        let (bw, bc) = rule(header_s, (header_s.border_bottom.0, &header_s.border_bottom.1));
        p.place(&Row::rule(header_s.padding[2], bw, header_s.margin[2], bc, 0.0, width), ml);
    }

    let card = style.part(Part::Card);
    let (stripe, [pad_t, pad_r, pad_b, pad_l]) = (card.border_left.0.max(0.0), card.padding);
    let inner = (width - stripe - pad_l - pad_r).max(72.0);
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Heading(text) => {
                let hs = style.part(Part::Heading);
                if hs.hidden {
                    continue;
                }
                let mut rows = Ink::new(hs, none).rows(0.0, width, text);
                let (bw, bc) = rule(hs, (hs.border_bottom.0, &hs.border_bottom.1));
                rows.push(Row::rule(hs.padding[2], bw, hs.margin[2], bc, 0.0, width));
                // A heading stays with the start of what follows it.
                let needed = hs.margin[0] + rows.iter().map(|r| r.h).sum::<f64>() + 60.0;
                if p.room() < needed && !p.at_top() {
                    p.new_page();
                }
                if !p.at_top() && i > 0 {
                    p.place(&Row::gap(hs.margin[0]), ml);
                }
                p.flow(&rows, ml);
            }
            Block::Note(text) => {
                let ns = style.part(Part::Note);
                if !ns.hidden {
                    p.flow(&Ink::new(ns, none).rows(0.0, width, text), ml);
                }
            }
            Block::Entry(e) => {
                if card.hidden {
                    continue;
                }
                let comment = clean(e.color, NO_COLOR);
                let rows = entry_rows(e, inner, style);
                let total: f64 = rows.iter().map(|r| r.h).sum::<f64>() + pad_t + pad_b;
                // A card that fits on a page is not split; a longer one is, row by row.
                if total > p.room() && total <= p.body_height() {
                    p.new_page();
                }
                let (fill, stripe_color) = (card.background.resolve(comment), card.border_left.1.resolve(comment));
                let (bottom_w, bottom_c) = (card.border_bottom.0.max(0.0), card.border_bottom.1.resolve(comment));
                let mut rows = rows.into_iter().peekable();
                loop {
                    if p.room() < pad_t + 20.0 + pad_b && !p.at_top() {
                        p.new_page();
                    }
                    // One segment of the card: as many rows as fit on this page (at least one).
                    // Its background goes down first, so tints inside show on top of it.
                    let mut segment = Vec::new();
                    let mut used = pad_t + pad_b;
                    while let Some(row) = rows.peek() {
                        if !segment.is_empty() && used + row.h > p.room() {
                            break;
                        }
                        used += row.h;
                        segment.push(rows.next().unwrap_or_default());
                    }
                    let last = rows.peek().is_none();
                    let top = p.y;
                    if let Some(color) = fill {
                        p.draw(&Op::Fill { color, x: 0.0, y: 0.0, w: width, h: used }, top, ml);
                    }
                    if let Some(color) = stripe_color.filter(|_| stripe > 0.0) {
                        p.draw(&Op::Fill { color, x: 0.0, y: 0.0, w: stripe, h: used }, top, ml);
                    }
                    if let Some(color) = bottom_c.filter(|_| last && bottom_w > 0.0) {
                        p.draw(&Op::Fill { color, x: 0.0, y: used - bottom_w, w: width, h: bottom_w }, top, ml);
                    }
                    p.y += pad_t;
                    for row in &segment {
                        p.place(row, ml + stripe + pad_l);
                    }
                    p.y += pad_b;
                    if last {
                        break;
                    }
                    p.new_page();
                }
                p.place(&Row::gap(card.margin[2]), ml);
            }
        }
    }

    // The footer: the report's name on the left, the page number on the right, a rule above.
    let fs = style.part(Part::Footer);
    let fi = Ink::new(fs, none);
    let (rule_w, rule_c) = rule(fs, (fs.border_top.0, &fs.border_top.1));
    let count = p.pages.len();
    let baseline = (mb * 0.53).max(fs.size);
    let mut footers = Vec::with_capacity(count);
    for n in 0..count {
        let label = fi.text(&format!("Page {} of {count}", n + 1));
        let right = w - mr - fi.width(&label);
        let left = fi.lines((right - ml - 12.0).max(36.0), &footer).into_iter().next().unwrap_or_default();
        footers.push((left, label, right));
    }
    if !fs.hidden && !p.fonts.contains(&fs.font()) {
        p.fonts.push(fs.font());
    }

    let mut doc = Document::new_empty();
    let mut fonts = Dict::new();
    for face in &p.fonts {
        let mut font = Dict::new();
        font.set(b"Type".to_vec(), Object::name("Font"));
        font.set(b"Subtype".to_vec(), Object::name("Type1"));
        font.set(b"BaseFont".to_vec(), Object::name(face.base_font()));
        font.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
        let r = doc.add(font);
        fonts.set(format!("F{}", *face as usize).into_bytes(), Object::Ref(r));
    }
    for ((back, front), (left, label, right)) in p.pages.into_iter().zip(footers) {
        let mut c = back;
        c.extend(front);
        if !fs.hidden {
            let [r, g, b] = fi.color.map(f64::from);
            let font = fs.font() as usize;
            for (x, text) in [(ml, left.as_str()), (right, label.as_str())] {
                c.extend(format!("BT /F{font} {:.2} Tf {r:.3} {g:.3} {b:.3} rg {x:.2} {baseline:.2} Td ", fs.size).into_bytes());
                c.extend(literal(&win_ansi(text)));
                c.extend_from_slice(b" Tj ET\n");
            }
            if let Some([r, g, b]) = rule_c.filter(|_| rule_w > 0.0).map(|c| c.map(f64::from)) {
                c.extend(format!("{r:.3} {g:.3} {b:.3} rg {ml:.2} {:.2} {width:.2} {rule_w:.2} re f\n", baseline + fs.size + 4.0).into_bytes());
            }
        }
        let mut res = Dict::new();
        res.set(b"Font".to_vec(), Object::Dict(fonts.clone()));
        add_page(&mut doc, w, h, res, Some(c))?;
    }
    set_title(&mut doc, &r.title);
    Ok((doc, escaped))
}
