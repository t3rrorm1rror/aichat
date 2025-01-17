use super::message::Message;

use crate::utils::count_tokens;

use anyhow::{bail, Result};

pub type TokensCountFactors = (usize, usize); // (per-messages, bias)

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub client: String,
    pub name: String,
    pub index: usize,
    pub max_tokens: Option<usize>,
    pub tokens_count_factors: TokensCountFactors,
}

impl Default for ModelInfo {
    fn default() -> Self {
        ModelInfo::new(0, "", "")
    }
}

impl ModelInfo {
    pub fn new(index: usize, client: &str, name: &str) -> Self {
        Self {
            index,
            client: client.into(),
            name: name.into(),
            max_tokens: None,
            tokens_count_factors: Default::default(),
        }
    }

    pub fn set_max_tokens(mut self, max_tokens: Option<usize>) -> Self {
        match max_tokens {
            None | Some(0) => self.max_tokens = None,
            _ => self.max_tokens = max_tokens,
        }
        self
    }

    pub fn set_tokens_count_factors(mut self, tokens_count_factors: TokensCountFactors) -> Self {
        self.tokens_count_factors = tokens_count_factors;
        self
    }

    pub fn id(&self) -> String {
        format!("{}:{}", self.client, self.name)
    }

    pub fn messages_tokens(&self, messages: &[Message]) -> usize {
        messages.iter().map(|v| count_tokens(&v.content)).sum()
    }

    pub fn total_tokens(&self, messages: &[Message]) -> usize {
        if messages.is_empty() {
            return 0;
        }
        let num_messages = messages.len();
        let message_tokens = self.messages_tokens(messages);
        let (per_messages, _) = self.tokens_count_factors;
        if messages[num_messages - 1].role.is_user() {
            num_messages * per_messages + message_tokens
        } else {
            (num_messages - 1) * per_messages + message_tokens
        }
    }

    pub fn max_tokens_limit(&self, messages: &[Message]) -> Result<()> {
        let (_, bias) = self.tokens_count_factors;
        let total_tokens = self.total_tokens(messages) + bias;
        if let Some(max_tokens) = self.max_tokens {
            if total_tokens >= max_tokens {
                bail!("Exceed max tokens limit")
            }
        }
        Ok(())
    }
}
