//! Word export of a paragraph's text: inline style runs (font, weight, slant, colour, size,
//! superscripts and subscripts) and the lines drawn under or through words (underlines,
//! strike-through), with symbol-font bullets as the characters they show.

use crate::{Block, Page, Run, esc, hex};

/// Most horizontal lines read from one page as possible underlines or strike-throughs.
const MAX_LINES: usize = 2_000;

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

/// Lines a run is drawn with.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Lines {
    pub under: bool,
    pub strike: bool,
}

/// What lines are drawn across a box (a word, or a run's first piece) of a run `span` wide.
pub(crate) type LinesOf<'a> = &'a dyn Fn([f64; 4], f64) -> Lines;

/// The thin horizontal lines a page draws: the underlines and strike-throughs of its words, among
/// others (rules, borders).
pub(crate) struct Underlines {
    lines: Vec<[f64; 4]>,
}

impl Underlines {
    pub(crate) fn new(page: &Page) -> Self {
        let lines = page
            .shapes
            .iter()
            .map(|s| s.rect)
            .filter(|r| {
                let (w, h) = (r[2] - r[0], r[3] - r[1]);
                h <= 3.0 && w >= 3.0 && w.is_finite()
            })
            .take(MAX_LINES)
            .collect();
        Self { lines }
    }

    /// The lines drawn across `rect` (a word, or a run's first piece): one just under its
    /// baseline is its underline, one through the middle of its letters strikes it out. A line
    /// much wider than the run (`span`) is a rule, not a line across its text.
    pub(crate) fn lines_of(&self, rect: [f64; 4], span: f64) -> Lines {
        let [x0, y0, x1, y1] = rect;
        let h = y1 - y0;
        let at = |lo: f64, hi: f64| {
            h > 0.0
                && self.lines.iter().any(|l| {
                    let cy = (l[1] + l[3]) / 2.0;
                    // Most of the box: a word's trailing punctuation is often left unlined.
                    let covered = l[2].min(x1) - l[0].max(x0);
                    cy >= y0 + h * lo && cy <= y0 + h * hi && covered >= (x1 - x0) * 0.8 - 1.0 && l[2] - l[0] < span * 3.0 + 6.0
                })
        };
        Lines { under: at(-0.5 / h.max(1.0), 0.4), strike: at(0.4, 0.75) }
    }
}

/// A block's runs as Word XML, with the lines the page draws across their words.
pub(crate) fn block_runs_xml(b: &Block, underlines: &Underlines) -> String {
    runs_xml(&b.runs(), &|rect, span| underlines.lines_of(rect, span))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{Image, Shape, docx, tests::part};

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
}
