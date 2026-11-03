//! Warnings about mistakes the parser accepts: a field name the form does not have, a name the
//! object model does not provide (a typo, or an Acrobat feature PdfCraft lacks), assigning to a
//! variable never declared, an assignment where a comparison was meant, and code after a
//! `return` that never runs.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use boa_engine::ast::declaration::{Binding, Variable};
use boa_engine::ast::expression::access::{PropertyAccess, PropertyAccessField, SimplePropertyAccess};
use boa_engine::ast::expression::literal::LiteralKind;
use boa_engine::ast::expression::operator::Assign;
use boa_engine::ast::expression::operator::Conditional;
use boa_engine::ast::expression::operator::assign::{AssignOp, AssignTarget};
use boa_engine::ast::expression::{Call, Identifier};
use boa_engine::ast::function::{ClassDeclaration, FunctionBody, FunctionDeclaration};
use boa_engine::ast::operations::bound_names;
use boa_engine::ast::statement::iteration::{DoWhileLoop, ForLoop, WhileLoop};
use boa_engine::ast::statement::{If, Statement};
use boa_engine::ast::visitor::{VisitWith, Visitor};
use boa_engine::ast::{Expression, Script, Span, Spanned, StatementList, StatementListItem};
use boa_engine::interner::{Interner, Sym};

use super::model::{Model, Object};
use crate::check::{Diagnostic, spanned};

/// Most warnings reported for one script.
pub(crate) const MAX_WARNINGS: usize = 100;

/// Most field names compared for a suggestion.
const MAX_CANDIDATES: usize = 5000;

/// Longest name compared for a suggestion, in characters.
const MAX_SUGGESTED_LEN: usize = 64;

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
    let mut check = Check { source, interner, model, knowledge, declared: names.declared, field_vars, found: Vec::new() };
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
/// variables it assigns (whose value may then be something else).
struct Declared<'a> {
    interner: &'a Interner,
    declared: BTreeSet<String>,
    field_vars: BTreeSet<String>,
    reassigned: BTreeSet<String>,
}

impl<'ast> Visitor<'ast> for Declared<'_> {
    type BreakTy = ();

    fn visit_binding(&mut self, node: &'ast Binding) -> ControlFlow<()> {
        for sym in bound_names(node) {
            self.declared.insert(text(self.interner, sym));
        }
        node.visit_with(self)
    }

    fn visit_variable(&mut self, node: &'ast Variable) -> ControlFlow<()> {
        if let (Binding::Identifier(id), Some(init)) = (node.binding(), node.init())
            && is_get_field_call(init, self.interner)
        {
            self.field_vars.insert(text(self.interner, id.sym()));
        }
        node.visit_with(self)
    }

    fn visit_function_declaration(&mut self, node: &'ast FunctionDeclaration) -> ControlFlow<()> {
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
    interner: &'a Interner,
    model: &'a Model,
    knowledge: Knowledge<'a>,
    declared: BTreeSet<String>,
    /// Variables only ever set from `getField`: their members are a Field's.
    field_vars: BTreeSet<String>,
    found: Vec<Diagnostic>,
}

impl Check<'_> {
    fn warn(&mut self, span: Span, message: String) {
        if self.found.len() < MAX_WARNINGS.saturating_mul(2) {
            self.found.push(Diagnostic::warning(message, spanned(self.source, span.start(), span.end())));
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
                (target && self.object_of(p.target()) == Some(Object::Event)).then_some(Object::Field)
            }
            Expression::Parenthesized(p) => self.object_of(p.expression()),
            _ => None,
        }
    }

    fn field_name(&mut self, call: &Call) {
        let Some(fields) = self.knowledge.fields else { return };
        let Some(Expression::Literal(literal)) = call.args().first() else { return };
        let LiteralKind::String(sym) = literal.kind() else { return };
        let name = text(self.interner, *sym);
        if name.is_empty() {
            return;
        }
        // A parent name stands for its kids ("total" for "total.0").
        let known = fields.iter().any(|f| *f == name || f.strip_prefix(name.as_str()).is_some_and(|rest| rest.starts_with('.')));
        if known {
            return;
        }
        let message = match suggest(&name, fields.iter().take(MAX_CANDIDATES).map(String::as_str)) {
            Some(near) if near.eq_ignore_ascii_case(&name) => {
                format!("this form has no field named \"{name}\"; did you mean \"{near}\"? (field names are case-sensitive)")
            }
            Some(near) => format!("this form has no field named \"{name}\"; did you mean \"{near}\"?"),
            None => format!("this form has no field named \"{name}\""),
        };
        self.warn(literal.span(), message);
    }

    fn member(&mut self, access: &SimplePropertyAccess) {
        let Some(object) = self.object_of(access.target()) else { return };
        let PropertyAccessField::Const(field) = access.field() else { return };
        let name = text(self.interner, field.sym());
        if self.model.has(object, &name) {
            return;
        }
        let members = self.model.members.get(&object).into_iter().flatten().map(String::as_str);
        let message = match suggest(&name, members) {
            Some(near) => format!("{} no \"{name}\"; did you mean \"{near}\"?", object.has()),
            None => format!("{} no \"{name}\" in PdfCraft (a typo, or an Acrobat feature PdfCraft does not provide)", object.has()),
        };
        self.warn(field.span(), message);
    }

    fn condition(&mut self, cond: &Expression) {
        if let Expression::Assign(a) = cond
            && a.op() == AssignOp::Assign
        {
            self.warn(a.span(), "an assignment where a condition is expected: did you mean == or ===?".into());
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
            let known = self.declared.contains(&name) || self.knowledge.shared.contains(&name) || self.model.globals.contains(&name);
            if !known {
                self.warn(id.span(), format!("\"{name}\" is not declared: add var, let or const"));
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
                    self.warn(span, format!("this code never runs: it comes after a {what}"));
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
/// case, or a few edits away (about one in three characters).
pub(crate) fn suggest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let len = name.chars().count();
    if len == 0 || len > MAX_SUGGESTED_LEN {
        return None;
    }
    let limit = (len / 3).clamp(1, 3);
    let lower = name.to_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        if candidate.chars().count() > MAX_SUGGESTED_LEN || candidate == name {
            continue;
        }
        let distance = if candidate.to_lowercase() == lower { 0 } else { edits(&lower, &candidate.to_lowercase()) };
        if distance <= limit && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, c)| c)
}

/// The edit distance between `a` and `b` (insertions, deletions, substitutions and swaps of
/// neighbours), on characters. Both are at most [`MAX_SUGGESTED_LEN`] characters long. Every index
/// is below `width`, the length of all three rows, by the loop bounds (`j < b.len()`).
fn edits(a: &str, b: &str) -> usize {
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
        std::mem::swap(&mut two_back, &mut previous);
        std::mem::swap(&mut previous, &mut row);
    }
    previous.last().copied().unwrap_or_default()
}
