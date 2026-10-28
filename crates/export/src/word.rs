//! Word export of a flowing page: paragraph formatting recovered from where the text sits
//! (alignment, indents, line pitch and the space between paragraphs), real lists, tables rebuilt
//! from their drawn borders and cell shading, rules under paragraphs, and inline style runs.

use crate::{Block, MAX_COLS, Page, Run, Shape, Table, emu, esc, hex, picture_xml, rgb8, room_for};

/// The text area of a page: the box its content occupies (user space, y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

/// A line's pitch as a multiple of its font size, when a page gives no paragraph to measure.
const DEFAULT_PITCH: f64 = 1.15;
/// Most cells a grid framing the page's text has (a border with rules under the header, over
/// the footer and down the middle); a grid with more is a table.
const MAX_FRAME_CELLS: usize = 6;
/// Most rows a table rebuilt from its borders may have.
const MAX_RULED_ROWS: usize = 500;
/// Most rules looked at on one page when rebuilding tables (pairs of them are compared).
const MAX_RULES: usize = 2_000;

/// Points as twentieths of a point (twips), bounded for Word.
pub(crate) fn twips(pt: f64) -> i64 {
    if pt.is_finite() { (pt * 20.0).round().clamp(-31_680.0, 31_680.0) as i64 } else { 0 }
}

/// The family of a PDF font name: `ABCDEF+LiberationSerif-BoldItalic` is "Liberation Serif",
/// `TimesNewRomanPSMT` "Times New Roman", `Arial,Bold` "Arial".
pub fn family(base_font: &str) -> String {
    let name =
        base_font.split_once('+').filter(|(tag, _)| tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase())).map_or(base_font, |(_, n)| n);
    let name = name.split([',', '-']).next().unwrap_or(name);
    let mut name = name.to_string();
    for suffix in ["PSMT", "PS", "MT", "Std", "Pro"] {
        if name.len() > suffix.len() + 2 && name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
            break;
        }
    }
    for style in ["Bold", "Italic", "Oblique", "Regular", "Semibold", "Light", "Medium", "Black", "Heavy"] {
        while name.len() > style.len() + 2 && name.ends_with(style) {
            name.truncate(name.len() - style.len());
        }
    }
    // Split CamelCase into words, keeping runs of capitals ("ITCAvant" stays one word).
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).and_then(|p| chars.get(p));
        let next = chars.get(i + 1);
        if c.is_ascii_uppercase()
            && prev.is_some_and(|p| p.is_ascii_lowercase() || (p.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase())))
        {
            out.push(' ');
        }
        out.push(*c);
    }
    out.trim().chars().filter(|c| !c.is_control()).collect()
}

/// The private-use characters symbol fonts map their bullets to (Symbol, Wingdings), as the
/// characters they show.
fn symbol_char(c: char) -> char {
    match c {
        '\u{F0B7}' | '\u{F0A8}' => '•',
        '\u{F0A7}' | '\u{F06E}' => '▪',
        '\u{F0D8}' | '\u{F0E8}' => '➢',
        '\u{F0FC}' => '✓',
        '\u{F076}' => '❖',
        '\u{F06F}' => '○',
        '\u{F02D}' => '–',
        c => c,
    }
}

/// The text with symbol-font bullets as the characters they show.
pub(crate) fn readable(s: &str) -> String {
    s.chars().map(symbol_char).collect()
}

/// What opens a list item.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Marker {
    Bullet(char),
    /// Word's number format, the text after the number ("." or ")"), and the item's number.
    Number(&'static str, char, u32),
}

impl Marker {
    /// Items of one list share this.
    fn kind(&self) -> (String, char) {
        match self {
            Marker::Bullet(c) => (format!("bullet{c}"), *c),
            Marker::Number(fmt, suffix, _) => ((*fmt).to_string(), *suffix),
        }
    }
}

fn roman(s: &str) -> Option<u32> {
    let value = |c| match c {
        'i' => Some(1),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        _ => None,
    };
    let digits: Vec<i64> = s.chars().take(8).map(value).collect::<Option<_>>()?;
    let mut total = 0i64;
    for (i, d) in digits.iter().enumerate() {
        // A smaller numeral before a larger one counts against it ("iv" is 4).
        if digits.get(i + 1).is_some_and(|n| n > d) {
            total -= d;
        } else {
            total += d;
        }
    }
    u32::try_from(total).ok().filter(|t| *t > 0)
}

/// The list marker a paragraph opens with, and how many characters it and the space after it
/// take: a bullet, or a number or letter followed by "." or ")" and a space.
pub(crate) fn marker(text: &str) -> Option<(Marker, usize)> {
    let lead = text.chars().take_while(|c| c.is_whitespace()).count();
    let rest: String = text.chars().skip(lead).collect();
    let mut chars = rest.chars();
    let first = symbol_char(chars.next()?);
    let spaced = |n: usize| {
        let after: String = rest.chars().skip(n).collect();
        let gap = after.chars().take_while(|c| c.is_whitespace()).count();
        (gap > 0 && after.chars().nth(gap).is_some()).then_some(lead + n + gap)
    };
    if matches!(first, '•' | '◦' | '▪' | '▫' | '■' | '□' | '●' | '○' | '‣' | '⁃' | '–' | '➢' | '✓' | '❖' | '·' | '*' | '-')
    {
        return spaced(1).map(|n| (Marker::Bullet(first), n));
    }
    // A Courier "o": the second-level bullet of Word's and LibreOffice's default lists.
    if first == 'o' && rest.chars().nth(1).is_some_and(char::is_whitespace) && rest.chars().nth(2).is_some_and(|c| c.is_whitespace()) {
        return spaced(1).map(|n| (Marker::Bullet('o'), n));
    }
    let head: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).take(6).collect();
    let count = head.chars().count();
    let suffix = rest.chars().nth(count).filter(|c| matches!(c, '.' | ')'))?;
    let n = spaced(count + 1)?;
    let marker = if head.chars().all(|c| c.is_ascii_digit()) && count <= 3 {
        Marker::Number("decimal", suffix, head.parse().ok()?)
    } else if count >= 2
        && let Some(v) = roman(&head)
    {
        Marker::Number("lowerRoman", suffix, v)
    } else if count >= 2
        && let Some(v) = roman(&head.to_ascii_lowercase()).filter(|_| head.chars().all(|c| c.is_ascii_uppercase()))
    {
        Marker::Number("upperRoman", suffix, v)
    } else if count == 1
        && let Some(c) = head.chars().next().filter(char::is_ascii_lowercase)
    {
        Marker::Number("lowerLetter", suffix, u32::from(c) - u32::from('a') + 1)
    } else if count == 1
        && let Some(c) = head.chars().next().filter(char::is_ascii_uppercase)
    {
        Marker::Number("upperLetter", suffix, u32::from(c) - u32::from('A') + 1)
    } else {
        return None;
    };
    Some((marker, n))
}

/// Word boxes with the ones that touch joined: one word drawn in pieces ("underlined" and its
/// comma, "1" and its ".").
fn merge_touching(words: &[[f64; 4]]) -> Vec<[f64; 4]> {
    let mut boxes: Vec<[f64; 4]> = Vec::with_capacity(words.len());
    for w in words {
        match boxes.last_mut() {
            Some(last) if (w[0] - last[2]).abs() < 0.5 => *last = [last[0], last[1].min(w[1]), w[2], last[3].max(w[3])],
            _ => boxes.push(*w),
        }
    }
    boxes
}

/// A run's word boxes, when they match its words one to one (touching pieces joined).
fn word_boxes(r: &Run) -> Option<Vec<[f64; 4]>> {
    let boxes = merge_touching(&r.words);
    (!boxes.is_empty() && boxes.len() == r.text.split_whitespace().count()).then_some(boxes)
}

/// Where the text starts after the first `n` characters of `runs` (a list marker and its space):
/// the box of the word there, or the start of the run it opens. `None` when that is not known.
fn text_start(runs: &[Run], mut n: usize) -> Option<f64> {
    for r in runs {
        let len = r.text.chars().count();
        if n >= len {
            n -= len;
            continue;
        }
        if n == 0 {
            return (r.rect != [0.0; 4]).then_some(r.rect[0]);
        }
        let before: String = r.text.chars().take(n).collect();
        return word_boxes(r).and_then(|b| b.get(before.split_whitespace().count()).map(|w| w[0]));
    }
    None
}

/// Runs without their first `n` characters (a list marker).
fn drop_chars(runs: &[Run], mut n: usize) -> Vec<Run> {
    let mut out = Vec::new();
    for r in runs {
        let len = r.text.chars().count();
        if n >= len {
            n -= len;
            continue;
        }
        let text: String = r.text.chars().skip(n).collect();
        // The words dropped take their boxes along, so the rest still match theirs.
        let dropped: String = r.text.chars().take(n).collect();
        let words = word_boxes(r).map_or_else(Vec::new, |b| b.into_iter().skip(dropped.split_whitespace().count()).collect());
        n = 0;
        out.push(Run { text, words, ..r.clone() });
    }
    out
}

/// The pitch of a block's rows (top to top), when it has more than one.
fn pitch(b: &Block) -> Option<f64> {
    let rows = b.rows();
    let (first, last) = (rows.first()?, rows.last()?);
    let n = rows.len().checked_sub(1).filter(|n| *n > 0)? as f64;
    let p = (first[3] - last[3]) / n;
    (p.is_finite() && p > 0.0).then_some(p)
}

/// The page's line pitch as a multiple of the font size: the median over its paragraphs of more
/// than one row.
pub(crate) fn pitch_ratio(pages: &[Page]) -> f64 {
    let mut ratios: Vec<f64> = pages
        .iter()
        .flat_map(|p| &p.blocks)
        .filter(|b| b.size > 0.0 && b.rotation.abs() <= 0.1)
        .filter_map(|b| pitch(b).map(|p| p / b.size))
        .filter(|r| r.is_finite() && (0.8..3.0).contains(r))
        .collect();
    ratios.sort_by(|a, b| a.total_cmp(b));
    ratios.get(ratios.len() / 2).copied().unwrap_or(DEFAULT_PITCH)
}

/// The vertical extent a block's lines take in Word with `line` points per row: from half a line
/// above its first row's middle to half a line below its last's.
fn line_box(b: &Block, line: f64) -> (f64, f64) {
    let rows = b.rows();
    let mid = |r: &[f64; 4]| (r[1] + r[3]) / 2.0;
    let top = rows.first().map_or(b.rect[3], |r| mid(r) + line / 2.0);
    let bottom = rows.last().map_or(b.rect[1], |r| mid(r) - line / 2.0);
    (top, bottom)
}

/// A block's line height in Word: its own row pitch, or the page's ratio times its size.
fn line_height(b: &Block, ratio: f64) -> f64 {
    let size = if b.size.is_finite() && b.size > 0.0 { b.size } else { 12.0 };
    pitch(b).filter(|p| *p < size * 3.0).unwrap_or(size * ratio).clamp(1.0, 1584.0)
}

/// The box the page's flowing content occupies: its text rows, images and tables.
pub(crate) fn frame(page: &Page, ratio: f64) -> Option<Frame> {
    let mut f: Option<Frame> = None;
    let mut add = |l: f64, r: f64, t: f64, b: f64| {
        if !(l.is_finite() && r.is_finite() && t.is_finite() && b.is_finite()) {
            return;
        }
        f = Some(match f {
            None => Frame { left: l, right: r, top: t, bottom: b },
            Some(f) => Frame { left: f.left.min(l), right: f.right.max(r), top: f.top.max(t), bottom: f.bottom.min(b) },
        });
    };
    for b in &page.blocks {
        let (top, bottom) = line_box(b, line_height(b, ratio));
        for r in b.rows() {
            add(r[0], r[2], top, bottom);
        }
    }
    for im in &page.images {
        add(im.rect[0], im.rect[2], im.rect[3], im.rect[1]);
    }
    f.filter(|f| f.right > f.left && f.top > f.bottom)
}

/// One cell of a table rebuilt from its borders.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuledCell {
    /// Grid columns it covers.
    pub span: usize,
    /// `Some(true)` opens a cell merged downwards, `Some(false)` continues one.
    pub vmerge: Option<bool>,
    /// The blocks inside it (indexes into the page's blocks), top to bottom.
    pub blocks: Vec<usize>,
    pub shade: Option<[f64; 3]>,
    /// Its box (user space).
    pub rect: [f64; 4],
}

/// A table whose borders the page draws: the grid lines, the cells and the border's look.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuledTable {
    pub rect: [f64; 4],
    /// Column edges, left to right, and row edges, top to bottom.
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    pub rows: Vec<Vec<RuledCell>>,
    /// Border width (points) and colour.
    pub border: (f64, [f64; 3]),
}

impl RuledTable {
    /// The table as plain text cells, for the writers that keep text only.
    pub(crate) fn plain(&self, blocks: &[Block]) -> Table {
        let rows = self
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| {
                        let first = c.blocks.first().and_then(|i| blocks.get(*i));
                        crate::Cell {
                            // A cell's paragraphs stay apart, a line each.
                            text: c.blocks.iter().filter_map(|i| blocks.get(*i)).map(|b| readable(&b.text)).collect::<Vec<_>>().join("\n"),
                            size: first.map_or(11.0, |b| b.size),
                            bold: first.is_some_and(|b| b.bold),
                            italic: first.is_some_and(|b| b.italic),
                            color: first.map_or([0.0; 3], |b| b.color),
                            span: c.span,
                        }
                    })
                    .collect()
            })
            .collect();
        Table { rect: self.rect, cols: self.xs.iter().take(self.xs.len().saturating_sub(1)).copied().collect(), rows }
    }
}

/// A horizontal (`y`, `x0`..`x1`) or vertical (`x`, `y0`..`y1`) rule and its thickness.
#[derive(Clone, Copy, Debug)]
struct Rule {
    at: f64,
    from: f64,
    to: f64,
    width: f64,
    color: [f64; 3],
}

/// The page's thin shapes as horizontal and vertical rules, collinear pieces joined.
fn rules(shapes: &[Shape]) -> (Vec<Rule>, Vec<Rule>) {
    let mut h = Vec::new();
    let mut v = Vec::new();
    for s in shapes.iter().take(MAX_RULES * 4) {
        let [x0, y0, x1, y1] = s.rect;
        let (w, ht) = (x1 - x0, y1 - y0);
        if !(w.is_finite() && ht.is_finite()) {
            continue;
        }
        if ht <= 3.0 && w >= 3.0 {
            h.push(Rule { at: (y0 + y1) / 2.0, from: x0, to: x1, width: ht, color: s.color });
        } else if w <= 3.0 && ht >= 3.0 {
            v.push(Rule { at: (x0 + x1) / 2.0, from: y0, to: y1, width: w, color: s.color });
        }
    }
    let join = |mut rs: Vec<Rule>| {
        rs.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.from.total_cmp(&b.from)));
        let mut out: Vec<Rule> = Vec::new();
        for r in rs {
            match out.last_mut() {
                Some(l) if (l.at - r.at).abs() < 1.0 && r.from <= l.to + 1.5 => l.to = l.to.max(r.to),
                _ => out.push(r),
            }
        }
        out.truncate(MAX_RULES);
        out
    };
    (join(h), join(v))
}

/// Cluster sorted values that lie within `tol` of the previous one.
fn clusters(mut values: Vec<f64>, tol: f64) -> Vec<f64> {
    values.sort_by(|a, b| a.total_cmp(b));
    let mut out: Vec<(f64, usize)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((c, n)) if (v - *c / *n as f64).abs() <= tol => {
                *c += v;
                *n += 1;
            }
            _ => out.push((v, 1)),
        }
    }
    out.into_iter().map(|(c, n)| c / n as f64).collect()
}

/// Tables whose grid the page draws: rules that cross each other, two or more each way. Each
/// block whose middle lies in a cell goes into it (and is consumed); a filled box covering most
/// of a cell is its shading. A grid that frames the page's text is not a table; its rules are
/// returned so they are not read as boxes or rules either.
pub(crate) fn ruled_tables(page: &Page) -> Ruled {
    let mut consumed = vec![false; page.blocks.len()];
    let mut frames = Vec::new();
    let (h, v) = rules(&page.shapes);
    if h.len() < 2 || v.len() < 2 {
        return Ruled { tables: Vec::new(), consumed, frames };
    }
    // Connected groups of crossing rules (union-find over h then v).
    let n = h.len() + v.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while let Some(&p) = parent.get(i) {
            if p == i {
                break;
            }
            let gp = parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = parent.get_mut(i) {
                *slot = gp;
            }
            i = p;
        }
        i
    }
    for (i, a) in h.iter().enumerate() {
        for (j, b) in v.iter().enumerate() {
            if b.at >= a.from - 2.0 && b.at <= a.to + 2.0 && a.at >= b.from - 2.0 && a.at <= b.to + 2.0 {
                let (ra, rb) = (root(&mut parent, i), root(&mut parent, h.len() + j));
                if let Some(slot) = parent.get_mut(ra) {
                    *slot = rb;
                }
            }
        }
    }
    let mut groups: Vec<(usize, Vec<usize>, Vec<usize>)> = Vec::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        let g = match groups.iter().position(|g| g.0 == r) {
            Some(k) => k,
            None => {
                groups.push((r, Vec::new(), Vec::new()));
                groups.len() - 1
            }
        };
        if let Some(g) = groups.get_mut(g) {
            if i < h.len() { g.1.push(i) } else { g.2.push(i - h.len()) }
        }
    }
    let mut tables = Vec::new();
    for (_, hs, vs) in groups {
        let hs: Vec<Rule> = hs.iter().filter_map(|i| h.get(*i)).copied().collect();
        let vs: Vec<Rule> = vs.iter().filter_map(|i| v.get(*i)).copied().collect();
        let xs = clusters(vs.iter().map(|r| r.at).collect(), 1.5);
        let mut ys = clusters(hs.iter().map(|r| r.at).collect(), 1.5);
        ys.reverse();
        if xs.len() < 2 || ys.len() < 2 || xs.len() > MAX_COLS + 1 || ys.len() > MAX_RULED_ROWS + 1 {
            continue;
        }
        let (Some(&x0), Some(&x1), Some(&y1), Some(&y0)) = (xs.first(), xs.last(), ys.first(), ys.last()) else { continue };
        // A box drawn around a paragraph (one cell, one block) is not a table.
        let rect = [x0, y0, x1, y1];
        let ncols = xs.len() - 1;
        let nrows = ys.len() - 1;
        let v_at = |x: f64, y: f64| vs.iter().any(|r| (r.at - x).abs() < 1.5 && r.from <= y && r.to >= y);
        let h_at = |y: f64, x: f64| hs.iter().any(|r| (r.at - y).abs() < 1.5 && r.from <= x && r.to >= x);
        // Each grid slot's owning cell, as (row, start column) of the cell.
        let mut owner: Vec<Vec<(usize, usize)>> = vec![vec![(0, 0); ncols]; nrows];
        let mut rows: Vec<Vec<RuledCell>> = Vec::with_capacity(nrows);
        for i in 0..nrows {
            let (Some(&top), Some(&bottom)) = (ys.get(i), ys.get(i + 1)) else { continue };
            let mid_y = (top + bottom) / 2.0;
            let mut row = Vec::new();
            let mut j = 0;
            while j < ncols {
                let mut span = 1;
                while j + span < ncols && !xs.get(j + span).is_some_and(|x| v_at(*x, mid_y)) {
                    span += 1;
                }
                let left = xs.get(j).copied().unwrap_or(x0);
                let right = xs.get(j + span).copied().unwrap_or(x1);
                let mid_x = (left + right) / 2.0;
                // No rule above this cell joins it to the one above, when that one is as wide.
                let above = i.checked_sub(1).and_then(|p| owner.get(p)).and_then(|r| r.get(j)).copied();
                let merged = i > 0
                    && !h_at(top, mid_x)
                    && above.is_some_and(|(r, c)| {
                        c == j
                            && rows
                                .get(r)
                                .and_then(|row: &Vec<RuledCell>| row.iter().find(|cell| cell.rect[0] == left))
                                .is_some_and(|cell| cell.span == span)
                    });
                if merged
                    && let Some((r, _)) = above
                    && let Some(cell) = rows.get_mut(r).and_then(|row| row.iter_mut().find(|cell| cell.rect[0] == left))
                    && cell.vmerge.is_none()
                {
                    cell.vmerge = Some(true);
                }
                let start = if merged { above.map_or((i, j), |(r, _)| (r, j)) } else { (i, j) };
                for k in j..j + span {
                    if let Some(slot) = owner.get_mut(i).and_then(|r| r.get_mut(k)) {
                        *slot = start;
                    }
                }
                row.push(RuledCell { span, vmerge: merged.then_some(false), blocks: Vec::new(), shade: None, rect: [left, bottom, right, top] });
                j += span;
            }
            rows.push(row);
        }
        // Blocks go to the cell (the merge's first) their middle lies in.
        for (bi, b) in page.blocks.iter().enumerate() {
            let (cx, cy) = ((b.rect[0] + b.rect[2]) / 2.0, (b.rect[1] + b.rect[3]) / 2.0);
            if b.rotation.abs() > 0.1 || cx < x0 || cx > x1 || cy < y0 || cy > y1 || consumed.get(bi).copied().unwrap_or(true) {
                continue;
            }
            let (Some(i), Some(j)) = (ys.windows(2).position(|w| cy <= w[0] && cy >= w[1]), xs.windows(2).position(|w| cx >= w[0] && cx <= w[1]))
            else {
                continue;
            };
            let Some(&(r, c)) = owner.get(i).and_then(|row| row.get(j)) else { continue };
            let left = xs.get(c).copied().unwrap_or(x0);
            if let Some(cell) = rows.get_mut(r).and_then(|row| row.iter_mut().find(|cell| cell.rect[0] == left)) {
                cell.blocks.push(bi);
                if let Some(slot) = consumed.get_mut(bi) {
                    *slot = true;
                }
            }
        }
        let filled = rows.iter().flatten().filter(|c| !c.blocks.is_empty()).count();
        // A frame around the text (a page border, perhaps with rules under the header, over the
        // footer or down the middle) is not a table. It is told by its geometry: few cells,
        // covering most of the page and holding most of its text. Its rules are then left out
        // of the boxes and rules found later.
        let page_area = page.width * page.height;
        let area = |r: [f64; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
        let texts: Vec<&Block> = page.blocks.iter().filter(|b| !b.text.trim().is_empty()).collect();
        let enclosed = texts
            .iter()
            .filter(|b| {
                let (cx, cy) = ((b.rect[0] + b.rect[2]) / 2.0, (b.rect[1] + b.rect[3]) / 2.0);
                cx >= x0 && cx <= x1 && cy >= y0 && cy <= y1
            })
            .count();
        let frame_like = rows.iter().flatten().count() <= MAX_FRAME_CELLS
            && page_area.is_finite()
            && area(rect) >= page_area * 0.5
            && !texts.is_empty()
            && enclosed * 5 >= texts.len() * 4;
        if frame_like {
            frames.extend(hs.iter().map(|r| [r.from, r.at, r.to, r.at]));
            frames.extend(vs.iter().map(|r| [r.at, r.from, r.at, r.to]));
        }
        if rows.iter().flatten().count() < 2 || filled == 0 || frame_like {
            // Not a grid of cells: release its blocks.
            for c in rows.iter().flatten() {
                for bi in &c.blocks {
                    if let Some(slot) = consumed.get_mut(*bi) {
                        *slot = false;
                    }
                }
            }
            continue;
        }
        // Shading: the last filled box (painted on top) covering most of a cell. The fills over
        // the table are picked once, topmost first and a bounded number, then each row's.
        let table_area = area(rect);
        let overlaps = |a: &[f64; 4], b: &[f64; 4]| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
        let fills: Vec<&Shape> = page
            .shapes
            .iter()
            .rev()
            .filter(|s| !s.stroked && overlaps(&s.rect, &rect) && area(s.rect) <= table_area * 1.05 && rgb8(s.color) != [255, 255, 255])
            .take(MAX_FILLS)
            .collect();
        for row in rows.iter_mut() {
            let band = row.iter().map(|c| c.rect).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
            let row_fills: Vec<&Shape> = fills.iter().copied().filter(|s| band.is_some_and(|b| overlaps(&s.rect, &b))).collect();
            for c in row.iter_mut() {
                let cell_area = area(c.rect);
                c.blocks.sort_by(|a, b| {
                    let (ta, tb) = (page.blocks.get(*a).map_or(0.0, |b| b.rect[3]), page.blocks.get(*b).map_or(0.0, |b| b.rect[3]));
                    tb.total_cmp(&ta)
                });
                c.shade = row_fills
                    .iter()
                    .find(|s| {
                        let i = [s.rect[0].max(c.rect[0]), s.rect[1].max(c.rect[1]), s.rect[2].min(c.rect[2]), s.rect[3].min(c.rect[3])];
                        cell_area > 0.0 && area(i) >= 0.6 * cell_area
                    })
                    .map(|s| s.color);
            }
        }
        let mut widths: Vec<f64> = hs.iter().chain(&vs).map(|r| r.width).filter(|w| w.is_finite()).collect();
        widths.sort_by(|a, b| a.total_cmp(b));
        let width = widths.get(widths.len() / 2).copied().unwrap_or(0.5);
        let color = hs.first().map_or([0.0; 3], |r| r.color);
        tables.push(RuledTable { rect, xs, ys, rows, border: (width, color) });
    }
    Ruled { tables, consumed, frames }
}

/// What [`ruled_tables`] finds on a page.
pub(crate) struct Ruled {
    pub tables: Vec<RuledTable>,
    /// Per block, whether a table took it.
    pub consumed: Vec<bool>,
    /// The rules of frames around the text (grids that are not tables), as `[x0, y0, x1, y1]`
    /// lines: they are neither boxes nor rules under paragraphs.
    pub frames: Vec<[f64; 4]>,
}

/// Whether a thin shape or rule `r` lies along one of `frames`' lines.
fn on_frame(r: [f64; 4], frames: &[[f64; 4]]) -> bool {
    frames.iter().any(|f| {
        let horizontal = f[1] == f[3];
        let (cx, cy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
        if horizontal {
            (cy - f[1]).abs() < 1.5 && r[0] >= f[0] - 2.0 && r[2] <= f[2] + 2.0
        } else {
            (cx - f[0]).abs() < 1.5 && r[1] >= f[1] - 2.0 && r[3] <= f[3] + 2.0
        }
    })
}

/// Lines a run is drawn with.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Lines {
    pub under: bool,
    pub strike: bool,
}

/// What lines are drawn across a box (a word, or a run's first piece) of a run `span` wide.
pub(crate) type LinesOf<'a> = &'a dyn Fn([f64; 4], f64) -> Lines;

/// Runs as Word XML. Symbol-font bullets become the characters they show.
pub(crate) fn runs_xml(runs: &[Run], underlined: LinesOf) -> String {
    let mut s = String::new();
    for r in runs {
        for (piece, lines) in split_by_lines(r, underlined) {
            s.push_str(&run_xml(&piece, lines));
        }
    }
    s
}

/// A run in pieces whose words share the lines drawn across them (an underlined link inside a
/// sentence of one font). Words are matched to their boxes one to one; when they cannot be, the
/// run is judged by its first box.
fn split_by_lines(r: &Run, lines_of: LinesOf) -> Vec<(Run, Lines)> {
    let words = r.text.split_whitespace().count();
    // How wide the run is: a line much wider is a rule, not a line across its words.
    let span = match (r.words.first(), r.words.last()) {
        (Some(a), Some(b)) => (b[2] - a[0]).max(r.rect[2] - r.rect[0]),
        _ => r.rect[2] - r.rect[0],
    };
    // Boxes that touch are one word drawn in pieces ("underlined" and its comma).
    let boxes = merge_touching(&r.words);
    if boxes.is_empty() || words != boxes.len() {
        return vec![(r.clone(), lines_of(r.rect, span))];
    }
    let flags: Vec<Lines> = boxes.iter().map(|w| lines_of(*w, span)).collect();
    let mut out: Vec<(Run, Lines)> = Vec::new();
    let mut word = 0usize;
    let mut in_word = false;
    let mut current = String::new();
    let mut current_flags = flags.first().copied().unwrap_or_default();
    for c in r.text.chars() {
        if !c.is_whitespace() && !in_word {
            in_word = true;
            let f = flags.get(word).copied().unwrap_or_default();
            if f != current_flags && !current.is_empty() {
                // Whitespace before a word goes with the word before it unless both are lined.
                out.push((Run { text: std::mem::take(&mut current), ..r.clone() }, current_flags));
            }
            current_flags = f;
            word += 1;
        } else if c.is_whitespace() {
            in_word = false;
        }
        current.push(c);
    }
    if !current.is_empty() {
        out.push((Run { text: current, ..r.clone() }, current_flags));
    }
    out
}

/// The run properties of a paragraph mark styled like `r`.
fn mark_rpr(r: &Run) -> String {
    let mut rpr = String::from("<w:rPr>");
    let font = esc(&r.font);
    if !font.is_empty() {
        rpr.push_str(&format!("<w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:cs=\"{font}\" w:eastAsia=\"{font}\"/>"));
    }
    if let Some(h) = hex(r.color) {
        rpr.push_str(&format!("<w:color w:val=\"{h}\"/>"));
    }
    let half_points = if r.size.is_finite() { (r.size * 2.0).round().clamp(2.0, 3276.0) as i64 } else { 24 };
    rpr.push_str(&format!("<w:sz w:val=\"{half_points}\"/><w:szCs w:val=\"{half_points}\"/></w:rPr>"));
    rpr
}

pub(crate) fn run_xml(r: &Run, lines: Lines) -> String {
    let mut rpr = String::new();
    let font = esc(&r.font);
    if !font.is_empty() {
        rpr.push_str(&format!("<w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:cs=\"{font}\" w:eastAsia=\"{font}\"/>"));
    }
    if r.bold {
        rpr.push_str("<w:b/>");
    }
    if r.italic {
        rpr.push_str("<w:i/>");
    }
    if lines.strike {
        rpr.push_str("<w:strike/>");
    }
    if let Some(h) = hex(r.color) {
        rpr.push_str(&format!("<w:color w:val=\"{h}\"/>"));
    }
    if r.rise.is_finite() && r.rise.abs() >= 0.5 {
        rpr.push_str(&format!("<w:position w:val=\"{}\"/>", (r.rise * 2.0).round().clamp(-3168.0, 3168.0) as i64));
    }
    let half_points = if r.size.is_finite() { (r.size * 2.0).round().clamp(2.0, 3276.0) as i64 } else { 24 };
    rpr.push_str(&format!("<w:sz w:val=\"{half_points}\"/><w:szCs w:val=\"{half_points}\"/>"));
    if lines.under {
        rpr.push_str("<w:u w:val=\"single\"/>");
    }
    format!("<w:r><w:rPr>{rpr}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(&readable(&r.text)))
}

/// How far a single full row may reach past its paragraph's right edge (points).
const RIGHT_SLACK: f64 = 3.0;

/// How a paragraph's rows sit between its margins.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Align {
    Left,
    Centre,
    Right,
    Justify,
}

/// A paragraph's alignment, from its rows' edges within `left`..`right`.
fn align(b: &Block, left: f64, right: f64) -> Align {
    let rows = b.rows();
    let size = if b.size.is_finite() && b.size > 0.0 { b.size } else { 12.0 };
    let tol = (size * 0.3).max(1.0);
    let spread = |f: &dyn Fn(&[f64; 4]) -> f64, rs: &[[f64; 4]]| {
        let v: Vec<f64> = rs.iter().map(f).collect();
        v.iter().copied().fold(f64::NEG_INFINITY, f64::max) - v.iter().copied().fold(f64::INFINITY, f64::min)
    };
    let width = right - left;
    if rows.len() >= 2 {
        let body = rows.get(1..).unwrap_or_default();
        let lefts_agree = spread(&|r| r[0], body) <= tol;
        let all_but_last = rows.get(..rows.len() - 1).unwrap_or_default();
        let rights_agree = spread(&|r| r[2], all_but_last) <= 1.5;
        let centres_agree = spread(&|r| (r[0] + r[2]) / 2.0, &rows) <= tol;
        let all_rights = spread(&|r| r[2], &rows) <= tol;
        if lefts_agree
            && rights_agree
            && all_but_last.iter().all(|r| right - r[2] < 2.0)
            && (rows.len() >= 3 || all_but_last.iter().all(|r| right - r[2] < 1.0))
        {
            return Align::Justify;
        }
        if !lefts_agree && all_rights {
            return Align::Right;
        }
        if !lefts_agree && centres_agree {
            return Align::Centre;
        }
        return Align::Left;
    }
    let Some(r) = rows.first() else { return Align::Left };
    let (gap_l, gap_r) = (r[0] - left, right - r[2]);
    if gap_l > size * 2.0 && (gap_l - gap_r).abs() < tol.max(width * 0.01) {
        Align::Centre
    } else if gap_l > size * 2.0 && gap_r < tol {
        Align::Right
    } else {
        Align::Left
    }
}

/// The paragraph properties of a block set between `left` and `right`: alignment, indents, the
/// space before it and an exact line height. `list` is the numbering (num id, level) and the
/// marker's left edge for a list item. `border` is a rule under the paragraph (width, colour,
/// distance below the text).
#[allow(clippy::too_many_arguments)]
fn ppr(b: &Block, style: &str, left: f64, right: f64, before: f64, line: f64, list: Option<(usize, usize, f64, f64)>, deco: Deco) -> (String, Align) {
    let rows = b.rows();
    let size = if b.size.is_finite() && b.size > 0.0 { b.size } else { 12.0 };
    let a = align(b, left, right);
    let mut s = String::from("<w:pPr>");
    s.push_str(style);
    if let Some((num, level, _, _)) = list {
        s.push_str(&format!("<w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{num}\"/></w:numPr>"));
    }
    let edge = |side: &str, w: f64, c: [f64; 3], space: f64| {
        let sz = (w * 8.0).round().clamp(2.0, 96.0) as i64;
        let colour = crate::hex(c).unwrap_or_else(|| "000000".into());
        format!("<w:{side} w:val=\"single\" w:sz=\"{sz}\" w:space=\"{}\" w:color=\"{colour}\"/>", space.round().clamp(0.0, 31.0) as i64)
    };
    if let Some(bx) = deco.boxed {
        s.push_str("<w:pBdr>");
        for (side, space) in ["top", "left", "bottom", "right"].iter().zip(bx.spaces) {
            s.push_str(&edge(side, bx.width, bx.color, space));
        }
        s.push_str("</w:pBdr>");
    } else if let Some((w, c, gap)) = deco.under {
        s.push_str(&format!("<w:pBdr>{}</w:pBdr>", edge("bottom", w, c, gap)));
    }
    if let Some(fill) = deco.shade.and_then(hex).or_else(|| deco.shade.map(|_| "000000".into())) {
        s.push_str(&format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>"));
    }
    s.push_str(&format!("<w:spacing w:before=\"{}\" w:after=\"0\" w:line=\"{}\" w:lineRule=\"exact\"/>", twips(before.max(0.0)), twips(line)));
    let (first, rest) = (rows.first().map_or(b.rect[0], |r| r[0]), rows.get(1).map_or(b.rect[0], |r| r[0]));
    let mut ind = String::new();
    match (list, a) {
        (Some((_, _, marker_x, text_x)), _) => {
            ind = format!("<w:ind w:left=\"{}\" w:hanging=\"{}\"/>", twips(text_x - left), twips(text_x - marker_x));
        }
        (None, Align::Left | Align::Justify) => {
            let body = if rows.len() >= 2 { rest } else { first };
            let l = (body - left).max(0.0);
            let maxr = rows.iter().map(|r| r[2]).fold(f64::NEG_INFINITY, f64::max);
            let r = if a == Align::Justify {
                (right - maxr).max(0.0)
            } else if rows.len() >= 2 && right - maxr > size * 0.25 {
                // Ragged rows end short of the margin by less than the next row's first word:
                // the text area reaches a quarter em past the longest row, less than any word
                // and its space, so no word moves up a row.
                (right - maxr - size * 0.25).max(0.0)
            } else if rows.len() == 1 && right - maxr < size {
                // A single row that (nearly) fills the line: the source's margin may lie a little
                // past it, so let it reach a few points further rather than wrap in Word.
                -RIGHT_SLACK
            } else {
                0.0
            };
            let fl = first - (left + l);
            let mut parts = String::new();
            if l > 0.5 {
                parts.push_str(&format!(" w:left=\"{}\"", twips(l)));
            }
            if r.abs() > 0.5 {
                parts.push_str(&format!(" w:right=\"{}\"", twips(r)));
            }
            if fl > 0.5 {
                parts.push_str(&format!(" w:firstLine=\"{}\"", twips(fl)));
            } else if fl < -0.5 {
                parts.push_str(&format!(" w:hanging=\"{}\"", twips(-fl)));
            }
            if !parts.is_empty() {
                ind = format!("<w:ind{parts}/>");
            }
        }
        _ => {}
    }
    s.push_str(&ind);
    let jc = match a {
        Align::Left => "",
        Align::Centre => "<w:jc w:val=\"center\"/>",
        Align::Right => "<w:jc w:val=\"right\"/>",
        Align::Justify => "<w:jc w:val=\"both\"/>",
    };
    s.push_str(jc);
    // The paragraph mark in the first run's style: Word sets a list's number in it too.
    if let Some(r) = b.runs().first() {
        s.push_str(&mark_rpr(r));
    }
    s.push_str("</w:pPr>");
    (s, a)
}

/// The runs of a block with a line break between its rows, for centred and right-aligned
/// paragraphs whose rows were set apart on purpose (an address, a title over two lines).
fn runs_with_breaks(b: &Block, runs: &[Run], underlined: LinesOf) -> String {
    let rows = b.rows();
    if rows.len() < 2 || runs.iter().any(|r| r.rect == [0.0; 4]) {
        return runs_xml(runs, underlined);
    }
    let mut s = String::new();
    let mut row = 0usize;
    for r in runs {
        // A run starting on a lower row than the current one ends that row first.
        let mid = (r.rect[1] + r.rect[3]) / 2.0;
        let at = rows.iter().position(|rr| mid >= rr[1] - 0.5 && mid <= rr[3] + 0.5).unwrap_or(row);
        if at > row {
            s.push_str("<w:r><w:br/></w:r>");
            row = at;
        }
        s.push_str(&runs_xml(&[Run { text: r.text.trim_start().to_string(), ..r.clone() }], underlined));
    }
    s
}

/// How a paragraph is set off from the text around it.
#[derive(Clone, Copy, Debug, Default)]
struct Deco {
    /// A rule under it: width, colour and distance below the text.
    under: Option<(f64, [f64; 3], f64)>,
    /// A box around it (and the paragraphs it shares the box with).
    boxed: Option<Boxed>,
    /// A filled box behind it.
    shade: Option<[f64; 3]>,
}

/// A ruled box around one or more paragraphs.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Boxed {
    width: f64,
    color: [f64; 3],
    /// The box (middle of its lines), user space.
    rect: [f64; 4],
    /// Space between the box and the text inside it: top, left, bottom, right (points).
    spaces: [f64; 4],
}

/// Most rules looked at when finding boxes drawn around paragraphs (sides are compared in
/// pairs).
const MAX_BOX_RULES: usize = 200;

/// Rules on the page that are not part of a table: horizontal lines. A line just under a run's
/// text is its underline; one under a whole paragraph is its bottom border; four that close
/// around paragraphs are their box.
struct Rules {
    lines: Vec<Shape>,
    /// Boxes closed by four rules around paragraphs, each sized to the text inside it.
    boxes: Vec<Boxed>,
    /// Filled boxes that can shade a paragraph: not white, not a sliver, under half the page.
    fills: Vec<Shape>,
}

/// Most filled boxes looked at to shade paragraphs.
const MAX_FILLS: usize = 2_000;

impl Rules {
    fn new(page: &Page, tables: &[RuledTable], frames: &[[f64; 4]], ratio: f64) -> Self {
        let lines = page
            .shapes
            .iter()
            .filter(|s| {
                let (w, h) = (s.rect[2] - s.rect[0], s.rect[3] - s.rect[1]);
                h <= 3.0 && w >= 3.0 && w.is_finite()
            })
            .filter(|s| {
                !tables.iter().any(|t| {
                    s.rect[1] >= t.rect[1] - 2.0 && s.rect[3] <= t.rect[3] + 2.0 && s.rect[0] >= t.rect[0] - 2.0 && s.rect[2] <= t.rect[2] + 2.0
                })
            })
            .filter(|s| !on_frame(s.rect, frames))
            .take(MAX_RULES)
            .cloned()
            .collect();
        let (h, v) = rules(&page.shapes);
        let h: Vec<Rule> = h.into_iter().filter(|r| !on_frame([r.from, r.at, r.to, r.at], frames)).collect();
        let v: Vec<Rule> = v.into_iter().filter(|r| !on_frame([r.at, r.from, r.at, r.to], frames)).collect();
        let (h, v) = (h.get(..h.len().min(MAX_BOX_RULES)).unwrap_or_default(), v.get(..v.len().min(MAX_BOX_RULES)).unwrap_or_default());
        let mut found = Vec::new();
        for top in h {
            for bottom in h.iter().filter(|b| b.at < top.at - 4.0 && (b.from - top.from).abs() < 2.0 && (b.to - top.to).abs() < 2.0) {
                let side = |x: f64| v.iter().any(|r| (r.at - x).abs() < 2.0 && r.from <= bottom.at + 2.0 && r.to >= top.at - 2.0);
                let inside = |t: &RuledTable| {
                    t.rect[0] <= top.from + 2.0 && t.rect[2] >= top.to - 2.0 && t.rect[1] <= bottom.at + 2.0 && t.rect[3] >= top.at - 2.0
                };
                if side(top.from) && side(top.to) && !tables.iter().any(inside) {
                    found.push(([top.from, bottom.at, top.to, top.at], top.width, top.color));
                    break;
                }
            }
        }
        let area = |r: &[f64; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
        let page_area = page.width * page.height;
        let fills = page
            .shapes
            .iter()
            .filter(|s| !s.stroked && rgb8(s.color) != [255, 255, 255] && area(&s.rect) < page_area * 0.5)
            .filter(|s| (s.rect[2] - s.rect[0]).min(s.rect[3] - s.rect[1]) > 3.0)
            .take(MAX_FILLS)
            .cloned()
            .collect();
        // Each box sized once to the text of every paragraph inside it (one pass over them; at
        // most MAX_BOX_RULES boxes).
        let mut extents: Vec<[f64; 3]> = vec![[f64::NEG_INFINITY, f64::INFINITY, f64::INFINITY]; found.len()];
        for o in page.blocks.iter().filter(|o| !o.text.trim().is_empty()) {
            let (top, bottom) = line_box(o, line_height(o, ratio));
            for (e, (bx, _, _)) in extents.iter_mut().zip(&found) {
                if Self::holds(bx, o.rect) {
                    *e = [e[0].max(top), e[1].min(bottom), e[2].min(o.rect[0])];
                }
            }
        }
        let boxes = found
            .iter()
            .zip(extents)
            .map(|((rect, width, color), [top, bottom, x0])| {
                let half = width / 2.0;
                let spaces =
                    [rect[3] - half - top, x0 - rect[0] - half, bottom - rect[1] - half, 0.0].map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
                Boxed { width: *width, color: *color, rect: *rect, spaces }
            })
            .collect();
        Self { lines, boxes, fills }
    }

    /// Whether box `bx` holds `r` (a point of slack each way).
    fn holds(bx: &[f64; 4], r: [f64; 4]) -> bool {
        r[0] >= bx[0] - 1.0 && r[2] <= bx[2] + 1.0 && r[1] >= bx[1] - 1.0 && r[3] <= bx[3] + 1.0
    }

    /// The innermost box around a block, sized to the text of every paragraph inside it.
    fn boxed(&self, b: &Block) -> Option<Boxed> {
        let area = |r: &[f64; 4]| (r[2] - r[0]) * (r[3] - r[1]);
        self.boxes.iter().filter(|x| Self::holds(&x.rect, b.rect)).min_by(|a, c| area(&a.rect).total_cmp(&area(&c.rect))).copied()
    }

    /// The filled box behind a block: the smallest non-white one holding all its text, smaller
    /// than half the page.
    fn shade(&self, b: &Block) -> Option<[f64; 3]> {
        let area = |r: &[f64; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
        self.fills
            .iter()
            .filter(|s| b.rect[0] >= s.rect[0] - 1.0 && b.rect[2] <= s.rect[2] + 1.0 && b.rect[1] >= s.rect[1] - 1.0 && b.rect[3] <= s.rect[3] + 1.0)
            .min_by(|a, b| area(&a.rect).total_cmp(&area(&b.rect)))
            .map(|s| s.color)
    }

    /// The lines drawn across `rect` (a word, or a run's first piece): one just under its
    /// baseline is its underline, one through the middle of its letters strikes it out. A line
    /// much wider than the run (`span`) is a rule, not a line across its text.
    fn lines_of(&self, rect: [f64; 4], span: f64) -> Lines {
        let [x0, y0, x1, y1] = rect;
        let h = y1 - y0;
        let at = |lo: f64, hi: f64| {
            h > 0.0
                && self.lines.iter().any(|l| {
                    let cy = (l.rect[1] + l.rect[3]) / 2.0;
                    // Most of the box: a word's trailing punctuation is often left unlined.
                    let covered = l.rect[2].min(x1) - l.rect[0].max(x0);
                    cy >= y0 + h * lo && cy <= y0 + h * hi && covered >= (x1 - x0) * 0.8 - 1.0 && l.rect[2] - l.rect[0] < span * 3.0 + 6.0
                })
        };
        Lines { under: at(-0.5 / h.max(1.0), 0.4), strike: at(0.4, 0.75) }
    }

    /// A rule under a whole paragraph (wider than half its text, between its last row and
    /// `below`): its width, colour and distance under the text.
    fn under(&self, b: &Block, below: f64) -> Option<(f64, [f64; 3], f64)> {
        let rows = b.rows();
        let last = rows.last()?;
        let width = b.rect[2] - b.rect[0];
        self.lines
            .iter()
            .find(|l| {
                let cy = (l.rect[1] + l.rect[3]) / 2.0;
                cy < last[1] - 0.5 && cy > below && last[1] - cy < b.size.max(1.0) * 1.5 && l.rect[2] - l.rect[0] >= width * 0.5
            })
            .map(|l| (l.rect[3] - l.rect[1], l.color, last[1] - (l.rect[1] + l.rect[3]) / 2.0))
    }
}

/// A list item: its numbering instance (num id), level, the marker's and the text's left edges,
/// and how many characters of marker its text opens with.
type ListItem = (usize, usize, f64, f64, usize);

/// A list still open while a page's items are read: its kind, its level, its numbering instance
/// (num id) and the number an item continuing it would carry (`None` for bullets).
struct OpenList {
    kind: (String, char),
    level: usize,
    num: usize,
    next: Option<u32>,
}

/// The lists on a page: per block, its numbering (num id, level, marker x, text x) and how many
/// characters of marker to drop.
pub(crate) struct Lists {
    /// Kinds of list (an abstract numbering each): Word format or bullet, and the text after a number.
    pub kinds: Vec<(String, char)>,
    /// Numbering instances: (kind index, start number).
    pub nums: Vec<(usize, u32)>,
}

impl Lists {
    pub(crate) fn new() -> Self {
        Self { kinds: Vec::new(), nums: Vec::new() }
    }

    /// Assign the page's list items to numbering instances. Items of the same kind continue one
    /// instance until a paragraph that is not a list item comes between them.
    fn page(&mut self, page: &Page, order: &[usize]) -> Vec<Option<ListItem>> {
        let mut out = vec![None; page.blocks.len()];
        let found: Vec<(usize, Marker, usize)> = order
            .iter()
            .filter_map(|&bi| {
                let b = page.blocks.get(bi)?;
                (b.rotation.abs() <= 0.1).then_some(())?;
                marker(&b.text).map(|(m, n)| (bi, m, n))
            })
            .collect();
        if found.is_empty() {
            return out;
        }
        // Levels: the distinct marker indents on the page, left to right.
        let xs =
            clusters(found.iter().filter_map(|(bi, _, _)| page.blocks.get(*bi)).map(|b| b.rows().first().map_or(b.rect[0], |r| r[0])).collect(), 4.0);
        // Where each block comes in reading order.
        let position: std::collections::HashMap<usize, usize> = order.iter().enumerate().map(|(p, bi)| (*bi, p)).collect();
        let mut open: Vec<OpenList> = Vec::new();
        let mut last_pos: Option<usize> = None;
        for (bi, m, n) in found {
            let Some(b) = page.blocks.get(bi) else { continue };
            let pos = position.get(&bi).copied().unwrap_or(0);
            // A paragraph that is not an item between this one and the last closes the lists.
            if last_pos.is_some_and(|p| pos > p + 1) {
                open.clear();
            }
            last_pos = Some(pos);
            let rows = b.rows();
            let marker_x = rows.first().map_or(b.rect[0], |r| r[0]);
            let level = xs.iter().filter(|x| **x < marker_x - 4.0).count().min(8);
            // An item closes the lists nested deeper than it: the sub-list under the next item
            // starts again ("a." after "2.").
            open.retain(|o| o.level <= level);
            let kind = m.kind();
            let k = match self.kinds.iter().position(|x| *x == kind) {
                Some(k) => k,
                None => {
                    self.kinds.push(kind.clone());
                    self.kinds.len() - 1
                }
            };
            let value = if let Marker::Number(_, _, v) = m { Some(v) } else { None };
            // An item continues the open list of its kind and level when it carries the next
            // number (any bullet does); otherwise it starts one at its own number, so Word shows
            // the numbers the page shows.
            let continuing = open.iter_mut().find(|o| o.kind == kind && o.level == level).filter(|o| value == o.next);
            let num = match continuing {
                Some(o) => {
                    o.next = value.map(|v| v.saturating_add(1));
                    o.num
                }
                None => {
                    open.retain(|o| !(o.kind == kind && o.level == level));
                    self.nums.push((k, value.unwrap_or(1)));
                    let num = self.nums.len();
                    open.push(OpenList { kind, level, num, next: value.map(|v| v.saturating_add(1)) });
                    num
                }
            };
            // The text starts at the word after the marker on the first row, or where the second
            // row starts, or a typical hanging indent after the marker.
            let text_x = text_start(&b.runs(), n)
                .filter(|x| *x > marker_x)
                .or_else(|| rows.get(1).map(|r| r[0]).filter(|x| *x > marker_x + 1.0))
                .unwrap_or(marker_x + 18.0);
            if let Some(slot) = out.get_mut(bi) {
                *slot = Some((num, level, marker_x, text_x, n));
            }
        }
        out
    }

    /// `word/numbering.xml`, or `None` when the document has no lists.
    pub(crate) fn xml(&self) -> Option<String> {
        if self.nums.is_empty() {
            return None;
        }
        let mut s = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
        );
        for (k, (kind, suffix)) in self.kinds.iter().enumerate() {
            s.push_str(&format!("<w:abstractNum w:abstractNumId=\"{k}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>"));
            for level in 0..9 {
                let indent = 360 * (level + 1) + 360;
                let (fmt, text) = match kind.strip_prefix("bullet") {
                    Some(b) => ("bullet", esc(b)),
                    None => (kind.as_str(), format!("%{}{}", level + 1, esc(&suffix.to_string()))),
                };
                s.push_str(&format!(
                    "<w:lvl w:ilvl=\"{level}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{indent}\" w:hanging=\"360\"/></w:pPr></w:lvl>"
                ));
            }
            s.push_str("</w:abstractNum>");
        }
        for (i, (k, start)) in self.nums.iter().enumerate() {
            s.push_str(&format!("<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{k}\"/>", i + 1));
            // Every list starts at its own number. Word otherwise carries the count on from an
            // earlier list of the same kind (a sub-list under the next item would go on from "c.").
            for level in 0..9 {
                s.push_str(&format!("<w:lvlOverride w:ilvl=\"{level}\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride>"));
            }
            s.push_str("</w:num>");
        }
        s.push_str("</w:numbering>");
        Some(s)
    }
}

/// One table rebuilt from its borders: fixed column widths from the grid, rows at least as tall
/// as drawn, the border's width and colour, shaded cells, and each cell's paragraphs aligned as
/// they sit in it.
fn ruled_table_xml(t: &RuledTable, page: &Page, frame: &Frame, ratio: f64, rules: &Rules) -> String {
    let widths: Vec<i64> = t.xs.windows(2).map(|w| twips(w[1] - w[0]).max(60)).collect();
    let total: i64 = widths.iter().sum();
    // Cell padding: the smallest gap between a cell's left edge and its text.
    let mut pads: Vec<f64> = t
        .rows
        .iter()
        .flatten()
        .flat_map(|c| {
            c.blocks.iter().filter_map(|i| page.blocks.get(*i)).map(move |b| b.rows().iter().map(|r| r[0]).fold(f64::INFINITY, f64::min) - c.rect[0])
        })
        .filter(|g| g.is_finite() && *g >= 0.0)
        .collect();
    pads.sort_by(|a, b| a.total_cmp(b));
    let pad = pads.first().copied().unwrap_or(5.4).clamp(0.0, 20.0);
    // The right padding is what right-aligned text leaves; a point less, so text that set a
    // hair wider in the source's font still fits on its row.
    let right_pad = t
        .rows
        .iter()
        .flatten()
        .flat_map(|c| {
            c.blocks
                .iter()
                .filter_map(|i| page.blocks.get(*i))
                .map(move |b| c.rect[2] - b.rows().iter().map(|r| r[2]).fold(f64::NEG_INFINITY, f64::max))
        })
        .filter(|g| g.is_finite() && *g >= 0.0)
        .fold(pad, f64::min);
    let right_pad = (right_pad - 1.0).max(0.0);
    let (bw, bc) = t.border;
    let sz = (bw * 8.0).round().clamp(2.0, 96.0) as i64;
    let colour = hex(bc).unwrap_or_else(|| "000000".into());
    let mut s = format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"{total}\" w:type=\"dxa\"/><w:tblInd w:w=\"{}\" w:type=\"dxa\"/><w:tblBorders>",
        twips(t.rect[0] - frame.left)
    );
    for edge in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        s.push_str(&format!("<w:{edge} w:val=\"single\" w:sz=\"{sz}\" w:space=\"0\" w:color=\"{colour}\"/>"));
    }
    let (p, rp) = (twips(pad), twips(right_pad));
    s.push_str(&format!(
        "</w:tblBorders><w:tblLayout w:type=\"fixed\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"{p}\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"{rp}\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr><w:tblGrid>"
    ));
    for w in &widths {
        s.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
    }
    s.push_str("</w:tblGrid>");
    for (i, row) in t.rows.iter().enumerate() {
        let h = match (t.ys.get(i), t.ys.get(i + 1)) {
            (Some(a), Some(b)) => a - b,
            _ => 0.0,
        };
        // The row's height from rule to rule, less the border Word draws inside it.
        s.push_str(&format!("<w:tr><w:trPr><w:cantSplit/><w:trHeight w:val=\"{}\" w:hRule=\"atLeast\"/></w:trPr>", twips((h - bw).max(1.0))));
        let mut column = 0usize;
        for c in row {
            let w: i64 = widths.iter().skip(column).take(c.span).sum();
            column += c.span;
            s.push_str(&format!("<w:tc><w:tcPr><w:tcW w:w=\"{w}\" w:type=\"dxa\"/>"));
            if c.span > 1 {
                s.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", c.span));
            }
            match c.vmerge {
                Some(true) => s.push_str("<w:vMerge w:val=\"restart\"/>"),
                Some(false) => s.push_str("<w:vMerge/>"),
                None => {}
            }
            if let Some(h) = c.shade.and_then(hex).or_else(|| c.shade.map(|_| "000000".into())) {
                s.push_str(&format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{h}\"/>"));
            }
            s.push_str("</w:tcPr>");
            let (left, right) = (c.rect[0] + pad, c.rect[2] - right_pad);
            // Text starts under the top border, which takes its width from the cell.
            let mut prev_bottom = c.rect[3] - bw;
            let mut wrote = false;
            if c.vmerge != Some(false) {
                let single = c.blocks.len() == 1;
                for bi in &c.blocks {
                    let Some(b) = page.blocks.get(*bi) else { continue };
                    let mut line = line_height(b, ratio);
                    // One row alone in its cell: the line is no taller than the cell (tables
                    // are often set tighter than the body text the page's pitch comes from).
                    if single && b.rows().len() == 1 {
                        line = line.min(c.rect[3] - c.rect[1] - bw).max(1.0);
                    }
                    let (top, bottom) = line_box(b, line);
                    let (pp, a) = ppr(b, "", left, right, prev_bottom - top, line, None, Deco::default());
                    let runs = b.runs();
                    let underlined = |rect: [f64; 4], span: f64| rules.lines_of(rect, span);
                    let body = if matches!(a, Align::Centre | Align::Right) {
                        runs_with_breaks(b, &runs, &underlined)
                    } else {
                        runs_xml(&runs, &underlined)
                    };
                    s.push_str(&format!("<w:p>{pp}{body}</w:p>"));
                    prev_bottom = bottom;
                    wrote = true;
                }
            }
            if !wrote {
                s.push_str("<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\"/></w:pPr></w:p>");
            }
            s.push_str("</w:tc>");
        }
        s.push_str("</w:tr>");
    }
    s.push_str("</w:tbl>");
    s
}

/// One part of a flowing page that Word sets in its own section: one column, or several side
/// by side (each column's left and right edge).
pub(crate) struct Segment {
    pub xml: String,
    pub columns: Vec<[f64; 2]>,
    /// The part ends with a table (Word needs a paragraph after it before the section ends).
    pub ends_in_table: bool,
    /// Space to leave above the part, in the paragraph that ends the section before it
    /// (points): a section set in columns starts its columns together, so the gap they share
    /// goes above it rather than into its first paragraph.
    pub lead: f64,
}

/// Something set in the flow, with its extent (user space, y up).
enum Piece<'p> {
    Para(usize, u8),
    Table(Table),
    Ruled(usize),
    Img(&'p crate::Image),
}

struct Placed<'p> {
    top: f64,
    bottom: f64,
    /// Rows of text (a paragraph's; 0 for other pieces).
    rows: usize,
    x0: f64,
    x1: f64,
    piece: Piece<'p>,
}

/// Most pieces compared pairwise when looking for column gutters.
const MAX_GUTTER_PIECES: usize = 2_000;

/// Most columns a page is set in.
const MAX_COLUMNS: usize = 4;

/// The page's column gutters: gaps, at least 8 pt wide and away from the frame's edges, that no
/// piece set beside another crosses. Each gutter is its left and right edge. Only pieces with
/// another beside them (overlapping it vertically, apart from it horizontally) count, so a
/// title over the columns, or a short line in one, does not close a gutter.
fn gutters(placed: &[Placed], frame: &Frame) -> Vec<[f64; 2]> {
    let width = frame.right - frame.left;
    let beside =
        |p: &Placed| placed.iter().take(MAX_GUTTER_PIECES).any(|q| p.top.min(q.top) - p.bottom.max(q.bottom) > 0.0 && (q.x0 > p.x1 || q.x1 < p.x0));
    let mut spans: Vec<[f64; 2]> =
        placed.iter().take(MAX_GUTTER_PIECES).filter(|p| p.x1 - p.x0 <= width * 0.6 && beside(p)).map(|p| [p.x0, p.x1]).collect();
    if spans.len() < 2 {
        return Vec::new();
    }
    spans.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let mut out = Vec::new();
    let mut reach = f64::NEG_INFINITY;
    for s in spans {
        if reach > f64::NEG_INFINITY && s[0] - reach >= 8.0 && reach > frame.left + width * 0.15 && s[0] < frame.right - width * 0.15 {
            out.push([reach, s[0]]);
        }
        reach = reach.max(s[1]);
    }
    out.truncate(MAX_COLUMNS - 1);
    out
}

/// A part of a page: its columns' edges (none for one column) and each column's pieces.
type Part = (Vec<[f64; 2]>, Vec<Vec<usize>>);

/// Split the pieces (top to bottom) into segments: runs of pieces that each fit in one column
/// and use at least two columns are set in columns; everything else in one.
fn segments(placed: &[Placed], frame: &Frame) -> Vec<Part> {
    let g = gutters(placed, frame);
    let single = |items: Vec<usize>| (Vec::new(), vec![items]);
    if g.is_empty() {
        return vec![single((0..placed.len()).collect())];
    }
    // A piece is in the column left of each gutter whose middle it does not reach (a row of
    // one that runs a little into the gutter still belongs to its column).
    let column_of = |p: &Placed| -> Option<usize> {
        let mid = |gt: &[f64; 2]| (gt[0] + gt[1]) / 2.0;
        let k = g.iter().filter(|gt| p.x0 >= mid(gt)).count();
        let crosses = g.iter().any(|gt| p.x0 < mid(gt) && p.x1 > mid(gt));
        (!crosses).then_some(k)
    };
    let mut out: Vec<Part> = Vec::new();
    let mut run: Vec<(usize, usize)> = Vec::new();
    let mut plain: Vec<usize> = Vec::new();
    let flush_run = |run: &mut Vec<(usize, usize)>, plain: &mut Vec<usize>, out: &mut Vec<Part>| {
        let used: std::collections::BTreeSet<usize> = run.iter().map(|r| r.1).collect();
        // Columns stand side by side: some piece in each column overlaps one in the next
        // vertically (a right-aligned address above a left-aligned greeting does not).
        let side_by_side = used.iter().zip(used.iter().skip(1)).all(|(a, b)| {
            run.iter()
                .filter(|r| r.1 == *a)
                .filter_map(|r| placed.get(r.0))
                .any(|p| run.iter().filter(|r| r.1 == *b).filter_map(|r| placed.get(r.0)).any(|q| p.top.min(q.top) - p.bottom.max(q.bottom) > 0.0))
        });
        // Columns, not a table's cells: prose (a third of the paragraphs run over three rows or
        // more), or pieces that mostly do not line up with one in another column, as a table's
        // cells do row by row (a sidebar of short lines beside the main text).
        let paras = run.iter().filter_map(|r| placed.get(r.0)).filter(|p| matches!(p.piece, Piece::Para(..))).count();
        let prose = run.iter().filter_map(|r| placed.get(r.0)).filter(|p| p.rows >= 3).count();
        let aligned = run
            .iter()
            .take(2_000)
            .filter(|a| {
                run.iter().take(2_000).any(|b| b.1 != a.1 && placed.get(a.0).zip(placed.get(b.0)).is_some_and(|(p, q)| (p.top - q.top).abs() < 1.5))
            })
            .count();
        if used.len() >= 2 && run.len() >= 3 && side_by_side && (prose * 3 >= paras || aligned * 2 < run.len()) {
            if !plain.is_empty() {
                out.push(single(std::mem::take(plain)));
            }
            let n = g.len() + 1;
            let mut cols: Vec<Vec<usize>> = vec![Vec::new(); n];
            for (i, c) in run.drain(..) {
                if let Some(col) = cols.get_mut(c) {
                    col.push(i);
                }
            }
            let edges: Vec<[f64; 2]> = (0..n)
                .map(|c| {
                    let left = if c == 0 { frame.left } else { g.get(c - 1).map_or(frame.left, |gt| gt[1]) };
                    let right = g.get(c).map_or(frame.right, |gt| gt[0]);
                    [left, right]
                })
                .collect();
            out.push((edges, cols));
        } else {
            plain.extend(run.drain(..).map(|r| r.0));
        }
    };
    for (i, p) in placed.iter().enumerate() {
        match column_of(p) {
            Some(c) => run.push((i, c)),
            None => {
                flush_run(&mut run, &mut plain, &mut out);
                plain.push(i);
            }
        }
    }
    flush_run(&mut run, &mut plain, &mut out);
    if !plain.is_empty() {
        out.push(single(plain));
    }
    out
}

/// A paragraph that only takes a hairline (between tables, or holding a section's properties).
pub(crate) const HAIRLINE: &str = "<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"20\" w:lineRule=\"exact\"/></w:pPr></w:p>";

/// A flowing page as Word body XML, in segments ([`Segment`]), and the page's margins (left,
/// right, top, bottom in points). `media` collects the pictures to embed and `lists` the
/// numbering the page uses.
pub(crate) fn flow_page<'a>(
    page: &'a Page,
    ratio: f64,
    body: f64,
    media: &mut Vec<(String, &'a crate::Image)>,
    lists: &mut Lists,
) -> (Vec<Segment>, Option<[f64; 4]>) {
    let Ruled { tables: ruled, consumed, frames } = ruled_tables(page);
    let f = frame(page, ratio).map(|mut f| {
        for t in &ruled {
            f.left = f.left.min(t.rect[0]);
            f.right = f.right.max(t.rect[2]);
            f.top = f.top.max(t.rect[3]);
            f.bottom = f.bottom.min(t.rect[1]);
        }
        f
    });
    // No usable box (no finite row): the page's own, so the text is still written.
    let page_box = || {
        let (w, h) = (
            if page.width.is_finite() && page.width > 0.0 { page.width } else { 612.0 },
            if page.height.is_finite() && page.height > 0.0 { page.height } else { 792.0 },
        );
        Frame { left: 0.0, right: w, top: h, bottom: 0.0 }
    };
    let fallback = f.is_none();
    let frame = f.unwrap_or_else(page_box);
    let rules = Rules::new(page, &ruled, &frames, ratio);
    let plain_page =
        Page { blocks: page.blocks.iter().zip(&consumed).map(|(b, c)| if *c { Block::default() } else { b.clone() }).collect(), ..page.clone() };
    // Pieces top to bottom: what the ruled tables left. Columns are found first; the text-only
    // table finder then looks at each part set in one column.
    let mut placed: Vec<Placed> = Vec::new();
    for (bi, b) in page.blocks.iter().enumerate() {
        if consumed.get(bi).copied().unwrap_or(false) || b.text.trim().is_empty() {
            continue;
        }
        let rows = b.rows();
        let x0 = rows.iter().map(|r| r[0]).fold(f64::INFINITY, f64::min);
        let x1 = rows.iter().map(|r| r[2]).fold(f64::NEG_INFINITY, f64::max);
        placed.push(Placed { top: b.rect[3], bottom: b.rect[1], rows: rows.len(), x0, x1, piece: Piece::Para(bi, crate::level(b, body)) });
    }
    placed.extend(ruled.iter().enumerate().map(|(i, t)| Placed {
        top: t.rect[3],
        bottom: t.rect[1],
        rows: 0,
        x0: t.rect[0],
        x1: t.rect[2],
        piece: Piece::Ruled(i),
    }));
    placed.extend(page.images.iter().map(|im| Placed {
        top: im.rect[3],
        bottom: im.rect[1],
        rows: 0,
        x0: im.rect[0],
        x1: im.rect[2],
        piece: Piece::Img(im),
    }));
    placed.sort_by(|a, b| b.top.total_cmp(&a.top).then(a.x0.total_cmp(&b.x0)));
    let mut segs = segments(&placed, &frame);
    for (edges, cols) in segs.iter_mut() {
        if !edges.is_empty() {
            continue;
        }
        let Some(items) = cols.first_mut() else { continue };
        let mut blocks = vec![Block::default(); plain_page.blocks.len()];
        for i in items.iter() {
            if let Some(Placed { piece: Piece::Para(bi, _), .. }) = placed.get(*i)
                && let (Some(slot), Some(b)) = (blocks.get_mut(*bi), plain_page.blocks.get(*bi))
            {
                *slot = b.clone();
            }
        }
        let (found, taken) = crate::tables(&blocks);
        if found.is_empty() {
            continue;
        }
        items.retain(|i| !matches!(placed.get(*i), Some(Placed { piece: Piece::Para(bi, _), .. }) if taken.get(*bi).copied().unwrap_or(false)));
        for t in found {
            items.push(placed.len());
            placed.push(Placed { top: t.rect[3], bottom: t.rect[1], rows: 0, x0: t.rect[0], x1: t.rect[2], piece: Piece::Table(t) });
        }
        items.sort_by(|a, b| {
            let (pa, pb) = (placed.get(*a), placed.get(*b));
            let key = |p: Option<&Placed>| p.map_or((0.0, 0.0), |p| (p.top, p.x0));
            let (ka, kb) = (key(pa), key(pb));
            kb.0.total_cmp(&ka.0).then(ka.1.total_cmp(&kb.1))
        });
    }
    // Reading order: segment by segment, column by column.
    let order: Vec<usize> = segs
        .iter()
        .flat_map(|(_, cols)| cols.iter().flatten())
        .filter_map(|i| placed.get(*i))
        .filter_map(|p| if let Piece::Para(bi, _) = p.piece { Some(bi) } else { None })
        .collect();
    let list_items = lists.page(page, &order);
    let mut out = Vec::new();
    let mut prev_bottom = frame.top;
    for (edges, cols) in segs {
        // Columns share the gap above their highest first piece: it goes above the section.
        let lead = if cols.len() > 1 {
            cols.iter()
                .filter_map(|col| col.first().and_then(|i| placed.get(*i)))
                .map(|p| match &p.piece {
                    Piece::Para(bi, _) => page.blocks.get(*bi).map_or(p.top, |b| line_box(b, line_height(b, ratio)).0),
                    _ => p.top,
                })
                .map(|top| prev_bottom - top)
                .fold(f64::INFINITY, f64::min)
                .clamp(0.0, 1584.0)
        } else {
            0.0
        };
        let start = prev_bottom - lead;
        let mut xml = String::new();
        let mut lowest = prev_bottom;
        let mut after_table = false;
        let n = cols.len();
        for (c, col) in cols.iter().enumerate() {
            let [left, right] = edges.get(c).copied().unwrap_or([frame.left, frame.right]);
            let mut s = String::new();
            prev_bottom = start;
            after_table = false;
            for (k, i) in col.iter().enumerate() {
                let Some(p) = placed.get(*i) else { continue };
                let below = col.get(k + 1).and_then(|j| placed.get(*j)).map_or(f64::NEG_INFINITY, |p| p.top);
                let cframe = Frame { left, right, ..frame };
                match &p.piece {
                    Piece::Para(bi, lvl) => {
                        let Some(b) = page.blocks.get(*bi) else { continue };
                        let line = line_height(b, ratio);
                        let (top, bottom) = line_box(b, line);
                        let style = match lvl {
                            1 => "<w:pStyle w:val=\"Heading1\"/>",
                            2 => "<w:pStyle w:val=\"Heading2\"/>",
                            _ => "",
                        };
                        // A numbered heading keeps its number as text: it is not a list.
                        let item = list_items.get(*bi).copied().flatten().filter(|_| *lvl == 0);
                        let boxed = rules.boxed(b);
                        let deco = Deco { under: if boxed.is_some() { None } else { rules.under(b, below) }, boxed, shade: rules.shade(b) };
                        // The first paragraph in a box starts with the box's top line and the
                        // space under it; the last ends with the space and line under it.
                        let in_box = |p: &Placed| match p.piece {
                            Piece::Para(o, _) => page.blocks.get(o).is_some_and(|o| rules.boxed(o).map(|x| x.rect) == boxed.map(|x| x.rect)),
                            _ => false,
                        };
                        let first_in_box =
                            boxed.is_some() && !k.checked_sub(1).and_then(|j| col.get(j)).and_then(|j| placed.get(*j)).is_some_and(in_box);
                        let last_in_box = boxed.is_some() && !col.get(k + 1).and_then(|j| placed.get(*j)).is_some_and(in_box);
                        let before = match boxed {
                            Some(bx) if first_in_box => prev_bottom - bx.rect[3] - bx.width / 2.0,
                            _ => prev_bottom - top,
                        };
                        let (pp, a) = ppr(b, style, left, right, before, line, item.map(|(n, l, mx, tx, _)| (n, l, mx, tx)), deco);
                        let mut runs = b.runs();
                        if let Some((_, _, _, _, n)) = item {
                            runs = drop_chars(&runs, n);
                        }
                        let underlined = |rect: [f64; 4], span: f64| rules.lines_of(rect, span);
                        let text = if matches!(a, Align::Centre | Align::Right) && item.is_none() {
                            runs_with_breaks(b, &runs, &underlined)
                        } else {
                            runs_xml(&runs, &underlined)
                        };
                        s.push_str(&format!("<w:p>{pp}{text}</w:p>"));
                        prev_bottom = match boxed {
                            Some(bx) if last_in_box => bx.rect[1] - bx.width / 2.0,
                            _ => bottom - deco.under.map_or(0.0, |(w, _, gap)| gap + w),
                        };
                        after_table = false;
                    }
                    Piece::Table(t) => {
                        if after_table {
                            s.push_str(HAIRLINE);
                        }
                        spacer(&mut s, prev_bottom - t.rect[3]);
                        s.push_str(&crate::docx_table(t, t.rect[0] - left));
                        prev_bottom = t.rect[1];
                        after_table = true;
                    }
                    Piece::Ruled(i) => {
                        let Some(t) = ruled.get(*i) else { continue };
                        if after_table {
                            s.push_str(HAIRLINE);
                        }
                        // Tables have no space before: an empty paragraph that tall stands in.
                        spacer(&mut s, prev_bottom - t.rect[3]);
                        s.push_str(&ruled_table_xml(t, page, &cframe, ratio, &rules));
                        prev_bottom = t.rect[1];
                        after_table = true;
                    }
                    Piece::Img(im) => {
                        if !room_for(media, im) {
                            continue;
                        }
                        let n = media.len() + 1;
                        let name = format!("image{n}.{}", im.ext);
                        let width = (right - left).max(1.0);
                        let (w, h) = ((im.rect[2] - im.rect[0]).max(1.0), (im.rect[3] - im.rect[1]).max(1.0));
                        let k = (width / w).min(1.0);
                        let (cx, cy) = (emu(w * k), emu(h * k));
                        let (gl, gr) = (im.rect[0] - left, right - im.rect[2]);
                        let jc = if gl > 2.0 && (gl - gr).abs() < width * 0.05 {
                            "<w:jc w:val=\"center\"/>"
                        } else if gl > 2.0 && gr < 2.0 {
                            "<w:jc w:val=\"right\"/>"
                        } else {
                            ""
                        };
                        let ind = if jc.is_empty() && gl > 0.5 { format!("<w:ind w:left=\"{}\"/>", twips(gl)) } else { String::new() };
                        s.push_str(&format!(
                            "<w:p><w:pPr><w:spacing w:before=\"{}\" w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/>{ind}{jc}</w:pPr><w:r><w:drawing><wp:inline><wp:extent cx=\"{cx}\" cy=\"{cy}\"/>",
                            twips((prev_bottom - im.rect[3]).max(0.0))
                        ));
                        s.push_str(&picture_xml(n, &name, (cx, cy), None));
                        s.push_str("</wp:inline></w:drawing></w:r></w:p>");
                        media.push((name, im));
                        prev_bottom = im.rect[1];
                        after_table = false;
                    }
                }
            }
            // A column break ends every column but the last, in a paragraph of its own one point
            // tall: its mark, which follows the break into the next column, takes no more. (A
            // break at the end of the column's last paragraph would carry a whole empty line.)
            if c + 1 < n {
                s.push_str("<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"20\" w:lineRule=\"exact\"/><w:rPr><w:sz w:val=\"2\"/></w:rPr></w:pPr><w:r><w:rPr><w:sz w:val=\"2\"/></w:rPr><w:br w:type=\"column\"/></w:r></w:p>");
                after_table = false;
            }
            lowest = lowest.min(prev_bottom);
            xml.push_str(&s);
        }
        prev_bottom = lowest;
        out.push(Segment { xml, columns: if n > 1 { edges } else { Vec::new() }, ends_in_table: after_table, lead });
    }
    // Margins from where the content sits, when it sits on the page; content partly off it
    // (a wrongly sized page) keeps the default margins.
    let on_page = frame.left >= -1.0 && frame.bottom >= -1.0 && frame.right <= page.width + 1.0 && frame.top <= page.height + 1.0;
    let margins = [frame.left, page.width - frame.right, page.height - frame.top, frame.bottom];
    // A page with no content of its own keeps the default margins.
    (out, (on_page && !fallback).then_some(margins))
}

/// An empty paragraph `gap` points tall (nothing for less than a point).
fn spacer(s: &mut String, gap: f64) {
    if gap > 1.0 {
        s.push_str(&format!(
            "<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"{}\" w:lineRule=\"exact\"/></w:pPr></w:p>",
            twips(gap.min(1584.0)).max(20)
        ));
    }
}

/// `w:cols` for a section set in `columns` (each one's left and right edge).
pub(crate) fn cols_xml(columns: &[[f64; 2]]) -> String {
    if columns.len() < 2 {
        return String::new();
    }
    let mut s = format!("<w:cols w:num=\"{}\" w:equalWidth=\"0\">", columns.len());
    for (i, c) in columns.iter().enumerate() {
        let space = columns.get(i + 1).map_or(0.0, |n| n[0] - c[1]);
        s.push_str(&format!("<w:col w:w=\"{}\" w:space=\"{}\"/>", twips((c[1] - c[0]).max(1.0)), twips(space.max(0.0))));
    }
    s.push_str("</w:cols>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{Image, docx, html, tests::part};

    /// A paragraph of `rows` rows of 11 pt text from `x0` to `x1`, its first row's top at
    /// `top`, 13 pt apart; the last row ends at `last` (ragged) or at `x1`.
    fn para(text: &str, x0: f64, x1: f64, top: f64, rows: usize, last: f64) -> Block {
        let lines: Vec<[f64; 4]> = (0..rows)
            .map(|i| {
                let t = top - 13.0 * i as f64;
                [x0, t - 11.0, if i + 1 == rows { last } else { x1 }, t]
            })
            .collect();
        let rect = lines.iter().copied().reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]).unwrap_or_default();
        Block { text: text.into(), rect, size: 11.0, lines, font: "Liberation Serif".into(), ..Block::default() }
    }

    fn page(blocks: Vec<Block>, shapes: Vec<Shape>) -> Page {
        Page { width: 612.0, height: 792.0, blocks, images: Vec::<Image>::new(), background: None, shapes }
    }

    fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> Shape {
        Shape { rect: [x0.min(x1) - 0.25, y0.min(y1) - 0.25, x0.max(x1) + 0.25, y0.max(y1) + 0.25], color: [0.0; 3], stroked: true }
    }

    /// A table drawn with borders comes back as a Word table with those borders, its shaded
    /// header and the cell that spans two columns where a border is missing; HTML gets the table.
    #[test]
    fn a_ruled_grid_becomes_a_bordered_table() {
        let mut shapes = vec![Shape { rect: [100.0, 680.0, 400.0, 700.0], color: [0.85, 0.89, 0.95], stroked: false }];
        for y in [700.0, 680.0, 660.0] {
            shapes.push(line(100.0, y, 400.0, y));
        }
        for x in [100.0, 300.0, 400.0] {
            shapes.push(line(x, 660.0, x, 700.0));
        }
        // The border between the first two columns is drawn in the header row only.
        shapes.push(line(200.0, 680.0, 200.0, 700.0));
        let cell = |t: &str, x: f64, top: f64| Block { size: 10.0, ..para(t, x, x + 40.0, top, 1, x + 40.0) };
        let blocks = vec![
            cell("Item", 105.0, 696.0),
            cell("Code", 205.0, 696.0),
            cell("Qty", 305.0, 696.0),
            cell("Wide widget", 105.0, 676.0),
            cell("4", 305.0, 676.0),
        ];
        let p = page(blocks, shapes);
        let xml = part(&docx(std::slice::from_ref(&p), "Grid"), "word/document.xml");
        assert_eq!(xml.matches("<w:tbl>").count(), 1, "{xml}");
        assert!(xml.contains("<w:insideV w:val=\"single\""), "the drawn borders: {xml}");
        assert_eq!(xml.matches("w:fill=\"D9E3F2\"").count(), 3, "the shaded header cells: {xml}");
        assert_eq!(xml.matches("<w:gridCol ").count(), 3, "{xml}");
        assert!(xml.contains("<w:gridSpan w:val=\"2\"/>"), "the cell without a border between: {xml}");
        let (item, code, wide) = (xml.find(">Item<"), xml.find(">Code<"), xml.find(">Wide widget<"));
        assert!(item.is_some() && item < code && code < wide, "cells in reading order: {xml}");
        assert!(html(&[p], "Grid").contains("<td colspan=\"2\">Wide widget</td>"));
    }

    /// Two columns of prose side by side are set in a two-column section, the left column
    /// first; a page of one column has no columns at all.
    #[test]
    fn side_by_side_prose_is_set_in_columns() {
        let long = "word ".repeat(30);
        let blocks = vec![
            para(&format!("Left one {long}"), 72.0, 290.0, 700.0, 5, 200.0),
            para(&format!("Right one {long}"), 320.0, 540.0, 700.0, 5, 250.0),
            para(&format!("Left two {long}"), 72.0, 290.0, 620.0, 5, 230.0),
            para(&format!("Right two {long}"), 320.0, 540.0, 620.0, 4, 300.0),
        ];
        let xml = part(&docx(&[page(blocks.clone(), Vec::new())], "Cols"), "word/document.xml");
        assert!(xml.contains("<w:cols w:num=\"2\""), "{xml}");
        assert_eq!(xml.matches("w:type=\"column\"").count(), 1, "{xml}");
        let at = |t: &str| xml.find(t);
        assert!(at(">Left one") < at(">Left two") && at(">Left two") < at(">Right one") && at(">Right one") < at(">Right two"), "{xml}");
        let single: Vec<Block> = blocks.into_iter().filter(|b| b.text.starts_with("Left")).collect();
        assert!(!part(&docx(&[page(single, Vec::new())], "One"), "word/document.xml").contains("<w:cols"));
    }

    /// Bullets and numbers become Word lists: numbering definitions, items that refer to them,
    /// and text without the marker.
    #[test]
    fn markers_become_word_lists() {
        let item = |t: &str, top: f64| {
            let mut b = para(t, 90.0, 300.0, top, 1, 300.0);
            let (marker, rest) = t.split_once(' ').unwrap_or((t, ""));
            b.runs = vec![
                Run { text: format!("{marker} "), size: 11.0, rect: [90.0, top - 11.0, 96.0, top], ..Run::default() },
                Run { text: rest.into(), size: 11.0, rect: [108.0, top - 11.0, 300.0, top], ..Run::default() },
            ];
            b
        };
        let p = page(
            vec![item("\u{F0B7} First item", 700.0), item("\u{F0B7} Second item", 687.0), item("1. One", 650.0), item("2. Two", 637.0)],
            Vec::new(),
        );
        let zip = docx(&[p], "Lists");
        let numbering = part(&zip, "word/numbering.xml");
        assert!(numbering.contains("<w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"•\"/>"), "{numbering}");
        assert!(numbering.contains("<w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/>"), "{numbering}");
        let xml = part(&zip, "word/document.xml");
        assert_eq!(xml.matches("<w:numPr>").count(), 4, "{xml}");
        assert!(xml.contains(">First item<") && !xml.contains("\u{F0B7}") && !xml.contains(">1. "), "{xml}");
        assert!(xml.contains("w:hanging=\"360\""), "the text starts 18 pt after the marker: {xml}");
        assert!(part(&zip, "word/_rels/document.xml.rels").contains("Target=\"numbering.xml\""));
    }

    /// Alignment comes from where the rows sit: a short centred row, a right-aligned one, and
    /// rows that all reach the right edge but the last are justified.
    #[test]
    fn alignment_follows_the_rows() {
        let blocks = vec![
            para("Full width body text that sets the text area of the page.", 72.0, 540.0, 720.0, 1, 540.0),
            para("Centred", 280.0, 332.0, 690.0, 1, 332.0),
            para("Right", 500.0, 540.0, 670.0, 1, 540.0),
            para(&"justified ".repeat(30), 72.0, 540.0, 640.0, 3, 300.0),
        ];
        let xml = part(&docx(&[page(blocks, Vec::new())], "Align"), "word/document.xml");
        let jc = |t: &str| {
            xml.split(t)
                .next()
                .and_then(|b| b.rsplit("<w:p>").next())
                .map(|p| p.split("<w:jc w:val=\"").nth(1).and_then(|j| j.split('"').next()).unwrap_or("left").to_string())
        };
        assert_eq!(jc(">Centred<").as_deref(), Some("center"), "{xml}");
        assert_eq!(jc(">Right<").as_deref(), Some("right"), "{xml}");
        assert_eq!(jc(">justified ").as_deref(), Some("both"), "{xml}");
        assert_eq!(jc(">Full width").as_deref(), Some("left"), "{xml}");
    }

    /// A line under one word of a run underlines that word alone; one through it strikes it out.
    #[test]
    fn lines_under_and_through_words_mark_only_those_words() {
        let mut b = para("plain linked struck words", 72.0, 300.0, 700.0, 1, 300.0);
        let words = [[72.0, 689.0, 100.0, 700.0], [104.0, 689.0, 140.0, 700.0], [144.0, 689.0, 180.0, 700.0], [184.0, 689.0, 220.0, 700.0]];
        b.runs = vec![Run { text: b.text.clone(), size: 11.0, rect: b.rect, words: words.to_vec(), ..Run::default() }];
        let shapes = vec![line(104.0, 690.0, 140.0, 690.0), line(144.0, 695.0, 180.0, 695.0)];
        let xml = part(&docx(&[page(vec![b], shapes)], "Lines"), "word/document.xml");
        let run_of = |w: &str| xml.split(&format!(">{w}")).next().and_then(|r| r.rsplit("<w:r>").next()).unwrap_or_default().to_string();
        assert!(run_of("linked ").contains("<w:u ") && !run_of("linked ").contains("<w:strike/>"), "{xml}");
        assert!(run_of("struck ").contains("<w:strike/>") && !run_of("struck ").contains("<w:u "), "{xml}");
        assert!(!run_of("plain ").contains("<w:u ") && !run_of("plain ").contains("<w:strike/>"), "{xml}");
    }

    /// Hostile geometry (non-finite rows, shapes and run boxes) neither panics nor reaches the
    /// document as a number Word can't read.
    #[test]
    fn non_finite_geometry_stays_out_of_the_document() {
        let mut b = para("Text", 72.0, 300.0, 700.0, 2, 200.0);
        b.lines.push([f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::NAN]);
        b.runs = vec![Run { text: "Text".into(), size: f64::NAN, rect: [f64::NAN; 4], words: vec![[f64::INFINITY; 4]], ..Run::default() }];
        let shapes = vec![
            Shape { rect: [f64::NAN, 0.0, f64::INFINITY, 1.0], color: [f64::NAN; 3], stroked: true },
            line(f64::NEG_INFINITY, 650.0, 300.0, 650.0),
        ];
        let blocks = vec![b.clone(), Block { rect: [f64::NAN; 4], ..b }];
        let xml = part(&docx(&[page(blocks, shapes)], "Hostile"), "word/document.xml");
        assert!(!xml.contains("NaN") && !xml.contains("inf"), "{xml}");
    }

    /// Hostile pages stay quick to export: a ruled grid over many filled boxes, a box around
    /// thousands of paragraphs, and one row of tens of thousands of words.
    #[test]
    fn hostile_pages_export_quickly() {
        let started = std::time::Instant::now();
        let mut shapes: Vec<Shape> = (0..=500).map(|i| line(10.0, 10.0 + i as f64 * 1.6, 600.0, 10.0 + i as f64 * 1.6)).collect();
        shapes.extend((0..=63).map(|j| line(10.0 + j as f64 * 9.2, 10.0, 10.0 + j as f64 * 9.2, 810.0)));
        shapes.extend(std::iter::repeat_n(Shape { rect: [11.0, 11.0, 12.0, 12.0], color: [0.5; 3], stroked: false }, 20_000));
        let grid =
            Page { height: 812.0, ..page(vec![Block { text: "x".into(), rect: [12.0, 12.0, 14.0, 14.0], size: 1.0, ..Block::default() }], shapes) };
        let _ = (docx(std::slice::from_ref(&grid), "Grid"), html(&[grid], "Grid"));

        let boxed: Vec<Block> = (0..8000)
            .map(|i| {
                let top = 700.0 - i as f64 * 0.08;
                Block {
                    text: format!("Paragraph {i}."),
                    rect: [72.0, top - 0.06, 500.0, top],
                    size: 0.06,
                    lines: vec![[72.0, top - 0.06, 500.0, top]],
                    ..Block::default()
                }
            })
            .collect();
        let frame =
            vec![line(36.0, 36.0, 576.0, 36.0), line(36.0, 756.0, 576.0, 756.0), line(36.0, 36.0, 36.0, 756.0), line(576.0, 36.0, 576.0, 756.0)];
        let _ = docx(&[page(boxed, frame)], "Box");

        let n = 80_000;
        let words: Vec<[f64; 4]> = (0..n).map(|i| [72.0 + i as f64, 700.0, 72.3 + i as f64, 701.0]).collect();
        let text = vec!["a"; n].join(" ");
        let run = Run { text: text.clone(), size: 1.0, rect: [72.0, 700.0, 572.0, 701.0], words, ..Run::default() };
        let long = Block {
            text,
            rect: [72.0, 700.0, 572.0, 701.0],
            size: 1.0,
            lines: vec![[72.0, 700.0, 572.0, 701.0]],
            runs: vec![run],
            ..Block::default()
        };
        let _ = docx(&[page(vec![long], Vec::new())], "Words");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    /// A border around the page with a rule under its header is a frame, not a table: the
    /// paragraphs inside stay paragraphs in every format.
    #[test]
    fn a_page_border_with_a_header_rule_is_not_a_table() {
        let shapes = vec![
            line(36.0, 36.0, 576.0, 36.0),
            line(36.0, 756.0, 576.0, 756.0),
            line(36.0, 36.0, 36.0, 756.0),
            line(576.0, 36.0, 576.0, 756.0),
            line(36.0, 720.0, 576.0, 720.0),
        ];
        let mut blocks = vec![para("Report title", 72.0, 300.0, 748.0, 1, 300.0)];
        blocks.extend((0..8).map(|i| para(&format!("Paragraph {i} of the body."), 72.0, 540.0, 700.0 - 40.0 * i as f64, 2, 300.0)));
        let p = page(blocks, shapes);
        let xml = part(&docx(std::slice::from_ref(&p), "Framed"), "word/document.xml");
        assert!(!xml.contains("<w:tbl>"), "{xml}");
        let h = html(&[p], "Framed");
        assert!(!h.contains("<table>") && h.contains("<p>Paragraph 0 of the body.</p>"), "{h}");
    }

    /// A numbered item set in one style with a bold word in it starts its text where the word
    /// after the number does, not at the bold word.
    #[test]
    fn a_list_item_with_a_bold_word_keeps_its_hanging_indent() {
        let mut b = para("1. Item with bold word", 72.0, 400.0, 700.0, 1, 400.0);
        let row = |x0: f64, x1: f64| [x0, 689.0, x1, 700.0];
        b.runs = vec![
            Run {
                text: "1. Item with ".into(),
                size: 11.0,
                rect: row(72.0, 150.0),
                words: vec![row(72.0, 80.0), row(90.0, 115.0), row(118.0, 140.0)],
                ..Run::default()
            },
            Run { text: "bold".into(), size: 11.0, bold: true, rect: row(143.0, 165.0), words: vec![row(143.0, 165.0)], ..Run::default() },
            Run { text: " word".into(), size: 11.0, rect: row(168.0, 195.0), words: vec![row(168.0, 195.0)], ..Run::default() },
        ];
        let xml = part(
            &docx(&[page(vec![b, para("Body text long enough to set the page's text area.", 72.0, 540.0, 650.0, 1, 540.0)], Vec::new())], "Item"),
            "word/document.xml",
        );
        assert!(xml.contains("w:left=\"360\" w:hanging=\"360\""), "the text starts 18 pt after the number: {xml}");
    }

    /// The numbers Word shows are the page's: a sub-list under the next item starts again at
    /// "a.", and a list whose numbers jump carries on from the jump.
    #[test]
    fn list_numbers_follow_the_page() {
        let item = |t: &str, x: f64, top: f64| para(t, x, 400.0, top, 1, 400.0);
        let blocks = vec![
            item("1. One", 72.0, 700.0),
            item("a. Sub one", 100.0, 687.0),
            item("b. Sub two", 100.0, 674.0),
            item("2. Two", 72.0, 661.0),
            item("a. Sub again", 100.0, 648.0),
            item("5. Five", 72.0, 635.0),
            para("Body text long enough to set the page's text area for the lists.", 72.0, 540.0, 600.0, 1, 540.0),
        ];
        let zip = docx(&[page(blocks, Vec::new())], "Nested");
        let xml = part(&zip, "word/document.xml");
        let num = |t: &str| {
            xml.split(&format!(">{t}<"))
                .next()
                .and_then(|p| p.rsplit("<w:numId w:val=\"").next())
                .and_then(|v| v.split('"').next())
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(num("One"), num("Two"), "{xml}");
        assert_eq!(num("Sub one"), num("Sub two"), "{xml}");
        assert_ne!(num("Sub again"), num("Sub one"), "the second sub-list starts again: {xml}");
        assert_ne!(num("Five"), num("Two"), "the jump to 5 starts a list at 5: {xml}");
        let numbering = part(&zip, "word/numbering.xml");
        let five = numbering
            .split(&format!("<w:num w:numId=\"{}\">", num("Five")))
            .nth(1)
            .and_then(|n| n.split("</w:num>").next())
            .unwrap_or_default()
            .to_string();
        assert!(five.contains("<w:startOverride w:val=\"5\"/>"), "{numbering}");
        // Word carries a count on across lists of one kind unless told to restart: the second
        // sub-list restarts at 1.
        let again = numbering
            .split(&format!("<w:num w:numId=\"{}\">", num("Sub again")))
            .nth(1)
            .and_then(|n| n.split("</w:num>").next())
            .unwrap_or_default()
            .to_string();
        assert!(again.contains("<w:startOverride w:val=\"1\"/>"), "{numbering}");
    }

    /// Tables drawn with borders stay tables when they have one column, or a cell holding
    /// several lines; a page border with a header rule and a rule down the middle frames the
    /// text instead, and draws no boxes of its own.
    #[test]
    fn ruled_tables_and_frames_are_told_apart() {
        // One column, three rows.
        let mut shapes: Vec<Shape> = [700.0, 680.0, 660.0, 640.0].iter().map(|y| line(100.0, *y, 300.0, *y)).collect();
        shapes.extend([line(100.0, 640.0, 100.0, 700.0), line(300.0, 640.0, 300.0, 700.0)]);
        let rows: Vec<Block> =
            ["Row one", "Row two", "Row three"].iter().enumerate().map(|(i, t)| para(t, 105.0, 200.0, 696.0 - 20.0 * i as f64, 1, 200.0)).collect();
        let xml = part(&docx(&[page(rows, shapes)], "One column"), "word/document.xml");
        assert!(xml.contains("<w:tbl>") && !xml.contains("<w:pBdr>"), "{xml}");

        // Two by two, the second cell holding six lines, on a page with little else.
        let mut shapes: Vec<Shape> = [700.0, 680.0, 580.0].iter().map(|y| line(100.0, *y, 400.0, *y)).collect();
        shapes.extend([100.0, 200.0, 400.0].iter().map(|x| line(*x, 580.0, *x, 700.0)));
        let mut blocks = vec![
            para("Name", 105.0, 150.0, 696.0, 1, 150.0),
            para("Notes", 205.0, 250.0, 696.0, 1, 250.0),
            para("Ada", 105.0, 150.0, 676.0, 1, 150.0),
        ];
        blocks.extend((0..6).map(|i| para(&format!("Note line {i}"), 205.0, 300.0, 676.0 - 15.0 * i as f64, 1, 300.0)));
        blocks.push(para("A closing line.", 72.0, 300.0, 500.0, 1, 300.0));
        let xml = part(&docx(&[page(blocks, shapes)], "Notes"), "word/document.xml");
        assert!(xml.contains("<w:tbl>"), "{xml}");

        // A page border, a rule under the header and a rule down the middle.
        let shapes = vec![
            line(36.0, 36.0, 576.0, 36.0),
            line(36.0, 756.0, 576.0, 756.0),
            line(36.0, 36.0, 36.0, 756.0),
            line(576.0, 36.0, 576.0, 756.0),
            line(36.0, 720.0, 576.0, 720.0),
            line(306.0, 36.0, 306.0, 720.0),
        ];
        let mut blocks = vec![para("Report title", 72.0, 300.0, 748.0, 1, 300.0)];
        // Columns of prose: paragraphs of four rows, the right column's starting lower.
        for (side, first) in [(72.0, 700.0), (330.0, 690.0)] {
            blocks.extend((0..8).map(|i| para(&format!("Paragraph {i} at {side}."), side, side + 220.0, first - 70.0 * i as f64, 4, side + 100.0)));
        }
        let p = page(blocks, shapes);
        let xml = part(&docx(std::slice::from_ref(&p), "Framed"), "word/document.xml");
        assert!(!xml.contains("<w:tbl>") && !xml.contains("<w:pBdr>"), "{xml}");
        assert!(!html(&[p], "Framed").contains("<table>"));
    }

    /// A list marker drawn in pieces, or without word boxes of its own, still puts the text
    /// where its first word starts.
    #[test]
    fn list_text_starts_at_its_first_word_however_the_marker_is_drawn() {
        let row = |x0: f64, x1: f64| [x0, 689.0, x1, 700.0];
        let body = || para("Body text long enough to set the page's text area for the item.", 72.0, 540.0, 650.0, 1, 540.0);
        let hanging = |runs: Vec<Run>| {
            let mut b = para("1. Item text", 72.0, 300.0, 700.0, 1, 300.0);
            b.runs = runs;
            let xml = part(&docx(&[page(vec![b, body()], Vec::new())], "Item"), "word/document.xml");
            xml.split("w:hanging=\"").nth(1).and_then(|h| h.split('"').next()).unwrap_or_default().to_string()
        };
        // "1" and "." drawn touching, then the words.
        let pieces = vec![Run {
            text: "1. Item text".into(),
            size: 11.0,
            rect: row(72.0, 140.0),
            words: vec![row(72.0, 77.0), row(77.0, 80.0), row(90.0, 112.0), row(115.0, 140.0)],
            ..Run::default()
        }];
        assert_eq!(hanging(pieces), "360");
        // The marker's run has no word boxes; the text's has.
        let split = vec![
            Run { text: "1. ".into(), size: 11.0, rect: row(72.0, 80.0), ..Run::default() },
            Run { text: "Item text".into(), size: 11.0, rect: row(90.0, 140.0), words: vec![row(90.0, 112.0), row(115.0, 140.0)], ..Run::default() },
        ];
        assert_eq!(hanging(split), "360");
    }

    /// A box (one ruled rectangle) around several paragraphs keeps its border; a ruled table
    /// whose cells hold an address each stays a bordered table; a framed page with only a few
    /// paragraphs is not a table, and a real table inside it stays one.
    #[test]
    fn boxes_address_tables_and_short_framed_pages() {
        let ln = |x0: f64, y0: f64, x1: f64, y1: f64| line(x0, y0, x1, y1);
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| vec![ln(x0, y0, x1, y0), ln(x0, y1, x1, y1), ln(x0, y0, x0, y1), ln(x1, y0, x1, y1)];
        let body = |n: usize, x: f64, top: f64| -> Vec<Block> {
            (0..n).map(|i| para(&format!("Line {i} at {x}"), x, x + 150.0, top - 14.0 * i as f64, 1, x + 150.0)).collect()
        };

        // A sidebar box around five paragraphs.
        let mut blocks = body(5, 110.0, 690.0);
        blocks.push(para("Text outside the box, long enough to set the page's text area.", 72.0, 540.0, 500.0, 1, 540.0));
        let xml = part(&docx(&[page(blocks, rect(100.0, 610.0, 300.0, 700.0))], "Box"), "word/document.xml");
        assert_eq!(xml.matches("<w:pBdr>").count(), 5, "{xml}");

        // Sender | recipient, four lines each, ruled.
        let mut shapes = rect(100.0, 620.0, 500.0, 700.0);
        shapes.push(ln(300.0, 620.0, 300.0, 700.0));
        let mut blocks = body(4, 105.0, 695.0);
        blocks.extend(body(4, 305.0, 695.0));
        blocks.push(para("A closing paragraph below the addresses.", 72.0, 540.0, 500.0, 1, 540.0));
        let xml = part(&docx(&[page(blocks, shapes)], "Addresses"), "word/document.xml");
        assert!(xml.contains("<w:tbl>") && xml.contains("<w:insideV w:val=\"single\""), "{xml}");

        // A page border and a header rule around three paragraphs and a real 2×2 table.
        let mut shapes = rect(36.0, 36.0, 576.0, 756.0);
        shapes.push(ln(36.0, 720.0, 576.0, 720.0));
        shapes.extend([700.0, 680.0, 660.0].iter().map(|y| ln(100.0, *y, 400.0, *y)));
        shapes.extend([100.0, 250.0, 400.0].iter().map(|x| ln(*x, 660.0, *x, 700.0)));
        let mut blocks = vec![para("Report title", 72.0, 300.0, 748.0, 1, 300.0)];
        for (t, x, top) in [("A", 105.0, 696.0), ("B", 255.0, 696.0), ("C", 105.0, 676.0), ("D", 255.0, 676.0)] {
            blocks.push(para(t, x, x + 20.0, top, 1, x + 20.0));
        }
        blocks.push(para("Body paragraph 0.", 72.0, 540.0, 600.0, 2, 300.0));
        let p = page(blocks, shapes);
        let xml = part(&docx(std::slice::from_ref(&p), "Short frame"), "word/document.xml");
        assert_eq!(xml.matches("<w:tbl>").count(), 1, "only the real table: {xml}");
        assert_eq!(xml.matches("<w:gridCol ").count(), 2, "{xml}");
        assert!(!xml.contains("<w:pBdr>"), "the frame draws no boxes: {xml}");
        let h = html(&[p], "Short frame");
        assert!(h.contains("<td>A</td><td>B</td>") && h.contains("<p>Body paragraph 0.</p>"), "{h}");

        // A page border and a header rule around three paragraphs.
        let mut shapes = rect(36.0, 36.0, 576.0, 756.0);
        shapes.push(ln(36.0, 720.0, 576.0, 720.0));
        let mut blocks = vec![para("Report title", 72.0, 300.0, 748.0, 1, 300.0)];
        blocks.extend((0..3).map(|i| para(&format!("Body paragraph {i}."), 72.0, 540.0, 600.0 - 40.0 * i as f64, 2, 300.0)));
        let xml = part(&docx(&[page(blocks, shapes)], "Three"), "word/document.xml");
        assert!(!xml.contains("<w:tbl>") && !xml.contains("<w:pBdr>"), "{xml}");
    }

    /// A box around a box is sized to every paragraph inside it, the inner box's too.
    #[test]
    fn an_outer_box_is_sized_to_everything_inside_it() {
        let ln = |x0: f64, y0: f64, x1: f64, y1: f64| line(x0, y0, x1, y1);
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| vec![ln(x0, y0, x1, y0), ln(x0, y1, x1, y1), ln(x0, y0, x0, y1), ln(x1, y0, x1, y1)];
        let mut shapes = rect(100.0, 600.0, 400.0, 700.0);
        shapes.extend(rect(120.0, 604.0, 380.0, 632.0));
        let blocks = vec![
            para("In the outer box only", 110.0, 300.0, 690.0, 1, 300.0),
            para("In the inner box", 130.0, 300.0, 625.0, 1, 300.0),
            para("Text outside both boxes, long enough to set the page's text area.", 72.0, 540.0, 500.0, 1, 540.0),
        ];
        let xml = part(&docx(&[page(blocks, shapes)], "Nested"), "word/document.xml");
        let outer = xml.split(">In the outer box only<").next().and_then(|p| p.rsplit("<w:p>").next()).unwrap_or_default().to_string();
        let bottom = outer
            .split("<w:bottom ")
            .nth(1)
            .and_then(|b| b.split("w:space=\"").nth(1))
            .and_then(|v| v.split('"').next())
            .unwrap_or_default()
            .to_string();
        // The outer box's lowest text is the inner box's paragraph, a few points above it.
        assert!(bottom.parse::<i64>().is_ok_and(|v| v < 20), "bottom space {bottom}: {xml}");
    }

    /// A marker drawn as touching pieces ("1" and ".") is dropped with both its boxes, so the
    /// words after it keep their underline.
    #[test]
    fn a_marker_in_pieces_leaves_the_words_after_it_matched() {
        let row = |x0: f64, x1: f64| [x0, 689.0, x1, 700.0];
        let mut b = para("1. see the handbook", 72.0, 300.0, 700.0, 1, 300.0);
        b.runs = vec![Run {
            text: "1. see the handbook".into(),
            size: 11.0,
            rect: row(72.0, 200.0),
            words: vec![row(72.0, 77.0), row(77.0, 80.0), row(90.0, 107.0), row(110.0, 127.0), row(130.0, 180.0)],
            ..Run::default()
        }];
        let xml = part(&docx(&[page(vec![b], vec![line(130.0, 690.0, 180.0, 690.0)])], "Item"), "word/document.xml");
        let run_of = |w: &str| xml.split(&format!(">{w}")).next().and_then(|r| r.rsplit("<w:r>").next()).unwrap_or_default().to_string();
        assert!(run_of("handbook").contains("<w:u ") && !run_of("see ").contains("<w:u "), "{xml}");
    }

    /// An empty page keeps Word's default margins.
    #[test]
    fn an_empty_page_keeps_default_margins() {
        let xml = part(&docx(&[page(Vec::new(), Vec::new())], "Empty"), "word/document.xml");
        assert!(xml.contains("w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\""), "{xml}");
    }

    /// A page whose rows have no usable position still has its text in the document.
    #[test]
    fn text_without_a_usable_position_is_kept() {
        let b = Block { text: "Still here".into(), rect: [f64::NAN; 4], size: 11.0, lines: vec![[f64::NAN; 4]], ..Block::default() };
        let xml = part(&docx(&[page(vec![b], Vec::new())], "Lost"), "word/document.xml");
        assert!(xml.contains(">Still here<"), "{xml}");
    }

    /// An underlined word in a list item stays underlined once the marker is taken off.
    #[test]
    fn a_list_item_keeps_its_underlined_word() {
        let mut b = para("\u{F0B7} see the handbook", 90.0, 300.0, 700.0, 1, 300.0);
        let row = |x0: f64, x1: f64| [x0, 689.0, x1, 700.0];
        // The bullet and the text in one run, so its word boxes include the bullet's.
        b.runs = vec![Run {
            text: "\u{F0B7} see the handbook".into(),
            size: 11.0,
            rect: row(90.0, 200.0),
            words: vec![row(90.0, 96.0), row(108.0, 125.0), row(128.0, 145.0), row(148.0, 200.0)],
            ..Run::default()
        }];
        let xml = part(&docx(&[page(vec![b], vec![line(148.0, 690.0, 200.0, 690.0)])], "Item"), "word/document.xml");
        let run_of = |w: &str| xml.split(&format!(">{w}")).next().and_then(|r| r.rsplit("<w:r>").next()).unwrap_or_default().to_string();
        assert!(run_of("handbook").contains("<w:u ") && !run_of("see ").contains("<w:u "), "{xml}");
    }

    #[test]
    fn font_names_become_families() {
        assert_eq!(family("ABCDEF+LiberationSerif-BoldItalic"), "Liberation Serif");
        assert_eq!(family("TimesNewRomanPSMT"), "Times New Roman");
        assert_eq!(family("Arial,Bold"), "Arial");
        assert_eq!(family("Arial-BoldMT"), "Arial");
        assert_eq!(family("Calibri"), "Calibri");
        assert_eq!(family("DejaVuSans"), "Deja Vu Sans");
        assert_eq!(family(""), "");
    }

    #[test]
    fn markers_are_recognised() {
        assert_eq!(marker("\u{F0B7} Item"), Some((Marker::Bullet('•'), 2)));
        assert_eq!(marker("12. Twelfth"), Some((Marker::Number("decimal", '.', 12), 4)));
        assert_eq!(marker("b) Second"), Some((Marker::Number("lowerLetter", ')', 2), 3)));
        assert_eq!(marker("iv. Fourth"), Some((Marker::Number("lowerRoman", '.', 4), 4)));
        assert_eq!(marker("1.5 metres"), None);
        assert_eq!(marker("Dr. Smith"), None);
        assert_eq!(marker("•"), None, "a marker alone is not an item");
        assert_eq!(marker(""), None);
    }
}
