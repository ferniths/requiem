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
            let original = messages[idx]
                .content
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let summary = summarize_tool_result(&original);
            messages[idx] = ChatMessage {
                role: requiem_model::request::Role::Tool,
                content: Some(serde_json::Value::String(format!(
                    "[compressed tool result] {summary}"
                ))),
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

/// Summarize a tool result for compression, preserving the most useful
/// information: errors, file paths, command output, and key metadata.
///
/// The output is capped at ~200 chars to keep the compressed message
/// small while retaining enough signal for the model to reason about
/// what the tool returned.
fn summarize_tool_result(text: &str) -> String {
    const MAX_SUMMARY: usize = 200;

    // If short enough, keep the whole thing.
    if text.len() <= MAX_SUMMARY {
        return text.to_string();
    }

    // Preserve error lines (they're the most useful for debugging).
    let error_lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            let lower = l.to_lowercase();
            lower.contains("error")
                || lower.contains("failed")
                || lower.contains("exception")
                || lower.contains("panic")
                || lower.starts_with("tool error")
        })
        .take(3)
        .collect();

    // Preserve file paths (useful for the model to track what was touched).
    let mut file_paths: Vec<&str> = Vec::new();
    for line in text.lines().take(50) {
        if line.contains(".rs")
            || line.contains(".py")
            || line.contains(".ts")
            || line.contains(".js")
            || line.contains(".go")
            || line.contains(".c")
            || line.contains(".cpp")
            || line.contains(".toml")
            || line.contains(".json")
            || line.contains(".yaml")
            || line.contains(".md")
            || line.contains(".sln")
            || line.contains(".cs")
        {
            let trimmed = line.trim();
            if trimmed.len() <= 80 && !file_paths.iter().any(|p| p.contains(trimmed)) {
                file_paths.push(trimmed);
                if file_paths.len() >= 3 {
                    break;
                }
            }
        }
    }

    // Build a compact summary.
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "{} lines, {} bytes",
        text.lines().count(),
        text.len()
    ));
    if !error_lines.is_empty() {
        let errs: Vec<String> = error_lines
            .iter()
            .map(|l| l.trim().chars().take(80).collect())
            .collect();
        parts.push(format!("errors: {}", errs.join("; ")));
    }
    if !file_paths.is_empty() {
        parts.push(format!("files: {}", file_paths.join(", ")));
    }
    // Preserve the first line (often a status or heading).
    if let Some(first) = text.lines().next() {
        let first_trimmed: String = first.trim().chars().take(60).collect();
        if first_trimmed.len() > 10 {
            parts.push(format!("first: {first_trimmed}"));
        }
    }

    let mut summary = parts.join(" | ");
    if summary.len() > MAX_SUMMARY {
        summary.truncate(MAX_SUMMARY);
    }
    summary
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
        // them all, but compression to short summaries helps.
        let budget = ContextBudget::new(100);
        let history: Vec<ChatMessage> = (0..10)
            .map(|i| tool_result(&format!("t{i}"), &format!("x{}", "y".repeat(500))))
            .collect();
        assert!(
            !budget.fits(&history),
            "precondition: history must be over budget"
        );
        let original_tokens = ContextBudget::estimate_tokens(&history);
        let result = Compressor.apply(history, &budget);
        assert!(result.compressed);
        assert!(result.tokens_freed > 0);
        // The compressed history must be smaller than the original.
        assert!(
            ContextBudget::estimate_tokens(&result.messages) < original_tokens,
            "compression must reduce estimated tokens"
        );
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

    #[test]
    fn summarize_preserves_error_lines() {
        let text = "line1 ok\nline2 ok\nerror: something broke in foo.rs\nline4 ok";
        let summary = summarize_tool_result(text);
        assert!(
            summary.contains("error: something broke in foo.rs"),
            "must keep error line, got: {summary}"
        );
    }

    #[test]
    fn summarize_preserves_file_paths() {
        let text = "compiling src/main.rs\ncompiling src/lib.rs\ncompiling src/utils.rs\nall good";
        let summary = summarize_tool_result(text);
        assert!(
            summary.contains("main.rs"),
            "must keep file paths, got: {summary}"
        );
    }

    #[test]
    fn summarize_keeps_short_content_whole() {
        let text = "ok: 42 tests passed";
        assert_eq!(summarize_tool_result(text), text);
    }

    #[test]
    fn summarize_caps_at_max_summary() {
        let text = "error: line\n".repeat(200);
        let summary = summarize_tool_result(&text);
        assert!(
            summary.len() <= 200,
            "summary too long: {} chars",
            summary.len()
        );
    }

    #[test]
    fn compressed_tool_result_contains_summary() {
        let budget = ContextBudget::new(50);
        let long_result = "error: failed to build\n".repeat(50);
        let history = vec![ChatMessage::user("hi"), tool_result("t1", &long_result)];
        assert!(!budget.fits(&history));
        let result = Compressor.apply(history, &budget);
        let compressed = &result.messages[1];
        let text = compressed
            .content
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            text.contains("[compressed tool result]"),
            "must have placeholder prefix"
        );
        assert!(text.contains("failed to build"), "must preserve error text");
    }
}
