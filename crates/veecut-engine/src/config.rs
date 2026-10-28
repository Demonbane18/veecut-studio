//! Global settings in ~/.veecut/config.toml.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;

/// VeeCut's global folder: `VEECUT_HOME`, else `~/.veecut`.
pub fn home() -> PathBuf {
    match std::env::var_os("VEECUT_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".veecut"),
    }
}

/// Settings from `config.toml` in [`home`]. Missing keys take their defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub skills: SkillsConfig,
    pub agent: AgentConfig,
    pub transcribe: TranscribeConfig,
}

/// How the skills catalog stays current.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Re-sync the catalog when it is older than this. 0 turns automatic syncing off.
    pub auto_sync_hours: u32,
    /// After an automatic sync, update installed SAFE skills whose upstream moved.
    pub auto_update: bool,
    /// Refuse to install skills not graded SAFE unless forced.
    pub safe_only: bool,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        SkillsConfig { auto_sync_hours: 24, auto_update: true, safe_only: true }
    }
}

/// Which coding agent runs by default, and optional command overrides. An override is the full
/// command as a list; `{prompt}` is replaced with the prompt, which is appended when absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub default: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<Vec<String>>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig { default: "opencode".into(), opencode: None, claude: None, codex: None, gemini: None }
    }
}

impl AgentConfig {
    /// The user's command override for an agent, if any.
    pub fn command(&self, kind: AgentKind) -> Option<&[String]> {
        match kind {
            AgentKind::OpenCode => self.opencode.as_deref(),
            AgentKind::Claude => self.claude.as_deref(),
            AgentKind::Codex => self.codex.as_deref(),
            AgentKind::Gemini => self.gemini.as_deref(),
        }
    }
}

/// Transcription defaults. `provider` is `auto`, `groq` or `local`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscribeConfig {
    pub provider: String,
    pub local_model: String,
}

impl Default for TranscribeConfig {
    fn default() -> Self {
        TranscribeConfig { provider: "auto".into(), local_model: "large-v3-turbo".into() }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        home().join("config.toml")
    }

    /// Reads `config.toml`; defaults when the file does not exist.
    pub fn load() -> Result<Config> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("{} is not valid; fix the line named above or delete the file to reset it", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(home()).with_context(|| format!("creating {}", home().display()))?;
        std::fs::write(&path, toml::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    /// The default agent, OpenCode when the name is not one VeeCut knows.
    pub fn default_agent(&self) -> AgentKind {
        AgentKind::from_name(&self.agent.default).unwrap_or(AgentKind::OpenCode)
    }
}

/// Sets variables from `<root>/.env` that are not already set. Missing file is fine; lines that
/// do not parse are skipped. Never prints values.
pub fn load_dotenv(root: &Path) {
    let Ok(text) = std::fs::read_to_string(root.join(".env")) else { return };
    for (key, value) in parse_dotenv(&text) {
        if std::env::var_os(&key).is_none() {
            // SAFETY: called at startup before VeeCut starts threads that read the environment.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

/// `KEY=value` pairs from dotenv text: `#` comments, blank lines, an optional `export ` prefix,
/// single or double quotes (double quotes understand `\n`, `\"` and `\\`), and trailing
/// ` # comments` after unquoted values.
fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, raw)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let raw = raw.trim();
        let value = if let Some(rest) = raw.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = rest.chars();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('n') => value.push('\n'),
                        Some(other) => value.push(other),
                        None => break,
                    },
                    c => value.push(c),
                }
            }
            if !closed {
                continue;
            }
            value
        } else if let Some(rest) = raw.strip_prefix('\'') {
            let Some(end) = rest.find('\'') else { continue };
            rest[..end].to_string()
        } else {
            raw.split(" #").next().unwrap_or("").trim_end().to_string()
        };
        out.push((key.to_string(), value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_handles_quotes_comments_and_export() {
        let text = r#"
# a comment
GROQ_API_KEY=gsk_plain # trailing note
export GITHUB_TOKEN="ghp_x\"y\nz"
SINGLE='raw $value # kept' # note
EMPTY=
URL=https://example.com/#anchor
not a pair
BAD KEY=1
UNCLOSED="oops
"#;
        let pairs = parse_dotenv(text);
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("GROQ_API_KEY"), Some("gsk_plain"));
        assert_eq!(get("GITHUB_TOKEN"), Some("ghp_x\"y\nz"));
        assert_eq!(get("SINGLE"), Some("raw $value # kept"));
        assert_eq!(get("EMPTY"), Some(""));
        assert_eq!(get("URL"), Some("https://example.com/#anchor"));
        assert_eq!(get("BAD KEY"), None);
        assert_eq!(get("UNCLOSED"), None);
        assert_eq!(pairs.len(), 5);
    }
}
