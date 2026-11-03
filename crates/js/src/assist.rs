//! Help for writing scripts: what the object model offers ([`model`]), warnings about mistakes
//! the parser accepts ([`lint`]) and suggestions as one types ([`complete`]).

pub mod complete;
pub mod lint;
pub mod model;

use std::collections::BTreeSet;

pub use complete::{Completion, Suggestion, SuggestionKind, complete};
pub use lint::Knowledge;
pub use model::{Model, Object, model};

use crate::check::{Diagnostic, with_script};

/// Check `source` as [`crate::check`] does and, when it parses, warn about likely mistakes in it
/// too (see [`lint`](mod@lint)). Errors come first, then warnings by place. Nothing is run.
pub fn lint(source: &str, knowledge: Knowledge<'_>) -> Vec<Diagnostic> {
    let model = model();
    with_script(source, |script, interner| lint::lint_script(source, script, interner, &model, knowledge))
}

/// The names `source` declares (with `var`, `let`, `const`, `function` or `class`, anywhere),
/// for the scripts it is shared with: a document-level script's for the field scripts. Empty if
/// it does not parse.
pub fn declared_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Through `with_script` for its guard and stack; the names come back in a diagnostic-free channel.
    let names = std::sync::Mutex::new(BTreeSet::new());
    let _ = with_script(source, |script, interner| {
        let found = lint::declared(script, interner);
        if let Ok(mut n) = names.lock() {
            *n = found;
        }
        Vec::new()
    });
    if let Ok(n) = names.into_inner() {
        out.extend(n);
    }
    out
}

#[cfg(test)]
mod tests;
