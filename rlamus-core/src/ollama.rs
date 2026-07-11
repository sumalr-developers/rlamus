use std::ffi::OsStr;

use ollama_rs::Ollama;
use url::Url;

#[derive(Debug, Clone)]
pub struct OllamaRunner {
    pub ollama: Ollama,
    pub model: String,
}

impl Default for OllamaRunner {
    fn default() -> Self {
        let url = std::env::var("OLLAMA_ENDPOINT")
            .map(|s| Url::parse(s.as_str()).expect("invalid OLLAMA_ENDPOINT environment variable"))
            .unwrap_or(Url::parse("http://localhost:11434").unwrap());
        Self {
            ollama: Ollama::from_url(url),
            model: String::default(),
        }
        .with_model_from_env_or("RLAMUS_MODEL", "gemma4:12b")
    }
}

impl OllamaRunner {
    pub fn with_model_from_env_or(
        mut self,
        key: impl AsRef<OsStr>,
        default: impl Into<String>,
    ) -> Self {
        let model = std::env::var(key).unwrap_or(default.into());
        self.model = model;
        self
    }
}
