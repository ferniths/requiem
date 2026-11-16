//! Verification loops for completed tasks.
//!
//! A `Verifier` re-checks a completed task before the scheduler marks
//! it `Completed`. If the check fails, the task's `verification_attempts`
//! is incremented and it is returned to `Available` for re-execution,
//! up to the scheduler's `max_verification_attempts` budget.

use requiem_core::error::RequiemError;
use requiem_core::task::Task;
use serde::{Deserialize, Serialize};

/// The outcome of a verification pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerifyResult {
    /// The task is correct; safe to mark `Completed`.
    Pass,
    /// The task is wrong; should be re-executed.
    Fail(String),
}

/// A verifier that can be attached to a task type.
#[async_trait::async_trait]
pub trait Verifier: Send + Sync {
    /// Run the verification check.
    async fn verify(
        &self,
        task: &Task,
        result: &serde_json::Value,
    ) -> Result<VerifyResult, RequiemError>;
}

/// A no-op verifier that always passes.
pub struct NoopVerifier;

#[async_trait::async_trait]
impl Verifier for NoopVerifier {
    async fn verify(
        &self,
        _task: &Task,
        _result: &serde_json::Value,
    ) -> Result<VerifyResult, RequiemError> {
        Ok(VerifyResult::Pass)
    }
}
