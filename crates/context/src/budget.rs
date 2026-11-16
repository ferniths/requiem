//! Context budgeting: tracks how many tokens the model window can hold
//! and how much the current history has consumed.

use requiem_model::request::ChatMessage;

/// A budget tracker for the model's context window.
///
/// `max_tokens` is the hard limit from the model config. The tracker
/// estimates token usage by dividing the JSON byte length by 4 (a rough
/// but fast heuristic — 1 token ≈ 4 bytes for typical English/mixed code).
pub struct ContextBudget {
    max_tokens: u32,
}

impl ContextBudget {
    pub fn new(max_tokens: u32) -> Self {
        Self { max_tokens }
    }

    /// The hard limit in tokens.
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// Estimate how many tokens a message set will consume.
    pub fn estimate_tokens(messages: &[ChatMessage]) -> u32 {
        let total_bytes: usize = messages
            .iter()
            .map(|m| {
                let role_str: String = format!("{}", m.role);
                let mut s = role_str.len();
                if let Some(c) = &m.content {
                    s += c.to_string().len();
                }
                s += m.tool_calls.len() * 16; // rough per-tool-call overhead
                s
            })
            .sum();
        // 4 bytes per token heuristic.
        (total_bytes as u32) / 4
    }

    /// Whether the current history fits within the budget.
    pub fn fits(&self, messages: &[ChatMessage]) -> bool {
        Self::estimate_tokens(messages) <= self.max_tokens
    }

    /// How many tokens are over budget (0 if under).
    pub fn over_by(&self, messages: &[ChatMessage]) -> u32 {
        let used = Self::estimate_tokens(messages);
        used.saturating_sub(self.max_tokens)
    }

    /// The fraction of the budget currently in use (0.0..=1.0+).
    pub fn usage_fraction(&self, messages: &[ChatMessage]) -> f32 {
        let used = Self::estimate_tokens(messages);
        if self.max_tokens == 0 {
            0.0
        } else {
            (used as f32) / (self.max_tokens as f32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> ChatMessage {
        ChatMessage::user(text)
    }

    #[test]
    fn empty_history_fits_any_budget() {
        let budget = ContextBudget::new(4096);
        assert!(budget.fits(&[]));
        assert_eq!(budget.over_by(&[]), 0);
    }

    #[test]
    fn large_history_exceeds_budget() {
        let budget = ContextBudget::new(10);
        let big: Vec<ChatMessage> = (0..100).map(|i| msg(&format!("token {i:04}"))).collect();
        assert!(!budget.fits(&big));
        assert!(budget.over_by(&big) > 0);
    }

    #[test]
    fn usage_fraction_monotonic() {
        let budget = ContextBudget::new(1000);
        let a = vec![msg("hello")];
        let b: Vec<ChatMessage> = (0..50).map(|i| msg(&format!("word {i}"))).collect();
        let fa = budget.usage_fraction(&a);
        let fb = budget.usage_fraction(&b);
        assert!(fb > fa);
    }
}
