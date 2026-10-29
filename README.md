# docs-mcp

An MCP server for searching, reading, asking about and summarizing reference
documentation: hardware specifications, datasheets, manuals, design docs,
project documentation. It ingests PDF, Markdown, plain text,
reStructuredText and HTML into a section tree with full-text search. It
serves that to agents over MCP, along with an agent of its own that answers
questions and writes summaries with citations.

It was built so a code-review agent
([sashiko](https://github.com/sashiko-dev/sashiko), reviewing Linux PCI
drivers) could check code against the exact text of a specification it
can't vendor. Nothing in it is specific to that use, though. Each deployment
describes its own documents in a small config file (see
[`examples/`](examples)).

The repository holds only code. Documents and the database built from them
stay on the server (`.gitignore` excludes `*.pdf`, `/docs` and `*.db`).

## Tools

| Tool | What it does |
| --- | --- |
| `list_documents` | The documents available: id, title, description, format, size. |
| `search_docs` | Stemmed full-text search over section numbers, titles and text, in one document or all, with snippets. A bare number such as `4.2.1` returns that section. |
| `read_section` | A section by id (`4.2.1`, or `#12` for an unnumbered heading) or by title: its text, its parent and its subsections. Long sections are paged with `offset`. PDF text keeps its table layout and marks page starts with `[page N]`. |
| `table_of_contents` | A document's top-level sections, or the subsections of one section. |
| `read_pages` | Up to 5 pages of a PDF. |
| `ask_docs` | Hands a question to a Claude agent that searches and reads the documents with the tools above. It answers with document, section and page citations. |
| `summarize` | The same agent summarizes a document, or a section with its subsections: an overview, then cited key points. An optional `focus` says who the summary is for. |

Tools that take a `doc` id let you omit it when the server holds only one
document.

## How it works

- **Ingest:** `docs-mcp ingest <file> <id> <title> [description]` splits a document into sections.
  - **PDF:** text comes from `pdftotext -layout` (poppler) and the section tree from the PDF's bookmarks (`mutool show outline`). Each section starts at its printed heading. Running headers and footers are removed. A PDF without bookmarks becomes one section per page.
  - **Markdown, text and reStructuredText:** sections come from `#` headings and underlined (Setext/reST) headings. Code fences are skipped.
  - **HTML:** the main content (`<main>`, `role="main"` or `<article>`) is converted to Markdown, then split the same way.

  Sections, pages and an FTS5 index (porter stemming) are stored in SQLite.
- **Serve:** `docs-mcp serve` serves streamable-HTTP MCP at `/mcp`, built on rmcp with no sessions, and an unauthenticated `/health` for uptime monitors.
- **Auth:** every client has its own bearer token. The server stores only SHA-256 hashes and re-reads them on `systemctl reload docs-mcp`.
- **Agent:** `ask_docs` and `summarize` call the Messages API directly in a manual tool-use loop: `claude-opus-5-5` by default, adaptive thinking, prompt caching, and server-side refusal fallbacks. They are enabled only when `ANTHROPIC_API_KEY` is set.

## Configuration

A deployment's config file is TOML, named by `DOCS_CONFIG`. Every field is
optional.

```toml
name = "widgets"                   # MCP server name
description = "The Widget Co. hardware manuals, for driver authors."
guidance = "Readers write Linux drivers; quote register tables exactly."
usage_note = "Licensed to Widget Co. partners; quote only what the task needs."

[agent]
model = "claude-opus-5-5"
effort = "medium"                  # low, medium, high, xhigh, max
max_turns = 16
```

`description` and `usage_note` go into the MCP server instructions.
`description`, `guidance` and `usage_note` all go into the agent's system
prompt.

## Deploy

The scripts build and run it on a VM (Ubuntu 24.04, built from
`deploy/cloud-init.yaml`) on a private network, behind an edge server whose
Caddy terminates HTTPS and proxies to it. Copy `deploy/config.example` to
`deploy/config` and set `DOCS_DOMAIN`, `DOCS_PRIVATE_IP` and `DOCS_INSTANCE`,
the path to the deployment's config.

```sh
deploy/deploy.sh                          # build on the VM and (re)start
deploy/upload.sh ~/Downloads/manual.pdf widget-2.0 'Widget Manual 2.0'
deploy/token.sh claude-code               # mint a client token into the keychain
deploy/api-key.sh                         # install the Anthropic key for the agent
```

Claude Code:

```sh
claude mcp add --transport http widgets https://docs.example.com/mcp \
    -H "Authorization: Bearer $(security find-generic-password -s widgets-mcp-token -a claude-code -w)"
```

## Licensing of what you serve

Many specifications are licensed to members or customers. Give tokens only
to clients that your license covers, and set `usage_note` so agents don't
paste more of the text than they need into places others can read.
