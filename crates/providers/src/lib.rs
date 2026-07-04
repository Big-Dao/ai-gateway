//! Provider adapters for the gateway. Request/response and streaming types
//! intentionally mirror each upstream's wire format, so several deserialized
//! fields are reserved for completeness but not yet consumed. Allow that here
//! rather than annotating every wire-format field individually.
#![allow(dead_code)]

pub mod anthropic;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;
pub use openai_compat::{FieldOverrides, OpenAICompatProvider};
