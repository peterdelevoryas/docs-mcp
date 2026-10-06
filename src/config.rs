//! What one deployment is about: its name, what its documents are, and any
//! guidance for the agent behind ask_docs and summarize. Read from the TOML
//! file named by DOCS_CONFIG; every field is optional. See examples/.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Short name, used as the MCP server name.
    pub name: String,
    /// One or two sentences on what the documents are and what they're
    /// good for. Goes into the MCP server instructions and the agent's
    /// system prompt.
    pub description: String,
    /// Extra instructions for the agent: what readers care about, the
    /// domain's terminology, how to cite.
    pub guidance: Option<String>,
    /// A note on how the documents may be used (e.g. a license that limits
    /// quoting), repeated to clients and the agent.
    pub usage_note: Option<String>,
    pub agent: AgentConfig,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub model: String,
    /// low, medium, high, xhigh or max.
    pub effort: String,
    /// Tool-use turns before the agent must answer.
    pub max_turns: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "docs".into(),
            description: "A collection of reference documentation.".into(),
            guidance: None,
            usage_note: None,
            agent: AgentConfig::default(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "claude-opus-5-5".into(),
            effort: "medium".into(),
            max_turns: 16,
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// MCP server instructions. Kept short: Claude Code truncates them at
    /// 2048 characters.
    pub fn instructions(&self) -> String {
        let mut s = format!(
            "{} search_docs finds sections by keyword (or by section number); \
read_section reads one, with its subsections; table_of_contents browses a \
document; read_pages reads PDF pages. ask_docs hands a whole question to an \
agent that researches the documents and answers with citations, and summarize \
has it summarize a document or section: use those for questions that span \
several sections, and the other tools for quick lookups. Cite document and \
section when you rely on the text.",
            self.description.trim()
        );
        if let Some(note) = &self.usage_note {
            s.push(' ');
            s.push_str(note.trim());
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_parse_and_fit() {
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "toml") {
                let c = Config::load(Some(&path)).unwrap();
                assert!(c.instructions().len() < 2048, "{}", path.display());
            }
        }
        assert_eq!(Config::load(None).unwrap().agent.max_turns, 16);
        assert!(toml::from_str::<Config>("nmae = 'typo'").is_err());
    }
}
