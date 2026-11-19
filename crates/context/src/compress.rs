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

/// Remove consecutive messages that are identical (same role, content,
/// tool_call_id, and tool-call structure). Two messages with the same
/// content but different roles are NOT duplicates.
fn dedup_consecutive(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for m in messages {
        if out.last().is_some()
            && out.last().unwrap().content == m.content
            && out.last().unwrap().role == m.role
            && out.last().unwrap().tool_call_id == m.tool_call_id
            && out.last().unwrap().tool_calls.len() == m.tool_calls.len()
        {
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
        // 10 messages with long content; budget of 100 tokens can't fit
        // them all, but compression to short placeholders helps.
        let budget = ContextBudget::new(100);
        let history: Vec<ChatMessage> = (0..10)
            .map(|i| tool_result(&format!("t{i}"), &format!("x{}", "y".repeat(500))))
            .collect();
        assert!(
            !budget.fits(&history),
            "precondition: history must be over budget"
        );
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

    #[test]
    fn dedup_same_content_different_roles_not_merged() {
        let a = ChatMessage::assistant("same content");
        let b = ChatMessage::user("same content");
        let deduped = dedup_consecutive(&[a, b]);
        assert_eq!(
            deduped.len(),
            2,
            "same content, different role must not dedup"
        );
    }

    #[test]
    fn dedup_same_content_different_tool_call_id_not_merged() {
        let a = ChatMessage {
            role: requiem_model::request::Role::Tool,
            content: Some(serde_json::json!("result")),
            tool_call_id: Some("t1".into()),
            tool_calls: Vec::new(),
        };
        let b = ChatMessage {
            role: requiem_model::request::Role::Tool,
            content: Some(serde_json::json!("result")),
            tool_call_id: Some("t2".into()),
            tool_calls: Vec::new(),
        };
        let deduped = dedup_consecutive(&[a, b]);
        assert_eq!(
            deduped.len(),
            2,
            "same content, different tool_call_id must not dedup"
        );
    }
}
