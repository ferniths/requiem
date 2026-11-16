//! Context compression: shrink old tool results and deduplicate repeated
//! content so the budget fits without losing the most recent context.

use crate::budget::ContextBudget;
use crate::ContextStrategy;
use requiem_model::request::ChatMessage;

/// The default context strategy:
///
/// 1. If the history fits the budget, return it unchanged.
/// 2. Otherwise, collapse the oldest tool-result messages to a short
///    placeholder (`"[compressed tool result]"`) until the budget fits.
/// 3. Deduplicate consecutive identical messages.
pub struct Compressor;

impl Default for Compressor {
    fn default() -> Self {
        Self
    }
}

impl ContextStrategy for Compressor {
    fn apply(&self, history: Vec<ChatMessage>, budget: &ContextBudget) -> crate::ManagedContext {
        let original_tokens = ContextBudget::estimate_tokens(&history);

        // Fast path: already fits.
        if budget.fits(&history) {
            let deduped = dedup_consecutive(&history);
            let freed = original_tokens.saturating_sub(ContextBudget::estimate_tokens(&deduped));
            return crate::ManagedContext {
                messages: deduped,
                compressed: freed > 0,
                tokens_freed: freed,
            };
        }

        // Compress from the oldest tool results.
        let mut messages = history;
        let mut compressed = false;
        // Compress tool results from oldest first until the budget fits
        // or we run out of uncompressed tool results.
        let tool_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == requiem_model::request::Role::Tool)
            .map(|(i, _)| i)
            .collect();

        for idx in tool_indices {
            if budget.fits(&messages) {
                break;
            }
            messages[idx] = ChatMessage {
                role: requiem_model::request::Role::Tool,
                content: Some(serde_json::json!("[compressed tool result]")),
                tool_call_id: messages[idx].tool_call_id.clone(),
                tool_calls: Vec::new(),
            };
            compressed = true;
        }

        let deduped = dedup_consecutive(&messages);
        let final_tokens = ContextBudget::estimate_tokens(&deduped);
        let freed = original_tokens.saturating_sub(final_tokens);

        crate::ManagedContext {
            messages: deduped,
            compressed,
            tokens_freed: freed,
        }
    }
}

/// Remove consecutive messages that are identical (same role + content).
fn dedup_consecutive(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for m in messages {
        if out.last().is_some() && out.last().unwrap().content == m.content {
            // Skip the duplicate.
            continue;
        }
        out.push(m.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_result(id: &str, text: &str) -> ChatMessage {
        ChatMessage::tool_result(id, text)
    }

    #[test]
    fn small_history_passes_through() {
        let budget = ContextBudget::new(100_000);
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
        ];
        let result = Compressor.apply(history, &budget);
        assert!(!result.compressed);
        assert_eq!(result.messages.len(), 3);
    }

    #[test]
    fn over_budget_compresses_tool_results() {
        let budget = ContextBudget::new(20);
        let history: Vec<ChatMessage> = (0..50)
            .map(|i| tool_result(&format!("t{i}"), &format!("result {i}")))
            .collect();
        let result = Compressor.apply(history, &budget);
        assert!(result.compressed);
        assert!(result.tokens_freed > 0);
        assert!(budget.fits(&result.messages));
    }

    #[test]
    fn dedup_consecutive_identical() {
        let a = ChatMessage::assistant("same");
        let b = ChatMessage::assistant("same");
        let c = ChatMessage::user("different");
        let deduped = dedup_consecutive(&[a, b, c]);
        assert_eq!(deduped.len(), 2);
    }
}
