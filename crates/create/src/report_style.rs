//! Report stylesheets: the look of a [`crate::report`] described in a small subset of CSS.
//!
//! A stylesheet is untrusted input (a user picks the file), so it is read leniently, the way a
//! browser reads CSS: a rule, property or value this subset does not have is skipped with a
//! warning naming its line, and the rest still applies. Only a stylesheet larger than
//! [`MAX_STYLESHEET_BYTES`] is refused.
//!
//! What a stylesheet can say is documented in `docs/report-stylesheets.md`: the report's parts
//! are classes (`.title`, `.card`, `.quote`…), `@page` sets the paper and margins, `var(--comment)`
//! is the comment's own colour, and `@import "classic";` starts from a built-in theme.

use crate::CreateError;

/// The largest stylesheet read (a theme is a few kilobytes).
pub const MAX_STYLESHEET_BYTES: usize = 64 * 1024;
/// At most this many warnings are kept; the last one says how many more there were.
const MAX_WARNINGS: usize = 50;
/// `color-mix()` inside `color-mix()` deeper than this is not read.
const MAX_PAINT_DEPTH: usize = 8;

/// The built-in themes: (id, display name, stylesheet).
pub const BUILTIN_THEMES: [(&str, &str, &str); 5] = [
    ("modern", "Modern", include_str!("../themes/modern.css")),
    ("simple", "Simple", include_str!("../themes/simple.css")),
    ("classic", "Classic", include_str!("../themes/classic.css")),
    ("compact", "Compact", include_str!("../themes/compact.css")),
    ("ink-saver", "Ink saver", include_str!("../themes/ink-saver.css")),
];

/// The stylesheet of the built-in theme `id` (any case).
pub fn builtin_theme(id: &str) -> Option<&'static str> {
    BUILTIN_THEMES.iter().find(|(i, _, _)| i.eq_ignore_ascii_case(id)).map(|(_, _, css)| *css)
}

/// One of the three standard font families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

/// A colour as a stylesheet gives it, resolved per comment by [`Paint::resolve`].
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// `transparent` / `none`: nothing is drawn.
    None,
    Rgb([f32; 3]),
    /// `var(--comment)`: the comment's own colour.
    Comment,
    /// `color-mix(in srgb, a p%, b)`: `p` (0–1) of `a`, the rest of `b`.
    Mix(Box<Paint>, f32, Box<Paint>),
}

impl Paint {
    /// The colour drawn for a comment whose colour is `comment`; `None` draws nothing.
    pub fn resolve(&self, comment: [f32; 3]) -> Option<[f32; 3]> {
        self.resolve_at(comment, 0)
    }
    fn resolve_at(&self, comment: [f32; 3], depth: usize) -> Option<[f32; 3]> {
        if depth > MAX_PAINT_DEPTH {
            return None;
        }
        let c = match self {
            Paint::None => return None,
            Paint::Rgb(c) => *c,
            Paint::Comment => comment,
            Paint::Mix(a, p, b) => {
                let (a, b) = (a.resolve_at(comment, depth + 1)?, b.resolve_at(comment, depth + 1)?);
                let p = if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.5 };
                [0, 1, 2].map(|i| a[i] * p + b[i] * (1.0 - p))
            }
        };
        c.iter().all(|v| v.is_finite()).then(|| c.map(|v| v.clamp(0.0, 1.0)))
    }
}

/// `line-height`: a multiple of the font size, or points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    Factor(f64),
    Points(f64),
}

/// The text and box properties of one part of the report.
#[derive(Clone, Debug, PartialEq)]
pub struct PartStyle {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    /// Points.
    pub size: f64,
    pub color: Paint,
    pub line_height: LineHeight,
    pub uppercase: bool,
    /// `display: none`.
    pub hidden: bool,
    pub background: Paint,
    /// Widths in points and colours of the left, top and bottom borders.
    pub border_left: (f64, Paint),
    pub border_top: (f64, Paint),
    pub border_bottom: (f64, Paint),
    /// Top, right, bottom, left, in points.
    pub padding: [f64; 4],
    pub margin: [f64; 4],
}

impl PartStyle {
    fn plain() -> PartStyle {
        PartStyle {
            family: Family::Sans,
            bold: false,
            italic: false,
            size: 10.0,
            color: Paint::Rgb([0.0; 3]),
            line_height: LineHeight::Factor(1.35),
            uppercase: false,
            hidden: false,
            background: Paint::None,
            border_left: (0.0, Paint::None),
            border_top: (0.0, Paint::None),
            border_bottom: (0.0, Paint::None),
            padding: [0.0; 4],
            margin: [0.0; 4],
        }
    }
    /// The distance between baselines, in points.
    pub fn leading(&self) -> f64 {
        let l = match self.line_height {
            LineHeight::Factor(f) => f * self.size,
            LineHeight::Points(p) => p,
        };
        if l.is_finite() { l.clamp(self.size * 0.8, self.size * 4.0) } else { self.size * 1.35 }
    }
    /// The standard font this part is set in.
    pub fn font(&self) -> pdfcraft_fonts::Std14 {
        use pdfcraft_fonts::Std14::*;
        match (self.family, self.bold, self.italic) {
            (Family::Sans, false, false) => Helvetica,
            (Family::Sans, true, false) => HelveticaBold,
            (Family::Sans, false, true) => HelveticaOblique,
            (Family::Sans, true, true) => HelveticaBoldOblique,
            (Family::Serif, false, false) => TimesRoman,
            (Family::Serif, true, false) => TimesBold,
            (Family::Serif, false, true) => TimesItalic,
            (Family::Serif, true, true) => TimesBoldItalic,
            (Family::Mono, false, false) => Courier,
            (Family::Mono, true, false) => CourierBold,
            (Family::Mono, false, true) => CourierOblique,
            (Family::Mono, true, true) => CourierBoldOblique,
        }
    }
}

/// The parts of a report a stylesheet can style, by class name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Title,
    Subtitle,
    Header,
    Heading,
    Note,
    Card,
    Number,
    Kind,
    Meta,
    Quote,
    Body,
    Reply,
    ReplyAuthor,
    ReplyMeta,
    ReplyBody,
    Footer,
}

impl Part {
    pub const ALL: [Part; 16] = [
        Part::Title,
        Part::Subtitle,
        Part::Header,
        Part::Heading,
        Part::Note,
        Part::Card,
        Part::Number,
        Part::Kind,
        Part::Meta,
        Part::Quote,
        Part::Body,
        Part::Reply,
        Part::ReplyAuthor,
        Part::ReplyMeta,
        Part::ReplyBody,
        Part::Footer,
    ];
    /// The class that selects this part (without the dot).
    pub fn class(self) -> &'static str {
        match self {
            Part::Title => "title",
            Part::Subtitle => "subtitle",
            Part::Header => "header",
            Part::Heading => "heading",
            Part::Note => "note",
            Part::Card => "card",
            Part::Number => "number",
            Part::Kind => "kind",
            Part::Meta => "meta",
            Part::Quote => "quote",
            Part::Body => "body",
            Part::Reply => "reply",
            Part::ReplyAuthor => "reply-author",
            Part::ReplyMeta => "reply-meta",
            Part::ReplyBody => "reply-body",
            Part::Footer => "footer",
        }
    }
}

/// How a report looks: paper, margins and each part's style.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportStyle {
    /// `@page { size }`; `None` uses the size the caller asks for.
    pub page_size: Option<(f64, f64)>,
    /// `@page { margin }`: top, right, bottom, left, in points.
    pub margin: [f64; 4],
    parts: Vec<PartStyle>,
}

impl Default for ReportStyle {
    /// The Modern theme.
    fn default() -> Self {
        let mut s = ReportStyle::plain();
        let mut warnings = Vec::new();
        apply(&mut s, BUILTIN_THEMES[0].2, &mut warnings, true);
        s
    }
}

impl ReportStyle {
    fn plain() -> ReportStyle {
        ReportStyle { page_size: None, margin: [54.0, 54.0, 60.0, 54.0], parts: Part::ALL.iter().map(|_| PartStyle::plain()).collect() }
    }

    pub fn part(&self, p: Part) -> &PartStyle {
        // `parts` holds one entry per `Part::ALL`, in order; `plain()` is the fallback.
        static PLAIN: std::sync::OnceLock<PartStyle> = std::sync::OnceLock::new();
        self.parts.get(p as usize).unwrap_or_else(|| PLAIN.get_or_init(PartStyle::plain))
    }

    fn part_mut(&mut self, p: Part) -> Option<&mut PartStyle> {
        self.parts.get_mut(p as usize)
    }

    /// Read `css` over the theme it imports (`@import "classic";`, Modern when it imports none).
    /// Returns the style and a warning for each thing it skipped.
    pub fn parse(css: &str) -> Result<(ReportStyle, Vec<String>), CreateError> {
        Self::parse_over("modern", css)
    }

    /// [`ReportStyle::parse`], starting from the built-in theme `base` when `css` imports none.
    pub fn parse_over(base: &str, css: &str) -> Result<(ReportStyle, Vec<String>), CreateError> {
        if css.len() > MAX_STYLESHEET_BYTES {
            return Err(CreateError::Invalid(format!(
                "the stylesheet is {} KB; at most {} KB is read",
                css.len() / 1024,
                MAX_STYLESHEET_BYTES / 1024
            )));
        }
        let mut warnings = Vec::new();
        let mut s = ReportStyle::plain();
        let base = imported_theme(css, &mut warnings).or_else(|| builtin_theme(base)).unwrap_or(BUILTIN_THEMES[0].2);
        apply(&mut s, base, &mut Vec::new(), true);
        apply(&mut s, css, &mut warnings, false);
        Ok((s, warnings))
    }

    /// The built-in theme `id`.
    pub fn builtin(id: &str) -> Option<ReportStyle> {
        let css = builtin_theme(id)?;
        let mut s = ReportStyle::plain();
        apply(&mut s, css, &mut Vec::new(), true);
        Some(s)
    }
}

/// The built-in theme named by the stylesheet's `@import`, if it has one.
fn imported_theme(css: &str, warnings: &mut Vec<String>) -> Option<&'static str> {
    let text = strip_comments(css, &mut Vec::new());
    let mut found = None;
    for (line, rule) in at_rules(&text) {
        let Some(rest) = rule.strip_prefix("@import") else { continue };
        let name = rest.trim().trim_end_matches(';').trim();
        let name = name.strip_prefix("url(").and_then(|n| n.strip_suffix(')')).unwrap_or(name).trim().trim_matches(['"', '\'']);
        let id = name.trim_end_matches(".css");
        match builtin_theme(id) {
            Some(css) if found.is_none() => found = Some(css),
            Some(_) => warn(warnings, line, "only the first @import is used".into()),
            None => warn(
                warnings,
                line,
                format!("@import \"{name}\": only the built-in themes can be imported ({})", BUILTIN_THEMES.map(|t| t.0).join(", ")),
            ),
        }
    }
    found
}

/// The `@import` statements of `text`, with their lines.
fn at_rules(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = text.get(pos..).and_then(|t| t.find("@import")) {
        let start = pos + i;
        let end = text.get(start..).and_then(|t| t.find(';')).map_or(text.len(), |j| start + j + 1);
        out.push((line_of(text, start), text.get(start..end).unwrap_or_default()));
        pos = end;
    }
    out
}

fn warn(warnings: &mut Vec<String>, line: usize, msg: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(format!("line {line}: {msg}"));
    } else if warnings.len() == MAX_WARNINGS {
        warnings.push("more problems were found than are listed".into());
    }
}

fn line_of(text: &str, byte: usize) -> usize {
    text.get(..byte).map_or(1, |t| t.matches('\n').count() + 1)
}

/// `css` with its comments replaced by spaces (newlines kept, so line numbers stay true).
fn strip_comments(css: &str, warnings: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i + 2..).unwrap_or_default();
        match after.find("*/") {
            Some(j) => {
                let comment = after.get(..j).unwrap_or_default();
                out.extend(comment.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
                out.push_str("    ");
                rest = after.get(j + 2..).unwrap_or_default();
            }
            None => {
                warn(warnings, line_of(css, css.len() - rest.len() + i), "a comment is never closed; the rest of the file is ignored".into());
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Apply the rules of `css` to `s`. `builtin` skips the `@import` check (the built-ins import
/// nothing).
fn apply(s: &mut ReportStyle, css: &str, warnings: &mut Vec<String>, builtin: bool) {
    let text = strip_comments(css, warnings);
    let mut pos = 0usize;
    let len = text.len();
    while pos < len {
        let Some(rest) = text.get(pos..) else { break };
        let skip = rest.len() - rest.trim_start().len();
        pos += skip;
        let Some(rest) = text.get(pos..) else { break };
        if rest.is_empty() {
            break;
        }
        let line = line_of(&text, pos);
        if rest.starts_with("@import") {
            pos += rest.find(';').map_or(rest.len(), |i| i + 1);
            if builtin {
                warn(warnings, line, "a built-in theme can't import another".into());
            }
            continue;
        }
        // A rule: a prelude, then a block in braces (nested braces are not part of this subset).
        let (Some(open), close) = (rest.find('{'), rest.find('}')) else {
            warn(warnings, line, format!("\"{}\" is not a rule", short(rest)));
            break;
        };
        if close.is_some_and(|c| c < open) {
            warn(warnings, line, "a closing brace without a rule".into());
            pos += close.map_or(rest.len(), |c| c + 1);
            continue;
        }
        let prelude = rest.get(..open).unwrap_or_default().trim();
        let body_start = open + 1;
        let end = rest.get(body_start..).and_then(|b| b.find('}')).map(|i| body_start + i);
        let block = match end {
            Some(e) => rest.get(body_start..e).unwrap_or_default(),
            None => {
                warn(warnings, line, "a rule is never closed".into());
                rest.get(body_start..).unwrap_or_default()
            }
        };
        let block_line = line_of(&text, pos + body_start);
        if block.contains('{') {
            warn(warnings, line, "nested rules are not read".into());
        } else if let Some(at) = prelude.strip_prefix('@') {
            if at.trim().eq_ignore_ascii_case("page") {
                page_rule(s, block, block_line, &text, pos + body_start, warnings);
            } else {
                warn(warnings, line, format!("@{} is not read", short(at)));
            }
        } else {
            let mut parts = Vec::new();
            for sel in prelude.split(',').map(str::trim) {
                match Part::ALL.iter().find(|p| sel.strip_prefix('.').is_some_and(|c| c.eq_ignore_ascii_case(p.class()))) {
                    Some(p) => parts.push(*p),
                    None => warn(
                        warnings,
                        line,
                        format!("selector \"{}\" is not a part of the report (use classes such as .title, .card, .quote)", short(sel)),
                    ),
                }
            }
            for (decl_line, prop, value) in declarations(block, &text, pos + body_start) {
                for p in &parts {
                    if let Some(part) = s.part_mut(*p)
                        && let Err(e) = property(part, &prop, &value)
                    {
                        warn(warnings, decl_line, e);
                        break;
                    }
                }
            }
        }
        pos += end.map_or(rest.len(), |e| e + 1);
    }
}

fn short(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(40).collect();
    if s.chars().count() > 40 {
        out.push('…');
    }
    out
}

/// The `property: value` pairs of a block, lower-cased property first, with their lines.
/// `offset` is where the block starts in `text`.
fn declarations(block: &str, text: &str, offset: usize) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let bytes = block.as_bytes();
    let push = |from: usize, to: usize, out: &mut Vec<(usize, String, String)>| {
        let decl = block.get(from..to).unwrap_or_default();
        let lead = decl.len() - decl.trim_start().len();
        let decl = decl.trim();
        if decl.is_empty() {
            return;
        }
        let line = line_of(text, offset + from + lead);
        match decl.split_once(':') {
            Some((p, v)) => {
                let v = v.trim();
                let v = v.strip_suffix("!important").unwrap_or(v).trim();
                out.push((line, p.trim().to_ascii_lowercase(), v.to_string()));
            }
            None => out.push((line, decl.to_ascii_lowercase(), String::new())),
        }
    };
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b';' if depth <= 0 => {
                push(start, i, &mut out);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(start, block.len(), &mut out);
    out
}

fn page_rule(s: &mut ReportStyle, block: &str, _line: usize, text: &str, offset: usize, warnings: &mut Vec<String>) {
    for (line, prop, value) in declarations(block, text, offset) {
        let result = match prop.as_str() {
            "size" => page_size(&value).map(|size| s.page_size = Some(size)),
            "margin" => box_sides(&value).map(|m| s.margin = m.map(|v| v.clamp(0.0, 288.0))),
            _ => Err(format!("@page {{ {prop} }} is not read (size and margin are)")),
        };
        if let Err(e) = result {
            warn(warnings, line, e);
        }
    }
}

fn page_size(v: &str) -> Result<(f64, f64), String> {
    let words: Vec<String> = v.split_whitespace().map(str::to_ascii_lowercase).collect();
    let landscape = words.iter().any(|w| w == "landscape");
    let named = words.iter().find_map(|w| match w.as_str() {
        "letter" => Some((612.0, 792.0)),
        "legal" => Some((612.0, 1008.0)),
        "a4" => Some((595.276, 841.89)),
        "a5" => Some((419.528, 595.276)),
        "a3" => Some((841.89, 1190.551)),
        _ => None,
    });
    let (w, h) = match named {
        Some(size) => size,
        None => {
            let lens: Vec<f64> = words.iter().filter(|w| *w != "landscape" && *w != "portrait").map(|w| length(w)).collect::<Result<_, _>>()?;
            match lens.as_slice() {
                [side] => (*side, *side),
                [w, h] => (*w, *h),
                _ => return Err(format!("size: \"{}\" is not a page size (letter, legal, a3, a4, a5, or a width and height)", short(v))),
            }
        }
    };
    if !(144.0..=14_400.0).contains(&w) || !(144.0..=14_400.0).contains(&h) {
        return Err("size: a page side must be between 2 and 200 inches".into());
    }
    Ok(if landscape { (w.max(h), w.min(h)) } else { (w, h) })
}

/// A length in points: `pt`, `px` (¾ pt), `mm`, `cm`, `in`, or a bare `0`.
fn length(v: &str) -> Result<f64, String> {
    let v = v.trim().to_ascii_lowercase();
    let units = [("pt", 1.0), ("px", 0.75), ("mm", 72.0 / 25.4), ("cm", 72.0 / 2.54), ("in", 72.0)];
    let (num, scale) =
        units.iter().find_map(|(u, k)| v.strip_suffix(u).map(|n| (n, *k))).unwrap_or((v.as_str(), if v == "0" { 1.0 } else { f64::NAN }));
    let n: f64 = num.trim().parse().map_err(|_| format!("\"{v}\" is not a length (use pt, px, mm, cm or in)"))?;
    let pts = n * scale;
    if !pts.is_finite() {
        return Err(format!("\"{v}\" is not a length (use pt, px, mm, cm or in)"));
    }
    if !(-1000.0..=1000.0).contains(&pts) {
        return Err(format!("\"{v}\" is too large"));
    }
    Ok(pts)
}

/// One to four lengths, as `padding` and `margin` take them: top, right, bottom, left.
fn box_sides(v: &str) -> Result<[f64; 4], String> {
    let l: Vec<f64> = v.split_whitespace().map(|w| if w.eq_ignore_ascii_case("auto") { Ok(0.0) } else { length(w) }).collect::<Result<_, _>>()?;
    Ok(match l.as_slice() {
        [a] => [*a; 4],
        [a, b] => [*a, *b, *a, *b],
        [a, b, c] => [*a, *b, *c, *b],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => return Err(format!("\"{}\" is not one to four lengths", short(v))),
    })
}

/// A border shorthand (`2pt solid #ccc`, `none`): width and colour.
fn border(v: &str) -> Result<(f64, Paint), String> {
    let mut width = None;
    let mut paint = None;
    for w in split_top(v, ' ') {
        let lw = w.to_ascii_lowercase();
        if matches!(lw.as_str(), "none" | "hidden") {
            return Ok((0.0, Paint::None));
        }
        if matches!(lw.as_str(), "solid" | "dashed" | "dotted" | "double") {
            continue;
        }
        if let Ok(l) = length(&w) {
            width = Some(l);
        } else {
            paint = Some(color(&w, 0)?);
        }
    }
    Ok((width.unwrap_or(1.0).clamp(0.0, 72.0), paint.unwrap_or(Paint::Rgb([0.0; 3]))))
}

/// Set one property of `p`.
fn property(p: &mut PartStyle, prop: &str, v: &str) -> Result<(), String> {
    let lv = v.to_ascii_lowercase();
    match prop {
        "color" => p.color = color(v, 0)?,
        "background" | "background-color" => p.background = color(v, 0)?,
        "font-family" => {
            let mut found = None;
            for f in v.split(',') {
                let f = f.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
                found = match f.as_str() {
                    "sans-serif" | "helvetica" | "arial" | "helvetica neue" | "system-ui" => Some(Family::Sans),
                    "serif" | "times" | "times new roman" | "times-roman" | "georgia" => Some(Family::Serif),
                    "monospace" | "courier" | "courier new" => Some(Family::Mono),
                    _ => None,
                };
                if found.is_some() {
                    break;
                }
            }
            p.family = found.ok_or_else(|| {
                format!("font-family \"{}\": only Helvetica (sans-serif), Times (serif) and Courier (monospace) can be used", short(v))
            })?;
        }
        "font-size" => {
            let s = length(v)?;
            if !(4.0..=96.0).contains(&s) {
                return Err(format!("font-size {} is outside 4–96 pt", short(v)));
            }
            p.size = s;
        }
        "font-weight" => {
            p.bold = match lv.as_str() {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => n.parse::<u16>().map(|w| w >= 600).map_err(|_| format!("font-weight \"{}\" is not a weight", short(v)))?,
            }
        }
        "font-style" => {
            p.italic = match lv.as_str() {
                "italic" | "oblique" => true,
                "normal" => false,
                _ => return Err(format!("font-style \"{}\" is not normal, italic or oblique", short(v))),
            }
        }
        "line-height" => {
            p.line_height = match lv.parse::<f64>() {
                Ok(f) if f.is_finite() && (0.5..=5.0).contains(&f) => LineHeight::Factor(f),
                Ok(_) => return Err("line-height must be between 0.5 and 5".into()),
                Err(_) if lv == "normal" => LineHeight::Factor(1.35),
                Err(_) => LineHeight::Points(length(v)?.clamp(1.0, 400.0)),
            }
        }
        "text-transform" => {
            p.uppercase = match lv.as_str() {
                "uppercase" => true,
                "none" => false,
                _ => return Err(format!("text-transform \"{}\" is not uppercase or none", short(v))),
            }
        }
        "display" => {
            p.hidden = match lv.as_str() {
                "none" => true,
                "block" | "inline" | "inline-block" | "flex" => false,
                _ => return Err(format!("display \"{}\" is not none or block", short(v))),
            }
        }
        "padding" => p.padding = box_sides(v)?.map(|x| x.clamp(0.0, 288.0)),
        "margin" => p.margin = box_sides(v)?.map(|x| x.clamp(-72.0, 288.0)),
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" | "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            let (which, side) = prop.split_once('-').unwrap_or_default();
            let i = match side {
                "top" => 0,
                "right" => 1,
                "bottom" => 2,
                _ => 3,
            };
            let l = length(v)?;
            if which == "padding" {
                p.padding[i] = l.clamp(0.0, 288.0);
            } else {
                p.margin[i] = l.clamp(-72.0, 288.0);
            }
        }
        "border-left" => p.border_left = border(v)?,
        "border-top" => p.border_top = border(v)?,
        "border-bottom" => p.border_bottom = border(v)?,
        "border-left-width" => p.border_left.0 = length(v)?.clamp(0.0, 72.0),
        "border-top-width" => p.border_top.0 = length(v)?.clamp(0.0, 72.0),
        "border-bottom-width" => p.border_bottom.0 = length(v)?.clamp(0.0, 72.0),
        "border-left-color" => p.border_left.1 = color(v, 0)?,
        "border-top-color" => p.border_top.1 = color(v, 0)?,
        "border-bottom-color" => p.border_bottom.1 = color(v, 0)?,
        "" => return Err("a declaration without a property".into()),
        other => return Err(format!("property \"{}\" is not read (see docs/report-stylesheets.md)", short(other))),
    }
    Ok(())
}

/// `s` split on `sep` outside parentheses and quotes, with empty pieces dropped.
fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            ('(', None) => depth += 1,
            (')', None) => depth -= 1,
            _ => {}
        }
        if quote.is_none() && depth <= 0 && (c == sep || (sep == ' ' && c.is_whitespace())) {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// A colour: `#rgb`, `#rrggbb` (with alpha, blended over white), `rgb()`/`rgba()`, a CSS colour
/// name, `transparent`, `var(--comment)` or `color-mix(in srgb, a p%, b)`.
fn color(v: &str, depth: usize) -> Result<Paint, String> {
    let bad = || format!("\"{}\" is not a colour", short(v));
    if depth > MAX_PAINT_DEPTH {
        return Err("colours nest too deeply".into());
    }
    let lv = v.trim().to_ascii_lowercase();
    if let Some(hex) = lv.strip_prefix('#') {
        let digits: Vec<f32> = hex.chars().map(|c| c.to_digit(16).map(|d| d as f32)).collect::<Option<_>>().ok_or_else(bad)?;
        let (rgb, a) = match digits.as_slice() {
            [r, g, b] => ([r * 17.0, g * 17.0, b * 17.0], 1.0),
            [r, g, b, a] => ([r * 17.0, g * 17.0, b * 17.0], a * 17.0 / 255.0),
            [r1, r2, g1, g2, b1, b2] => ([r1 * 16.0 + r2, g1 * 16.0 + g2, b1 * 16.0 + b2], 1.0),
            [r1, r2, g1, g2, b1, b2, a1, a2] => ([r1 * 16.0 + r2, g1 * 16.0 + g2, b1 * 16.0 + b2], (a1 * 16.0 + a2) / 255.0),
            _ => return Err(bad()),
        };
        return Ok(Paint::Rgb(over_white(rgb.map(|c| c / 255.0), a)));
    }
    if let Some(args) = lv.strip_prefix("rgba(").or_else(|| lv.strip_prefix("rgb(")).and_then(|a| a.strip_suffix(')')) {
        let flat = args.replace(['/', ','], " ");
        let parts: Vec<&str> = flat.split_whitespace().collect();
        let num = |s: &str, scale: f32| -> Result<f32, String> {
            let (n, k) = match s.strip_suffix('%') {
                Some(p) => (p, 100.0),
                None => (s, scale),
            };
            let x: f32 = n.parse().map_err(|_| bad())?;
            if x.is_finite() { Ok((x / k).clamp(0.0, 1.0)) } else { Err(bad()) }
        };
        return match parts.as_slice() {
            [r, g, b] => Ok(Paint::Rgb([num(r, 255.0)?, num(g, 255.0)?, num(b, 255.0)?])),
            [r, g, b, a] => Ok(Paint::Rgb(over_white([num(r, 255.0)?, num(g, 255.0)?, num(b, 255.0)?], num(a, 1.0)?))),
            _ => Err(bad()),
        };
    }
    if lv.starts_with("var(") {
        let name = lv.trim_start_matches("var(").trim_end_matches(')').trim();
        return if name == "--comment" {
            Ok(Paint::Comment)
        } else {
            Err(format!("var({name}): only var(--comment), the comment's colour, is defined"))
        };
    }
    if let Some(args) = lv.strip_prefix("color-mix(").and_then(|a| a.strip_suffix(')')) {
        let parts = split_top(args, ',');
        let [space, a, b] = parts.as_slice() else { return Err(bad()) };
        if space.trim() != "in srgb" {
            return Err("color-mix: only \"in srgb\" is read".into());
        }
        let side = |s: &str| -> Result<(Paint, Option<f32>), String> {
            let words = split_top(s, ' ');
            let (pct, col): (Vec<&String>, Vec<&String>) = words.iter().partition(|w| w.ends_with('%'));
            let p = match pct.first() {
                Some(p) => Some(p.trim_end_matches('%').parse::<f32>().map_err(|_| bad())? / 100.0),
                None => None,
            };
            let c = col.first().ok_or_else(bad)?;
            Ok((color(c, depth + 1)?, p))
        };
        let ((pa, ca), (pb, cb)) = (side(a)?, side(b)?);
        let p = match (ca, cb) {
            (Some(x), _) => x,
            (None, Some(y)) => 1.0 - y,
            (None, None) => 0.5,
        };
        if !p.is_finite() {
            return Err(bad());
        }
        return Ok(Paint::Mix(Box::new(pa), p.clamp(0.0, 1.0), Box::new(pb)));
    }
    let named = match lv.as_str() {
        "transparent" | "none" => return Ok(Paint::None),
        "black" => [0, 0, 0],
        "white" => [255, 255, 255],
        "gray" | "grey" => [128, 128, 128],
        "darkgray" | "darkgrey" => [169, 169, 169],
        "lightgray" | "lightgrey" => [211, 211, 211],
        "dimgray" | "dimgrey" => [105, 105, 105],
        "silver" => [192, 192, 192],
        "red" => [255, 0, 0],
        "maroon" => [128, 0, 0],
        "orange" => [255, 165, 0],
        "yellow" => [255, 255, 0],
        "olive" => [128, 128, 0],
        "lime" => [0, 255, 0],
        "green" => [0, 128, 0],
        "teal" => [0, 128, 128],
        "aqua" | "cyan" => [0, 255, 255],
        "blue" => [0, 0, 255],
        "navy" => [0, 0, 128],
        "purple" => [128, 0, 128],
        "fuchsia" | "magenta" => [255, 0, 255],
        "steelblue" => [70, 130, 180],
        "slategray" | "slategrey" => [112, 128, 144],
        "whitesmoke" => [245, 245, 245],
        "gainsboro" => [220, 220, 220],
        _ => return Err(bad()),
    };
    Ok(Paint::Rgb(named.map(|c: u8| f32::from(c) / 255.0)))
}

/// `c` at opacity `a`, drawn on white (the paper).
fn over_white(c: [f32; 3], a: f32) -> [f32; 3] {
    let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 };
    c.map(|v| v * a + (1.0 - a))
}
