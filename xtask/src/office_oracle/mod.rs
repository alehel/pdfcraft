//! `cargo xtask office-oracle`: how well Export to Word recovers a word-processor document.
//!
//! Text and render oracles compare us against another PDF reader. Word export has no such
//! reference, since no other program's `.docx` is "the right answer". It does have one exact
//! answer, though: the document the PDF was made from. So the harness starts from that document
//! instead of from a PDF:
//!
//! ```text
//! fixture.fodt ─LibreOffice─▶ source.docx                  (the answer)
//!              └LibreOffice─▶ source.pdf ─pdfcraft─▶ ours.docx ─LibreOffice─▶ ours.pdf
//! ```
//!
//! and scores two things:
//!
//! - **structure**: pandoc reads `source.docx` and `ours.docx`, and [`score`] compares headings,
//!   paragraphs, list items and their depth, table cells and spans, bold, italic, underline,
//!   footnotes and pictures. The words and their reading order come from `pdftotext` on the two
//!   PDFs, because pandoc does not read text boxes and our positioned pages are made of them.
//! - **layout**: Poppler renders `source.pdf` and `ours.pdf`, and `pdfcraft_oracle::pixels`
//!   counts the share of pixels with no near match, as `render-oracle` does. LibreOffice lays out
//!   both, so the score reflects what the `.docx` asks for, not how two renderers differ.
//!
//! The fixtures ([`fixtures`]) are synthetic and generated on every run; nothing they produce is
//! committed. Like the other oracles this reports rather than gates, except that it fails when
//! our export fails outright, since that is a bug whatever the scores are.

mod fixtures;
mod score;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::gates::{release_cli, run_args, target_dir};
use score::{Outline, Scores};

/// One fixture's result.
struct Outcome {
    name: &'static str,
    about: &'static str,
    scores: Option<Scores>,
    /// Pages in the source PDF and in ours.
    pages: (usize, usize),
    /// Mean pixel disagreement over the pages, in percent; a page only one side has counts 100%.
    pixels: Option<f64>,
    /// Why a step failed, when one did.
    error: Option<String>,
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i.checked_add(1)?)).cloned();
    let out = value("--out").map_or_else(|| target_dir().join("office-oracle"), PathBuf::from);
    let only = value("--only");
    let dpi: u32 = value("--dpi").and_then(|v| v.parse().ok()).unwrap_or(50);
    let json_out = value("--json").map(PathBuf::from);
    let tagged = args.iter().any(|a| a == "--tagged");

    let probe = |p: Result<pdfcraft_oracle::Oracle, pdfcraft_oracle::Absent>| match p {
        Ok(o) => Ok(o),
        Err(absent) => Err(anyhow::anyhow!("{absent}")),
    };
    let soffice = probe(pdfcraft_oracle::soffice())?;
    let pandoc = probe(pdfcraft_oracle::pandoc())?;
    let pdftoppm = probe(pdfcraft_oracle::pdftoppm())?;
    let pdftotext = probe(pdfcraft_oracle::pdftotext())?;

    let mut fixtures = fixtures::all()?;
    if let Some(only) = &only {
        fixtures.retain(|f| f.name.contains(only.as_str()));
        if fixtures.is_empty() {
            bail!("no fixture name contains `{only}`");
        }
    }

    run_args(&["build", "--release", "-p", "pdfcraft-cli"])?;
    let cli = release_cli();

    let (src, ours, px) = (out.join("source"), out.join("ours"), out.join("pixels"));
    for d in [&src, &ours, &px] {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let out = out.canonicalize()?;
    let (src, ours, px) = (out.join("source"), out.join("ours"), out.join("pixels"));
    let profile = out.join("libreoffice-profile");

    // Every output is removed before it is made, so a step that fails cannot leave the previous
    // run's file behind to be scored as if it had worked.
    let mut fodts = Vec::new();
    for f in &fixtures {
        for p in [
            src.join(format!("{}.docx", f.name)),
            src.join(format!("{}.pdf", f.name)),
            ours.join(format!("{}.docx", f.name)),
            ours.join(format!("{}.pdf", f.name)),
        ] {
            remove_if_present(&p)?;
        }
        let path = src.join(format!("{}.fodt", f.name));
        std::fs::write(&path, &f.fodt).with_context(|| format!("writing {}", path.display()))?;
        fodts.push(path);
    }

    let pdf_filter = if tagged { r#"pdf:writer_pdf_Export:{"UseTaggedPDF":{"type":"boolean","value":"true"}}"# } else { "pdf" };
    convert(&soffice, &profile, "docx:MS Word 2007 XML", &src, &fodts)?;
    convert(&soffice, &profile, pdf_filter, &src, &fodts)?;

    // Our export, one process per fixture so one failure does not hide the rest.
    let mut results: Vec<Outcome> = Vec::new();
    let mut exported = Vec::new();
    for f in &fixtures {
        let mut r = Outcome { name: f.name, about: f.about, scores: None, pages: (0, 0), pixels: None, error: None };
        let pdf = src.join(format!("{}.pdf", f.name));
        let docx = ours.join(format!("{}.docx", f.name));
        if !pdf.is_file() {
            r.error = Some("LibreOffice did not write the source PDF".into());
        } else if let Err(e) = export(&cli, &out, &pdf, &docx) {
            r.error = Some(format!("pdfcraft export failed: {e:#}"));
        } else {
            exported.push(docx);
        }
        results.push(r);
    }
    convert(&soffice, &profile, "pdf", &ours, &exported)?;

    for r in results.iter_mut().filter(|r| r.error.is_none()) {
        let (source_docx, ours_docx) = (src.join(format!("{}.docx", r.name)), ours.join(format!("{}.docx", r.name)));
        let (source_pdf, ours_pdf) = (src.join(format!("{}.pdf", r.name)), ours.join(format!("{}.pdf", r.name)));
        let read = |docx: &Path, pdf: &Path| -> anyhow::Result<(Outline, Vec<String>)> {
            let outline = outline(&pandoc, docx).context("pandoc")?;
            let output = run_timed(pdftotext.command().arg("-q").arg(pdf).arg("-"), Duration::from_secs(60))?;
            if !output.status.success() {
                bail!("pdftotext {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
            }
            Ok((outline, crate::gates::word_sequence(&String::from_utf8_lossy(&output.stdout))))
        };
        match (read(&source_docx, &source_pdf), read(&ours_docx, &ours_pdf)) {
            (Ok((a, aw)), Ok((b, bw))) => r.scores = Some(Scores::compare(&a, &b, &aw, &bw)),
            (Err(e), _) => r.error = Some(format!("reading the source failed: {e:#}")),
            (_, Err(e)) => r.error = Some(format!("reading our .docx or its PDF failed: {e:#}")),
        }
        let a = pages(&pdftoppm, &source_pdf, &px.join(format!("{}-source", r.name)), dpi);
        let b = pages(&pdftoppm, &ours_pdf, &px.join(format!("{}-ours", r.name)), dpi);
        match (a, b) {
            (Ok(a), Ok(b)) => {
                r.pages = (a.len(), b.len());
                r.pixels = Some(disagreement(&a, &b));
            }
            (Err(e), _) | (_, Err(e)) => r.error = r.error.take().or(Some(format!("rendering for the pixel comparison failed: {e:#}"))),
        }
    }

    report(&results, &soffice, &pandoc, dpi, tagged);
    println!("files: {}", out.display());
    if let Some(path) = json_out {
        std::fs::write(&path, serde_json::to_string_pretty(&to_json(&results))?).with_context(|| format!("writing {}", path.display()))?;
    }
    let ours_failed: Vec<&str> = results.iter().filter(|r| r.error.as_deref().is_some_and(|e| e.starts_with("pdfcraft"))).map(|r| r.name).collect();
    if !ours_failed.is_empty() {
        bail!("our Word export failed on {}", ours_failed.join(", "));
    }
    if results.iter().all(|r| r.scores.is_none()) {
        bail!("no fixture could be scored");
    }
    Ok(())
}

fn remove_if_present(p: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| format!("removing {}", p.display())),
        _ => Ok(()),
    }
}

/// Convert `files` with LibreOffice in one process (it takes seconds to start), into `outdir`.
/// The private profile keeps it from clashing with a LibreOffice the user has open.
fn convert(soffice: &pdfcraft_oracle::Oracle, profile: &Path, filter: &str, outdir: &Path, files: &[PathBuf]) -> anyhow::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut cmd = soffice.command();
    cmd.arg("--headless")
        .arg(format!("-env:UserInstallation={}", file_url(profile)))
        .arg("--convert-to")
        .arg(filter)
        .arg("--outdir")
        .arg(outdir)
        .args(files);
    let timeout = Duration::from_secs(60u64.saturating_add(20u64.saturating_mul(files.len() as u64)));
    let output = run_timed(&mut cmd, timeout).context("running LibreOffice")?;
    if !output.status.success() {
        bail!("LibreOffice failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// A `file://` URL for an absolute path, percent-encoding what a URL cannot hold as is.
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            url.push(char::from(b));
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// Run our export through the automation tools, confined to `root`.
fn export(cli: &Path, root: &Path, pdf: &Path, docx: &Path) -> anyhow::Result<()> {
    let script = json!([
        { "tool": "doc_open", "args": { "path": pdf } },
        { "tool": "doc_export_office", "args": { "doc": 1, "path": docx } },
    ]);
    let script_path = docx.with_extension("script.json");
    std::fs::write(&script_path, serde_json::to_vec_pretty(&script)?)?;
    let output = run_timed(Command::new(cli).arg("run").arg("--script").arg(&script_path).arg("--root").arg(root), Duration::from_secs(120))?;
    if !output.status.success() {
        bail!("{}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    if !docx.is_file() {
        bail!("exited successfully but wrote no {}", docx.display());
    }
    Ok(())
}

fn outline(pandoc: &pdfcraft_oracle::Oracle, docx: &Path) -> anyhow::Result<Outline> {
    let output = run_timed(pandoc.command().arg(docx).arg("-t").arg("json"), Duration::from_secs(60))?;
    if !output.status.success() {
        bail!("{}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    let json: Value = serde_json::from_slice(&output.stdout).context("pandoc's JSON")?;
    Ok(Outline::from_pandoc(&json))
}

/// A decoded grayscale page.
type Raster = (usize, usize, Vec<u8>);

/// Every page of `pdf`, rendered by Poppler at `dpi` into files starting with `prefix`.
fn pages(pdftoppm: &pdfcraft_oracle::Oracle, pdf: &Path, prefix: &Path, dpi: u32) -> anyhow::Result<Vec<Raster>> {
    let (Some(dir), Some(stem)) = (prefix.parent(), prefix.file_name().map(|s| s.to_string_lossy().into_owned())) else {
        bail!("bad output prefix {}", prefix.display());
    };
    let page_files = |dir: &Path| -> anyhow::Result<Vec<(u32, PathBuf)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name.strip_prefix(&format!("{stem}-")).and_then(|rest| rest.strip_suffix(".pgm")).and_then(|n| n.parse::<u32>().ok());
            if let Some(n) = number {
                found.push((n, entry.path()));
            }
        }
        found.sort();
        Ok(found)
    };
    for (_, old) in page_files(dir)? {
        remove_if_present(&old)?;
    }
    let output = run_timed(pdftoppm.command().arg("-gray").arg("-r").arg(dpi.to_string()).arg(pdf).arg(prefix), Duration::from_secs(120))?;
    if !output.status.success() {
        bail!("pdftoppm {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    let mut decoded = Vec::new();
    for (_, path) in page_files(dir)? {
        let bytes = std::fs::read(&path)?;
        decoded.push(pdfcraft_oracle::pixels::decode_pgm(&bytes).with_context(|| format!("decoding {}", path.display()))?);
    }
    Ok(decoded)
}

/// Mean disagreement in percent over the longer document's pages. A page that only one side has,
/// or whose size differs, counts as wholly different: the layout did not survive.
fn disagreement(a: &[Raster], b: &[Raster]) -> f64 {
    let n = a.len().max(b.len());
    if n == 0 {
        return 0.0;
    }
    let total: f64 = (0..n)
        .map(|i| match (a.get(i), b.get(i)) {
            (Some((aw, ah, ap)), Some((bw, bh, bp))) => {
                match (pdfcraft_oracle::pixels::Gray::new(*aw, *ah, ap), pdfcraft_oracle::pixels::Gray::new(*bw, *bh, bp)) {
                    (Some(x), Some(y)) => pdfcraft_oracle::pixels::compare(x, y, 8).map_or(100.0, |d| d.percent()),
                    _ => 100.0,
                }
            }
            _ => 100.0,
        })
        .sum();
    total / n as f64
}

/// Run `cmd` to completion with captured output, killing it after `timeout`.
fn run_timed(cmd: &mut Command, timeout: Duration) -> anyhow::Result<Output> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().with_context(|| format!("starting {cmd:?}"))?;
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}s: {cmd:?}", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

fn cell(v: Option<f64>) -> String {
    v.map_or_else(|| "  –  ".into(), |v| format!("{v:.3}"))
}

fn mean(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let v: Vec<f64> = values.flatten().collect();
    if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) }
}

fn report(results: &[Outcome], soffice: &pdfcraft_oracle::Oracle, pandoc: &pdfcraft_oracle::Oracle, dpi: u32, tagged: bool) {
    let scored: Vec<&Outcome> = results.iter().filter(|r| r.scores.is_some()).collect();
    let structure = mean(scored.iter().map(|r| r.scores.as_ref().and_then(Scores::structure)));
    let pixels = mean(results.iter().map(|r| r.pixels));
    println!(
        "office oracle: {} of {} fixtures scored ({}, {}; {} source PDFs; pixels at {dpi} dpi)",
        scored.len(),
        results.len(),
        soffice.version(),
        pandoc.version(),
        if tagged { "tagged" } else { "untagged" },
    );
    println!(
        "  structure mean {} (1 is a perfect round trip); pixel disagreement mean {} (0 is identical layout)",
        cell(structure),
        pixels.map_or_else(|| "–".into(), |p| format!("{p:.2}%"))
    );
    let mut header = format!("{:<14}", "fixture");
    for n in Scores::NAMES {
        header.push_str(&format!(" {n:>5}"));
    }
    println!("{header}  {:>6} {:>7} {:>7}", "struct", "pages", "pixels");
    for r in results {
        let mut line = format!("{:<14}", r.name);
        match &r.scores {
            Some(s) => {
                for v in s.values() {
                    line.push_str(&format!(" {:>5}", cell(v)));
                }
                line.push_str(&format!("  {:>6}", cell(s.structure())));
            }
            None => line.push_str(&format!(" {:<width$}", "", width = 6 * Scores::NAMES.len() + 7)),
        }
        let pages = format!("{}/{}", r.pages.0, r.pages.1);
        let pixels = r.pixels.map_or_else(|| "–".into(), |p| format!("{p:.2}%"));
        println!("{line} {pages:>7} {pixels:>7}");
        if let Some(e) = &r.error {
            println!("{:<14} ! {e}", "");
        }
    }
    let mut means = format!("{:<14}", "mean");
    for i in 0..Scores::NAMES.len() {
        means.push_str(&format!(" {:>5}", cell(mean(scored.iter().map(|r| r.scores.as_ref().and_then(|s| s.values().get(i).copied().flatten()))))));
    }
    println!("{means}  {:>6}", cell(structure));
    println!("columns: pages = source/ours; pixels = share of pixels with no near match, LibreOffice laying out both");
    for r in results {
        println!("  {:<14} {}", r.name, r.about);
    }
}

fn to_json(results: &[Outcome]) -> Value {
    let fixtures: Vec<Value> = results
        .iter()
        .map(|r| {
            let scores = r.scores.as_ref().map(|s| {
                let mut m = serde_json::Map::new();
                for (name, v) in Scores::NAMES.iter().zip(s.values()) {
                    m.insert((*name).into(), v.map_or(Value::Null, Value::from));
                }
                m.insert("structure".into(), s.structure().map_or(Value::Null, Value::from));
                Value::Object(m)
            });
            json!({
                "name": r.name,
                "about": r.about,
                "scores": scores,
                "pages": { "source": r.pages.0, "ours": r.pages.1 },
                "pixels_percent": r.pixels,
                "error": r.error,
            })
        })
        .collect();
    json!({ "fixtures": fixtures })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_escape_what_a_url_cannot_hold() {
        assert_eq!(file_url(Path::new("/tmp/a b/ø#")), "file:///tmp/a%20b/%C3%B8%23");
    }

    #[test]
    fn a_missing_or_resized_page_counts_as_wholly_different() {
        let white = (4, 4, vec![255u8; 16]);
        let same = disagreement(std::slice::from_ref(&white), std::slice::from_ref(&white));
        assert!(same.abs() < 1e-9, "{same}");
        let extra = disagreement(std::slice::from_ref(&white), &[white.clone(), white.clone()]);
        assert!((extra - 50.0).abs() < 1e-9, "{extra}");
        let resized = disagreement(&[white], &[(5, 4, vec![255u8; 20])]);
        assert!((resized - 100.0).abs() < 1e-9, "{resized}");
        assert!(disagreement(&[], &[]).abs() < 1e-9);
    }
}
