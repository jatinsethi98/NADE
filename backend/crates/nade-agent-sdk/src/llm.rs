//! The model-facing trait.

use async_trait::async_trait;
use std::sync::Arc;

use crate::error::Result;
use crate::message::{ChatRequest, ChatResponse};

/// One turn of conversation with a language model.
///
/// Implementations are adapters: translate [`ChatRequest`] into the provider's
/// wire format, translate the reply back into [`ChatResponse`]. They own
/// retries, rate limiting and authentication — the engine does none of that and
/// treats any [`Err`] as "the host should retry this job later".
///
/// Implementors must be `Send + Sync` so an [`Engine`](crate::Engine) can live
/// in an `Arc` behind a multi-threaded server.
#[async_trait]
pub trait Llm: Send + Sync {
    /// Ask the model for one turn.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
}

#[async_trait]
impl<T: Llm + ?Sized> Llm for Arc<T> {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        (**self).chat(req).await
    }
}

#[async_trait]
impl<T: Llm + ?Sized> Llm for Box<T> {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        (**self).chat(req).await
    }
}
