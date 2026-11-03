//! Script editing support: syntax colouring ([`highlight`]) and syntax checking ([`check`]).
//!
//! Neither runs the script. [`check`] parses it with boa's parser, the one [`crate::run`] uses,
//! so a script it passes is one the engine will accept, and the reason it gives for a failure is
//! the engine's own. [`highlight`] is a fast, forgiving tokenizer for colouring text as it is
//! typed: it never fails, and it only has to be good enough to colour.

use std::ops::Range;

use boa_engine::Source;
use boa_engine::ast::scope::Scope;
use boa_engine::interner::Interner;
use boa_engine::parser::{Error as ParseError, Parser, lexer::Error as LexError};

#[cfg(not(target_arch = "wasm32"))]
use super::run::SCRIPT_STACK;
use super::run::refuse;

/// A problem [`check`] or [`crate::assist::lint`] found in a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// An error stops the script from running; a warning is a likely mistake that runs.
    pub severity: Severity,
    /// What is wrong, without the position (that is in `at`): the parser's words for an error.
    pub message: String,
    /// Where, when the problem has a place (a script that is too long or too deeply nested has none).
    pub at: Option<Location>,
}

/// How serious a [`Diagnostic`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The script will not run: a syntax error, or too long or deeply nested to parse safely.
    Error,
    /// The script runs, but probably not as meant.
    Warning,
}

impl Diagnostic {
    pub(crate) fn error(message: impl Into<String>, at: Option<Location>) -> Self {
        Diagnostic { severity: Severity::Error, message: message.into(), at }
    }

    pub(crate) fn warning(message: impl Into<String>, at: Option<Location>) -> Self {
        Diagnostic { severity: Severity::Warning, message: message.into(), at }
    }
}

/// A place in a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// 1-based line, counting `\n`, `\r\n`, a lone `\r`, U+2028 and U+2029 as line ends, as JavaScript does.
    pub line: u32,
    /// 1-based column, in characters (Unicode scalar values).
    pub column: u32,
    /// The bytes of the source the problem covers: the offending token, or one character. Always
    /// on character boundaries and within the source; empty only at the end of the source.
    pub bytes: Range<usize>,
}

/// Syntax-check `source` as a script, without running it. Returns the problems found, empty for a
/// script the engine accepts. The parser stops at the first syntax error, so there is at most one.
pub fn check(source: &str) -> Vec<Diagnostic> {
    with_script(source, |_, _| Vec::new())
}

/// Parse `source` and, if it parses, return what `then` finds in its syntax tree; if it does not,
/// the syntax error. On the script thread's stack, as running does: the guard ([`refuse`]) bounds
/// brackets, but nested conditionals still recurse a few thousand levels in the parser and in
/// anything that walks the tree.
pub(crate) fn with_script<F>(source: &str, then: F) -> Vec<Diagnostic>
where
    F: FnOnce(&boa_engine::ast::Script, &Interner) -> Vec<Diagnostic> + Send,
{
    // The same guard as running: a script the runner refuses would also take the parser too deep.
    if let Some(why) = refuse(source) {
        return vec![Diagnostic::error(why, None)];
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::scope(|scope| {
            let spawned =
                std::thread::Builder::new().name("pdfcraft-js-check".into()).stack_size(SCRIPT_STACK).spawn_scoped(scope, || parse(source, then));
            match spawned {
                Ok(thread) => thread.join().unwrap_or_else(|_| vec![Diagnostic::error("the syntax check stopped with an internal error", None)]),
                Err(e) => vec![Diagnostic::error(format!("the syntax check could not start: {e}"), None)],
            }
        })
    }
    #[cfg(target_arch = "wasm32")]
    parse(source, then)
}

fn parse<F>(source: &str, then: F) -> Vec<Diagnostic>
where
    F: FnOnce(&boa_engine::ast::Script, &Interner) -> Vec<Diagnostic>,
{
    let mut parser = Parser::new(Source::from_bytes(source));
    let mut interner = Interner::default();
    match parser.parse_script(&Scope::new_global(), &mut interner) {
        Ok(script) => then(&script, &interner),
        Err(e) => vec![diagnostic(source, &e)],
    }
}

/// `e` as a [`Diagnostic`] placed in `source`.
fn diagnostic(source: &str, e: &ParseError) -> Diagnostic {
    let point = |line: u32, column: u32| {
        let start = byte_at(source, line, column);
        Some(Location { line, column, bytes: start..char_end(source, start) })
    };
    match e {
        ParseError::Expected { expected, found, span, .. } => {
            let message = match &**expected {
                [one] => format!("expected '{one}', got '{found}'"),
                many => format!("expected one of {}, got '{found}'", many.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(", ")),
            };
            Diagnostic::error(message, spanned(source, span.start(), span.end()))
        }
        ParseError::Unexpected { message, found, span } => {
            Diagnostic::error(format!("unexpected '{found}': {message}"), spanned(source, span.start(), span.end()))
        }
        ParseError::AbruptEnd => {
            // The script stops in the middle of something; point at its last character.
            let end = source.trim_end().len();
            let start = source.get(..end).and_then(|s| s.char_indices().next_back()).map_or(0, |(i, _)| i);
            let (line, column) = line_column(source, start);
            Diagnostic::error(
                "the script ends before what it opened is closed (a bracket, string, comment or statement)",
                Some(Location { line, column, bytes: start..char_end(source, start) }),
            )
        }
        ParseError::Lex { err: LexError::Syntax(message, at) } => Diagnostic::error(message.to_string(), point(at.line_number(), at.column_number())),
        // boa reports an unclosed string or template without a place; the lexer below finds it.
        ParseError::Lex { err: LexError::IO(e) } => {
            let at = lex(source).1.map(|start| {
                let (line, column) = line_column(source, start);
                Location { line, column, bytes: start..char_end(source, start) }
            });
            Diagnostic::error(e.to_string(), at)
        }
        ParseError::General { message, position } => Diagnostic::error(message.to_string(), point(position.line_number(), position.column_number())),
        ParseError::ScopeAnalysis { err } => Diagnostic::error(*err, None),
    }
}

/// The location from `start` to `end` (both boa positions), at least one character wide.
pub(crate) fn spanned(source: &str, start: boa_engine::ast::Position, end: boa_engine::ast::Position) -> Option<Location> {
    let (line, column) = (start.line_number(), start.column_number());
    let from = byte_at(source, line, column);
    let to = byte_at(source, end.line_number(), end.column_number()).max(char_end(source, from));
    Some(Location { line, column, bytes: from..to })
}

/// The end of the character starting at byte `at` (`at` itself at the end of `source`).
fn char_end(source: &str, at: usize) -> usize {
    source.get(at..).and_then(|rest| rest.chars().next()).map_or(at, |c| at.saturating_add(c.len_utf8()))
}

/// Calls `visit(byte, line, column)` for each character of `source` and once more for its end,
/// counting lines as JavaScript does, until `visit` returns `true`.
fn walk(source: &str, mut visit: impl FnMut(usize, u32, u32) -> bool) {
    let (mut line, mut column) = (1u32, 1u32);
    let mut chars = source.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if visit(i, line, column) {
            return;
        }
        match c {
            '\r' => {
                if chars.peek().is_some_and(|(_, n)| *n == '\n') {
                    chars.next();
                }
                (line, column) = (line.saturating_add(1), 1);
            }
            '\n' | '\u{2028}' | '\u{2029}' => (line, column) = (line.saturating_add(1), 1),
            _ => column = column.saturating_add(1),
        }
    }
    visit(source.len(), line, column);
}

/// The byte where 1-based `line` and `column` fall: the end of the line for a column past it,
/// the end of the source for a line past it.
fn byte_at(source: &str, line: u32, column: u32) -> usize {
    let mut found = source.len();
    walk(source, |i, l, c| {
        if l > line || (l == line && c >= column) {
            found = i;
            return true;
        }
        false
    });
    // A column past the line's end lands on the next line's first character; step back to the
    // line end, which is where the parser was.
    if line_column(source, found).0 > line {
        found = source.get(..found).and_then(|s| s.char_indices().next_back()).map_or(0, |(i, _)| i);
    }
    found
}

/// The 1-based line and column of byte `at` (rounded down to a character boundary).
fn line_column(source: &str, at: usize) -> (u32, u32) {
    let mut found = (1, 1);
    walk(source, |i, l, c| {
        found = (l, c);
        i >= at
    });
    found
}

/// What a stretch of script is, for colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// A reserved word: `var`, `function`, `if`, `return`, `this` …
    Keyword,
    /// `true`, `false`, `null`, `undefined`, `NaN`, `Infinity`.
    Literal,
    /// A number.
    Number,
    /// A quoted string.
    String,
    /// A template literal's text (the code in its `${…}` is coloured as code).
    Template,
    /// A regular expression literal.
    Regex,
    /// A comment.
    Comment,
    /// A name from the Acrobat object model: `event`, `app`, `util`, `console`, `getField` …
    Api,
}

/// A coloured stretch of a script: `bytes` (on character boundaries) is `kind`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub bytes: Range<usize>,
}

const KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const LITERALS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];

/// Globals PdfCraft provides from Acrobat's object model (see the crate documentation).
const API: &[&str] = &["app", "color", "console", "display", "event", "getField", "global", "util"];

/// Keywords after which a `/` starts a regular expression rather than dividing.
const BEFORE_REGEX: &[&str] =
    &["return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw", "instanceof", "yield", "await"];

/// Deepest `${` nesting of template literals tracked; deeper ones are coloured as template text.
const MAX_TEMPLATE_DEPTH: usize = 64;

/// The coloured stretches of `source`, in order and not overlapping; text between them (names,
/// operators, white space) is plain. Never fails: unclosed strings, comments and templates run
/// to the end of their line or of the source.
pub fn highlight(source: &str) -> Vec<Token> {
    lex(source).0
}

/// [`highlight`]'s tokens, and where the first string or template left open starts.
fn lex(source: &str) -> (Vec<Token>, Option<usize>) {
    let mut lexer = Lexer { s: source, i: 0, out: Vec::new(), regex_ok: true, after_dot: false, braces: Vec::new(), unclosed: None };
    lexer.run();
    (lexer.out, lexer.unclosed)
}

struct Lexer<'a> {
    s: &'a str,
    /// The next byte to read; always on a character boundary.
    i: usize,
    out: Vec<Token>,
    /// Whether a `/` here starts a regular expression.
    regex_ok: bool,
    /// Whether the last token was a `.` (a name after it is a property, never a keyword).
    after_dot: bool,
    /// For each open `${`, the `{` opened inside it and not yet closed, and where its template starts.
    braces: Vec<(usize, usize)>,
    /// Where the first string or template that is never closed starts.
    unclosed: Option<usize>,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i..).and_then(|r| r.chars().next())
    }

    fn peek2(&self) -> Option<char> {
        self.s.get(self.i..).and_then(|r| r.chars().nth(1))
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i = self.i.saturating_add(c.len_utf8());
        Some(c)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        if self.i > start {
            self.out.push(Token { kind, bytes: start..self.i });
        }
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.i;
            if c == '/' && self.peek2() == Some('/') {
                self.skip_to_line_end();
                self.push(TokenKind::Comment, start);
            } else if c == '/' && self.peek2() == Some('*') {
                self.i = self
                    .s
                    .get(start.saturating_add(2)..)
                    .and_then(|r| r.find("*/"))
                    .map_or(self.s.len(), |end| start.saturating_add(2).saturating_add(end).saturating_add(2));
                self.push(TokenKind::Comment, start);
            } else if c == '/' && self.regex_ok {
                self.regex();
                self.push(TokenKind::Regex, start);
                self.after(false, false);
            } else if c == '\'' || c == '"' {
                self.bump();
                if !self.string(c) {
                    self.unclosed.get_or_insert(start);
                }
                self.push(TokenKind::String, start);
                self.after(false, false);
            } else if c == '`' {
                self.bump();
                self.template(start, start);
            } else if let Some(&(0, opened)) = self.braces.last()
                && c == '}'
            {
                // The end of a template's `${…}`: back into its text.
                self.braces.pop();
                self.bump();
                self.template(start, opened);
            } else if c.is_ascii_digit() || (c == '.' && self.peek2().is_some_and(|d| d.is_ascii_digit())) {
                self.number();
                self.push(TokenKind::Number, start);
                self.after(false, false);
            } else if is_name_start(c) {
                self.word(start);
            } else {
                self.bump();
                match c {
                    '{' => {
                        if let Some((open, _)) = self.braces.last_mut() {
                            *open = open.saturating_add(1);
                        }
                    }
                    '}' => {
                        if let Some((open, _)) = self.braces.last_mut() {
                            *open = open.saturating_sub(1);
                        }
                    }
                    _ => {}
                }
                if !c.is_whitespace() {
                    self.after(!matches!(c, ')' | ']' | '}'), c == '.');
                }
            }
        }
    }

    fn after(&mut self, regex_ok: bool, after_dot: bool) {
        self.regex_ok = regex_ok;
        self.after_dot = after_dot;
    }

    fn skip_to_line_end(&mut self) {
        while self.peek().is_some_and(|c| !is_line_end(c)) {
            self.bump();
        }
    }

    /// The rest of a string opened by `quote`, to its close or the end of the line. Returns
    /// whether it was closed.
    fn string(&mut self, quote: char) -> bool {
        while let Some(c) = self.peek() {
            if is_line_end(c) {
                return false;
            }
            self.bump();
            if c == '\\' {
                // An escaped character, or a line continuation (`\r\n` counts as one).
                if self.bump() == Some('\r') && self.peek() == Some('\n') {
                    self.bump();
                }
            } else if c == quote {
                return true;
            }
        }
        false
    }

    /// A regular expression: to its closing `/` outside a class, then its flags.
    fn regex(&mut self) {
        self.bump();
        let mut class = false;
        while let Some(c) = self.peek() {
            if is_line_end(c) {
                return;
            }
            self.bump();
            match c {
                '\\' => {
                    if self.peek().is_some_and(|c| !is_line_end(c)) {
                        self.bump();
                    }
                }
                '[' => class = true,
                ']' => class = false,
                '/' if !class => break,
                _ => {}
            }
        }
        while self.peek().is_some_and(is_name_part) {
            self.bump();
        }
    }

    /// Template text from `start` (just past its opening `` ` `` or `}`): to the closing `` ` ``,
    /// or to a `${`, after which the lexer reads code until the matching `}`.
    /// `opened` is where to report the template if it is never closed.
    fn template(&mut self, start: usize, opened: usize) {
        while let Some(c) = self.bump() {
            match c {
                '\\' => {
                    self.bump();
                }
                '`' => {
                    self.push(TokenKind::Template, start);
                    self.after(false, false);
                    return;
                }
                '$' if self.peek() == Some('{') && self.braces.len() < MAX_TEMPLATE_DEPTH => {
                    self.bump();
                    self.push(TokenKind::Template, start);
                    self.braces.push((0, opened));
                    self.after(true, false);
                    return;
                }
                _ => {}
            }
        }
        self.unclosed.get_or_insert(opened);
        self.push(TokenKind::Template, start);
    }

    fn number(&mut self) {
        let hex = self.peek() == Some('0') && matches!(self.peek2(), Some('x' | 'X'));
        let mut last = '\0';
        while let Some(c) = self.peek() {
            let sign = matches!(c, '+' | '-') && matches!(last, 'e' | 'E') && !hex;
            if !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || sign) {
                break;
            }
            last = c;
            self.bump();
        }
    }

    fn word(&mut self, start: usize) {
        while self.peek().is_some_and(is_name_part) {
            self.bump();
        }
        let word = self.s.get(start..self.i).unwrap_or_default();
        if self.after_dot {
            // A property: `x.default`, `this.event` …
            self.after(false, false);
            return;
        }
        let kind = if KEYWORDS.contains(&word) {
            Some(TokenKind::Keyword)
        } else if LITERALS.contains(&word) {
            Some(TokenKind::Literal)
        } else if API.contains(&word) {
            Some(TokenKind::Api)
        } else {
            None
        };
        if let Some(kind) = kind {
            self.push(kind, start);
        }
        self.after(BEFORE_REGEX.contains(&word), false);
    }
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_name_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphabetic()
}

fn is_name_part(c: char) -> bool {
    is_name_start(c) || c.is_alphanumeric() || c == '\u{200C}' || c == '\u{200D}'
}

#[cfg(test)]
mod tests;
