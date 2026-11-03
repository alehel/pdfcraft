//! Warnings about mistakes the parser accepts: a field name the form does not have, a name the
//! object model does not provide (a typo, or an Acrobat feature PdfCraft lacks), assigning to a
//! variable never declared, an assignment where a comparison was meant, and code after a
//! `return` that never runs.

use std::collections::{BTreeSet, HashSet};
use std::ops::ControlFlow;

use boa_engine::ast::declaration::{Binding, Variable};
use boa_engine::ast::expression::access::{PropertyAccess, PropertyAccessField, SimplePropertyAccess};
use boa_engine::ast::expression::literal::LiteralKind;
use boa_engine::ast::expression::operator::Assign;
use boa_engine::ast::expression::operator::Conditional;
use boa_engine::ast::expression::operator::assign::{AssignOp, AssignTarget};
use boa_engine::ast::expression::{Call, Identifier};
use boa_engine::ast::function::{
    AsyncFunctionDeclaration, AsyncGeneratorDeclaration, ClassDeclaration, FunctionBody, FunctionDeclaration, GeneratorDeclaration,
};
use boa_engine::ast::operations::bound_names;
use boa_engine::ast::statement::iteration::{DoWhileLoop, ForLoop, WhileLoop};
use boa_engine::ast::statement::{If, Statement};
use boa_engine::ast::visitor::{VisitWith, Visitor};
use boa_engine::ast::{Expression, Script, Span, Spanned, StatementList, StatementListItem};
use boa_engine::interner::{Interner, Sym};

use super::model::{Model, Object};
use crate::check::{Diagnostic, Lines, Wording, spanned};

/// Most warnings reported for one script.
pub(crate) const MAX_WARNINGS: usize = 100;

/// Most field names compared for a suggestion.
const MAX_CANDIDATES: usize = 5000;

/// Longest name compared for a suggestion, in characters.
const MAX_SUGGESTED_LEN: usize = 64;

/// Most comparison steps (characters of one name times characters of the other) spent on the
/// "did you mean" suggestions of one script; past it, warnings come without one. Some
/// milliseconds: a script naming thousands of fields the form lacks must not stall typing.
const SUGGESTION_BUDGET: usize = 4_000_000;

const NO_FIELD: &str = "this form has no field named \"{name}\"";
const NO_FIELD_NEAR: &str = "this form has no field named \"{name}\"; did you mean \"{near}\"?";
const NO_FIELD_CASE: &str = "this form has no field named \"{name}\"; did you mean \"{near}\"? (field names are case-sensitive)";
const NO_MEMBER: &str = "{object} has no \"{name}\" in PdfCraft (a typo, or an Acrobat feature PdfCraft does not provide)";
const NO_MEMBER_NEAR: &str = "{object} has no \"{name}\"; did you mean \"{near}\"?";
const NO_DOCUMENT_MEMBER: &str = "the document has no \"{name}\" in PdfCraft (a typo, or an Acrobat feature PdfCraft does not provide)";
const NO_DOCUMENT_MEMBER_NEAR: &str = "the document has no \"{name}\"; did you mean \"{near}\"?";
const NO_FIELD_MEMBER: &str = "Field objects have no \"{name}\" in PdfCraft (a typo, or an Acrobat feature PdfCraft does not provide)";
const NO_FIELD_MEMBER_NEAR: &str = "Field objects have no \"{name}\"; did you mean \"{near}\"?";
const ASSIGNED_CONDITION: &str = "an assignment where a condition is expected: did you mean == or ===?";
const UNDECLARED: &str = "\"{name}\" is not declared: add var, let or const";
const UNREACHABLE: &str = "this code never runs: it comes after a {what}";

/// Every warning's wording, holes and all, for an interface that says them in its own language.
/// `{object}` is a global's name (`app`, `util` …) and `{what}` a keyword (`return`, `break` …),
/// both as written in a script.
pub const WORDINGS: &[&str] = &[
    NO_FIELD,
    NO_FIELD_NEAR,
    NO_FIELD_CASE,
    NO_MEMBER,
    NO_MEMBER_NEAR,
    NO_DOCUMENT_MEMBER,
    NO_DOCUMENT_MEMBER_NEAR,
    NO_FIELD_MEMBER,
    NO_FIELD_MEMBER_NEAR,
    ASSIGNED_CONDITION,
    UNDECLARED,
    UNREACHABLE,
];

/// A wording and the values of its holes.
fn worded(text: &'static str, holes: &[(&'static str, &str)]) -> Wording {
    Wording { text, holes: holes.iter().map(|(hole, value)| (*hole, value.to_string())).collect() }
}

/// What the script runs with, beyond the object model.
#[derive(Clone, Copy, Debug, Default)]
pub struct Knowledge<'a> {
    /// The form's field names; `None` when the script is checked without a document, which
    /// turns the field name check off.
    pub fields: Option<&'a [String]>,
    /// Names the document-level scripts declare, which every script of the document can use.
    pub shared: &'a [String],
}

pub(crate) fn lint_script(source: &str, script: &Script, interner: &Interner, model: &Model, knowledge: Knowledge<'_>) -> Vec<Diagnostic> {
    let mut names = Declared { interner, declared: BTreeSet::new(), field_vars: BTreeSet::new(), reassigned: BTreeSet::new() };
    let _ = names.visit_script(script);
    let field_vars = names.field_vars.difference(&names.reassigned).cloned().collect();
    let mut check = Check {
        source,
        lines: Lines::new(source),
        interner,
        model,
        knowledge,
        declared: names.declared,
        field_vars,
        known_fields: None,
        budget: SUGGESTION_BUDGET,
        found: Vec::new(),
    };
    let _ = check.visit_script(script);
    let mut found = check.found;
    found.sort_by_key(|d| d.at.as_ref().map(|at| (at.bytes.start, at.bytes.end)));
    found.dedup();
    found.truncate(MAX_WARNINGS);
    found
}

/// The names `script` declares anywhere.
pub(crate) fn declared(script: &Script, interner: &Interner) -> BTreeSet<String> {
    let mut names = Declared { interner, declared: BTreeSet::new(), field_vars: BTreeSet::new(), reassigned: BTreeSet::new() };
    let _ = names.visit_script(script);
    names.declared
}

fn text(interner: &Interner, sym: Sym) -> String {
    interner.resolve(sym).map(|s| s.to_string()).unwrap_or_default()
}

/// Whether `e` names `getField` (bare or `this.getField`).
fn is_get_field(e: &Expression, interner: &Interner) -> bool {
    match e {
        Expression::Identifier(id) => text(interner, id.sym()) == "getField",
        Expression::PropertyAccess(PropertyAccess::Simple(p)) => {
            matches!(p.target(), Expression::This(_)) && matches!(p.field(), PropertyAccessField::Const(f) if text(interner, f.sym()) == "getField")
        }
        _ => false,
    }
}

fn is_get_field_call(e: &Expression, interner: &Interner) -> bool {
    matches!(e, Expression::Call(c) if is_get_field(c.function(), interner))
}

/// The names the script declares anywhere, the variables it sets from `getField`, and the
/// variables bound or assigned some other way (whose value may then be something else). Names,
/// not scopes: a name is a Field variable only if no binding or assignment of it anywhere is
/// something else.
struct Declared<'a> {
    interner: &'a Interner,
    declared: BTreeSet<String>,
    field_vars: BTreeSet<String>,
    reassigned: BTreeSet<String>,
}

impl<'ast> Visitor<'ast> for Declared<'_> {
    type BreakTy = ();

    /// Every binding but `var f = getField(…)` (see [`Self::visit_variable`]): a parameter, a
    /// `catch`, a loop variable, a pattern, `var f;` or `var f = 1`.
    fn visit_binding(&mut self, node: &'ast Binding) -> ControlFlow<()> {
        for sym in bound_names(node) {
            let name = text(self.interner, sym);
            self.reassigned.insert(name.clone());
            self.declared.insert(name);
        }
        node.visit_with(self)
    }

    fn visit_variable(&mut self, node: &'ast Variable) -> ControlFlow<()> {
        if let (Binding::Identifier(id), Some(init)) = (node.binding(), node.init())
            && is_get_field_call(init, self.interner)
        {
            let name = text(self.interner, id.sym());
            self.declared.insert(name.clone());
            self.field_vars.insert(name);
            return self.visit_expression(init);
        }
        node.visit_with(self)
    }

    fn visit_function_declaration(&mut self, node: &'ast FunctionDeclaration) -> ControlFlow<()> {
        self.declared.insert(text(self.interner, node.name().sym()));
        node.visit_with(self)
    }

    fn visit_generator_declaration(&mut self, node: &'ast GeneratorDeclaration) -> ControlFlow<()> {
        self.declared.insert(text(self.interner, node.name().sym()));
        node.visit_with(self)
    }

    fn visit_async_function_declaration(&mut self, node: &'ast AsyncFunctionDeclaration) -> ControlFlow<()> {
        self.declared.insert(text(self.interner, node.name().sym()));
        node.visit_with(self)
    }

    fn visit_async_generator_declaration(&mut self, node: &'ast AsyncGeneratorDeclaration) -> ControlFlow<()> {
        self.declared.insert(text(self.interner, node.name().sym()));
        node.visit_with(self)
    }

    fn visit_class_declaration(&mut self, node: &'ast ClassDeclaration) -> ControlFlow<()> {
        self.declared.insert(text(self.interner, node.name().sym()));
        node.visit_with(self)
    }

    fn visit_assign(&mut self, node: &'ast Assign) -> ControlFlow<()> {
        if let AssignTarget::Identifier(id) = node.lhs() {
            let name = text(self.interner, id.sym());
            if is_get_field_call(node.rhs(), self.interner) {
                self.field_vars.insert(name);
            } else {
                self.reassigned.insert(name);
            }
        }
        node.visit_with(self)
    }
}

struct Check<'a> {
    source: &'a str,
    lines: Lines,
    interner: &'a Interner,
    model: &'a Model,
    knowledge: Knowledge<'a>,
    declared: BTreeSet<String>,
    /// Variables only ever set from `getField`: their members are a Field's.
    field_vars: BTreeSet<String>,
    /// The form's field names and their parents' ("total" for "total.0"), made at the first
    /// `getField` with a name.
    known_fields: Option<HashSet<&'a str>>,
    /// What is left of [`SUGGESTION_BUDGET`].
    budget: usize,
    found: Vec<Diagnostic>,
}

impl<'a> Check<'a> {
    /// Whether another warning would still be kept: past twice the most reported (the rest are
    /// sorted and cut), none is worked out at all.
    fn room(&self) -> bool {
        self.found.len() < MAX_WARNINGS.saturating_mul(2)
    }

    fn warn(&mut self, span: Span, wording: Wording) {
        if self.room() {
            self.found.push(Diagnostic::warning(wording, Some(spanned(&self.lines, self.source, span.start(), span.end()))));
        }
    }

    /// The object `e` is, if it is one of the object model's.
    fn object_of(&self, e: &Expression) -> Option<Object> {
        match e {
            Expression::Identifier(id) => {
                let name = text(self.interner, id.sym());
                if self.field_vars.contains(&name) {
                    return Some(Object::Field);
                }
                // A script that declares `app` (say) means its own.
                Object::named(&name).filter(|_| !self.declared.contains(&name))
            }
            Expression::Call(c) if is_get_field(c.function(), self.interner) => Some(Object::Field),
            Expression::PropertyAccess(PropertyAccess::Simple(p)) => {
                let target = matches!(p.field(), PropertyAccessField::Const(f) if text(self.interner, f.sym()) == "target");
                (target && self.is_event(p.target())).then_some(Object::Field)
            }
            Expression::Parenthesized(p) => self.object_of(p.expression()),
            _ => None,
        }
    }

    /// Whether `e` is the `event` object itself. Only a name is looked at, never a member: a
    /// chain of `.target`s would otherwise be walked again for each access in it.
    fn is_event(&self, mut e: &Expression) -> bool {
        while let Expression::Parenthesized(p) = e {
            e = p.expression();
        }
        matches!(e, Expression::Identifier(_)) && self.object_of(e) == Some(Object::Event)
    }

    /// Whether the form has field `name`, or fields under it ("total" for "total.0").
    fn is_field(&mut self, fields: &'a [String], name: &str) -> bool {
        let known = self.known_fields.get_or_insert_with(|| {
            let mut known = HashSet::with_capacity(fields.len());
            for f in fields {
                known.insert(f.as_str());
                // `.` is ASCII, so every prefix ending before one is on a character boundary.
                known.extend(f.match_indices('.').filter_map(|(i, _)| f.get(..i)));
            }
            known
        });
        known.contains(name)
    }

    fn field_name(&mut self, call: &Call) {
        let Some(fields) = self.knowledge.fields else { return };
        if !self.room() {
            return;
        }
        let Some(Expression::Literal(literal)) = call.args().first() else { return };
        let LiteralKind::String(sym) = literal.kind() else { return };
        let name = text(self.interner, *sym);
        if name.is_empty() {
            return;
        }
        if self.is_field(fields, &name) {
            return;
        }
        let message = match suggest(&name, fields.iter().take(MAX_CANDIDATES).map(String::as_str), &mut self.budget) {
            Some(near) if near.eq_ignore_ascii_case(&name) => worded(NO_FIELD_CASE, &[("name", &name), ("near", near)]),
            Some(near) => worded(NO_FIELD_NEAR, &[("name", &name), ("near", near)]),
            None => worded(NO_FIELD, &[("name", &name)]),
        };
        self.warn(literal.span(), message);
    }

    fn member(&mut self, access: &SimplePropertyAccess) {
        // With no model (the sandbox could not be read), every name would look unknown.
        if !self.room() || self.model.members.is_empty() {
            return;
        }
        let Some(object) = self.object_of(access.target()) else { return };
        let PropertyAccessField::Const(field) = access.field() else { return };
        let name = text(self.interner, field.sym());
        if self.model.has(object, &name) {
            return;
        }
        let members = self.model.members.get(&object).into_iter().flatten().map(String::as_str);
        let near = suggest(&name, members, &mut self.budget);
        let message = match (object.global(), object, near) {
            (_, Object::Document, Some(near)) => worded(NO_DOCUMENT_MEMBER_NEAR, &[("name", &name), ("near", near)]),
            (_, Object::Document, None) => worded(NO_DOCUMENT_MEMBER, &[("name", &name)]),
            (Some(global), _, Some(near)) => worded(NO_MEMBER_NEAR, &[("object", global), ("name", &name), ("near", near)]),
            (Some(global), _, None) => worded(NO_MEMBER, &[("object", global), ("name", &name)]),
            (None, _, Some(near)) => worded(NO_FIELD_MEMBER_NEAR, &[("name", &name), ("near", near)]),
            (None, _, None) => worded(NO_FIELD_MEMBER, &[("name", &name)]),
        };
        self.warn(field.span(), message);
    }

    fn condition(&mut self, cond: &Expression) {
        if let Expression::Assign(a) = cond
            && a.op() == AssignOp::Assign
        {
            self.warn(a.span(), worded(ASSIGNED_CONDITION, &[]));
        }
    }
}

impl<'ast> Visitor<'ast> for Check<'_> {
    type BreakTy = ();

    fn visit_call(&mut self, node: &'ast Call) -> ControlFlow<()> {
        if is_get_field(node.function(), self.interner) {
            self.field_name(node);
        }
        node.visit_with(self)
    }

    fn visit_simple_property_access(&mut self, node: &'ast SimplePropertyAccess) -> ControlFlow<()> {
        self.member(node);
        node.visit_with(self)
    }

    fn visit_assign(&mut self, node: &'ast Assign) -> ControlFlow<()> {
        if let AssignTarget::Identifier(id) = node.lhs() {
            let name = text(self.interner, id.sym());
            // With no model (the sandbox could not be read), every global would look undeclared.
            let known = self.declared.contains(&name)
                || self.knowledge.shared.contains(&name)
                || self.model.globals.contains(&name)
                || self.model.globals.is_empty();
            if !known {
                self.warn(id.span(), worded(UNDECLARED, &[("name", &name)]));
            }
        }
        node.visit_with(self)
    }

    fn visit_if(&mut self, node: &'ast If) -> ControlFlow<()> {
        self.condition(node.cond());
        node.visit_with(self)
    }

    fn visit_while_loop(&mut self, node: &'ast WhileLoop) -> ControlFlow<()> {
        self.condition(node.condition());
        node.visit_with(self)
    }

    fn visit_do_while_loop(&mut self, node: &'ast DoWhileLoop) -> ControlFlow<()> {
        self.condition(node.cond());
        node.visit_with(self)
    }

    fn visit_for_loop(&mut self, node: &'ast ForLoop) -> ControlFlow<()> {
        if let Some(cond) = node.condition() {
            self.condition(cond);
        }
        node.visit_with(self)
    }

    fn visit_conditional(&mut self, node: &'ast Conditional) -> ControlFlow<()> {
        self.condition(node.condition());
        node.visit_with(self)
    }

    fn visit_statement_list(&mut self, node: &'ast StatementList) -> ControlFlow<()> {
        self.unreachable(node.statements());
        node.visit_with(self)
    }

    fn visit_function_body(&mut self, node: &'ast FunctionBody) -> ControlFlow<()> {
        // A function's statements are walked one by one, not as a list.
        self.unreachable(node.statement_list().statements());
        node.visit_with(self)
    }
}

impl Check<'_> {
    /// Warn about the first statement after a `return`, `throw`, `break` or `continue` in `items`.
    fn unreachable(&mut self, items: &[StatementListItem]) {
        let mut after: Option<&str> = None;
        for item in items {
            if let Some(what) = after {
                // A function declaration is hoisted: it is defined even after a return.
                if let StatementListItem::Statement(_) = item
                    && let Some(span) = first_span(item)
                {
                    self.warn(span, worded(UNREACHABLE, &[("what", what)]));
                }
                break;
            }
            if let StatementListItem::Statement(statement) = item {
                after = match statement.as_ref() {
                    Statement::Return(_) => Some("return"),
                    Statement::Throw(_) => Some("throw"),
                    Statement::Break(_) => Some("break"),
                    Statement::Continue(_) => Some("continue"),
                    _ => None,
                };
            }
        }
    }
}

/// The span of the first expression or name in `item` (statements have no span of their own).
fn first_span(item: &StatementListItem) -> Option<Span> {
    struct First(Option<Span>);
    impl<'ast> Visitor<'ast> for First {
        type BreakTy = ();
        fn visit_expression(&mut self, node: &'ast Expression) -> ControlFlow<()> {
            self.0 = Some(node.span());
            ControlFlow::Break(())
        }
        fn visit_identifier(&mut self, node: &'ast Identifier) -> ControlFlow<()> {
            self.0 = Some(node.span());
            ControlFlow::Break(())
        }
    }
    let mut first = First(None);
    let _ = first.visit_statement_list_item(item);
    first.0
}

/// The candidate closest to `name`, if one is close enough to be a likely typo: the same but for
/// case, or a few edits away (about one in three characters). Each comparison is paid for from
/// `budget` (the two names' lengths multiplied); once it runs out, the best found so far.
pub(crate) fn suggest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>, budget: &mut usize) -> Option<&'a str> {
    let len = name.chars().count();
    if len == 0 || len > MAX_SUGGESTED_LEN {
        return None;
    }
    let limit = (len / 3).clamp(1, 3);
    let lower = name.to_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        let other = candidate.chars().count();
        // Names whose lengths differ by more than `limit` are more than `limit` edits apart.
        if other > MAX_SUGGESTED_LEN || other.abs_diff(len) > limit || candidate == name {
            continue;
        }
        let Some(left) = budget.checked_sub(len.saturating_add(1).saturating_mul(other.saturating_add(1))) else { break };
        *budget = left;
        let candidate_lower = candidate.to_lowercase();
        let distance = if candidate_lower == lower { 0 } else { edits(&lower, &candidate_lower, limit) };
        if distance <= limit && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, c)| c)
}

/// The edit distance between `a` and `b` (insertions, deletions, substitutions and swaps of
/// neighbours), on characters, or more than `limit` as soon as it must be. Both are at most
/// [`MAX_SUGGESTED_LEN`] characters long. Every index is below `width`, the length of all three
/// rows, by the loop bounds (`j < b.len()`).
fn edits(a: &str, b: &str, limit: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let width = b.len().saturating_add(1);
    // Rows i-2, i-1 and i of the table.
    let mut two_back = vec![0usize; width];
    let mut previous: Vec<usize> = (0..width).collect();
    let mut row = vec![0usize; width];
    for (i, ca) in a.iter().enumerate() {
        row[0] = i.saturating_add(1);
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let mut best = previous[j].saturating_add(cost).min(previous[j + 1].saturating_add(1)).min(row[j].saturating_add(1));
            if i > 0 && j > 0 && a.get(i - 1) == Some(cb) && b.get(j - 1) == Some(ca) {
                best = best.min(two_back[j - 1].saturating_add(1));
            }
            row[j + 1] = best;
        }
        // A step down comes from this row or, swapping, from the one before it plus one, and no
        // step makes the distance smaller: with both rows past `limit`, so is every row below.
        let past = |r: &[usize]| r.iter().min().is_some_and(|m| *m > limit);
        if past(&row) && past(&previous) {
            return limit.saturating_add(1);
        }
        std::mem::swap(&mut two_back, &mut previous);
        std::mem::swap(&mut previous, &mut row);
    }
    previous.last().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The whole table, with no early way out: what [`edits`] must agree with.
    fn distance(a: &str, b: &str) -> usize {
        edits(a, b, usize::MAX)
    }

    #[test]
    fn with_no_model_no_name_is_called_unknown() {
        let source = "app.alert(1);\nx = 1;\nevent.target.valeu = 2;";
        let found =
            crate::check::with_script(source, |script, interner| lint_script(source, script, interner, &Model::default(), Knowledge::default()));
        assert_eq!(found.map(|f| f.len()), Ok(0));
    }

    proptest! {
        /// Stopping once the distance must be past `limit` never changes a distance within it.
        #[test]
        fn stopping_early_keeps_every_near_distance(a in "[abc]{0,10}", b in "[abc]{0,10}", limit in 0usize..4) {
            let full = distance(&a, &b);
            let early = edits(&a, &b, limit);
            prop_assert_eq!(full.min(limit + 1), early.min(limit + 1), "{} {}", a, b);
        }
    }
}
