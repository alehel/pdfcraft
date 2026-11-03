//! Text operations of the script editor, on plain strings so they are tested without a UI:
//! keeping the indentation on Enter, Tab and Shift+Tab on lines, taking a level off a line when
//! its `}` is typed, and finding the bracket that matches the one at the caret.
//!
//! Positions are character indices (what egui's cursors count) unless a name says bytes.

use std::ops::Range;

use pdfcraft_engine::js::{Token, TokenKind};

/// Most lines looked at to find the script's indentation.
const MAX_LINES_FOR_UNIT: usize = 2000;

/// Most characters scanned for a matching bracket.
const MAX_BRACKET_SCAN: usize = 200_000;

/// The text after an edit and the selection to put back (`start == end` for a caret).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Edit {
    pub text: String,
    pub selection: Range<usize>,
}

/// The byte where character `index` starts (the end for an index past it).
pub(super) fn byte_of(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(b, _)| b)
}

/// The character index of byte `byte` (rounded down to a character start).
pub(super) fn char_of(text: &str, byte: usize) -> usize {
    text.char_indices().take_while(|(b, _)| *b < byte).count()
}

/// The indentation the script uses: a tab if its first indented line starts with one, else the
/// fewest spaces any line is indented by (2 to 8), else two spaces.
pub(super) fn indent_unit(text: &str) -> String {
    let mut fewest: Option<usize> = None;
    for line in text.lines().take(MAX_LINES_FOR_UNIT) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            if fewest.is_none() {
                return "\t".into();
            }
            continue;
        }
        let spaces = line.chars().take_while(|c| *c == ' ').count();
        if spaces >= 2 {
            fewest = Some(fewest.map_or(spaces, |f| f.min(spaces)));
        }
    }
    " ".repeat(fewest.unwrap_or(2).min(8))
}

/// The byte range of the line holding byte `at` (without its line end).
fn line_at(text: &str, at: usize) -> Range<usize> {
    let start = text.get(..at).and_then(|b| b.rfind('\n')).map_or(0, |i| i.saturating_add(1));
    let end = text.get(at..).and_then(|a| a.find('\n')).map_or(text.len(), |i| at.saturating_add(i));
    start..end.max(start)
}

fn leading_whitespace(line: &str) -> &str {
    let n = line.len().saturating_sub(line.trim_start_matches([' ', '\t']).len());
    line.get(..n).unwrap_or_default()
}

/// `text` with the characters in `selection` replaced by `insert`, the caret after it.
pub(super) fn replace(text: &str, selection: Range<usize>, insert: &str) -> Edit {
    let (from, to) = (byte_of(text, selection.start), byte_of(text, selection.end.max(selection.start)));
    let mut out = String::with_capacity(text.len().saturating_add(insert.len()));
    out.push_str(text.get(..from).unwrap_or_default());
    out.push_str(insert);
    out.push_str(text.get(to..).unwrap_or_default());
    let caret = selection.start.saturating_add(insert.chars().count());
    Edit { text: out, selection: caret..caret }
}

/// Enter: a new line indented as the caret's line, one level more after an opening bracket.
/// Between a bracket pair (`{|}`) the closing one moves to its own line below.
pub(super) fn newline(text: &str, selection: Range<usize>, unit: &str) -> Edit {
    let at = byte_of(text, selection.start);
    let line = line_at(text, at);
    let current = text.get(line.clone()).unwrap_or_default();
    let indent = leading_whitespace(current).to_string();
    let before = text.get(line.start..at).unwrap_or_default().trim_end();
    let opens = before.ends_with(['{', '(', '[']);
    let after = text.get(byte_of(text, selection.end.max(selection.start))..).unwrap_or_default();
    let closes = after.trim_start_matches([' ', '\t']).starts_with(['}', ')', ']']);
    if opens {
        let inner = format!("\n{indent}{unit}");
        if closes {
            let edit = replace(text, selection, &format!("{inner}\n{indent}"));
            let caret = edit.selection.start.saturating_sub(indent.chars().count().saturating_add(1));
            return Edit { selection: caret..caret, ..edit };
        }
        return replace(text, selection, &inner);
    }
    replace(text, selection, &format!("\n{indent}"))
}

/// The character range of the lines `selection` covers. A selection that ends at the start of
/// a line does not take that line.
fn lines_of(text: &str, selection: &Range<usize>) -> Range<usize> {
    let (from, mut to) = (byte_of(text, selection.start), byte_of(text, selection.end.max(selection.start)));
    if to > from && text.get(..to).is_some_and(|b| b.ends_with('\n')) {
        to = to.saturating_sub(1);
    }
    let first = line_at(text, from);
    let last = line_at(text, to);
    char_of(text, first.start)..char_of(text, last.end)
}

/// Whether Tab should indent lines rather than type: the selection spans more than one line.
pub(super) fn spans_lines(text: &str, selection: &Range<usize>) -> bool {
    let (from, to) = (byte_of(text, selection.start), byte_of(text, selection.end.max(selection.start)));
    text.get(from..to).is_some_and(|s| s.trim_end_matches('\n').contains('\n'))
}

/// Tab (or Shift+Tab, `outdent`) on the lines the selection covers: each non-empty line gains
/// (or loses) one `unit`. The selection keeps covering the same text.
pub(super) fn indent_lines(text: &str, selection: Range<usize>, outdent: bool, unit: &str) -> Edit {
    let lines = lines_of(text, &selection);
    let (from, to) = (byte_of(text, lines.start), byte_of(text, lines.end));
    let block = text.get(from..to).unwrap_or_default();
    let mut out = String::with_capacity(block.len().saturating_add(unit.len().saturating_mul(16)));
    // Characters added (positive) or removed on the first line, and in all.
    let (mut first_delta, mut total_delta, mut first) = (0isize, 0isize, true);
    for (i, line) in block.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let delta: isize = if line.trim().is_empty() {
            out.push_str(line);
            0
        } else if outdent {
            let lead = leading_whitespace(line);
            let take = if lead.starts_with('\t') { 1 } else { lead.chars().take_while(|c| *c == ' ').count().min(unit.chars().count().max(1)) };
            out.push_str(line.get(take..).unwrap_or(line));
            -isize::try_from(take).unwrap_or(0)
        } else {
            out.push_str(unit);
            out.push_str(line);
            isize::try_from(unit.chars().count()).unwrap_or(0)
        };
        if first {
            first_delta = delta;
            first = false;
        }
        total_delta = total_delta.saturating_add(delta);
    }
    let mut text_out = String::with_capacity(text.len().saturating_add(out.len()));
    text_out.push_str(text.get(..from).unwrap_or_default());
    text_out.push_str(&out);
    text_out.push_str(text.get(to..).unwrap_or_default());
    let shift = |at: usize, delta: isize, floor: usize| at.saturating_add_signed(delta).max(floor);
    let start = if selection.start == lines.start { selection.start } else { shift(selection.start, first_delta, lines.start) };
    let end = shift(selection.end.max(selection.start), total_delta, start);
    Edit { text: text_out, selection: start..end }
}

/// Typing `}` on a line with nothing but indentation before the caret: the line loses one level.
pub(super) fn close_brace(text: &str, selection: Range<usize>, unit: &str) -> Option<Edit> {
    if selection.start != selection.end {
        return None;
    }
    let at = byte_of(text, selection.start);
    let line = line_at(text, at);
    let before = text.get(line.start..at)?;
    if before.is_empty() || !before.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    // One tab, or up to one unit of spaces.
    let removed = if before.ends_with('\t') { 1 } else { before.chars().rev().take_while(|c| *c == ' ').count().min(unit.chars().count()) };
    Some(replace(text, selection.start.saturating_sub(removed)..selection.start, "}"))
}

/// The bytes of the bracket at the caret (just before it, else just after it) and of the one that
/// matches it, skipping brackets in strings, templates, comments and regular expressions.
pub(super) fn matching_bracket(text: &str, tokens: &[Token], caret: usize) -> Option<(usize, usize)> {
    let caret = byte_of(text, caret);
    let in_literal = |b: usize| {
        let i = tokens.partition_point(|t| t.bytes.end <= b);
        tokens
            .get(i)
            .is_some_and(|t| t.bytes.start <= b && matches!(t.kind, TokenKind::String | TokenKind::Template | TokenKind::Comment | TokenKind::Regex))
    };
    let before = text.get(..caret).and_then(|b| b.char_indices().next_back());
    let after = text.get(caret..).and_then(|a| a.chars().next()).map(|c| (caret, c));
    let (at, c) = [before, after].into_iter().flatten().find(|(i, c)| "()[]{}".contains(*c) && !in_literal(*i))?;
    let (open, close, forward) = match c {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        _ => ('{', '}', false),
    };
    let mut depth = 0usize;
    let scan: Box<dyn Iterator<Item = (usize, char)>> = if forward {
        Box::new(text.get(at..)?.char_indices().map(move |(i, c)| (at.saturating_add(i), c)))
    } else {
        Box::new(text.get(..at.saturating_add(1))?.char_indices().rev())
    };
    for (i, ch) in scan.take(MAX_BRACKET_SCAN) {
        if ch != open && ch != close || in_literal(i) {
            continue;
        }
        let deeper = if forward { ch == open } else { ch == close };
        if deeper {
            depth = depth.saturating_add(1);
        } else {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some((at, i));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(marked: &str) -> (String, Range<usize>) {
        // `[` … `]` marks a selection, `|` a caret.
        if let Some(caret) = marked.find('|') {
            let i = marked.get(..caret).unwrap_or_default().chars().count();
            return (marked.replacen('|', "", 1), i..i);
        }
        let s = marked.find('«').expect("a selection");
        let text = marked.replacen('«', "", 1);
        let e = text.find('»').expect("its end");
        let text = text.replacen('»', "", 1);
        (text.clone(), char_of(&text, s)..char_of(&text, e))
    }

    fn show(edit: &Edit) -> String {
        let mut t = edit.text.clone();
        let (s, e) = (byte_of(&t, edit.selection.start), byte_of(&t, edit.selection.end));
        if s == e {
            t.insert(s, '|');
        } else {
            t.insert(e, '»');
            t.insert(s, '«');
        }
        t
    }

    #[test]
    fn the_unit_follows_the_script() {
        assert_eq!(indent_unit("a\n    b\n        c"), "    ");
        assert_eq!(indent_unit("a\n\tb"), "\t");
        assert_eq!(indent_unit("a\n  b"), "  ");
        assert_eq!(indent_unit("plain"), "  ");
        assert_eq!(indent_unit("a\n                          b"), "        ", "at most eight");
    }

    #[test]
    fn enter_keeps_the_indentation_and_opens_blocks() {
        let (t, s) = at("if (a) {\n  x = 1;|\n}");
        assert_eq!(show(&newline(&t, s, "  ")), "if (a) {\n  x = 1;\n  |\n}");
        let (t, s) = at("if (a) {|");
        assert_eq!(show(&newline(&t, s, "  ")), "if (a) {\n  |");
        let (t, s) = at("  f({|})");
        assert_eq!(show(&newline(&t, s, "  ")), "  f({\n    |\n  })");
        let (t, s) = at("é|");
        assert_eq!(show(&newline(&t, s, "\t")), "é\n|");
    }

    #[test]
    fn tab_and_shift_tab_move_whole_lines() {
        let (t, s) = at("«a\nb»\n\nc");
        assert_eq!(show(&indent_lines(&t, s, false, "  ")), "«  a\n  b»\n\nc");
        let (t, s) = at("    «a\n  b\nc»");
        assert_eq!(show(&indent_lines(&t, s, true, "  ")), "  «a\nb\nc»");
        // A selection ending at the start of a line leaves that line alone.
        let (t, s) = at("«a\n»b");
        assert_eq!(show(&indent_lines(&t, s, false, "\t")), "«\ta\n»b");
        assert!(spans_lines("a\nb", &(0..3)) && !spans_lines("ab\n", &(0..3)));
    }

    #[test]
    fn a_closing_brace_takes_a_level_off_its_line() {
        let (t, s) = at("if (a) {\n    |");
        assert_eq!(close_brace(&t, s, "  ").map(|e| show(&e)).as_deref(), Some("if (a) {\n  }|"));
        let (t, s) = at("x = {|");
        assert_eq!(close_brace(&t, s, "  "), None, "not after other text");
        let (t, s) = at("\t\t|");
        assert_eq!(close_brace(&t, s, "\t").map(|e| show(&e)).as_deref(), Some("\t}|"));
    }

    #[test]
    fn brackets_match_outside_strings_and_comments() {
        let text = "f(a, \")\", [1, (2)]) // )";
        let tokens = pdfcraft_engine::js::highlight(text);
        let open = text.find('(').expect("open");
        let close = text.find(") //").expect("close");
        assert_eq!(matching_bracket(text, &tokens, char_of(text, open) + 1), Some((open, close)));
        assert_eq!(matching_bracket(text, &tokens, char_of(text, close) + 1), Some((close, open)));
        assert_eq!(matching_bracket(text, &tokens, 3), None, "not next to a bracket");
        assert_eq!(matching_bracket("((", &[], 1), None, "unmatched");
    }
}
