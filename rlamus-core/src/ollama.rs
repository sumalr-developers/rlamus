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
        let model = std::env::var("RLAMUS_MODEL").unwrap_or("gemma4:12b".into());
        Self {
            ollama: Ollama::from_url(url),
            model,
        }
    }
}
