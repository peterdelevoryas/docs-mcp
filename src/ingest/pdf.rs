//! PDFs. Text comes from `pdftotext -layout` (poppler), which keeps tables
//! readable; the section tree comes from the PDF's bookmarks (`mutool show
//! <pdf> outline`). Each bookmark names a page, and the section's text runs
//! from its printed heading on that page to the next bookmark's heading. A
//! PDF without bookmarks becomes one section per page.

use std::{collections::HashMap, path::Path, process::Command};

use anyhow::{Context, Result, bail};

use super::{Document, Section, split_number};

pub fn ingest(pdf: &Path) -> Result<Document> {
    let pages = extract_pages(pdf)?;
    let outline = read_outline(pdf)?;
    let sections = if outline.is_empty() {
        by_page(&pages)
    } else {
        split(&pages, &outline)
    };
    Ok(Document { pages, sections })
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("running {:?} (is it installed?)", cmd.get_program()))?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One string per page (index 0 is page 1), without running headers and
/// footers, and with blank-line runs collapsed.
fn extract_pages(pdf: &Path) -> Result<Vec<String>> {
    let text = run(Command::new("pdftotext").arg("-layout").arg(pdf).arg("-"))?;
    let mut pages: Vec<&str> = text.split('\u{c}').collect();
    // pdftotext ends the last page with a form feed too.
    if pages.last().is_some_and(|p| p.trim().is_empty()) {
        pages.pop();
    }
    Ok(strip_running_lines(&pages))
}

/// A line with its digits blanked, so "Page 12" and "Page 13" compare equal.
fn shape(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

/// Removes running headers and footers: a page's first or last line whose
/// shape repeats on at least a third of the pages.
fn strip_running_lines(pages: &[&str]) -> Vec<String> {
    let lines: Vec<Vec<&str>> = pages
        .iter()
        .map(|p| p.lines().map(str::trim_end).collect())
        .collect();
    let first = |ls: &Vec<&str>| ls.iter().position(|l| !l.is_empty());
    let last = |ls: &Vec<&str>| ls.iter().rposition(|l| !l.is_empty());
    let mut counts: HashMap<String, usize> = HashMap::new();
    for ls in &lines {
        let mut seen = Vec::new();
        for i in [first(ls), last(ls)].into_iter().flatten() {
            let s = shape(ls[i]);
            if !seen.contains(&s) {
                *counts.entry(s.clone()).or_default() += 1;
                seen.push(s);
            }
        }
    }
    let threshold = (pages.len() / 3).max(3);
    let running = |l: &str| counts.get(&shape(l)).is_some_and(|&n| n >= threshold);
    let mut out = Vec::with_capacity(pages.len());
    for mut ls in lines {
        if let Some(i) = first(&ls)
            && running(ls[i])
        {
            ls.drain(..=i);
        }
        if let Some(i) = last(&ls)
            && running(ls[i])
        {
            ls.truncate(i);
        }
        out.push(collapse_blank_lines(&ls));
    }
    out
}

fn collapse_blank_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in lines {
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    pub depth: usize,
    pub title: String,
    pub page: usize,
}

fn read_outline(pdf: &Path) -> Result<Vec<Bookmark>> {
    let text = run(Command::new("mutool").arg("show").arg(pdf).arg("outline"))?;
    Ok(text.lines().filter_map(parse_outline_line).collect())
}

/// Parses one line of `mutool show outline`, e.g.
/// `|\t\t\t"Section 1.3.1 Overview"\t#page=148&zoom=nan,0,579`.
fn parse_outline_line(line: &str) -> Option<Bookmark> {
    let open = line.find('"')?;
    let close = line.rfind("\"\t#page=")?;
    if close <= open {
        return None;
    }
    let depth = line[..open].matches('\t').count();
    let title = line[open + 1..close].replace("\\\"", "\"");
    let rest = &line[close + "\"\t#page=".len()..];
    let page: usize = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some(Bookmark {
        depth,
        title: title.trim().to_string(),
        page,
    })
}

/// Splits a bookmark title into its number and the rest. Bookmarks often
/// spell out "Section 1.3.1 …", "Chapter 7. …" or "Appendix A. …" where the
/// page prints just "1.3.1 …".
pub fn split_title(title: &str) -> (Option<String>, String) {
    for prefix in ["Section ", "Chapter ", "Appendix ", "Annex ", "Part "] {
        if let Some(rest) = title.strip_prefix(prefix)
            && let (Some(n), name) = split_number(rest)
        {
            return (Some(n), name);
        }
    }
    split_number(title)
}

/// Lowercase, single-spaced, without the § anchors some generators print
/// after headings.
fn normalize(s: &str) -> String {
    s.replace('§', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A position in the document: (page index, byte offset in that page).
type Pos = (usize, usize);

/// Where a bookmark's printed heading starts, searching its page (and the
/// next, in case the bookmark points just above a page break) from `after`.
fn find_heading(pages: &[String], mark: &Bookmark, after: Pos) -> Pos {
    let (number, name) = split_title(&mark.title);
    // Prefer the numbered forms; the bare title can also occur in prose
    // before the heading, so it's only a fallback.
    let mut tries: Vec<Vec<String>> = Vec::new();
    if let Some(n) = &number {
        tries.push(vec![format!("{n} {name}"), format!("{n}. {name}")]);
    }
    tries.push(vec![name.clone()]);
    let first = mark.page.saturating_sub(1).max(after.0);
    for candidates in tries {
        // Long headings wrap; matching the first 40 characters is enough.
        let keys: Vec<String> = candidates
            .iter()
            .map(|c| normalize(c).chars().take(40).collect())
            .filter(|k: &String| !k.is_empty())
            .collect();
        for (p, page) in pages.iter().enumerate().skip(first).take(2) {
            let mut offset = 0;
            for line in page.split_inclusive('\n') {
                let here = (p, offset);
                offset += line.len();
                if here < after {
                    continue;
                }
                let line = normalize(line);
                if keys.iter().any(|k| line.starts_with(k.as_str())) {
                    return here;
                }
            }
        }
    }
    // Not printed where expected: start at the top of its page.
    let top = (
        mark.page.saturating_sub(1).min(pages.len().saturating_sub(1)),
        0,
    );
    top.max(after)
}

fn text_between(pages: &[String], start: Pos, end: Pos) -> String {
    let mut out = String::new();
    for (p, page) in pages
        .iter()
        .enumerate()
        .take(end.0.min(pages.len().saturating_sub(1)) + 1)
        .skip(start.0)
    {
        let from = if p == start.0 { start.1 } else { 0 };
        let to = if p == end.0 { end.1 } else { page.len() };
        if from >= to {
            continue;
        }
        let chunk = page[from..to].trim_end();
        if chunk.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("[page {}]\n", p + 1));
        out.push_str(chunk);
    }
    out
}

fn split(pages: &[String], outline: &[Bookmark]) -> Vec<Section> {
    // Unnumbered bookmarks on the cover page that have children (the
    // document title, a date) are containers, not sections; nor are
    // generated tables of contents, which would only add noise to searches.
    let container = |i: usize| {
        let m = &outline[i];
        m.page == 1
            && split_title(&m.title).0.is_none()
            && outline.get(i + 1).is_some_and(|next| next.depth > m.depth)
    };
    let marks: Vec<&Bookmark> = outline
        .iter()
        .enumerate()
        .filter(|&(i, _)| !container(i))
        .map(|(_, m)| m)
        .filter(|m| {
            let t = m.title.to_lowercase();
            t != "table of contents" && t != "contents" && !t.starts_with("list of ")
        })
        .collect();
    let mut starts: Vec<Pos> = Vec::with_capacity(marks.len());
    let mut after = (0, 0);
    for mark in &marks {
        let pos = find_heading(pages, mark, after);
        starts.push(pos);
        after = pos;
    }
    let doc_end = (
        pages.len().saturating_sub(1),
        pages.last().map_or(0, String::len),
    );
    let mut sections: Vec<Section> = Vec::with_capacity(marks.len());
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (depth, ord)
    for (i, mark) in marks.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(doc_end);
        while stack.last().is_some_and(|&(d, _)| d >= mark.depth) {
            stack.pop();
        }
        let (number, title) = split_title(&mark.title);
        let page_end = if end.1 == 0 && end.0 > starts[i].0 {
            end.0
        } else {
            end.0 + 1
        };
        sections.push(Section {
            ord: i,
            number,
            title,
            depth: stack.len(),
            parent: stack.last().map(|&(_, ord)| ord),
            page_start: Some(starts[i].0 + 1),
            page_end: Some(page_end),
            text: text_between(pages, starts[i], end),
        });
        stack.push((mark.depth, i));
    }
    sections
}

fn by_page(pages: &[String]) -> Vec<Section> {
    let mut sections = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        if page.trim().is_empty() {
            continue;
        }
        sections.push(Section {
            ord: sections.len(),
            number: None,
            title: format!("Page {}", i + 1),
            depth: 0,
            parent: None,
            page_start: Some(i + 1),
            page_end: Some(i + 1),
            text: format!("[page {}]\n{page}", i + 1),
        });
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_lines() {
        let m = parse_outline_line(
            "|\t\t\t\t\t\"Section 4.2.1 Widget Control Register (Offset 10h)\"\t#page=169&zoom=nan,0,228",
        )
        .unwrap();
        assert_eq!(m.depth, 5);
        assert_eq!(m.page, 169);
        assert_eq!(
            split_title(&m.title),
            (
                Some("4.2.1".into()),
                "Widget Control Register (Offset 10h)".into()
            )
        );
        assert_eq!(
            split_title("Chapter 7. Configuration"),
            (Some("7".into()), "Configuration".into())
        );
        assert_eq!(
            split_title("Appendix A. Examples"),
            (Some("A".into()), "Examples".into())
        );
        assert_eq!(split_title("Glossary"), (None, "Glossary".into()));
    }

    #[test]
    fn splits_sections_at_printed_headings() {
        let pages = vec![
            "Intro text\n\n1.1 First §\nalpha\n1.2 Second §\nbeta".to_string(),
            "gamma\n1.2.1 Child §\ndelta".to_string(),
        ];
        let mark = |depth, title: &str, page| Bookmark {
            depth,
            title: title.into(),
            page,
        };
        let outline = vec![
            mark(0, "Widget Manual", 1),
            mark(1, "Chapter 1. Intro", 1),
            mark(2, "Section 1.1 First", 1),
            mark(2, "Section 1.2 Second", 1),
            mark(3, "Section 1.2.1 Child", 2),
        ];
        let s = split(&pages, &outline);
        // The cover-page title bookmark is a container, not a section.
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].depth, 0);
        assert_eq!(s[1].number.as_deref(), Some("1.1"));
        assert_eq!(s[1].parent, Some(0));
        assert_eq!(s[1].text, "[page 1]\n1.1 First §\nalpha");
        assert_eq!(s[2].text, "[page 1]\n1.2 Second §\nbeta\n\n[page 2]\ngamma");
        assert_eq!((s[2].page_start, s[2].page_end), (Some(1), Some(2)));
        assert_eq!(s[3].parent, Some(2));
        assert_eq!(s[3].text, "[page 2]\n1.2.1 Child §\ndelta");
    }

    #[test]
    fn strips_running_headers_and_footers() {
        let pages: Vec<String> = (1..=6)
            .map(|n| format!("   Widget Manual Rev 2\n\n\nbody {n}\n\n\n\nmore   \n\n        Page {n}\n"))
            .collect();
        let refs: Vec<&str> = pages.iter().map(String::as_str).collect();
        let out = strip_running_lines(&refs);
        assert_eq!(out[2], "body 3\n\nmore");
    }
}
