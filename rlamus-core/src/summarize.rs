use ollama_rs::{
    error::OllamaError,
    generation::chat::{ChatMessage, request::ChatMessageRequest},
};

use crate::ollama::OllamaRunner;

pub struct Summarize {
    runner: OllamaRunner,
}

impl Summarize {
    pub fn new(runner: OllamaRunner) -> Self {
        Self { runner }
    }

    pub async fn summarize(&self, doc: impl ToString) -> Result<String, OllamaError> {
        let req = ChatMessageRequest::new(
            self.runner.model.clone(),
            vec![
                ChatMessage::system(
                    "You are tasked with summarizing whatever document the user sends. Your response includes NO extra explanations, but few paragraphs on the most important topics.".into(),
                ),
                ChatMessage::user(doc.to_string()),
            ],
        );
        let res = self.runner.ollama.send_chat_messages(req).await?;
        Ok(res.message.content)
    }
}
