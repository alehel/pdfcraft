//! The synthetic word-processor documents the office oracle converts.
//!
//! Each fixture is a Flat OpenDocument text file (`.fodt`, one XML file, no zip). LibreOffice
//! writes it out as the source `.docx` and the reference `.pdf`. Every fixture is written here,
//! with text from [`prose`], so nothing is borrowed from a real document (AGENTS.md §1), and the
//! files exist only under the output directory, never in the repository. The fonts are whatever
//! LibreOffice finds under the names asked for. Embedded subsets of them must not be committed
//! either (§1.4), which is the other reason the PDFs are generated rather than checked in.
//!
//! Each fixture tests one thing a Word document commonly has, so a low score points at a single
//! missing feature rather than at a mixture of them.

use std::fmt::Write as _;

/// A named fixture and its `.fodt` source.
pub struct Fixture {
    pub name: &'static str,
    /// What the fixture tests, for the report.
    pub about: &'static str,
    pub fodt: String,
}

pub fn all() -> anyhow::Result<Vec<Fixture>> {
    Ok(vec![
        headings(),
        emphasis(),
        bullets(),
        numbered(),
        table_ruled(),
        table_merged(),
        alignment(),
        fonts(),
        columns(),
        header_footer(),
        footnotes(),
        landscape(),
        images()?,
        long_report(),
    ])
}

/// Deterministic filler prose: `n` sentences starting at `seed`, built from fixed word lists so
/// fixtures are long enough to lay out like real documents without quoting anything.
pub fn prose(seed: usize, n: usize) -> String {
    const SUBJECTS: &[&str] = &[
        "The regional office",
        "Our planning group",
        "The new warehouse",
        "Each project team",
        "The finance committee",
        "A small pilot study",
        "The maintenance crew",
        "The spring survey",
    ];
    const VERBS: &[&str] = &["reviewed", "reported", "measured", "scheduled", "compared", "approved", "described", "updated"];
    const OBJECTS: &[&str] = &[
        "the delivery times for every route",
        "a shorter checklist for site visits",
        "the cost of heating the older buildings",
        "three options for the parking area",
        "the training plan for the autumn intake",
        "how often the river path floods",
        "the figures from the previous quarter",
        "a proposal to share equipment between depots",
    ];
    const TAILS: &[&str] = &[
        "before the end of the month",
        "with help from the local council",
        "and found no major problems",
        "after two rounds of feedback",
        "using the same method as last year",
        "although some data was missing",
    ];
    let mut out = String::new();
    for i in 0..n {
        let k = seed.wrapping_mul(7).wrapping_add(i);
        let pick = |list: &[&'static str], salt: usize| list.get(k.wrapping_mul(salt).wrapping_add(salt) % list.len()).copied().unwrap_or("");
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{} {} {} {}.", pick(SUBJECTS, 1), pick(VERBS, 3), pick(OBJECTS, 5), pick(TAILS, 11));
    }
    out
}

/// XML-escaped text.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The page a fixture is laid out on.
struct Page {
    width_cm: f64,
    height_cm: f64,
    margin_cm: f64,
    /// Paragraph XML for the running header and footer, if any.
    header: Option<String>,
    footer: Option<String>,
}

const A4: Page = Page { width_cm: 21.0, height_cm: 29.7, margin_cm: 2.0, header: None, footer: None };

/// A complete `.fodt`: `styles` go into automatic styles (paragraph, text, table, list, section
/// and graphic styles named by `body`), `body` into the text.
fn document(page: &Page, styles: &str, body: &str) -> String {
    let orientation = if page.width_cm > page.height_cm { "landscape" } else { "portrait" };
    let header = page.header.as_ref().map(|h| format!("<style:header>{h}</style:header>")).unwrap_or_default();
    let footer = page.footer.as_ref().map(|f| format!("<style:footer>{f}</style:footer>")).unwrap_or_default();
    let hf_props = r#"<style:header-style><style:header-footer-properties fo:min-height="0.6cm" fo:margin-bottom="0.3cm"/></style:header-style><style:footer-style><style:header-footer-properties fo:min-height="0.6cm" fo:margin-top="0.3cm"/></style:footer-style>"#;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.text">
<office:styles>
<style:default-style style:family="paragraph"><style:paragraph-properties fo:margin-bottom="0.2cm"/><style:text-properties fo:font-family="'Liberation Serif'" fo:font-size="11pt" fo:language="en" fo:country="GB"/></style:default-style>
</office:styles>
<office:automatic-styles>
<style:page-layout style:name="PL"><style:page-layout-properties fo:page-width="{w}cm" fo:page-height="{h}cm" style:print-orientation="{orientation}" fo:margin-top="{m}cm" fo:margin-bottom="{m}cm" fo:margin-left="{m}cm" fo:margin-right="{m}cm"/>{hf_props}</style:page-layout>
<style:style style:name="B" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
<style:style style:name="I" style:family="text"><style:text-properties fo:font-style="italic"/></style:style>
{styles}
</office:automatic-styles>
<office:master-styles><style:master-page style:name="Standard" style:page-layout-name="PL">{header}{footer}</style:master-page></office:master-styles>
<office:body><office:text>
{body}
</office:text></office:body></office:document>
"#,
        w = page.width_cm,
        h = page.height_cm,
        m = page.margin_cm,
    )
}

fn h(level: u8, text: &str) -> String {
    format!(r#"<text:h text:outline-level="{level}">{}</text:h>"#, esc(text))
}

fn p(text: &str) -> String {
    format!("<text:p>{}</text:p>", esc(text))
}

fn p_styled(style: &str, text: &str) -> String {
    format!(r#"<text:p text:style-name="{style}">{}</text:p>"#, esc(text))
}

fn span(style: &str, text: &str) -> String {
    format!(r#"<text:span text:style-name="{style}">{}</text:span>"#, esc(text))
}

/// A list of `items` (depth, text) under list style `style`; depth 0 is the outer level.
fn list(style: &str, items: &[(usize, String)]) -> String {
    // Nested ODF lists live inside the previous item, so open and close as depth changes.
    let mut out = format!(r#"<text:list text:style-name="{style}">"#);
    let mut depth = 0usize;
    let mut open_item = false;
    for (d, text) in items {
        let d = (*d).min(depth.saturating_add(1));
        while depth > d {
            out.push_str("</text:list-item></text:list>");
            depth -= 1;
        }
        if d > depth {
            out.push_str("<text:list>");
            depth = d;
        } else if open_item {
            out.push_str("</text:list-item>");
        }
        let _ = write!(out, "<text:list-item><text:p>{}</text:p>", esc(text));
        open_item = true;
    }
    while depth > 0 {
        out.push_str("</text:list-item></text:list>");
        depth -= 1;
    }
    if open_item {
        out.push_str("</text:list-item>");
    }
    out.push_str("</text:list>");
    out
}

fn label_alignment(level: usize) -> String {
    let margin = 0.63 * (level as f64 + 1.0);
    format!(
        r#"<style:list-level-properties text:list-level-position-and-space-mode="label-alignment"><style:list-level-label-alignment text:label-followed-by="listtab" text:list-tab-stop-position="{margin:.2}cm" fo:text-indent="-0.63cm" fo:margin-left="{margin:.2}cm"/></style:list-level-properties>"#
    )
}

fn headings() -> Fixture {
    let body = [
        h(1, "Annual facilities review"),
        p(&prose(1, 4)),
        h(2, "Buildings and grounds"),
        p(&prose(2, 5)),
        h(3, "Heating and insulation"),
        p(&prose(3, 3)),
        h(3, "Roof repairs"),
        p(&prose(4, 3)),
        h(2, "Transport"),
        p(&prose(5, 5)),
        p(&prose(6, 4)),
    ]
    .concat();
    Fixture { name: "headings", about: "heading levels 1-3 between body paragraphs", fodt: document(&A4, "", &body) }
}

fn emphasis() -> Fixture {
    let styles = r#"<style:style style:name="BI" style:family="text"><style:text-properties fo:font-weight="bold" fo:font-style="italic"/></style:style>
<style:style style:name="U" style:family="text"><style:text-properties style:text-underline-style="solid" style:text-underline-width="auto" style:text-underline-color="font-color"/></style:style>
<style:style style:name="S" style:family="text"><style:text-properties style:text-line-through-style="solid"/></style:style>
<style:style style:name="Sup" style:family="text"><style:text-properties style:text-position="super 58%"/></style:style>"#;
    let body = [
        h(1, "Notes on the inspection"),
        format!(
            "<text:p>{} {} {} {} {}</text:p>",
            esc("The inspector checked the"),
            span("B", "fire exits"),
            esc("and the"),
            span("I", "emergency lighting"),
            esc("on every floor of the main building."),
        ),
        format!(
            "<text:p>{} {} {} {} {}</text:p>",
            esc("Two doors were"),
            span("BI", "propped open"),
            esc("and one sign was"),
            span("U", "hard to read"),
            esc("from the stairwell."),
        ),
        format!(
            "<text:p>{} {} {} {}{} {}</text:p>",
            esc("The old deadline of"),
            span("S", "March"),
            esc("has moved to June, and the area is now 120 m"),
            span("Sup", "2"),
            esc("."),
            esc(&prose(7, 2)),
        ),
        p(&prose(8, 4)),
    ]
    .concat();
    Fixture {
        name: "emphasis",
        about: "bold, italic, bold italic, underline, strikethrough and superscript runs mid-sentence",
        fodt: document(&A4, styles, &body),
    }
}

fn bullets() -> Fixture {
    let mut levels = String::new();
    for (level, ch) in ["•", "◦", "▪"].iter().enumerate() {
        let _ = write!(
            levels,
            r#"<text:list-level-style-bullet text:level="{}" text:bullet-char="{ch}">{}</text:list-level-style-bullet>"#,
            level + 1,
            label_alignment(level)
        );
    }
    let styles = format!(r#"<text:list-style style:name="L">{levels}</text:list-style>"#);
    let items: Vec<(usize, String)> = vec![
        (0, "Check the delivery schedule".into()),
        (1, "Morning routes".into()),
        (1, "Evening routes".into()),
        (2, "Weekend cover".into()),
        (0, "Order replacement filters".into()),
        (0, "Book the training room for the first week".into()),
        (1, "Projector and screen".into()),
    ];
    let body = [h(1, "Actions for next week"), p(&prose(9, 2)), list("L", &items), p(&prose(10, 3))].concat();
    Fixture { name: "bullets", about: "bulleted list nested three levels deep", fodt: document(&A4, &styles, &body) }
}

fn numbered() -> Fixture {
    let mut levels = String::new();
    for (level, format) in ["1", "a", "i"].iter().enumerate() {
        let _ = write!(
            levels,
            r#"<text:list-level-style-number text:level="{}" style:num-suffix="." style:num-format="{format}">{}</text:list-level-style-number>"#,
            level + 1,
            label_alignment(level)
        );
    }
    let styles = format!(r#"<text:list-style style:name="N">{levels}</text:list-style>"#);
    let items: Vec<(usize, String)> = vec![
        (0, "Switch off the main supply at the panel".into()),
        (0, "Remove the cover plate".into()),
        (1, "Keep the four screws together".into()),
        (1, "Note which wire goes to which terminal".into()),
        (2, "Take a photograph if unsure".into()),
        (0, "Fit the new unit and restore power".into()),
        (0, "Test every outlet on the circuit".into()),
    ];
    let body = [h(1, "Replacing a wall unit"), p(&prose(11, 2)), list("N", &items), p(&prose(12, 2))].concat();
    Fixture { name: "numbered", about: "numbered list with lettered and roman sub-levels", fodt: document(&A4, &styles, &body) }
}

const TABLE_STYLES: &str = r##"<style:style style:name="T" style:family="table"><style:table-properties style:width="17cm" table:align="left"/></style:style>
<style:style style:name="C" style:family="table-cell"><style:table-cell-properties fo:padding="0.1cm" fo:border="0.5pt solid #000000"/></style:style>
<style:style style:name="CH" style:family="table-cell"><style:table-cell-properties fo:padding="0.1cm" fo:border="0.5pt solid #000000" fo:background-color="#d9e2f3"/></style:style>"##;

fn cell(style: &str, text: &str, bold: bool) -> String {
    let inner = if bold { span("B", text) } else { esc(text) };
    format!(r#"<table:table-cell table:style-name="{style}" office:value-type="string"><text:p>{inner}</text:p></table:table-cell>"#)
}

fn table_ruled() -> Fixture {
    let rows = [
        ["Depot", "Vehicles", "Staff", "Cost per week"],
        ["North", "12", "31", "4 200"],
        ["Harbour", "8", "19", "2 950"],
        ["Station Road", "15", "40", "5 610"],
        ["Airfield", "4", "9", "1 380"],
    ];
    let mut t = r#"<table:table table:name="Depots" table:style-name="T"><table:table-column table:number-columns-repeated="4"/>"#.to_owned();
    for (i, r) in rows.iter().enumerate() {
        t.push_str("<table:table-row>");
        for c in r {
            t.push_str(&cell(if i == 0 { "CH" } else { "C" }, c, i == 0));
        }
        t.push_str("</table:table-row>");
    }
    t.push_str("</table:table>");
    let body = [h(1, "Depot summary"), p(&prose(13, 2)), t, p(&prose(14, 3))].concat();
    Fixture { name: "table-ruled", about: "ruled table with a shaded bold header row", fodt: document(&A4, TABLE_STYLES, &body) }
}

fn table_merged() -> Fixture {
    let covered = "<table:covered-table-cell/>";
    let span_cell = |text: &str, cols: usize, rows: usize| {
        format!(
            r#"<table:table-cell table:style-name="CH" office:value-type="string" table:number-columns-spanned="{cols}" table:number-rows-spanned="{rows}"><text:p>{}</text:p></table:table-cell>"#,
            span("B", text)
        )
    };
    let t = [
        r#"<table:table table:name="Survey" table:style-name="T"><table:table-column table:number-columns-repeated="5"/>"#.to_owned(),
        "<table:table-row>".into(),
        span_cell("Area", 1, 2),
        span_cell("Spring", 2, 1),
        covered.into(),
        span_cell("Autumn", 2, 1),
        covered.into(),
        "</table:table-row><table:table-row>".into(),
        covered.into(),
        cell("CH", "Visits", true),
        cell("CH", "Issues", true),
        cell("CH", "Visits", true),
        cell("CH", "Issues", true),
        "</table:table-row>".into(),
        "<table:table-row>".into(),
        cell("C", "Park", false),
        cell("C", "14", false),
        cell("C", "3", false),
        cell("C", "11", false),
        cell("C", "1", false),
        "</table:table-row><table:table-row>".into(),
        cell("C", "Library", false),
        cell("C", "9", false),
        cell("C", "0", false),
        cell("C", "12", false),
        cell("C", "2", false),
        "</table:table-row></table:table>".into(),
    ]
    .concat();
    let body = [h(1, "Survey results"), p(&prose(15, 2)), t, p(&prose(16, 2))].concat();
    Fixture { name: "table-merged", about: "table header with column and row spans", fodt: document(&A4, TABLE_STYLES, &body) }
}

fn alignment() -> Fixture {
    let styles = r#"<style:style style:name="Pc" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>
<style:style style:name="Pr" style:family="paragraph"><style:paragraph-properties fo:text-align="end"/></style:style>
<style:style style:name="Pj" style:family="paragraph"><style:paragraph-properties fo:text-align="justify"/></style:style>
<style:style style:name="Pi" style:family="paragraph"><style:paragraph-properties fo:text-indent="1cm" fo:margin-left="0cm"/></style:style>
<style:style style:name="Pq" style:family="paragraph"><style:paragraph-properties fo:margin-left="2cm" fo:margin-right="2cm" fo:margin-top="0.4cm" fo:margin-bottom="0.4cm"/></style:style>
<style:style style:name="Ps" style:family="paragraph"><style:paragraph-properties fo:line-height="150%"/></style:style>"#;
    let body = [
        p_styled("Pc", "Centred title line"),
        p_styled("Pr", "Right-aligned date: 3 May"),
        p_styled("Pj", &prose(17, 5)),
        p_styled("Pi", &prose(18, 4)),
        p_styled("Pq", &prose(19, 3)),
        p_styled("Ps", &prose(20, 4)),
        p(&prose(21, 3)),
    ]
    .concat();
    Fixture {
        name: "alignment",
        about: "centred, right-aligned and justified paragraphs, first-line indent, block indent, 1.5 line spacing",
        fodt: document(&A4, styles, &body),
    }
}

fn fonts() -> Fixture {
    let styles = r##"<style:style style:name="Fsans" style:family="text"><style:text-properties fo:font-family="'Liberation Sans'"/></style:style>
<style:style style:name="Fmono" style:family="text"><style:text-properties fo:font-family="'Liberation Mono'" fo:font-size="10pt"/></style:style>
<style:style style:name="Fcarlito" style:family="text"><style:text-properties fo:font-family="Carlito"/></style:style>
<style:style style:name="Fbig" style:family="text"><style:text-properties fo:font-size="20pt"/></style:style>
<style:style style:name="Fsmall" style:family="text"><style:text-properties fo:font-size="8pt"/></style:style>
<style:style style:name="Fred" style:family="text"><style:text-properties fo:color="#c00000"/></style:style>
<style:style style:name="Fblue" style:family="text"><style:text-properties fo:color="#1f4e79" fo:font-weight="bold"/></style:style>
<style:style style:name="Fhi" style:family="text"><style:text-properties fo:background-color="#ffff00"/></style:style>
<style:style style:name="Psans" style:family="paragraph"><style:text-properties fo:font-family="'Liberation Sans'" fo:font-size="12pt"/></style:style>"##;
    let body = [
        format!("<text:p>{} {}</text:p>", span("Fbig", "Large heading-sized text"), esc("followed by body text.")),
        format!("<text:p>{} {} {}</text:p>", esc("A serif sentence with"), span("Fsans", "a sans-serif phrase"), esc("in the middle of it.")),
        format!("<text:p>{} {} {}</text:p>", esc("Run the command"), span("Fmono", "report --all --quiet"), esc("to print the summary.")),
        format!("<text:p>{} {}</text:p>", span("Fcarlito", "This sentence is set in Carlito."), esc(&prose(22, 1))),
        format!("<text:p>{} {} {}</text:p>", esc("Totals in"), span("Fred", "red are overdue"), esc("and the rest are on time.")),
        format!("<text:p>{} {} {}</text:p>", span("Fblue", "Note:"), esc("the"), span("Fhi", "highlighted figures are estimates.")),
        format!("<text:p>{}</text:p>", span("Fsmall", &prose(23, 2))),
        p_styled("Psans", &prose(24, 3)),
    ]
    .concat();
    Fixture {
        name: "fonts",
        about: "serif, sans, mono and Carlito faces; 8-20 pt sizes; coloured, bold and highlighted runs",
        fodt: document(&A4, styles, &body),
    }
}

fn columns() -> Fixture {
    let styles = r#"<style:style style:name="Sect2" style:family="section"><style:section-properties text:dont-balance-text-columns="false"><style:columns fo:column-count="2" fo:column-gap="0.8cm"/></style:section-properties></style:style>"#;
    let inner = [h(2, "Background"), p(&prose(25, 6)), h(2, "Findings"), p(&prose(26, 7)), p(&prose(27, 6))].concat();
    let body = [
        h(1, "Newsletter: summer edition"),
        p(&prose(28, 2)),
        format!(r#"<text:section text:style-name="Sect2" text:name="Cols">{inner}</text:section>"#),
        p(&prose(29, 2)),
    ]
    .concat();
    Fixture { name: "columns", about: "two-column section between full-width paragraphs", fodt: document(&A4, styles, &body) }
}

fn header_footer() -> Fixture {
    let page = Page {
        header: Some("<text:p>Site handbook — draft for comment</text:p>".into()),
        footer: Some(r#"<text:p>Page <text:page-number text:select-page="current">1</text:page-number></text:p>"#.into()),
        ..A4
    };
    let mut body = vec![h(1, "Site handbook")];
    for i in 0..12 {
        body.push(h(2, &format!("Section {}", i + 1)));
        body.push(p(&prose(30 + i, 6)));
    }
    Fixture { name: "header-footer", about: "running header and page-number footer over several pages", fodt: document(&page, "", &body.concat()) }
}

fn footnotes() -> Fixture {
    let note = |id: usize, text: &str| {
        format!(
            r#"<text:note text:id="ftn{id}" text:note-class="footnote"><text:note-citation>{id}</text:note-citation><text:note-body><text:p>{}</text:p></text:note-body></text:note>"#,
            esc(text)
        )
    };
    let body = [
        h(1, "Water use"),
        format!(
            "<text:p>{}{} {}</text:p>",
            esc("Use fell by a tenth over the year."),
            note(1, "Measured at the two main meters only."),
            esc(&prose(42, 3))
        ),
        format!("<text:p>{}{} {}</text:p>", esc("The garden now uses rainwater."), note(2, "From the tank fitted in April."), esc(&prose(43, 3))),
        p(&prose(44, 4)),
    ]
    .concat();
    Fixture { name: "footnotes", about: "footnotes referenced mid-paragraph", fodt: document(&A4, "", &body) }
}

fn landscape() -> Fixture {
    let page = Page { width_cm: 29.7, height_cm: 21.0, ..A4 };
    let rows = [
        ["Week", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
        ["1", "Ana", "Ben", "Cleo", "Dev", "Eli", "—"],
        ["2", "Ben", "Cleo", "Dev", "Eli", "Ana", "Ben"],
        ["3", "Cleo", "Dev", "Eli", "Ana", "Ben", "—"],
    ];
    let mut t = r#"<table:table table:name="Rota" table:style-name="T"><table:table-column table:number-columns-repeated="7"/>"#.to_owned();
    for (i, r) in rows.iter().enumerate() {
        t.push_str("<table:table-row>");
        for c in r {
            t.push_str(&cell(if i == 0 { "CH" } else { "C" }, c, i == 0));
        }
        t.push_str("</table:table-row>");
    }
    t.push_str("</table:table>");
    let body = [h(1, "Duty rota"), p(&prose(45, 2)), t, p(&prose(46, 2))].concat();
    Fixture { name: "landscape", about: "landscape page with a wide table", fodt: document(&page, TABLE_STYLES, &body) }
}

fn images() -> anyhow::Result<Fixture> {
    let picture = picture(240, 160)?;
    let styles = r#"<style:style style:name="Gwrap" style:family="graphic"><style:graphic-properties style:wrap="parallel" style:horizontal-pos="right" style:horizontal-rel="paragraph" style:vertical-pos="top" style:vertical-rel="paragraph" fo:margin-left="0.3cm"/></style:style>
<style:style style:name="Gin" style:family="graphic"><style:graphic-properties style:vertical-pos="top" style:vertical-rel="baseline"/></style:style>
<style:style style:name="Pc" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#;
    let frame = |name: &str, style: &str, anchor: &str, w: f64, hgt: f64| {
        format!(
            r#"<draw:frame draw:name="{name}" draw:style-name="{style}" text:anchor-type="{anchor}" svg:width="{w}cm" svg:height="{hgt}cm"><draw:image><office:binary-data>{}</office:binary-data></draw:image></draw:frame>"#,
            base64(&picture)
        )
    };
    let body = [
        h(1, "Site photographs"),
        p(&prose(47, 2)),
        format!(r#"<text:p text:style-name="Pc">{}</text:p>"#, frame("inline", "Gin", "as-char", 9.0, 6.0)),
        p_styled("Pc", "Figure 1: the north yard"),
        format!("<text:p>{}{}</text:p>", frame("float", "Gwrap", "paragraph", 6.0, 4.0), esc(&prose(48, 8))),
        p(&prose(49, 3)),
    ]
    .concat();
    Ok(Fixture {
        name: "images",
        about: "a centred inline picture with a caption and a picture with text wrapped round it",
        fodt: document(&A4, styles, &body),
    })
}

fn long_report() -> Fixture {
    let mut levels = String::new();
    let _ =
        write!(levels, r#"<text:list-level-style-bullet text:level="1" text:bullet-char="•">{}</text:list-level-style-bullet>"#, label_alignment(0));
    let styles = format!("{TABLE_STYLES}\n<text:list-style style:name=\"L\">{levels}</text:list-style>");
    let mut body = vec![h(1, "Operations report")];
    for s in 0..4 {
        body.push(h(2, &format!("Part {}", s + 1)));
        body.push(p(&prose(50 + s, 5)));
        let items: Vec<(usize, String)> = (0..3).map(|i| (0, prose(60 + s * 3 + i, 1))).collect();
        body.push(list("L", &items));
        body.push(p(&prose(80 + s, 4)));
        let mut t = format!(r#"<table:table table:name="R{s}" table:style-name="T"><table:table-column table:number-columns-repeated="3"/>"#);
        for (i, r) in [["Item", "Planned", "Actual"], ["Visits", "40", "37"], ["Repairs", "12", "15"]].iter().enumerate() {
            t.push_str("<table:table-row>");
            for c in r {
                t.push_str(&cell(if i == 0 { "CH" } else { "C" }, c, i == 0));
            }
            t.push_str("</table:table-row>");
        }
        t.push_str("</table:table>");
        body.push(t);
    }
    Fixture {
        name: "long-report",
        about: "several pages mixing headings, lists, paragraphs and tables",
        fodt: document(&A4, &styles, &body.concat()),
    }
}

/// A small generated picture: a two-tone landscape, so a missing or misplaced image shows.
fn picture(w: u32, hgt: u32) -> anyhow::Result<Vec<u8>> {
    let mut rgb = Vec::with_capacity((w as usize).saturating_mul(hgt as usize).saturating_mul(3));
    for y in 0..hgt {
        for x in 0..w {
            let hill = f64::from(hgt) * (0.6 + 0.15 * (f64::from(x) / 30.0).sin());
            let px = if f64::from(y) > hill { [60, 130, 70] } else { [150u8, 190, 230u8.saturating_sub((y / 4) as u8)] };
            rgb.extend_from_slice(&px);
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, hgt);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgb)?;
    writer.finish()?;
    Ok(out)
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |n: u32| A.get((n & 63) as usize).copied().map_or('=', char::from);
    let mut s = String::with_capacity(data.len().div_ceil(3).saturating_mul(4));
    for c in data.chunks(3) {
        let byte = |i: usize| u32::from(c.get(i).copied().unwrap_or(0));
        let n = byte(0) << 16 | byte(1) << 8 | byte(2);
        s.push(sym(n >> 18));
        s.push(sym(n >> 12));
        s.push(if c.len() > 1 { sym(n >> 6) } else { '=' });
        s.push(if c.len() > 2 { sym(n) } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_have_unique_names_and_balanced_lists() {
        let all = all().expect("fixtures build");
        let mut names: Vec<_> = all.iter().map(|f| f.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len(), "duplicate fixture name");
        for f in &all {
            for tag in ["text:list", "text:list-item", "table:table", "text:section"] {
                let open = f.fodt.matches(&format!("<{tag}>")).count() + f.fodt.matches(&format!("<{tag} ")).count();
                let close = f.fodt.matches(&format!("</{tag}>")).count();
                assert_eq!(open, close, "{}: <{tag}> opened {open} times, closed {close}", f.name);
            }
        }
    }

    #[test]
    fn nested_lists_nest_inside_the_previous_item() {
        let xml = list("L", &[(0, "a".into()), (1, "b".into()), (0, "c".into())]);
        assert_eq!(
            xml,
            r#"<text:list text:style-name="L"><text:list-item><text:p>a</text:p><text:list><text:list-item><text:p>b</text:p></text:list-item></text:list></text:list-item><text:list-item><text:p>c</text:p></text:list-item></text:list>"#
        );
    }

    #[test]
    fn prose_is_deterministic_and_escaping_is_complete() {
        assert_eq!(prose(3, 4), prose(3, 4));
        assert_ne!(prose(3, 4), prose(4, 4));
        assert_eq!(esc(r#"a<b>&"c""#), "a&lt;b&gt;&amp;&quot;c&quot;");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
