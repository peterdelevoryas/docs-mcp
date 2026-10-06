//! SQLite storage: one row per document, per PDF page, and per section,
//! with an FTS5 index over section numbers, titles, and text. Requests are
//! read-only and each opens its own connection on a blocking thread;
//! ingesting replaces a document in one transaction.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use schemars::JsonSchema;
use serde::Serialize;

use crate::ingest;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT,
    format TEXT NOT NULL,
    pages INTEGER NOT NULL,
    source_file TEXT NOT NULL,
    ingested_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pages (
    doc TEXT NOT NULL,
    page INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (doc, page)
);
CREATE TABLE IF NOT EXISTS sections (
    id INTEGER PRIMARY KEY,
    doc TEXT NOT NULL,
    ord INTEGER NOT NULL,
    number TEXT,
    title TEXT NOT NULL,
    depth INTEGER NOT NULL,
    parent_ord INTEGER,
    page_start INTEGER,
    page_end INTEGER,
    text TEXT NOT NULL,
    UNIQUE (doc, ord)
);
CREATE INDEX IF NOT EXISTS sections_by_number ON sections (doc, number);
CREATE INDEX IF NOT EXISTS sections_by_parent ON sections (doc, parent_ord);
CREATE VIRTUAL TABLE IF NOT EXISTS sections_fts USING fts5 (
    number, title, text,
    content = 'sections', content_rowid = 'id',
    tokenize = 'porter unicode61'
);
";

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct DocInfo {
    /// Pass this as `doc` to the other tools.
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// pdf, markdown or html.
    pub format: String,
    /// PDF pages (0 for formats without pages).
    pub pages: u32,
    pub sections: u32,
}

#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct SectionRef {
    /// Pass this as `section` to read_section: the heading's number (e.g.
    /// 7.5.3) when it has one, else `#<n>`.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct Hit {
    pub doc: String,
    #[serde(flatten)]
    pub section: SectionRef,
    /// Matching text, with matches marked «like this».
    pub snippet: String,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct SectionText {
    pub doc: String,
    #[serde(flatten)]
    pub section: SectionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<SectionRef>,
    /// Text of this section up to its first subsection. In PDFs, `[page N]`
    /// lines mark where each page begins.
    pub text: String,
    /// Characters in the whole section.
    pub total_chars: usize,
    /// Pass as `offset` to read the rest; absent when this is the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    /// Subsections, each readable with read_section.
    pub subsections: Vec<SectionRef>,
}

#[derive(Serialize, JsonSchema, Debug)]
pub struct Page {
    pub page: u32,
    pub text: String,
}

pub struct NewDoc<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub format: &'a str,
    pub source: &'a str,
}

const SECTION_COLS: &str = "ord, number, title, page_start";

fn section_ref(r: &rusqlite::Row, at: usize) -> rusqlite::Result<SectionRef> {
    let ord: i64 = r.get(at)?;
    let number: Option<String> = r.get(at + 1)?;
    Ok(SectionRef {
        id: number.clone().unwrap_or_else(|| format!("#{ord}")),
        number,
        title: r.get(at + 2)?,
        page: r.get(at + 3)?,
    })
}

impl Store {
    /// Opens (creating if needed) the database and its schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn =
            Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { path })
    }

    fn read_conn(&self) -> Result<Connection> {
        Ok(Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?)
    }

    async fn with_conn<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || f(&store.read_conn()?)).await?
    }

    /// Adds a document, or replaces the one with the same id.
    pub fn replace(&self, new: &NewDoc, doc: &ingest::Document) -> Result<()> {
        let mut conn = Connection::open(&self.path)?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sections WHERE doc = ?1", [new.id])?;
        tx.execute("DELETE FROM pages WHERE doc = ?1", [new.id])?;
        tx.execute(
            "INSERT OR REPLACE INTO documents
             (id, title, description, format, pages, source_file, ingested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                new.id,
                new.title,
                new.description,
                new.format,
                doc.pages.len() as i64,
                new.source,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        {
            let mut page = tx.prepare("INSERT INTO pages (doc, page, text) VALUES (?1, ?2, ?3)")?;
            for (i, text) in doc.pages.iter().enumerate() {
                page.execute(params![new.id, (i + 1) as i64, text])?;
            }
            let mut section = tx.prepare(
                "INSERT INTO sections
                 (doc, ord, number, title, depth, parent_ord, page_start, page_end, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for s in &doc.sections {
                section.execute(params![
                    new.id,
                    s.ord as i64,
                    s.number,
                    s.title,
                    s.depth as i64,
                    s.parent.map(|p| p as i64),
                    s.page_start.map(|p| p as i64),
                    s.page_end.map(|p| p as i64),
                    s.text
                ])?;
            }
        }
        tx.execute("INSERT INTO sections_fts (sections_fts) VALUES ('rebuild')", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Removes a document. Returns whether it existed.
    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut conn = Connection::open(&self.path)?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sections WHERE doc = ?1", [id])?;
        tx.execute("DELETE FROM pages WHERE doc = ?1", [id])?;
        let n = tx.execute("DELETE FROM documents WHERE id = ?1", [id])?;
        tx.execute("INSERT INTO sections_fts (sections_fts) VALUES ('rebuild')", [])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub async fn documents(&self) -> Result<Vec<DocInfo>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT d.id, d.title, d.description, d.format, d.pages,
                        (SELECT count(*) FROM sections WHERE doc = d.id)
                 FROM documents d ORDER BY d.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(DocInfo {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    description: r.get(2)?,
                    format: r.get(3)?,
                    pages: r.get(4)?,
                    sections: r.get(5)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Checks that a named document exists.
    pub async fn check_doc(&self, doc: String) -> Result<String> {
        self.with_conn(move |c| {
            check_doc(c, &doc)?;
            Ok(doc)
        })
        .await
    }

    /// Searches one document, or all of them.
    pub async fn search(&self, doc: Option<String>, query: String, limit: u32) -> Result<Vec<Hit>> {
        self.with_conn(move |c| search(c, doc.as_deref(), &query, limit)).await
    }

    pub async fn section(
        &self,
        doc: Option<String>,
        section: String,
        offset: usize,
        max: usize,
    ) -> Result<SectionText> {
        self.with_conn(move |c| {
            let doc = resolve_doc(c, doc.as_deref())?;
            read_section(c, &doc, &section, offset, max)
        })
        .await
    }

    /// Top-level sections of a document, or the subsections of `under`.
    pub async fn contents(&self, doc: Option<String>, under: Option<String>) -> Result<(String, Vec<SectionRef>)> {
        self.with_conn(move |c| {
            let doc = resolve_doc(c, doc.as_deref())?;
            let parent = match &under {
                Some(s) => Some(find_section(c, &doc, s)?.0),
                None => None,
            };
            let list = children(c, &doc, parent)?;
            Ok((doc, list))
        })
        .await
    }

    pub async fn pages(&self, doc: Option<String>, start: u32, end: u32) -> Result<(String, Vec<Page>)> {
        self.with_conn(move |c| {
            let doc = resolve_doc(c, doc.as_deref())?;
            let mut stmt = c.prepare(
                "SELECT page, text FROM pages WHERE doc = ?1 AND page BETWEEN ?2 AND ?3 ORDER BY page",
            )?;
            let rows = stmt.query_map(params![doc, start, end], |r| {
                Ok(Page {
                    page: r.get(0)?,
                    text: r.get(1)?,
                })
            })?;
            let pages: Vec<Page> = rows.collect::<Result<_, _>>()?;
            if pages.is_empty() {
                bail!("{doc} has no pages {start}-{end} (only PDFs have pages)");
            }
            Ok((doc, pages))
        })
        .await
    }

    pub async fn count_documents(&self) -> Result<u64> {
        self.with_conn(|c| {
            let n: i64 = c.query_row("SELECT count(*) FROM documents", [], |r| r.get(0))?;
            Ok(n as u64)
        })
        .await
    }
}

fn known_docs(c: &Connection) -> Vec<String> {
    c.prepare("SELECT id FROM documents ORDER BY id")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default()
}

fn check_doc(c: &Connection, doc: &str) -> Result<()> {
    let found: Option<String> = c
        .query_row("SELECT id FROM documents WHERE id = ?1", [doc], |r| r.get(0))
        .optional()?;
    if found.is_none() {
        bail!("no document {doc:?}; documents: {}", known_docs(c).join(", "));
    }
    Ok(())
}

/// The document a call refers to: the one named, or the only one there is.
fn resolve_doc(c: &Connection, doc: Option<&str>) -> Result<String> {
    if let Some(id) = doc {
        check_doc(c, id)?;
        return Ok(id.to_string());
    }
    let docs = known_docs(c);
    match &docs[..] {
        [only] => Ok(only.clone()),
        [] => bail!("no documents have been ingested"),
        _ => bail!("several documents are available; pass doc (one of: {})", docs.join(", ")),
    }
}

fn looks_like_number(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()))
        && s.chars().any(|c| c.is_ascii_digit())
}

/// Turns free text into an FTS5 query over its words, each quoted so
/// punctuation can't be read as syntax.
fn words_query(query: &str, joiner: &str) -> String {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(joiner)
}

fn search(c: &Connection, doc: Option<&str>, query: &str, limit: u32) -> Result<Vec<Hit>> {
    if let Some(d) = doc {
        check_doc(c, d)?;
    }
    let mut hits: Vec<Hit> = Vec::new();
    // A bare section number ("7.5.3") goes straight to that section.
    let trimmed = query.trim().trim_start_matches("Section ").trim_end_matches('.');
    if looks_like_number(trimmed) {
        let mut stmt = c.prepare(&format!(
            "SELECT doc, {SECTION_COLS} FROM sections
             WHERE number = ?1 AND (?2 IS NULL OR doc = ?2) ORDER BY doc, ord LIMIT 5"
        ))?;
        let rows = stmt.query_map(params![trimmed, doc], |r| {
            let section = section_ref(r, 1)?;
            Ok(Hit {
                doc: r.get(0)?,
                snippet: section.title.clone(),
                section,
            })
        })?;
        for hit in rows {
            hits.push(hit?);
        }
    }
    let mut stmt = c.prepare(
        "SELECT s.doc, s.ord, s.number, s.title, s.page_start,
                snippet(sections_fts, 2, '«', '»', ' … ', 32)
         FROM sections_fts JOIN sections s ON s.id = sections_fts.rowid
         WHERE sections_fts MATCH ?1 AND (?2 IS NULL OR s.doc = ?2)
         ORDER BY bm25(sections_fts, 5.0, 10.0, 1.0)
         LIMIT ?3",
    )?;
    let run = |stmt: &mut rusqlite::Statement, q: &str| -> rusqlite::Result<Vec<Hit>> {
        stmt.query_map(params![q, doc, limit], |r| {
            Ok(Hit {
                doc: r.get(0)?,
                section: section_ref(r, 1)?,
                snippet: r.get(5)?,
            })
        })?
        .collect()
    };
    // Try the query as written (so "exact phrase", AND/OR/NOT and prefix*
    // work); if it isn't valid FTS5 syntax, or finds nothing, fall back to
    // requiring all its words, then to any of them.
    let mut found = run(&mut stmt, query).unwrap_or_default();
    if found.is_empty() {
        let all = words_query(query, " ");
        if all.is_empty() {
            bail!("the query has no words to search for");
        }
        found = run(&mut stmt, &all)?;
        if found.is_empty() {
            found = run(&mut stmt, &words_query(query, " OR "))?;
        }
    }
    for hit in found {
        if !hits.iter().any(|h| h.doc == hit.doc && h.section.id == hit.section.id) {
            hits.push(hit);
        }
    }
    hits.truncate(limit as usize);
    Ok(hits)
}

/// Finds a section by id (`#12`), by number ("7.5.3", "Section 7.5.3") or,
/// failing that, by title (case-insensitive). Returns its ord and a reference.
fn find_section(c: &Connection, doc: &str, section: &str) -> Result<(i64, SectionRef)> {
    let wanted = section.trim();
    let row = |r: &rusqlite::Row| Ok((r.get::<_, i64>(0)?, section_ref(r, 0)?));
    if let Some(ord) = wanted.strip_prefix('#').and_then(|n| n.parse::<i64>().ok()) {
        return c
            .query_row(
                &format!("SELECT {SECTION_COLS} FROM sections WHERE doc = ?1 AND ord = ?2"),
                params![doc, ord],
                row,
            )
            .optional()?
            .with_context(|| format!("no section {wanted} in {doc}"));
    }
    let number = wanted
        .trim_start_matches("Section ")
        .trim_start_matches("Chapter ")
        .trim_start_matches("Appendix ")
        .trim_end_matches('.');
    let by_number = c
        .query_row(
            &format!(
                "SELECT {SECTION_COLS} FROM sections
                 WHERE doc = ?1 AND number = ?2 ORDER BY ord LIMIT 1"
            ),
            params![doc, number],
            row,
        )
        .optional()?;
    if let Some(found) = by_number {
        return Ok(found);
    }
    c.query_row(
        &format!(
            "SELECT {SECTION_COLS} FROM sections
             WHERE doc = ?1 AND (lower(title) = lower(?2)
                 OR lower(coalesce(number || ' ', '') || title) = lower(?2))
             ORDER BY ord LIMIT 1"
        ),
        params![doc, wanted],
        row,
    )
    .optional()?
    .with_context(|| format!("no section {wanted:?} in {doc}; search_docs finds sections by topic"))
}

fn children(c: &Connection, doc: &str, parent: Option<i64>) -> Result<Vec<SectionRef>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {SECTION_COLS} FROM sections WHERE doc = ?1 AND parent_ord IS ?2 ORDER BY ord"
    ))?;
    let rows = stmt.query_map(params![doc, parent], |r| section_ref(r, 0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn read_section(
    c: &Connection,
    doc: &str,
    section: &str,
    offset: usize,
    max: usize,
) -> Result<SectionText> {
    let (ord, sref) = find_section(c, doc, section)?;
    let (page_end, parent_ord, text): (Option<u32>, Option<i64>, String) = c.query_row(
        "SELECT page_end, parent_ord, text FROM sections WHERE doc = ?1 AND ord = ?2",
        params![doc, ord],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let parent = match parent_ord {
        Some(p) => c
            .query_row(
                &format!("SELECT {SECTION_COLS} FROM sections WHERE doc = ?1 AND ord = ?2"),
                params![doc, p],
                |r| section_ref(r, 0),
            )
            .optional()?,
        None => None,
    };
    let total_chars = text.chars().count();
    if offset > total_chars {
        bail!("offset {offset} is past the end of the section ({total_chars} characters)");
    }
    let mut chunk: String = text.chars().skip(offset).take(max).collect();
    let mut next = offset + chunk.chars().count();
    // End on a line break when one is reasonably close, so table rows and
    // sentences aren't cut in half.
    if next < total_chars
        && let Some(cut) = chunk.rfind('\n')
        && cut > chunk.len() / 2
    {
        chunk.truncate(cut + 1);
        next = offset + chunk.chars().count();
    }
    Ok(SectionText {
        doc: doc.to_string(),
        section: sref,
        page_end,
        parent,
        text: chunk,
        total_chars,
        next_offset: (next < total_chars).then_some(next),
        subsections: children(c, doc, Some(ord))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{Document, Section};

    fn section(ord: usize, number: Option<&str>, title: &str, parent: Option<usize>, text: &str) -> Section {
        Section {
            ord,
            number: number.map(str::to_string),
            title: title.to_string(),
            depth: usize::from(parent.is_some()),
            parent,
            page_start: Some(10 + ord),
            page_end: Some(10 + ord),
            text: text.to_string(),
        }
    }

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "docs-store-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("t.db")).unwrap();
        let manual = Document {
            pages: vec!["one".into(), "two".into(), "three".into()],
            sections: vec![
                section(0, Some("4"), "Configuration", None, "chapter intro"),
                section(1, Some("4.2.1"), "Widget Control Register (Offset 10h)", Some(0),
                    "Speed Limit - sets the highest speed the widget may run at.\nEnable bit."),
                section(2, Some("2.3"), "Batch Mode", None, "In Batch Mode, requests are grouped into batches."),
            ],
        };
        let new = |id, title| NewDoc { id, title, description: None, format: "pdf", source: "x.pdf" };
        store.replace(&new("widget-2.0", "Widget Manual 2.0"), &manual).unwrap();
        let guide = Document {
            pages: vec![],
            sections: vec![Section { page_start: None, page_end: None, ..section(0, None, "Getting started", None, "Install the widget driver first.") }],
        };
        store.replace(&NewDoc { format: "markdown", ..new("guide", "User guide") }, &guide).unwrap();
        store
    }

    #[tokio::test]
    async fn search_read_and_contents() {
        let s = store();
        assert_eq!(s.documents().await.unwrap().len(), 2);
        // With two documents, calls that need one must name it.
        assert!(s.section(None, "4.2.1".into(), 0, 100).await.is_err());
        let doc = Some("widget-2.0".to_string());

        let hits = s.search(doc.clone(), "speed limit".into(), 5).await.unwrap();
        assert_eq!(hits[0].section.id, "4.2.1");
        assert!(hits[0].snippet.contains('«'), "{hits:?}");
        // Searching everything finds both documents.
        let all = s.search(None, "widget".into(), 10).await.unwrap();
        assert!(all.iter().any(|h| h.doc == "guide") && all.iter().any(|h| h.doc == "widget-2.0"));
        // Stemming: "batches" finds "batch".
        assert_eq!(s.search(doc.clone(), "batches".into(), 5).await.unwrap()[0].section.id, "2.3");
        // A bare number jumps to the section; broken syntax still searches.
        assert_eq!(
            s.search(doc.clone(), "4.2.1".into(), 5).await.unwrap()[0].section.title,
            "Widget Control Register (Offset 10h)"
        );
        assert!(!s.search(doc.clone(), "\"unbalanced (widget".into(), 5).await.unwrap().is_empty());

        let sec = s.section(doc.clone(), "Section 4.2.1".into(), 0, 40).await.unwrap();
        assert_eq!(sec.parent.unwrap().id, "4");
        assert!(sec.next_offset.is_some());
        let rest = s.section(doc.clone(), "4.2.1".into(), sec.next_offset.unwrap(), 10_000).await.unwrap();
        assert_eq!(format!("{}{}", sec.text, rest.text).chars().count(), sec.total_chars);
        assert!(rest.next_offset.is_none());
        assert_eq!(s.section(doc.clone(), "batch mode".into(), 0, 100).await.unwrap().section.id, "2.3");
        // Unnumbered sections are addressed by #ord.
        let g = s.section(Some("guide".into()), "#0".into(), 0, 100).await.unwrap();
        assert_eq!(g.section.title, "Getting started");
        assert!(g.section.page.is_none());

        let (_, top) = s.contents(doc.clone(), None).await.unwrap();
        assert_eq!(top.len(), 2);
        let (_, under) = s.contents(doc.clone(), Some("4".into())).await.unwrap();
        assert_eq!(under[0].id, "4.2.1");
        assert_eq!(s.pages(doc, 2, 3).await.unwrap().1.len(), 2);
        assert!(s.pages(Some("guide".into()), 1, 1).await.is_err());

        assert!(s.remove("guide").unwrap());
        // One document left: it's the default.
        assert!(s.section(None, "4.2.1".into(), 0, 100).await.is_ok());
    }
}
