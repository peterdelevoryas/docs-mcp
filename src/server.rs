use axum::http::request::Parts;
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{
        tool::Extension,
        wrapper::{Json, Parameters},
    },
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    agent::{Agent, Answer, summarize_request},
    auth::Client,
    config::Config,
    store::{DocInfo, Hit, Page, SectionRef, SectionText, Store},
};

/// Characters of section text per read_section call.
const READ_CHARS: usize = 16_000;
const MAX_PAGES: u32 = 5;

#[derive(Clone)]
pub struct DocsServer {
    store: Store,
    agent: Option<Agent>,
    name: String,
    instructions: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Keywords, e.g. "widget control speed limit". Words are stemmed;
    /// supports "exact phrase", AND/OR/NOT and prefix*. A bare section
    /// number (4.2.1) returns that section.
    pub query: String,
    /// Document id from list_documents; omit to search all documents.
    pub doc: Option<String>,
    /// Maximum results (default 10, max 20).
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadSectionParams {
    /// Section id as returned by the other tools (a number like "4.2.1", or
    /// "#12" for an unnumbered heading), or an exact title.
    pub section: String,
    /// Document id; may be omitted when there is only one document.
    pub doc: Option<String>,
    /// next_offset from a previous read of the same section.
    pub offset: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ContentsParams {
    /// Document id; may be omitted when there is only one document.
    pub doc: Option<String>,
    /// List the subsections of this section; omit for the top level.
    pub under: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PagesParams {
    /// First PDF page (as in the `[page N]` markers and citations).
    pub start: u32,
    /// Last page, at most start + 4 (default: start).
    pub end: Option<u32>,
    /// Document id; may be omitted when there is only one document.
    pub doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AskParams {
    /// A self-contained question, with any context the answer depends on.
    pub question: String,
    /// Limit the agent to this document.
    pub doc: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SummarizeParams {
    /// Document id; may be omitted when there is only one document.
    pub doc: Option<String>,
    /// A section id or title to summarize (with its subsections); omit for
    /// the whole document.
    pub section: Option<String>,
    /// Who the summary is for or what to emphasize, e.g. "a driver author
    /// implementing suspend/resume".
    pub focus: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Documents {
    pub documents: Vec<DocInfo>,
}

#[derive(Serialize, JsonSchema)]
pub struct Hits {
    pub results: Vec<Hit>,
}

#[derive(Serialize, JsonSchema)]
pub struct Contents {
    pub doc: String,
    pub sections: Vec<SectionRef>,
}

#[derive(Serialize, JsonSchema)]
pub struct Pages {
    pub doc: String,
    pub pages: Vec<Page>,
}

#[tool_router]
impl DocsServer {
    pub fn new(store: Store, agent: Option<Agent>, config: &Config) -> Self {
        Self {
            store,
            agent,
            name: config.name.clone(),
            instructions: config.instructions(),
        }
    }

    #[tool(
        description = "List the documents available: id, title, description, format, size. \
            Other tools take a doc id; it may be omitted when there is only one document.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_documents(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<Documents>, ErrorData> {
        log(&parts, "list_documents", "");
        Ok(Json(Documents {
            documents: self.store.documents().await.map_err(internal)?,
        }))
    }

    #[tool(
        description = "Search the documents by keyword: section numbers, titles and text, \
            stemmed. Returns the best-matching sections with snippets; read one with \
            read_section. Use short keyword queries in the documents' own terminology, and try \
            synonyms.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search_docs(
        &self,
        Parameters(p): Parameters<SearchParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<Hits>, ErrorData> {
        log(&parts, "search_docs", &p.query);
        let limit = p.limit.unwrap_or(10).clamp(1, 20);
        let results = self.store.search(p.doc, p.query, limit).await.map_err(invalid)?;
        Ok(Json(Hits { results }))
    }

    #[tool(
        description = "Read a section by id (4.2.1, #12) or exact title: its text up to its \
            first subsection (PDF tables keep their columns; [page N] lines mark page starts), \
            its parent, and its subsections. Long sections come in parts: pass next_offset \
            back as offset.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_section(
        &self,
        Parameters(p): Parameters<ReadSectionParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<SectionText>, ErrorData> {
        log(&parts, "read_section", &p.section);
        let section = self
            .store
            .section(p.doc, p.section, p.offset.unwrap_or(0), READ_CHARS)
            .await
            .map_err(invalid)?;
        Ok(Json(section))
    }

    #[tool(
        description = "List a document's top-level sections, or the subsections of one \
            section (e.g. under = \"4.2\").",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn table_of_contents(
        &self,
        Parameters(p): Parameters<ContentsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<Contents>, ErrorData> {
        log(&parts, "table_of_contents", p.under.as_deref().unwrap_or(""));
        let (doc, sections) = self.store.contents(p.doc, p.under).await.map_err(invalid)?;
        Ok(Json(Contents { doc, sections }))
    }

    #[tool(
        description = "Read PDF pages by number (up to 5 at a time), e.g. a page cited in a \
            search result or a table that spans sections.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_pages(
        &self,
        Parameters(p): Parameters<PagesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<Pages>, ErrorData> {
        log(&parts, "read_pages", &p.start.to_string());
        let end = p.end.unwrap_or(p.start);
        if end < p.start || end - p.start >= MAX_PAGES {
            return Err(ErrorData::invalid_params(
                format!("end must be between start and start + {}", MAX_PAGES - 1),
                None,
            ));
        }
        let (doc, pages) = self.store.pages(p.doc, p.start, end).await.map_err(invalid)?;
        Ok(Json(Pages { doc, pages }))
    }

    #[tool(
        description = "Ask a question about the documents; an agent searches and reads them \
            and answers with document, section and page citations, quoting the key text. \
            Takes 20-90 seconds. Best for questions that span several sections; for a single \
            definition, read_section is faster.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn ask_docs(
        &self,
        Parameters(p): Parameters<AskParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<Answer>, ErrorData> {
        log(&parts, "ask_docs", &p.question);
        let agent = self.agent()?;
        let request = match p.doc {
            Some(doc) => {
                let doc = self.store.check_doc(doc).await.map_err(invalid)?;
                format!("Answer from document {doc}.\n\n{}", p.question)
            }
            None => p.question,
        };
        self.answered(agent.run(request).await)
    }

    #[tool(
        description = "Summarize a document, or one section with its subsections, with \
            citations: an overview and the key points. Optionally say who it's for or what to \
            emphasize. Takes 20-90 seconds.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn summarize(
        &self,
        Parameters(p): Parameters<SummarizeParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<Answer>, ErrorData> {
        log(&parts, "summarize", p.section.as_deref().unwrap_or("(document)"));
        let agent = self.agent()?;
        // Resolve the document and section up front, so a typo fails fast
        // instead of sending the agent looking.
        let (doc, section) = match &p.section {
            Some(s) => {
                let found = self
                    .store
                    .section(p.doc.clone(), s.clone(), 0, 1)
                    .await
                    .map_err(invalid)?;
                (found.doc, Some(found.section.id))
            }
            None => (self.store.contents(p.doc.clone(), None).await.map_err(invalid)?.0, None),
        };
        let request = summarize_request(&doc, section.as_deref(), p.focus.as_deref());
        self.answered(agent.run(request).await)
    }
}

impl DocsServer {
    fn agent(&self) -> Result<&Agent, ErrorData> {
        self.agent.as_ref().ok_or_else(|| {
            ErrorData::invalid_request(
                "the agent isn't configured on this server (no Anthropic API key)",
                None,
            )
        })
    }

    fn answered(&self, result: anyhow::Result<Answer>) -> Result<Json<Answer>, ErrorData> {
        let answer = result.map_err(internal)?;
        tracing::info!(sources = ?answer.sources, model = %answer.model, "agent answered");
        Ok(Json(answer))
    }
}

#[tool_handler]
impl ServerHandler for DocsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(&self.name, env!("CARGO_PKG_VERSION")))
            .with_instructions(&self.instructions)
    }
}

fn log(parts: &Parts, tool: &str, arg: &str) {
    let source = parts
        .extensions
        .get::<Client>()
        .map_or("?", |c| c.source.as_str());
    let arg: String = arg.chars().take(200).collect();
    tracing::info!(%source, tool, arg, "call");
}

fn invalid(e: anyhow::Error) -> ErrorData {
    ErrorData::invalid_params(format!("{e:#}"), None)
}

fn internal(e: anyhow::Error) -> ErrorData {
    tracing::error!("{e:#}");
    ErrorData::internal_error(format!("{e:#}"), None)
}
