use ollama_rs::{
    error::OllamaError,
    generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest},
};

use crate::ollama::OllamaRunner;

pub struct Embeddings {
    runner: OllamaRunner,
}

pub struct Response {
    pub embeddings: Vec<Vec<f32>>,
    pub model_name: String,
}

impl Embeddings {
    pub fn new(runner: OllamaRunner) -> Self {
        Self { runner }
    }

    pub async fn get_embeddings(
        &self,
        documents: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Response, OllamaError> {
        let inputs = EmbeddingsInput::Multiple(documents.into_iter().map(|it| it.into()).collect());
        let req = GenerateEmbeddingsRequest::new(self.runner.model.clone(), inputs);
        Ok(Response {
            embeddings: self
                .runner
                .ollama
                .generate_embeddings(req)
                .await?
                .embeddings,
            model_name: self.runner.model.clone(),
        })
    }
}
