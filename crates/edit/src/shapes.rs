//! The plain rectangles a page paints: filled boxes and stroked horizontal or vertical lines, with
//! their colours, for exports that rebuild table borders, cell shading and rules (Export to Word).
//! Curves, slanted lines and shapes under a rotating matrix are left out; so are forms.

use pdfcraft_content::{Matrix, Pieces};
use pdfcraft_cos::Document;

use crate::background::{Space, cmyk, colour};
use crate::{EditError, Rgb};

/// How deep `q` may nest while reading (deeper saves are not tracked).
const MAX_SAVES: usize = 64;
/// Most rectangles read from one page, and most points one path may hold.
const MAX_SHAPES: usize = 20_000;
const MAX_PATH_POINTS: usize = 4_096;
/// Largest coordinate a reported rectangle may have (points from the origin).
const MAX_COORDINATE: f64 = 1e7;

/// A rectangle painted on the page in one colour, in user space (y up). A stroked line is the
/// box its stroke covers, so a rule is a rectangle as tall (or wide) as its line width.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedRect {
    pub rect: [f64; 4],
    pub color: Rgb,
    /// Painted by stroking a line (a border or a rule), rather than by filling a box.
    pub stroked: bool,
}

#[derive(Clone)]
struct State {
    ctm: Matrix,
    fill_space: Space,
    stroke_space: Space,
    fill: Option<Rgb>,
    stroke: Option<Rgb>,
    width: f64,
}

/// The rectangles the page's own content fills, and the horizontal and vertical lines it strokes.
pub fn painted_rects(doc: &Document, page: usize) -> Result<Vec<PaintedRect>, EditError> {
    let pages = pdfcraft_model::pages(doc);
    let p = pages.get(page).ok_or(EditError::NoSuchPage(page))?;
    let streams = crate::text::content_streams(doc, &p.dict);
    let pieces = Pieces::join(&streams.iter().map(|(_, d)| d.as_slice()).collect::<Vec<_>>());
    let ops = pieces.parse().ops;

    let mut st =
        State { ctm: Matrix::IDENTITY, fill_space: Space::Gray, stroke_space: Space::Gray, fill: Some([0.0; 3]), stroke: Some([0.0; 3]), width: 1.0 };
    let mut saved: Vec<State> = Vec::new();
    let mut lost_saves = 0usize;
    // The current path in page space: each subpath's points, and whether it was closed.
    let mut subpaths: Vec<(Vec<(f64, f64)>, bool)> = Vec::new();
    let mut points = 0usize;
    let mut out = Vec::new();
    for op in &ops {
        if out.len() >= MAX_SHAPES {
            break;
        }
        match op.op.as_slice() {
            b"q" => {
                if saved.len() < MAX_SAVES {
                    saved.push(st.clone());
                } else {
                    lost_saves = lost_saves.saturating_add(1);
                }
            }
            b"Q" => {
                if lost_saves > 0 {
                    lost_saves -= 1;
                } else if let Some(s) = saved.pop() {
                    st = s;
                }
            }
            b"cm" => {
                if let Some(m) = Matrix::from_operands(&op.operands) {
                    st.ctm = m.then(&st.ctm);
                }
            }
            b"w" => {
                if let Some([w]) = op.nums::<1>() {
                    st.width = if w.is_finite() { w.abs() } else { 1.0 };
                }
            }
            b"g" => (st.fill_space, st.fill) = (Space::Gray, op.nums::<1>().map(|[g]| [g; 3])),
            b"rg" => (st.fill_space, st.fill) = (Space::Rgb, op.nums::<3>()),
            b"k" => (st.fill_space, st.fill) = (Space::Cmyk, op.nums::<4>().map(cmyk)),
            b"G" => (st.stroke_space, st.stroke) = (Space::Gray, op.nums::<1>().map(|[g]| [g; 3])),
            b"RG" => (st.stroke_space, st.stroke) = (Space::Rgb, op.nums::<3>()),
            b"K" => (st.stroke_space, st.stroke) = (Space::Cmyk, op.nums::<4>().map(cmyk)),
            b"cs" | b"CS" => {
                let space = match op.name(0) {
                    Some(b"DeviceGray" | b"G") => Space::Gray,
                    Some(b"DeviceRGB" | b"RGB") => Space::Rgb,
                    Some(b"DeviceCMYK" | b"CMYK") => Space::Cmyk,
                    _ => Space::Other,
                };
                if op.op.as_slice() == b"cs" {
                    (st.fill_space, st.fill) = (space, None);
                } else {
                    (st.stroke_space, st.stroke) = (space, None);
                }
            }
            b"sc" | b"scn" => st.fill = colour(st.fill_space, op),
            b"SC" | b"SCN" => st.stroke = colour(st.stroke_space, op),
            b"re" => {
                if let Some([x, y, w, h]) = op.nums::<4>()
                    && points < MAX_PATH_POINTS
                {
                    let c = |x: f64, y: f64| st.ctm.apply(x, y);
                    subpaths.push((vec![c(x, y), c(x + w, y), c(x + w, y + h), c(x, y + h)], true));
                    points += 4;
                }
            }
            b"m" => {
                if let Some([x, y]) = op.nums::<2>()
                    && points < MAX_PATH_POINTS
                {
                    subpaths.push((vec![st.ctm.apply(x, y)], false));
                    points += 1;
                }
            }
            b"l" => {
                if let (Some([x, y]), Some((path, _))) = (op.nums::<2>(), subpaths.last_mut())
                    && points < MAX_PATH_POINTS
                {
                    path.push(st.ctm.apply(x, y));
                    points += 1;
                }
            }
            // A curve makes its subpath neither a box nor straight lines.
            b"c" | b"v" | b"y" => {
                if let Some((path, _)) = subpaths.last_mut() {
                    path.clear();
                }
            }
            b"h" => {
                if let Some((_, closed)) = subpaths.last_mut() {
                    *closed = true;
                }
            }
            b"n" => {
                subpaths.clear();
                points = 0;
            }
            b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"S" | b"s" => {
                let name = op.op.as_slice();
                let fills = !matches!(name, b"S" | b"s");
                let strokes = matches!(name, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                let closes = matches!(name, b"s" | b"b" | b"b*");
                for (path, closed) in subpaths.drain(..) {
                    if fills && let (Some(color), Some(rect)) = (st.fill, axis_box(&path)) {
                        out.push(PaintedRect { rect, color: clamp(color), stroked: false });
                    }
                    if strokes && let Some(color) = st.stroke {
                        // The line width in page space (the CTM's mean scale), at least a hairline.
                        let [a, b, c, d, _, _] = st.ctm.0;
                        let scale = (a * d - b * c).abs().sqrt();
                        let half = if scale.is_finite() { (st.width * scale).max(0.25) / 2.0 } else { 0.125 };
                        let closing = (closed || closes) && path.len() > 2;
                        let ends = path.first().copied().filter(|_| closing);
                        for (p, q) in path.iter().zip(path.iter().skip(1)).map(|(p, q)| (*p, *q)).chain(path.last().copied().zip(ends)) {
                            if let Some(rect) = segment_box(p, q, half) {
                                out.push(PaintedRect { rect, color: clamp(color), stroked: true });
                            }
                        }
                    }
                }
                points = 0;
            }
            _ => {}
        }
    }
    // Coordinates past any page (an overflowing matrix) are not drawn anywhere a reader sees.
    out.retain(|s| s.rect.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE));
    out.truncate(MAX_SHAPES);
    Ok(out)
}

fn clamp(c: Rgb) -> Rgb {
    c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

/// The box of a subpath that is an axis-aligned rectangle (four corners, edges horizontal or
/// vertical, a repeated closing point allowed).
fn axis_box(path: &[(f64, f64)]) -> Option<[f64; 4]> {
    let mut pts: Vec<(f64, f64)> = path.to_vec();
    if pts.len() == 5 && pts.first().zip(pts.last()).is_some_and(|(a, b)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3) {
        pts.pop();
    }
    if pts.len() != 4 || pts.iter().any(|p| !(p.0.is_finite() && p.1.is_finite())) {
        return None;
    }
    let straight = (0..4).all(|i| {
        let (Some(a), Some(b)) = (pts.get(i), pts.get((i + 1) % 4)) else { return false };
        (a.0 - b.0).abs() < 1e-3 || (a.1 - b.1).abs() < 1e-3
    });
    let rect = [
        pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
        pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
        pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
        pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
    ];
    (straight && rect[2] > rect[0] && rect[3] > rect[1]).then_some(rect)
}

/// The box a horizontal or vertical line from `p` to `q` covers when stroked `half` either side.
fn segment_box(p: (f64, f64), q: (f64, f64), half: f64) -> Option<[f64; 4]> {
    let (dx, dy) = ((q.0 - p.0).abs(), (q.1 - p.1).abs());
    if !(dx.is_finite() && dy.is_finite()) || dx.max(dy) < 0.5 {
        return None;
    }
    if dy < 0.01 * dx.max(1.0) {
        let y = (p.1 + q.1) / 2.0;
        Some([p.0.min(q.0), y - half, p.0.max(q.0), y + half])
    } else if dx < 0.01 * dy.max(1.0) {
        let x = (p.0 + q.0) / 2.0;
        Some([x - half, p.1.min(q.1), x + half, p.1.max(q.1)])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_and_lines_keep_their_colours() {
        let segs = segment_box((10.0, 20.0), (110.0, 20.0), 0.5);
        assert_eq!(segs, Some([10.0, 19.5, 110.0, 20.5]));
        assert_eq!(segment_box((0.0, 0.0), (50.0, 40.0), 0.5), None, "slanted lines are left out");
        assert_eq!(segment_box((0.0, 0.0), (f64::NAN, 0.0), 0.5), None);
        assert_eq!(axis_box(&[(0.0, 0.0), (5.0, 0.0), (5.0, 3.0), (0.0, 3.0)]), Some([0.0, 0.0, 5.0, 3.0]));
        assert_eq!(axis_box(&[(0.0, 0.0), (5.0, 1.0), (5.0, 3.0), (0.0, 3.0)]), None);
        assert_eq!(axis_box(&[(0.0, 0.0), (5.0, 0.0)]), None);
    }
}
