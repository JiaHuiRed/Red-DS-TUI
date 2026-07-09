//! Exit Plan Mode tool - allows AI to自主退出计划模式

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use crate::tools::plan::SharedPlanState;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tui::app::AppMode;

/// Shared reference to the app mode for mode switching
pub type SharedAppMode = Arc<Mutex<AppMode>>;

/// Tool for exiting Plan mode
pub struct ExitPlanModeTool {
    app_mode: SharedAppMode,
    plan_state: SharedPlanState,
}

impl ExitPlanModeTool {
    pub fn new(app_mode: SharedAppMode, plan_state: SharedPlanState) -> Self {
        Self {
            app_mode,
            plan_state,
        }
    }
}

#[async_trait]
impl ToolSpec for ExitPlanModeTool {
    fn name(&self) -> &'static str {
        "exit_plan_mode"
    }

    fn description(&self) -> &'static str {
        "Exit Plan mode and return to Agent mode. Use this when you have completed your investigation and plan, and are ready for the user to review and approve implementation. The current plan will be preserved for the user to review."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Optional summary of the plan and findings"
                }
            },
            "required": []
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![] // No special capabilities needed
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto // Auto-approve, no user confirmation needed
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let summary = input
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Plan completed");

        let mut mode = self.app_mode.lock().await;
        let current_mode = *mode;

        if current_mode != AppMode::Plan {
            return Ok(ToolResult::success(
                "Not currently in Plan mode. Use enter_plan_mode first.".to_string(),
            ));
        }

        // Check if plan is empty
        let plan_snapshot = {
            let plan = self.plan_state.lock().await;
            plan.snapshot()
        };

        if plan_snapshot.items.is_empty() {
            return Ok(ToolResult::error(
                "Cannot exit Plan mode with an empty plan. Use update_plan to create a plan first.".to_string(),
            ));
        }

        // Switch back to Agent mode
        *mode = AppMode::Agent;

        let (pending, in_progress, completed) = {
            let plan = self.plan_state.lock().await;
            plan.counts()
        };

        Ok(ToolResult::success(format!(
            "Exited Plan mode and returned to Agent mode.\n\n\
             Summary: {summary}\n\n\
             Plan status:\n\
             - Pending: {pending}\n\
             - In progress: {in_progress}\n\
             - Completed: {completed}\n\n\
             The plan is ready for your review. You can now proceed with implementation."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::plan::PlanState;

    #[tokio::test]
    async fn test_exit_plan_mode() {
        let app_mode = Arc::new(Mutex::new(AppMode::Plan));
        let plan_state = Arc::new(Mutex::new(PlanState::default()));
        let tool = ExitPlanModeTool::new(app_mode.clone(), plan_state.clone());

        // Add a plan item first
        {
            let mut plan = plan_state.lock().await;
            plan.update(crate::tools::plan::UpdatePlanArgs {
                explanation: None,
                plan: vec![crate::tools::plan::PlanItemArg {
                    step: "Test step".to_string(),
                    status: crate::tools::plan::StepStatus::Completed,
                }],
            });
        }

        let result = tool
            .execute(json!({}), &ToolContext::new("."))
            .await
            .unwrap();

        assert!(result.content.contains("Exited Plan mode"));
        assert_eq!(*app_mode.lock().await, AppMode::Agent);
    }

    #[tokio::test]
    async fn test_exit_plan_mode_not_in_plan() {
        let app_mode = Arc::new(Mutex::new(AppMode::Agent));
        let plan_state = Arc::new(Mutex::new(PlanState::default()));
        let tool = ExitPlanModeTool::new(app_mode.clone(), plan_state);

        let result = tool
            .execute(json!({}), &ToolContext::new("."))
            .await
            .unwrap();

        assert!(result.content.contains("Not currently in Plan mode"));
    }

    #[tokio::test]
    async fn test_exit_plan_mode_empty_plan() {
        let app_mode = Arc::new(Mutex::new(AppMode::Plan));
        let plan_state = Arc::new(Mutex::new(PlanState::default()));
        let tool = ExitPlanModeTool::new(app_mode.clone(), plan_state);

        let result = tool
            .execute(json!({}), &ToolContext::new("."))
            .await
            .unwrap();

        assert!(result.content.contains("empty plan"));
        assert_eq!(*app_mode.lock().await, AppMode::Plan); // Should still be in Plan mode
    }
}
