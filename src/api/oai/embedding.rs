use std::time::Duration;

use axum::{extract::State, Json};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{
    api::request_info,
    middleware::{Array, GenerateRequest, ThreadRequest, ThreadState, Token, TokenCounter},
};

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct EmbeddingRequest {
    input: Array<String>,
    embed_layer: usize,
}

impl From<EmbeddingRequest> for GenerateRequest {
    fn from(value: EmbeddingRequest) -> Self {
        let EmbeddingRequest { input, embed_layer } = value;
        Self {
            prompt: Vec::from(input).join(""),
            max_tokens: 1,
            embed: true,
            embed_layer,
            ..Default::default()
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EmbeddingData {
    object: String,
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Debug, Serialize)]
pub struct EmbeddingResponse {
    object: String,
    model: String,
    data: Vec<EmbeddingData>,
    #[serde(rename = "usage")]
    counter: TokenCounter,
}

/// `/api/oai/embeddings`, `/api/oai/v1/embeddings`.
pub async fn embeddings(
    State(ThreadState(sender)): State<ThreadState>,
    Json(request): Json<EmbeddingRequest>,
) -> Json<EmbeddingResponse> {
    let info = request_info(sender.clone(), Duration::from_secs(1)).await;
    let model_name = info.reload.model_path.to_string_lossy().into_owned();

    let (token_sender, token_receiver) = flume::unbounded();
    let _ = sender.send(ThreadRequest::Generate {
        request: request.into(),
        tokenizer: info.tokenizer,
        sender: token_sender,
    });

    let mut token_counter = TokenCounter::default();
    let mut embedding = Vec::new();
    let mut stream = token_receiver.into_stream();

    while let Some(token) = stream.next().await {
        match token {
            Token::Stop(_, counter) => token_counter = counter,
            Token::Embed(emb) => {
                embedding = emb;
                break;
            }
            _ => {}
        }
    }

    Json(EmbeddingResponse {
        object: "list".into(),
        model: model_name,
        data: vec![EmbeddingData {
            object: "embedding".into(),
            index: 0,
            embedding,
        }],
        counter: token_counter,
    })
}