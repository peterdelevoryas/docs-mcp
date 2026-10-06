//! Markdown and other plain text. Headings are ATX (`## Title`) or
//! underlined (Setext and reStructuredText: a line of `=`, `-`, `~` … at
//! least as long as the title). For underlines, each character gets the
//! next level the first time it appears, as reStructuredText does; text
//! inside fenced code blocks is never a heading. Text before the first
//! heading becomes an untitled section, and a file with no headings is one
//! section.

use super::{Document, Section, split_number};

struct Heading {
    /// Line index where the heading starts (its overline, if any).
    line: usize,
    /// Line index where the section's body starts.
    body: usize,
    level: usize,
    title: String,
}

const UNDERLINE_CHARS: &str = "=-~^\"'`#*+_:.";

fn underline_char(line: &str) -> Option<char> {
    let t = line.trim_end();
    let c = t.chars().next()?;
    (t.len() >= 3 && UNDERLINE_CHARS.contains(c) && t.chars().all(|x| x == c)).then_some(c)
}

fn atx(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    let level = t.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim().to_string();
    (!title.is_empty()).then_some((level, title))
}

fn headings(lines: &[&str]) -> Vec<Heading> {
    let mut found = Vec::new();
    let mut fence: Option<&str> = None;
    // Underline characters in the order first seen, for their levels.
    let mut underline_levels: Vec<(char, bool)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        if let Some(f) = fence {
            if t.starts_with(f) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(&t[..3]);
            i += 1;
            continue;
        }
        if let Some((level, title)) = atx(line) {
            found.push(Heading { line: i, body: i + 1, level, title });
            i += 1;
            continue;
        }
        // Overlined reStructuredText title: ====\nTitle\n====
        if let Some(c) = underline_char(line)
            && i + 2 < lines.len()
            && !lines[i + 1].trim().is_empty()
            && underline_char(lines[i + 2]) == Some(c)
        {
            let level = level_for(&mut underline_levels, c, true);
            found.push(Heading {
                line: i,
                body: i + 3,
                level,
                title: lines[i + 1].trim().to_string(),
            });
            i += 3;
            continue;
        }
        // Underlined: Title\n-----, with a blank line (or nothing) before.
        if i + 1 < lines.len()
            && !t.is_empty()
            && (i == 0 || lines[i - 1].trim().is_empty())
            && let Some(c) = underline_char(lines[i + 1])
            && lines[i + 1].trim_end().len() >= line.trim().chars().count().min(3)
            && !t.starts_with(['-', '*', '+', '>', '|'])
        {
            let level = level_for(&mut underline_levels, c, false);
            found.push(Heading { line: i, body: i + 2, level, title: t.trim().to_string() });
            i += 2;
            continue;
        }
        i += 1;
    }
    found
}

/// A heading's plain text: Markdown links become their text, and empty or
/// pilcrow permalinks ("[¶](#id)", which generated HTML docs put after
/// headings) disappear.
fn clean_title(title: &str) -> String {
    let mut out = String::new();
    let mut rest = title;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|c| open + c) else {
            break;
        };
        let Some(end) = rest[close..].find(')').map(|e| close + e) else {
            break;
        };
        out.push_str(&rest[..open]);
        let text = &rest[open + 1..close];
        if text.trim() != "¶" && text.trim() != "#" {
            out.push_str(text);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out.replace('¶', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn level_for(levels: &mut Vec<(char, bool)>, c: char, overline: bool) -> usize {
    match levels.iter().position(|&l| l == (c, overline)) {
        Some(p) => p + 1,
        None => {
            levels.push((c, overline));
            levels.len()
        }
    }
}

fn body(lines: &[&str], start: usize, end: usize) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in &lines[start.min(end)..end] {
        let line = line.trim_end();
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

pub fn parse(text: &str) -> Document {
    let lines: Vec<&str> = text.lines().collect();
    let heads = headings(&lines);
    let mut sections = Vec::new();
    let preamble_end = heads.first().map_or(lines.len(), |h| h.line);
    let preamble = body(&lines, 0, preamble_end);
    if !preamble.is_empty() {
        sections.push(Section {
            ord: 0,
            number: None,
            title: if heads.is_empty() { "(document)" } else { "(preamble)" }.to_string(),
            depth: 0,
            parent: None,
            page_start: None,
            page_end: None,
            text: preamble,
        });
    }
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (level, ord)
    for (i, h) in heads.iter().enumerate() {
        let end = heads.get(i + 1).map_or(lines.len(), |n| n.line);
        while stack.last().is_some_and(|&(l, _)| l >= h.level) {
            stack.pop();
        }
        let ord = sections.len();
        let (number, title) = split_number(&clean_title(&h.title));
        let text = body(&lines, h.body, end);
        sections.push(Section {
            ord,
            number,
            title: title.clone(),
            depth: stack.len(),
            parent: stack.last().map(|&(_, o)| o),
            page_start: None,
            page_end: None,
            // Keep the heading in the text, as in PDFs.
            text: if text.is_empty() {
                clean_title(&h.title)
            } else {
                format!("{}\n\n{text}", clean_title(&h.title))
            },
        });
        stack.push((h.level, ord));
    }
    Document { pages: Vec::new(), sections }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atx_and_fences() {
        let doc = parse(
            "Intro.\n\n# Guide\n\nHello.\n\n## 1.2 Setup\n\n```sh\n# not a heading\n```\n\n### Details ###\nMore\n## Next\nEnd\n",
        );
        let titles: Vec<(&str, Option<&str>, usize, Option<usize>)> = doc
            .sections
            .iter()
            .map(|s| (s.title.as_str(), s.number.as_deref(), s.depth, s.parent))
            .collect();
        assert_eq!(
            titles,
            [
                ("(preamble)", None, 0, None),
                ("Guide", None, 0, None),
                ("Setup", Some("1.2"), 1, Some(1)),
                ("Details", None, 2, Some(2)),
                ("Next", None, 1, Some(1)),
            ]
        );
        assert_eq!(doc.sections[2].text, "1.2 Setup\n\n```sh\n# not a heading\n```");
    }

    #[test]
    fn underlined_headings() {
        let doc = parse(
            "=====\nTitle\n=====\n\nIntro\n\nUsage\n=====\n\nText\n\nOptions\n-------\n\n- a list\n- item\n\nOther\n=====\nx\n",
        );
        let titles: Vec<(&str, usize)> =
            doc.sections.iter().map(|s| (s.title.as_str(), s.depth)).collect();
        assert_eq!(titles, [("Title", 0), ("Usage", 1), ("Options", 2), ("Other", 1)]);
    }

    #[test]
    fn titles_lose_link_syntax() {
        assert_eq!(clean_title("Coding style[¶](#coding-style \"Permalink\")"), "Coding style");
        assert_eq!(clean_title("[The Kernel](../index.html) docs"), "The Kernel docs");
        assert_eq!(clean_title("Use `a[0]` here"), "Use `a[0]` here");
    }

    #[test]
    fn no_headings_is_one_section() {
        let doc = parse("just\ntext\n");
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].title, "(document)");
    }
}
