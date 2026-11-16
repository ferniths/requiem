//! Token and cost accounting.
//!
//! REQUIEM tracks input/output/total tokens per agent, per task, and
//! globally. Where the model reports real token counts (via the API's
//! `usage` field) we use those; otherwise we mark the value as an estimate
//! and never fabricate precision.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::agent::AgentId;
use crate::{Id, Timestamp};

/// Whether a token count is authoritative or an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CountKind {
    /// The model API reported this number.
    #[default]
    Reported,
    /// We estimated it (e.g. from character length) because the API did not
    /// expose a usage field.
    Estimated,
}

/// A single token-accounting sample (one model call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSample {
    /// Input tokens for the request.
    pub input: u32,
    /// Output tokens for the response.
    pub output: u32,
    /// Whether these are reported or estimated.
    pub kind: CountKind,
    /// When the sample was taken.
    pub at: Timestamp,
}

impl TokenSample {
    /// Total tokens for this sample.
    pub fn total(&self) -> u32 {
        self.input + self.output
    }
}

/// Accumulated accounting for a set of samples.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenTotals {
    /// Sum of input tokens.
    pub input: u64,
    /// Sum of output tokens.
    pub output: u64,
    /// Whether any sample was only an estimate. If true the totals are a
    /// mixture and the UI should flag them.
    pub has_estimates: bool,
}

impl TokenTotals {
    /// Fold a sample into these totals.
    pub fn add_sample(&mut self, s: &TokenSample) {
        self.input += u64::from(s.input);
        self.output += u64::from(s.output);
        if s.kind == CountKind::Estimated {
            self.has_estimates = true;
        }
    }

    /// Total tokens (input + output).
    pub fn total(&self) -> u64 {
        self.input + self.output
    }

    /// Add another totals object (e.g. merging per-agent into a per-task).
    pub fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.has_estimates = self.has_estimates || other.has_estimates;
    }
}

/// The full accounting ledger for a task session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenLedger {
    /// Per-agent totals.
    pub per_agent: HashMap<AgentId, TokenTotals>,
    /// Per-task totals.
    pub per_task: HashMap<Id, TokenTotals>,
    /// Global total across everything.
    pub global: TokenTotals,
}

impl TokenLedger {
    /// Record a sample against an agent and a task simultaneously.
    pub fn record(&mut self, agent: &AgentId, task: &Id, sample: TokenSample) {
        let agent_total = self.per_agent.entry(agent.clone()).or_default();
        let task_total = self.per_task.entry(task.clone()).or_default();
        agent_total.add_sample(&sample);
        task_total.add_sample(&sample);
        self.global.add_sample(&sample);
    }

    /// Total tokens for one agent.
    pub fn agent_total(&self, agent: &AgentId) -> &TokenTotals {
        static ZERO: TokenTotals = TokenTotals {
            input: 0,
            output: 0,
            has_estimates: false,
        };
        self.per_agent.get(agent).unwrap_or(&ZERO)
    }

    /// Total tokens for one task.
    pub fn task_total(&self, task: &Id) -> &TokenTotals {
        static ZERO: TokenTotals = TokenTotals {
            input: 0,
            output: 0,
            has_estimates: false,
        };
        self.per_task.get(task).unwrap_or(&ZERO)
    }
}
