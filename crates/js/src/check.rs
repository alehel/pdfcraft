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
    /// What is wrong, without the position (that is in `at`), in English: the parser's words for
    /// an error.
    pub message: String,
    /// Where, when the problem has a place (a script that is too long or too deeply nested has none).
    pub at: Option<Location>,
    /// For a warning, its wording with its values apart, for an interface that says it in its own
    /// language; `None` for an error, which is the parser's.
    pub wording: Option<Wording>,
}

/// A message in two parts, as an interface translates it: one of the [`crate::assist::WORDINGS`]
/// with named holes (`{name}`), and what goes in each, from the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wording {
    pub text: &'static str,
    pub holes: Vec<(&'static str, String)>,
}

impl Wording {
    /// The message read with `wording` (the interface's translation of [`text`](Self::text), or
    /// the text itself). A value goes in as it stands and is not read again, so a name with
    /// braces in it stays whole; a hole the wording lacks is left out, and one it has but this
    /// does not fill stays as written.
    pub fn said(&self, wording: &str) -> String {
        let mut out = String::with_capacity(wording.len().saturating_add(self.holes.iter().map(|(_, v)| v.len()).sum::<usize>()));
        let mut rest = wording;
        while let Some(open) = rest.find('{') {
            // `{` and `}` are ASCII, so these are character boundaries.
            let Some(close) = rest.get(open..).and_then(|r| r.find('}')).map(|c| open.saturating_add(c)) else { break };
            let name = rest.get(open.saturating_add(1)..close).unwrap_or_default();
            out.push_str(rest.get(..open).unwrap_or_default());
            match self.holes.iter().find(|(hole, _)| *hole == name) {
                Some((_, value)) => out.push_str(value),
                None => out.push_str(rest.get(open..=close).unwrap_or_default()),
            }
            rest = rest.get(close.saturating_add(1)..).unwrap_or_default();
        }
        out.push_str(rest);
        out
    }
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
        Diagnostic { severity: Severity::Error, message: message.into(), at, wording: None }
    }

    pub(crate) fn warning(wording: Wording, at: Option<Location>) -> Self {
        Diagnostic { severity: Severity::Warning, message: wording.said(wording.text), at, wording: Some(wording) }
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
    with_script(source, |_, _| ()).err().into_iter().collect()
}

/// Parse `source` and, if it parses, return what `then` makes of its syntax tree; if it does not,
/// the syntax error. On the script thread's stack, as running does: the guard ([`refuse`]) bounds
/// brackets, but nested conditionals still recurse a few thousand levels in the parser and in
/// anything that walks the tree.
pub(crate) fn with_script<T, F>(source: &str, then: F) -> Result<T, Diagnostic>
where
    T: Send,
    F: FnOnce(&boa_engine::ast::Script, &Interner) -> T + Send,
{
    // The same guard as running: a script the runner refuses would also take the parser too deep.
    if let Some(why) = refuse(source) {
        return Err(Diagnostic::error(why, None));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::scope(|scope| {
            let spawned =
                std::thread::Builder::new().name("pdfcraft-js-check".into()).stack_size(SCRIPT_STACK).spawn_scoped(scope, || parse(source, then));
            match spawned {
                Ok(thread) => thread.join().unwrap_or_else(|_| Err(Diagnostic::error("the syntax check stopped with an internal error", None))),
                Err(e) => Err(Diagnostic::error(format!("the syntax check could not start: {e}"), None)),
            }
        })
    }
    #[cfg(target_arch = "wasm32")]
    parse(source, then)
}

fn parse<T, F>(source: &str, then: F) -> Result<T, Diagnostic>
where
    F: FnOnce(&boa_engine::ast::Script, &Interner) -> T,
{
    let mut parser = Parser::new(Source::from_bytes(source));
    let mut interner = Interner::default();
    match parser.parse_script(&Scope::new_global(), &mut interner) {
        Ok(script) => Ok(then(&script, &interner)),
        Err(e) => Err(diagnostic(source, &e)),
    }
}

/// `e` as a [`Diagnostic`] placed in `source`.
fn diagnostic(source: &str, e: &ParseError) -> Diagnostic {
    let lines = Lines::new(source);
    let point = |line: u32, column: u32| {
        let start = lines.byte_at(source, line, column);
        Some(Location { line, column, bytes: start..char_end(source, start) })
    };
    match e {
        ParseError::Expected { expected, found, span, .. } => {
            let message = match &**expected {
                [one] => format!("expected '{one}', got '{found}'"),
                many => format!("expected one of {}, got '{found}'", many.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(", ")),
            };
            Diagnostic::error(message, Some(spanned(&lines, source, span.start(), span.end())))
        }
        ParseError::Unexpected { message, found, span } => {
            Diagnostic::error(format!("unexpected '{found}': {message}"), Some(spanned(&lines, source, span.start(), span.end())))
        }
        ParseError::AbruptEnd => {
            // The script stops in the middle of something; point at its last character.
            let end = source.trim_end().len();
            let start = source.get(..end).and_then(|s| s.char_indices().next_back()).map_or(0, |(i, _)| i);
            let (line, column) = lines.line_column(source, start);
            Diagnostic::error(
                "the script ends before what it opened is closed (a bracket, string, comment or statement)",
                Some(Location { line, column, bytes: start..char_end(source, start) }),
            )
        }
        ParseError::Lex { err: LexError::Syntax(message, at) } => Diagnostic::error(message.to_string(), point(at.line_number(), at.column_number())),
        // boa reports an unclosed string or template without a place; the lexer below finds it.
        ParseError::Lex { err: LexError::IO(e) } => {
            let at = lex(source).1.map(|start| {
                let (line, column) = lines.line_column(source, start);
                Location { line, column, bytes: start..char_end(source, start) }
            });
            Diagnostic::error(e.to_string(), at)
        }
        ParseError::General { message, position } => Diagnostic::error(message.to_string(), point(position.line_number(), position.column_number())),
        ParseError::ScopeAnalysis { err } => Diagnostic::error(*err, None),
    }
}

/// The location from `start` to `end` (both boa positions), at least one character wide.
pub(crate) fn spanned(lines: &Lines, source: &str, start: boa_engine::ast::Position, end: boa_engine::ast::Position) -> Location {
    let (line, column) = (start.line_number(), start.column_number());
    let from = lines.byte_at(source, line, column);
    let to = lines.byte_at(source, end.line_number(), end.column_number()).max(char_end(source, from));
    Location { line, column, bytes: from..to }
}

/// The end of the character starting at byte `at` (`at` itself at the end of `source`).
fn char_end(source: &str, at: usize) -> usize {
    source.get(at..).and_then(|rest| rest.chars().next()).map_or(at, |c| at.saturating_add(c.len_utf8()))
}

/// Where each line of a script starts, counting line ends as JavaScript does (`\n`, `\r\n`, a
/// lone `\r`, U+2028, U+2029): each place is then found within its own line, not by walking the
/// script from its start, which made a long script with many warnings slow.
pub(crate) struct Lines {
    /// Bytes; the first is 0.
    starts: Vec<usize>,
}

impl Lines {
    pub(crate) fn new(source: &str) -> Self {
        let mut starts = vec![0];
        let mut chars = source.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let next = match c {
                '\r' => match chars.peek() {
                    Some(&(n, '\n')) => {
                        chars.next();
                        n.saturating_add(1)
                    }
                    _ => i.saturating_add(1),
                },
                '\n' | '\u{2028}' | '\u{2029}' => i.saturating_add(c.len_utf8()),
                _ => continue,
            };
            starts.push(next);
        }
        Lines { starts }
    }

    /// The byte where 1-based `line` and `column` fall: the end of the line for a column past it
    /// (where the parser was), the end of the source for a line past it.
    pub(crate) fn byte_at(&self, source: &str, line: u32, column: u32) -> usize {
        let Some(index) = usize::try_from(line).ok().and_then(|l| l.checked_sub(1)) else { return 0 };
        let Some(&start) = self.starts.get(index) else { return source.len() };
        let next = self.starts.get(index.saturating_add(1)).copied();
        let text = source.get(start..next.unwrap_or(source.len())).unwrap_or_default();
        let wanted = usize::try_from(column).unwrap_or(usize::MAX).saturating_sub(1);
        match (text.char_indices().nth(wanted), next) {
            (Some((i, _)), _) => start.saturating_add(i),
            // The line's last character: its line end.
            (None, Some(next)) => source.get(..next).and_then(|s| s.char_indices().next_back()).map_or(0, |(i, _)| i),
            (None, None) => source.len(),
        }
    }

    /// The 1-based line and column of the first character at or after byte `at` (the `\n` of a
    /// `\r\n` belongs to its `\r`, so its byte gives the next line's start).
    pub(crate) fn line_column(&self, source: &str, at: usize) -> (u32, u32) {
        let mut at = at.min(source.len());
        while !source.is_char_boundary(at) {
            at = at.saturating_add(1);
        }
        if at > 0 && source.get(at.saturating_sub(1)..).is_some_and(|r| r.starts_with("\r\n")) {
            at = at.saturating_add(1);
        }
        let index = self.starts.partition_point(|s| *s <= at).saturating_sub(1);
        let start = self.starts.get(index).copied().unwrap_or(0);
        let column = source.get(start..at).map_or(0, |before| before.chars().count());
        let number = |n: usize| u32::try_from(n.saturating_add(1)).unwrap_or(u32::MAX);
        (number(index), number(column))
    }
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
pub(crate) const API: &[&str] = &["app", "color", "console", "display", "event", "getField", "util"];

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
    let mut lexer = Lexer {
        s: source,
        i: 0,
        out: Vec::new(),
        regex_ok: true,
        after_dot: false,
        braces: Vec::new(),
        unclosed: None,
        keyword_paren: false,
        parens: Vec::new(),
    };
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
    /// Whether the last token was `if`, `while`, `for` or `with`, whose `(…)` a regular
    /// expression may follow (`if (x) /re/.test(s)`), unlike other brackets' (`(a + b) / 2`).
    keyword_paren: bool,
    /// For each open `(`, whether it was such a keyword's.
    parens: Vec<bool>,
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
                // Whether a `/` after `c` starts a regular expression: not after a value's
                // closing bracket, but after a keyword's `(…)` and after a block's `}`.
                let mut regex_ok = c != ']';
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
                    '(' => self.parens.push(self.keyword_paren),
                    ')' => regex_ok = self.parens.pop().unwrap_or(false),
                    _ => {}
                }
                if !c.is_whitespace() {
                    self.after(regex_ok, c == '.');
                }
            }
        }
    }

    fn after(&mut self, regex_ok: bool, after_dot: bool) {
        self.regex_ok = regex_ok;
        self.after_dot = after_dot;
        self.keyword_paren = false;
    }

    fn skip_to_line_end(&mut self) {
        while self.peek().is_some_and(|c| !is_line_end(c)) {
            self.bump();
        }
    }

    /// The rest of a string opened by `quote`, to its close or the end of the line. Returns
    /// whether it was closed. U+2028 and U+2029 end a line but not a string (ECMAScript 2019).
    fn string(&mut self, quote: char) -> bool {
        while let Some(c) = self.peek() {
            if matches!(c, '\n' | '\r') {
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
        self.keyword_paren = matches!(word, "if" | "while" | "for" | "with");
    }
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_name_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphabetic()
}

pub(crate) fn is_name_part(c: char) -> bool {
    is_name_start(c) || c.is_alphanumeric() || c == '\u{200C}' || c == '\u{200D}'
}

#[cfg(test)]
mod tests;
