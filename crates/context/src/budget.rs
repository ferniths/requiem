//! Context budgeting: tracks how many tokens the model window can hold
//! and how much the current history has consumed.

use requiem_model::request::ChatMessage;

/// A budget tracker for the model's context window.
///
/// `max_tokens` is the hard limit from the model config. The tracker
/// estimates token usage by dividing the JSON byte length by 4 (a rough
/// but fast heuristic — 1 token ≈ 4 bytes for typical English/mixed code).
///
/// A safety margin (default 10%) is subtracted from the budget so that
/// the compression threshold triggers slightly before the true limit,
/// accounting for estimation error and provider-side padding.
pub struct ContextBudget {
    max_tokens: u32,
    /// Fraction of the budget reserved as headroom (0.0..1.0).
    safety_margin: f32,
}

impl ContextBudget {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            max_tokens,
            safety_margin: 0.10,
        }
    }

    /// Set a custom safety margin (0.0..1.0). Higher values trigger
    /// compression earlier.
    pub fn with_safety_margin(mut self, margin: f32) -> Self {
        self.safety_margin = margin.clamp(0.0, 0.50);
        self
    }

    /// The hard limit in tokens.
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// The effective budget after applying the safety margin.
    pub fn effective_limit(&self) -> u32 {
        (self.max_tokens as f32 * (1.0 - self.safety_margin)) as u32
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

    /// Whether the current history fits within the effective budget
    /// (hard limit minus safety margin).
    pub fn fits(&self, messages: &[ChatMessage]) -> bool {
        Self::estimate_tokens(messages) <= self.effective_limit()
    }

    /// How many tokens are over the effective budget (0 if under).
    pub fn over_by(&self, messages: &[ChatMessage]) -> u32 {
        let used = Self::estimate_tokens(messages);
        used.saturating_sub(self.effective_limit())
    }

    /// The fraction of the *hard* budget currently in use (0.0..=1.0+).
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

    #[test]
    fn safety_margin_reduces_effective_limit() {
        let budget = ContextBudget::new(1000);
        assert_eq!(budget.effective_limit(), 900, "10% margin on 1000");
        let tight = ContextBudget::new(1000).with_safety_margin(0.25);
        assert_eq!(tight.effective_limit(), 750, "25% margin on 1000");
    }

    #[test]
    fn fits_uses_effective_limit() {
        // With zero margin, effective limit == hard limit (100 tokens).
        let zero_margin = ContextBudget::new(100).with_safety_margin(0.0);
        // ~93 tokens estimated: fits within the 100-token hard limit.
        let msgs: Vec<ChatMessage> = (0..25).map(|i| msg(&format!("word {i:04}"))).collect();
        assert!(
            zero_margin.fits(&msgs),
            "93 tokens must fit in 100-token budget with zero margin"
        );
        // ~186 tokens: clearly over, must not fit even with zero margin.
        let big: Vec<ChatMessage> = (0..50).map(|i| msg(&format!("word {i:04}"))).collect();
        assert!(!zero_margin.fits(&big), "2x over must not fit");
        // Default 10% margin: effective limit = 90, so 93 does NOT fit.
        let default_budget = ContextBudget::new(100);
        assert!(
            !default_budget.fits(&msgs),
            "93 > 90 effective limit, must not fit"
        );
    }
}
