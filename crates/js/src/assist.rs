//! Help for writing scripts: what the object model offers (`model`), warnings about mistakes
//! the parser accepts ([`lint`]) and suggestions as one types ([`complete`]).

mod complete;
mod lint;
mod model;

pub use complete::{Completion, Suggestion, SuggestionKind, complete};
pub use lint::{Knowledge, WORDINGS};

use crate::check::{Diagnostic, with_script};

/// Check `source` as [`crate::check`] does and, when it parses, warn about likely mistakes in it
/// too: a field name the form does not have, a name the object model does not provide (a typo,
/// or an Acrobat feature PdfCraft lacks), assigning to a variable never declared, an assignment
/// where a comparison was meant, and code after a `return` that never runs. The error comes
/// alone; warnings come by place. Nothing is run.
pub fn lint(source: &str, knowledge: Knowledge<'_>) -> Vec<Diagnostic> {
    let model = model::model();
    with_script(source, |script, interner| lint::lint_script(source, script, interner, &model, knowledge)).unwrap_or_else(|e| vec![e])
}

/// Every suggestion's one-line description ([`Suggestion::detail`]), for an interface that says
/// them in its own language.
pub fn descriptions() -> impl Iterator<Item = &'static str> {
    model::DESCRIPTIONS.iter().map(|(_, _, d)| *d).chain([complete::EVENT_DETAIL])
}

/// The names `source` declares (with `var`, `let`, `const`, `function` or `class`, anywhere),
/// for the scripts it is shared with: a document-level script's for the field scripts. Empty if
/// it does not parse.
pub fn declared_names(source: &str) -> Vec<String> {
    with_script(source, lint::declared).map(|names| names.into_iter().collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests;
