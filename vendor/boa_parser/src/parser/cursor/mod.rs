//! Cursor implementation for the parser.
mod buffered_lexer;

use crate::{
    Error,
    lexer::{InputElement, Lexer, Token, TokenKind},
    parser::{OrAbrupt, ParseResult},
    source::ReadChar,
};
use boa_ast::{LinearPosition, Position, PositionGroup, Punctuator, Spanned};
use boa_interner::{Interner, Sym};
use buffered_lexer::BufferedLexer;
use rustc_hash::FxHashSet;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// PdfCraft patch: how many recursive `parse` calls may be open at once. The parser recurses on
/// the source's nesting, and a stack overflow aborts the process, past any `catch_unwind`. Every
/// recursion cycle passes [`Cursor::enter_nesting`]. An expression's own descent is the costly
/// one: `(` opens 3 of these and takes about 28 KiB of stack in a release build, so 256 is 84
/// parentheses and about 2.4 MiB, or 126 calls `f(` and about 3 MiB.
#[cfg(not(target_arch = "wasm32"))]
const MAX_NESTING: u32 = 256;

/// PdfCraft patch: [`MAX_NESTING`] in the browser, whose stack is 1 MiB: measured under V8, the
/// costliest shapes stay within 700 KiB of it (`f(` 46 times, `a?.[` 47 times).
#[cfg(target_arch = "wasm32")]
const MAX_NESTING: u32 = 96;

/// PdfCraft patch: the tallest syntax tree that loops may build. `a+a+a` and `a.b.c` take no
/// parser recursion, but their trees are as tall as they are long, and scope analysis, the
/// compiler, `Clone` and `Drop` all recurse on the tree. The compiler's register allocator also
/// scans every register in use, one per level, so its cost grows with height too.
#[cfg(not(target_arch = "wasm32"))]
const MAX_TREE_HEIGHT: u32 = 4096;

/// PdfCraft patch: [`MAX_TREE_HEIGHT`] in the browser: measured under V8, 2,400 links are enough
/// to overflow its stack in the compiler.
#[cfg(target_arch = "wasm32")]
const MAX_TREE_HEIGHT: u32 = 1024;

/// PdfCraft patch: the most different names one function, or the script's top level, may declare,
/// however its blocks divide them. Scope analysis and the compiler look names up by walking a
/// list, so the cost grows with the square of this: 40,000 `var`s in a function took 14 s, and a
/// script that is nothing but functions of 2048 names takes 0.6 s.
const MAX_NAMES_IN_FUNCTION: usize = 2048;

/// PdfCraft patch: the most `let` and `const` names one function may declare, counting a name again
/// each time. Each takes a register until the function ends, and the compiler's register allocator
/// scans them all: `{let a,b,…}` over and over, with 54 names, took 4 s.
const MAX_LEXICAL_NAMES_IN_FUNCTION: u32 = 8192;

/// PdfCraft patch: what [`Cursor::enter_nesting`] and [`Cursor::chain_link`] keep count of.
#[derive(Debug, Clone, Copy, Default)]
struct Nesting {
    /// Recursive `parse` calls open now.
    depth: u32,
    /// An upper bound on the height of the trees finished under the innermost open call, plus
    /// the chain links made in it: a link goes on top of what it follows.
    height: u32,
    /// Whether a labelled function is open: see [`Cursor::enter_labelled_function`].
    in_labelled_function: bool,
    /// The innermost function's `let` and `const` names: see [`Cursor::declare_lexical_name`].
    lexical_names: u32,
}

/// PdfCraft patch: an open recursive `parse` call, closed when dropped, on every way out of it.
#[derive(Debug)]
pub(super) struct NestingGuard {
    nesting: Rc<Cell<Nesting>>,
    cost: u32,
    outer_height: u32,
}

impl Drop for NestingGuard {
    fn drop(&mut self) {
        let nesting = self.nesting.get();
        self.nesting.set(Nesting {
            depth: nesting.depth.saturating_sub(self.cost),
            height: self.outer_height.max(nesting.height.saturating_add(1)),
            ..nesting
        });
    }
}

/// PdfCraft patch: an open labelled function.
#[derive(Debug)]
pub(super) struct LabelledFunctionGuard {
    nesting: Rc<Cell<Nesting>>,
}

impl Drop for LabelledFunctionGuard {
    fn drop(&mut self) {
        self.nesting.set(Nesting {
            in_labelled_function: false,
            ..self.nesting.get()
        });
    }
}

/// PdfCraft patch: an open function's parameters or body; dropped, the names counted are those
/// of the function around it again.
#[derive(Debug)]
pub(super) struct FunctionNamesGuard {
    names: Rc<RefCell<FxHashSet<Sym>>>,
    outer: FxHashSet<Sym>,
    nesting: Rc<Cell<Nesting>>,
    outer_lexical_names: u32,
}

impl Drop for FunctionNamesGuard {
    fn drop(&mut self) {
        // Never borrowed but within these calls, so this can't panic.
        self.names.replace(std::mem::take(&mut self.outer));
        self.nesting.set(Nesting {
            lexical_names: self.outer_lexical_names,
            ..self.nesting.get()
        });
    }
}

/// The result of a peek for a semicolon.
#[derive(Debug)]
pub(super) enum SemicolonResult<'s> {
    Found(Option<&'s Token>),
    NotFound(&'s Token),
}

/// Token cursor.
///
/// This internal structure gives basic testable operations to the parser.
#[derive(Debug)]
pub(super) struct Cursor<R> {
    buffered_lexer: BufferedLexer<R>,

    /// Tracks if the cursor is in a arrow function declaration.
    arrow: bool,

    /// Indicate if the cursor is used in `JSON.parse`.
    json_parse: bool,

    /// A unique identifier for each parser instance.
    /// This is used to generate unique identifiers tagged template literals.
    identifier: u32,

    /// Tracks the number of tagged templates that are currently being parsed.
    tagged_templates_count: u32,

    /// PdfCraft patch: see [`Self::enter_nesting`] and [`Self::chain_link`].
    nesting: Rc<Cell<Nesting>>,

    /// PdfCraft patch: the names the innermost function declares; see [`Self::declare_name`].
    function_names: Rc<RefCell<FxHashSet<Sym>>>,
}

impl<R> Cursor<R>
where
    R: ReadChar,
{
    /// Creates a new cursor with the given reader.
    pub(super) fn new(reader: R) -> Self {
        Self {
            buffered_lexer: Lexer::new(reader).into(),
            arrow: false,
            json_parse: false,
            identifier: 0,
            tagged_templates_count: 0,
            nesting: Rc::default(),
            function_names: Rc::default(),
        }
    }

    /// PdfCraft patch: records a recursive `parse` call, open until the guard is dropped, or
    /// returns an ordinary parse error if too many are open already.
    pub(super) fn enter_nesting(&mut self, interner: &mut Interner) -> ParseResult<NestingGuard> {
        self.enter_nesting_at(1, interner)
    }

    /// PdfCraft patch: [`Self::enter_nesting`], for a call that counts as `cost` of them.
    pub(super) fn enter_nesting_at(
        &mut self,
        cost: u32,
        interner: &mut Interner,
    ) -> ParseResult<NestingGuard> {
        let nesting = self.nesting.get();
        let depth = nesting.depth.saturating_add(cost);
        if depth > MAX_NESTING {
            return Err(self.limit_error("the code is nested too deeply to run safely", interner));
        }
        self.nesting.set(Nesting {
            depth,
            height: 0,
            ..nesting
        });
        Ok(NestingGuard {
            nesting: Rc::clone(&self.nesting),
            cost,
            outer_height: nesting.height,
        })
    }

    /// PdfCraft patch: records a link added to a tree by a loop rather than by recursion, which
    /// is one level of height, or returns an ordinary parse error if the tree is too tall.
    pub(super) fn chain_link(&mut self, interner: &mut Interner) -> ParseResult<()> {
        let nesting = self.nesting.get();
        if nesting.height >= MAX_TREE_HEIGHT {
            return Err(self.chain_error(interner));
        }
        self.nesting.set(Nesting {
            height: nesting.height.saturating_add(1),
            ..nesting
        });
        Ok(())
    }

    /// PdfCraft patch: `parse`, with the height of what it builds measured on its own.
    pub(super) fn measure_height<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> ParseResult<T>,
    ) -> ParseResult<(T, u32)> {
        let outer = self.nesting.get().height;
        self.set_height(0);
        let parsed = parse(self)?;
        let measured = self.nesting.get().height;
        self.set_height(outer);
        Ok((parsed, measured))
    }

    /// PdfCraft patch: records that the innermost open call holds a tree of `height`, or returns
    /// an ordinary parse error if that is too tall.
    pub(super) fn include_height(
        &mut self,
        height: u32,
        interner: &mut Interner,
    ) -> ParseResult<()> {
        if height > MAX_TREE_HEIGHT {
            return Err(self.chain_error(interner));
        }
        self.set_height(self.nesting.get().height.max(height));
        Ok(())
    }

    /// PdfCraft patch: sets the height held for the innermost open call.
    fn set_height(&self, height: u32) {
        self.nesting.set(Nesting {
            height,
            ..self.nesting.get()
        });
    }

    /// PdfCraft patch: records a labelled function (`a: function b() {}`, which sloppy code may
    /// have), or returns an ordinary parse error if it is inside another. The compiler compiles
    /// each of them twice, once to hoist it and once where it stands, so nested ones cost twice
    /// as much per level: 24 levels, 400 bytes of script, took over a minute. No form does this.
    pub(super) fn enter_labelled_function(
        &mut self,
        interner: &mut Interner,
    ) -> ParseResult<LabelledFunctionGuard> {
        let nesting = self.nesting.get();
        if nesting.in_labelled_function {
            return Err(self.limit_error(
                "a labelled function inside another can't run safely",
                interner,
            ));
        }
        self.nesting.set(Nesting {
            in_labelled_function: true,
            ..nesting
        });
        Ok(LabelledFunctionGuard {
            nesting: Rc::clone(&self.nesting),
        })
    }

    /// PdfCraft patch: starts counting the names of a function's parameters or body, until the
    /// guard is dropped.
    pub(super) fn enter_function_names(&self) -> FunctionNamesGuard {
        let nesting = self.nesting.get();
        self.nesting.set(Nesting {
            lexical_names: 0,
            ..nesting
        });
        FunctionNamesGuard {
            names: Rc::clone(&self.function_names),
            outer: self.function_names.take(),
            nesting: Rc::clone(&self.nesting),
            outer_lexical_names: nesting.lexical_names,
        }
    }

    /// PdfCraft patch: records a declared name, or returns an ordinary parse error if the
    /// function has too many. A name declared again costs nothing: `var i` in every loop is fine.
    pub(super) fn declare_name(&mut self, name: Sym, interner: &mut Interner) -> ParseResult<()> {
        let too_many = {
            let mut names = self.function_names.borrow_mut();
            names.insert(name) && names.len() > MAX_NAMES_IN_FUNCTION
        };
        if too_many {
            return Err(self.limit_error(
                "one function declares too many names to run safely",
                interner,
            ));
        }
        Ok(())
    }

    /// PdfCraft patch: records a `let` or `const` name, or returns an ordinary parse error if the
    /// function has too many.
    pub(super) fn declare_lexical_name(&mut self, interner: &mut Interner) -> ParseResult<()> {
        let nesting = self.nesting.get();
        if nesting.lexical_names >= MAX_LEXICAL_NAMES_IN_FUNCTION {
            return Err(self.limit_error(
                "one function declares too many names to run safely",
                interner,
            ));
        }
        self.nesting.set(Nesting {
            lexical_names: nesting.lexical_names.saturating_add(1),
            ..nesting
        });
        Ok(())
    }

    /// PdfCraft patch: the error for too tall a tree.
    fn chain_error(&mut self, interner: &mut Interner) -> Error {
        self.limit_error(
            "the chain of operators, calls or else-ifs is too long to run safely",
            interner,
        )
    }

    /// PdfCraft patch: the error for a limit reached at the next token.
    fn limit_error(&mut self, message: &'static str, interner: &mut Interner) -> Error {
        let position = match self.peek(0, interner) {
            Ok(Some(token)) => token.span().start(),
            _ => Position::new(1, 1),
        };
        Error::general(message, position)
    }

    /// Sets the goal symbol of the cursor to `Module`.
    pub(super) fn set_module(&mut self) {
        self.buffered_lexer.set_module(true);
    }

    /// Returns `true` if the cursor is currently parsing a `Module`.
    pub(super) const fn module(&self) -> bool {
        self.buffered_lexer.module()
    }

    pub(super) fn set_goal(&mut self, elm: InputElement) {
        self.buffered_lexer.set_goal(elm);
    }

    /// Lexes the next tokens as a regex assuming that the starting '/' has already been consumed.
    /// If `init_with_eq` is `true`, then assuming that the starting '/=' has already been consumed.
    pub(super) fn lex_regex(
        &mut self,
        start: PositionGroup,
        interner: &mut Interner,
        init_with_eq: bool,
    ) -> ParseResult<Token> {
        self.buffered_lexer.lex_regex(start, interner, init_with_eq)
    }

    pub(super) fn lex_template(
        &mut self,
        start: PositionGroup,
        interner: &mut Interner,
    ) -> ParseResult<Token> {
        self.buffered_lexer.lex_template(start, interner)
    }

    /// Advances the cursor and returns the next token.
    pub(super) fn next(&mut self, interner: &mut Interner) -> ParseResult<Option<Token>> {
        self.buffered_lexer.next(true, interner)
    }

    /// Advances the cursor without returning the next token.
    ///
    /// # Panics
    ///
    /// This function will panic if there is no further token in the cursor.
    #[track_caller]
    pub(super) fn advance(&mut self, interner: &mut Interner) {
        self.next(interner)
            .expect("tried to advance cursor, but the buffer was empty");
    }

    /// Peeks a future token, without consuming it or advancing the cursor.
    /// This peeking **skips** line terminators.
    ///
    /// You can skip some tokens with the `skip_n` option.
    pub(super) fn peek(
        &mut self,
        skip_n: usize,
        interner: &mut Interner,
    ) -> ParseResult<Option<&Token>> {
        self.buffered_lexer.peek(skip_n, true, interner)
    }

    /// Peeks a future token, without consuming it or advancing the cursor.
    /// This peeking **does not skips** line terminators.
    ///
    /// You can skip some tokens with the `skip_n` option.
    pub(super) fn peek_no_skip_line_term(
        &mut self,
        skip_n: usize,
        interner: &mut Interner,
    ) -> ParseResult<Option<&Token>> {
        self.buffered_lexer.peek(skip_n, false, interner)
    }

    /// Gets the current strict mode for the cursor.
    pub(super) const fn strict(&self) -> bool {
        self.buffered_lexer.strict()
    }

    /// Sets the strict mode to strict or non-strict.
    pub(super) fn set_strict(&mut self, strict: bool) {
        self.buffered_lexer.set_strict(strict);
    }

    /// Returns if the cursor is currently in an arrow function declaration.
    pub(super) const fn arrow(&self) -> bool {
        self.arrow
    }

    /// Set if the cursor is currently in a arrow function declaration.
    pub(super) fn set_arrow(&mut self, arrow: bool) {
        self.arrow = arrow;
    }

    /// Returns if the cursor is currently used in `JSON.parse`.
    pub(super) const fn json_parse(&self) -> bool {
        self.json_parse
    }

    /// Set if the cursor is currently used in `JSON.parse`.
    pub(super) fn set_json_parse(&mut self, json_parse: bool) {
        self.json_parse = json_parse;
    }

    /// Set the identifier of the cursor.
    #[inline]
    pub(super) fn set_identifier(&mut self, identifier: u32) {
        self.identifier = identifier;
    }

    /// Get the identifier for a tagged template.
    #[inline]
    pub(super) fn tagged_template_identifier(&mut self) -> u64 {
        self.tagged_templates_count += 1;

        let identifier = u64::from(self.identifier);
        let count = u64::from(self.tagged_templates_count);

        (count << 32) | identifier
    }

    /// Returns an error if the next token is not of kind `kind`.
    pub(super) fn expect<K>(
        &mut self,
        kind: K,
        context: &'static str,
        interner: &mut Interner,
    ) -> ParseResult<Token>
    where
        K: Into<TokenKind>,
    {
        let next_token = self.next(interner).or_abrupt()?;
        let kind = kind.into();

        if next_token.kind() == &kind {
            Ok(next_token)
        } else {
            Err(Error::expected(
                [kind.to_string(interner)],
                next_token.to_string(interner),
                next_token.span(),
                context,
            ))
        }
    }

    /// It will peek for the next token, to see if it's a semicolon.
    ///
    /// It will automatically insert a semicolon if needed, as specified in the [spec][spec].
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-automatic-semicolon-insertion
    pub(super) fn peek_semicolon(
        &mut self,
        interner: &mut Interner,
    ) -> ParseResult<SemicolonResult<'_>> {
        self.peek_no_skip_line_term(0, interner)?
            .map_or(Ok(SemicolonResult::Found(None)), |tk| match tk.kind() {
                TokenKind::Punctuator(Punctuator::Semicolon | Punctuator::CloseBlock)
                | TokenKind::LineTerminator => Ok(SemicolonResult::Found(Some(tk))),
                _ => Ok(SemicolonResult::NotFound(tk)),
            })
    }

    /// Consumes the next token if it is a semicolon, or returns a `Error` if it's not.
    ///
    /// It will automatically insert a semicolon if needed, as specified in the [spec][spec].
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-automatic-semicolon-insertion
    pub(super) fn expect_semicolon(
        &mut self,
        context: &'static str,
        interner: &mut Interner,
    ) -> ParseResult<()> {
        match self.peek_semicolon(interner)? {
            SemicolonResult::Found(Some(tk)) => match *tk.kind() {
                TokenKind::Punctuator(Punctuator::Semicolon) | TokenKind::LineTerminator => {
                    let _next = self.buffered_lexer.next(false, interner)?;
                    Ok(())
                }
                _ => Ok(()),
            },
            SemicolonResult::Found(None) => Ok(()),
            SemicolonResult::NotFound(tk) => Err(Error::expected(
                [";".to_owned()],
                tk.to_string(interner),
                tk.span(),
                context,
            )),
        }
    }

    /// It will make sure that the peeked token (skipping n tokens) is not a line terminator.
    ///
    /// It expects that the token stream does not end here.
    ///
    /// This is just syntactic sugar for a `.peek(skip_n)` call followed by a check that the result
    /// is not a line terminator or `None`.
    pub(super) fn peek_expect_no_lineterminator(
        &mut self,
        skip_n: usize,
        context: &'static str,
        interner: &mut Interner,
    ) -> ParseResult<&Token> {
        let tok = self.peek_no_skip_line_term(skip_n, interner).or_abrupt()?;

        if tok.kind() == &TokenKind::LineTerminator {
            Err(Error::unexpected(
                tok.to_string(interner),
                tok.span(),
                context,
            ))
        } else {
            Ok(tok)
        }
    }

    /// Check if the peeked token is a line terminator.
    pub(super) fn peek_is_line_terminator(
        &mut self,
        skip_n: usize,
        interner: &mut Interner,
    ) -> ParseResult<Option<bool>> {
        self.peek_no_skip_line_term(skip_n, interner)?
            .map_or(Ok(None), |t| {
                Ok(Some(t.kind() == &TokenKind::LineTerminator))
            })
    }

    /// Advance the cursor to the next token and retrieve it, only if it's of `kind` type.
    ///
    /// When the next token is a `kind` token, get the token, otherwise return `None`.
    ///
    /// No next token also returns None.
    pub(super) fn next_if<K>(
        &mut self,
        kind: K,
        interner: &mut Interner,
    ) -> ParseResult<Option<Token>>
    where
        K: Into<TokenKind>,
    {
        if let Some(token) = self.peek(0, interner)?
            && token.kind() == &kind.into()
        {
            self.next(interner)
        } else {
            Ok(None)
        }
    }

    /// Gets current linear position in the source code.
    #[inline]
    pub(super) fn linear_pos(&self) -> LinearPosition {
        self.buffered_lexer.linear_pos()
    }

    pub(super) fn take_source(&mut self) -> boa_ast::SourceText {
        self.buffered_lexer.take_source()
    }
}
