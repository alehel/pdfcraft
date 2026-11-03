//! Suggestions as one types: the form's field names inside `getField("…")`, an object's names
//! after `event.`, `app.`, `util.`, `this.` or a field (`getField("a").`, `event.target.`, a
//! variable set from `getField`), and the document's names (`getField`, `numFields`, `app`, …)
//! for a bare word. Read from the text around the caret, so it works in a script that does not
//! parse yet, which is most of the time while typing.

use std::ops::Range;

use super::model::{Model, Object, describe, model};
use crate::check::{TokenKind, highlight};

/// Longest script suggestions are made in (the engine's own limit).
const MAX_SOURCE_BYTES: usize = crate::MAX_SCRIPT_BYTES;

/// Most suggestions offered at once.
pub const MAX_SUGGESTIONS: usize = 50;

/// How far back from the caret the object before a `.` is looked for, in bytes.
const MAX_LOOK_BACK: usize = 512;

/// What a suggestion is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    /// A field of the form.
    Field,
    /// A function or method.
    Function,
    /// A value: a property, or an object such as `app`.
    Property,
}

/// One suggestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// What the list shows.
    pub label: String,
    /// What replaces [`Completion::replace`] when it is chosen.
    pub insert: String,
    pub kind: SuggestionKind,
    /// One line about it, for names of the object model.
    pub detail: Option<&'static str>,
}

/// Suggestions for the caret, and the bytes of the script a chosen one replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// On character boundaries; it contains the caret.
    pub replace: Range<usize>,
    pub items: Vec<Suggestion>,
}

fn is_name(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphanumeric()
}

/// Suggestions for the caret at byte `cursor` of `source`, given the form's `fields`. `None` where
/// there is nothing to suggest: in a comment, a number or another string, past the end, or a
/// bare word that is already complete.
pub fn complete(source: &str, cursor: usize, fields: &[String]) -> Option<Completion> {
    if cursor > source.len() || !source.is_char_boundary(cursor) || source.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let tokens = highlight(source);
    let here = tokens.iter().find(|t| t.bytes.start < cursor && cursor <= t.bytes.end);
    if let Some(t) = here {
        match t.kind {
            TokenKind::String => return field_names(source, cursor, t.bytes.clone(), fields),
            TokenKind::Comment | TokenKind::Template | TokenKind::Regex | TokenKind::Number => return None,
            TokenKind::Keyword | TokenKind::Literal | TokenKind::Api => {}
        }
    }
    let before = source.get(..cursor)?;
    let start = before.char_indices().rev().take_while(|(_, c)| is_name(*c)).last().map_or(cursor, |(i, _)| i);
    let after = source.get(cursor..)?;
    let end = cursor.saturating_add(after.char_indices().find(|(_, c)| !is_name(*c)).map_or(after.len(), |(i, _)| i));
    let prefix = source.get(start..cursor)?;
    if prefix.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let head = source.get(..start)?;
    let model = model();
    let items = match head.strip_suffix('.') {
        Some(object) => {
            let object = object_before(source, object, &model)?;
            let items = members(&model, object, prefix);
            let word = source.get(start..end).unwrap_or_default();
            if items.len() == 1 && items.first().is_some_and(|i| i.label == word) {
                return None;
            }
            items
        }
        None if prefix.is_empty() => return None,
        None => {
            let mut items = members(&model, Object::Document, prefix);
            if matches("event", &prefix.to_lowercase()) {
                items.push(Suggestion {
                    label: "event".into(),
                    insert: "event".into(),
                    kind: SuggestionKind::Property,
                    detail: Some("the event the script runs for: value, change, rc, target, …"),
                });
            }
            rank(&mut items, prefix);
            // A word already written out in full needs no suggestion.
            if items.len() == 1 && items.first().is_some_and(|i| i.label == prefix) {
                return None;
            }
            items
        }
    };
    (!items.is_empty()).then_some(Completion { replace: start..end, items })
}

/// Field names for the caret inside the string `token`, when the string is `getField`'s argument.
fn field_names(source: &str, cursor: usize, token: Range<usize>, fields: &[String]) -> Option<Completion> {
    let text = source.get(token.clone())?;
    let quote = text.chars().next()?;
    let closed = text.len() >= 2 && text.ends_with(quote) && !text.ends_with(&format!("\\{quote}"));
    if closed && cursor == token.end {
        return None;
    }
    let call = source.get(..token.start)?.trim_end().strip_suffix('(')?.trim_end();
    let name_start = call.char_indices().rev().take_while(|(_, c)| is_name(*c)).last().map(|(i, _)| i)?;
    if call.get(name_start..)? != "getField" {
        return None;
    }
    let inner = token.start.saturating_add(quote.len_utf8());
    let inner_end = if closed { token.end.saturating_sub(quote.len_utf8()) } else { token.end };
    let prefix = source.get(inner..cursor)?;
    let lower = prefix.to_lowercase();
    let mut items: Vec<Suggestion> = fields
        .iter()
        // From its start or the start of a part ("city" in "address.city").
        .filter(|f| matches(f, &lower) || f.split('.').any(|part| part.to_lowercase().starts_with(&lower)))
        .map(|f| Suggestion {
            label: f.clone(),
            // An unclosed string gets its closing quote.
            insert: if closed { f.clone() } else { format!("{f}{quote}") },
            kind: SuggestionKind::Field,
            detail: None,
        })
        .collect();
    rank(&mut items, prefix);
    (!items.is_empty()).then_some(Completion { replace: inner..inner_end.max(cursor), items })
}

/// The object the text `head` (everything before the `.`) ends with.
fn object_before(source: &str, head: &str, model: &Model) -> Option<Object> {
    let near = head.len().saturating_sub(MAX_LOOK_BACK);
    let near = (near..=head.len()).find(|i| head.is_char_boundary(*i)).unwrap_or(head.len());
    let tail = head.get(near..)?;
    if let Some(call) = tail.strip_suffix(')') {
        // `getField("…").` or `this.getField("…").`
        let open = call.rfind('(')?;
        let callee = call.get(..open)?.trim_end();
        let word = callee.strip_suffix("getField")?;
        return word.chars().next_back().is_none_or(|c| !is_name(c)).then_some(Object::Field);
    }
    let start = tail.char_indices().rev().take_while(|(_, c)| is_name(*c)).last().map(|(i, _)| i)?;
    let word = tail.get(start..)?;
    let before = tail.get(..start)?;
    if word == "this" {
        return Some(Object::Document);
    }
    if word == "target" && before.ends_with("event.") {
        return Some(Object::Field);
    }
    if before.ends_with('.') {
        return None;
    }
    if set_from_get_field(source, word) {
        return Some(Object::Field);
    }
    Object::named(word).filter(|o| model.members.contains_key(o))
}

/// Whether `source` sets variable `name` from `getField` (`var f = getField("a")`, `f = this.getField(…)`).
fn set_from_get_field(source: &str, name: &str) -> bool {
    source.match_indices(name).any(|(at, _)| {
        let starts_word = source.get(..at).and_then(|b| b.chars().next_back()).is_none_or(|c| !is_name(c));
        let rest = source.get(at.saturating_add(name.len())..).unwrap_or_default().trim_start();
        let Some(value) = rest.strip_prefix('=').filter(|v| !v.starts_with('=')) else { return false };
        let value = value.trim_start();
        starts_word && (value.starts_with("getField(") || value.starts_with("this.getField("))
    })
}

fn members(model: &Model, object: Object, prefix: &str) -> Vec<Suggestion> {
    let lower = prefix.to_lowercase();
    let mut items: Vec<Suggestion> = model
        .members
        .get(&object)
        .into_iter()
        .flatten()
        .filter(|name| matches(name, &lower))
        .map(|name| {
            let detail = describe(object, name);
            let kind = if model.functions.contains(&(object, name.clone())) { SuggestionKind::Function } else { SuggestionKind::Property };
            Suggestion { label: name.clone(), insert: name.clone(), kind, detail }
        })
        .collect();
    rank(&mut items, prefix);
    items
}

/// Whether `name` is offered for the lower-cased `prefix`: it starts with it, or, from three
/// characters on, contains it (`Color` finds `fillColor`, but `al` does not find `setInterval`).
fn matches(name: &str, prefix: &str) -> bool {
    let name = name.to_lowercase();
    name.starts_with(prefix) || (prefix.chars().count() >= 3 && name.contains(prefix))
}

/// Names that start with `prefix` first (ignoring case), then those that contain it; each group
/// in alphabetical order; at most [`MAX_SUGGESTIONS`].
fn rank(items: &mut Vec<Suggestion>, prefix: &str) {
    let lower = prefix.to_lowercase();
    items.sort_by(|a, b| {
        let (la, lb) = (a.label.to_lowercase(), b.label.to_lowercase());
        (!la.starts_with(&lower), la).cmp(&(!lb.starts_with(&lower), lb))
    });
    items.dedup_by(|a, b| a.label == b.label);
    items.truncate(MAX_SUGGESTIONS);
}
