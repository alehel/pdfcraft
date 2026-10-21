# Comment summary stylesheets

> **Last reviewed:** 2026-10-22 · **Last updated:** 2026-10-22 · **Change:** minor (stylesheet limits; the box properties each part draws)

The comment summary (Comments ▸ "…" ▸ Create PDF comment summary, or the `comments_summarize`
tool) is drawn in a **theme**. Five are built in, and a **stylesheet** can restyle any of them.
A stylesheet is a small subset of CSS: a file you can open, edit and share like any other `.css`
file, read by PdfCraft's own parser (`crates/create/src/report_style.rs`).

## Built-in themes

| Theme | Look |
|---|---|
| `modern` (default) | Cards on light grey, a stripe in each comment's colour, the marked text on a tint of it, each author's name in their colour |
| `simple` | The plain look of the first summaries: one size of Helvetica in black, no fills, rules or stripes |
| `classic` | A printed review in Times: black text and rules, no fills |
| `compact` | Modern, smaller and tighter, for long reviews |
| `ink-saver` | No filled areas; each comment's colour only in a thin stripe, authors' names in their colours |

The stylesheets are in `crates/create/themes/`. To start your own, pick a theme in the Summarize
Options dialog and use **Export CSS…** (or the `comments_summary_themes` tool), edit the file, and
load it with **Load CSS…**. It then appears in the Theme menu under its file name, and the preview
beside the options shows it. The app remembers the theme and the loaded stylesheet's text.

## How a stylesheet applies

In the app, a loaded stylesheet starts from the Modern theme; with the tool, from the `theme`
argument. To start from a particular theme, import it first:

```css
@import "classic";
.kind { color: #b00020; }
```

Later rules win, as in CSS. Only the built-in themes can be imported.

## Parts

Each part of the report is a class. A rule may name several, separated by commas.

| Class | What it is |
|---|---|
| `.title` | "Summary of Comments" |
| `.subtitle` | The file name and the count line under the title |
| `.header` | The space and rule under the title block (`padding-bottom`, `border-bottom`, `margin-bottom`) |
| `.heading` | A group heading: "Page 3", an author or a comment type, following the sort |
| `.note` | A message in place of comments ("No comments match the summary options.") |
| `.card` | One comment: its box (`background`, `border-left` stripe, `border-bottom`, `padding`, `margin-bottom`) |
| `.number` | "#2" |
| `.kind` | The comment type ("Highlight") |
| `.author` | Who wrote the comment (left out when the summary is sorted by author: the heading names them) |
| `.meta` | Date and page |
| `.quote` | The text a highlight, underline, strikethrough or squiggly marks |
| `.body` | The comment's note |
| `.reply` | One reply's band: `margin-left` is the indent per level, `border-left` the thread line, `padding-left` the space before the text |
| `.reply-author`, `.reply-meta`, `.reply-body` | A reply's author, date and text |
| `.footer` | The report's name and "Page N of M" at the bottom of each page; `border-top` is the rule above |

## One comment type

Add a comment type to a comment's part to style only that type. Such a rule outranks the part's
plain rules wherever it stands, as the more specific selector does in CSS:

```css
.card.strikethrough { background: #fde8e8; }
.quote.highlight { font-weight: bold; }
.kind.sticky-note { color: #8a6d00; }
```

The parts that take a type are `.card`, `.number`, `.kind`, `.author`, `.meta`, `.quote`, `.body`, `.reply`,
`.reply-author`, `.reply-meta` and `.reply-body`. The types are the names the summary prints, in
lower case with hyphens: `sticky-note`, `text-box`, `callout`, `typewriter`, `highlight`,
`underline`, `strikethrough`, `squiggly`, `rectangle`, `oval`, `line`, `pencil`, `polygon`,
`cloud`, `polygonal-line`, `stamp`, `inserted-text`, `file-attachment`, `sound`, `redaction`. The
PDF names work too (`strikeout`, `freetext`, `square`, `circle`, `ink`, `caret`, `redact`…).

## Properties

| Property | Values |
|---|---|
| `font-family` | `Helvetica`/`sans-serif`, `Times`/`serif`, `Courier`/`monospace` (the standard PDF fonts; the first family in the list that is one of these is used) |
| `font-size` | 4–96 pt |
| `font-weight` | `normal`, `bold`, or 100–900 (600 and up is bold) |
| `font-style` | `normal`, `italic`, `oblique` |
| `color`, `background`, `background-color` | A colour (below) |
| `line-height` | A multiple of the font size (`1.4`) or a length |
| `text-transform` | `uppercase`, `none` |
| `display` | `none` hides the part; `block` shows it |
| `padding`, `margin` and their `-top`/`-right`/`-bottom`/`-left` | One to four lengths |
| `border-left`, `border-top`, `border-bottom` | `<width> solid <colour>` or `none`; also `-width` and `-color` |

`@page` takes `size` (`letter`, `legal`, `a3`, `a4`, `a5`, or a width and height, optionally
`landscape`) and `margin`. Without a `size`, the summary is US Letter.

**Lengths:** `pt`, `px` (¾ pt), `mm`, `cm`, `in`, or `0`.

**Colours:** `#rgb`, `#rrggbb` (and with alpha, drawn over white), `rgb()`/`rgba()`, CSS colour
names, `transparent`/`none`, and:

- `var(--comment)`: the comment's own colour (a light grey for comments without one).
- `var(--author)`: the colour of whoever wrote the comment, and on a reply's parts (`.reply`,
  `.reply-author`, `.reply-meta`, `.reply-body`) of whoever wrote the reply. See below.
- `color-mix(in srgb, <colour> <p>%, <colour>)`: for example
  `color-mix(in srgb, var(--comment) 25%, white)` is the Modern theme's tint behind marked text.

## Authors' colours

Every author in the document gets a colour, so a review with several people reads at a glance. A
stylesheet doesn't know who will review a document, so the colours come from the summary, not the
stylesheet: each author gets the next colour of a ten-colour palette, in the order of their names,
and the Summarize Options dialog lists the authors so you can pick another colour for any of them
(**Auto** gives the automatic one back). The app remembers the colours picked; the
`comments_summarize` tool takes them as `author_colors` and lists every author with the colour
used.

A stylesheet decides where the colours show, through `var(--author)`. Modern, Compact and Ink saver
set each author's name in it (`.author` and `.reply-author`); Simple and Classic keep names black,
and the dialog says so. For example, to put the colour in the card's stripe instead of the
comment's colour:

```css
.card { border-left: 3pt solid var(--author); }
```

## What is not read

Anything else (other selectors, nesting, `@media`, other properties, other `var()` names) is
skipped with a warning naming its line, the way a browser skips CSS it doesn't know, and the rest
still applies. So is a property a part doesn't draw (`background` on `.title`, `padding` on
`.footer`): the warning says what that part takes. The box properties each part draws are
`.header`: `padding-bottom`, `border-bottom`, `margin-bottom`; `.heading`: `margin-top`,
`margin-bottom`, `padding-bottom`, `border-bottom`; `.card`: `background`, `border-left`,
`border-bottom`, `padding`, `margin-bottom`; `.quote` and `.reply`: `background`, `border-left`,
`padding`, `margin`; `.body`: `margin`; `.footer`: `border-top`. Every part that holds text takes
the text properties. The dialog lists the warnings when a stylesheet is loaded; the tool returns
them in `warnings`.

A stylesheet is read with limits, so that one can't make the summary, or the app that remembers
it, slow: a stylesheet over 64 KB is refused, a rule reads its first 64 selectors, and at most
4096 rules for comment types are kept (a later rule for the same part, type and property
replaces an earlier one). `var(--comment)` and `var(--author)` outside a comment's parts (the
title, headings, footer) stand for fixed greys.

Text is set in the standard PDF fonts, which hold Western European characters; others are written
as `[U+XXXX]` and the summary says so. Fonts can't be embedded from a stylesheet.

## Revision history

- 2026-10-22: Limits on what a stylesheet can make the parser keep; page sizes up to 200 inches as documented; the box properties each part draws listed in full.
- 2026-10-21: Authors' colours: the `.author` part, `var(--author)`, colours picked in the dialog or given to the tool.
- 2026-10-21: Comment-type selectors (`.card.highlight`); a warning for a property a part doesn't draw.
- 2026-10-21: First version: five built-in themes, stylesheets, `@import`, `var(--comment)`, `color-mix()`.
