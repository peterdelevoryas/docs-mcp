//! Turns a document into sections: a tree of headings, each with the text
//! up to the next heading. The format is chosen by file extension.

mod markdown;
mod pdf;

use std::path::Path;

use anyhow::{Context, Result, bail};

pub struct Document {
    /// PDF pages (index 0 is page 1); empty for formats without pages.
    pub pages: Vec<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone)]
pub struct Section {
    /// Document order, from 0.
    pub ord: usize,
    /// The heading's number when it has one: "7.5.3", "7" for a chapter,
    /// "A" or "A.1" in an appendix.
    pub number: Option<String>,
    pub title: String,
    pub depth: usize,
    /// ord of the enclosing section.
    pub parent: Option<usize>,
    pub page_start: Option<usize>,
    pub page_end: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Pdf,
    Markdown,
    Html,
}

impl Format {
    pub fn of(path: &Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default();
        Ok(match ext.as_str() {
            "pdf" => Format::Pdf,
            "md" | "markdown" | "txt" | "text" | "rst" | "adoc" => Format::Markdown,
            "html" | "htm" | "xhtml" => Format::Html,
            _ => bail!(
                "don't know how to read {} (expected .pdf, .md, .txt, .rst or .html)",
                path.display()
            ),
        })
    }
}

pub fn ingest(path: &Path) -> Result<Document> {
    let doc = match Format::of(path)? {
        Format::Pdf => pdf::ingest(path)?,
        Format::Markdown => markdown::parse(&read(path)?),
        Format::Html => {
            let html = main_content(&read(path)?);
            let md = htmd::HtmlToMarkdown::builder()
                .skip_tags(vec!["script", "style", "nav", "noscript", "svg"])
                .build()
                .convert(&html)
                .context("converting HTML")?;
            markdown::parse(&md)
        }
    };
    if doc.sections.is_empty() {
        bail!("{} has no text", path.display());
    }
    Ok(doc)
}

/// The page's main content (`<main>`, `role="main"`, `<article>`), so site
/// navigation and sidebars don't become sections; else the whole body.
fn main_content(html: &str) -> String {
    let page = scraper::Html::parse_document(html);
    for selector in ["main", "[role=main]", "article", "body"] {
        let selector = scraper::Selector::parse(selector).expect("a valid selector");
        if let Some(el) = page.select(&selector).next() {
            return el.html();
        }
    }
    html.to_string()
}

fn read(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Splits a heading into its number and title: "4.2.1 Widget Control" ->
/// 4.2.1, "7. Configuration" -> 7, "A.1 Examples" -> A.1. A number is
/// dotted parts of digits, or a capital letter followed by digit parts.
pub fn split_number(heading: &str) -> (Option<String>, String) {
    let heading = heading.trim();
    if let Some((first, rest)) = heading.split_once(char::is_whitespace) {
        let num = first.trim_end_matches(['.', ':', ')']);
        let parts: Vec<&str> = num.split('.').collect();
        let digits = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit());
        let letter = |p: &str| p.len() == 1 && p.chars().all(|c| c.is_ascii_uppercase());
        let is_number = !num.is_empty()
            && (parts.iter().all(|p| digits(p))
                || (letter(parts[0])
                    && (parts.len() > 1 || first.ends_with('.'))
                    && parts[1..].iter().all(|p| digits(p))));
        if is_number && !rest.trim().is_empty() {
            return (Some(num.to_string()), rest.trim().to_string());
        }
    }
    (None, heading.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let n = |s: &str| split_number(s);
        assert_eq!(n("4.2.1 Widget Control"), (Some("4.2.1".into()), "Widget Control".into()));
        assert_eq!(n("7. Configuration"), (Some("7".into()), "Configuration".into()));
        assert_eq!(n("A.1 Examples"), (Some("A.1".into()), "Examples".into()));
        assert_eq!(n("B. Encoding"), (Some("B".into()), "Encoding".into()));
        assert_eq!(n("A Link represents"), (None, "A Link represents".into()));
        assert_eq!(n("Overview"), (None, "Overview".into()));
        assert_eq!(n("2026 Roadmap"), (Some("2026".into()), "Roadmap".into()));
    }

    #[test]
    fn html_main_content() {
        let html = "<html><body><nav><h2>Site menu</h2></nav>\
            <div role=\"main\"><h1>Guide<a href=\"#guide\">¶</a></h1><p>Hi <a href=\"x.html\">there</a>.</p>\
            <h2>1.2 Setup</h2><p>Run it.</p></div><div class=\"sidebar\"><h3>Search</h3></div></body></html>";
        let dir = std::env::temp_dir().join(format!("docs-html-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("page.html");
        std::fs::write(&path, html).unwrap();
        let doc = ingest(&path).unwrap();
        let titles: Vec<&str> = doc.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Guide", "Setup"]);
        assert_eq!(doc.sections[1].number.as_deref(), Some("1.2"));
    }

    #[test]
    fn formats() {
        assert_eq!(Format::of(Path::new("a/b.PDF")).unwrap(), Format::Pdf);
        assert_eq!(Format::of(Path::new("README.md")).unwrap(), Format::Markdown);
        assert_eq!(Format::of(Path::new("x.rst")).unwrap(), Format::Markdown);
        assert_eq!(Format::of(Path::new("x.html")).unwrap(), Format::Html);
        assert!(Format::of(Path::new("x.docx")).is_err());
    }
}
