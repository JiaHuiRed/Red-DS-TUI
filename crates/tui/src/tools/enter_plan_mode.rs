//! Enter Plan Mode tool - allows AI to自主进入计划模式

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tui::app::AppMode;

/// Shared reference to the app mode for mode switching
pub type SharedAppMode = Arc<Mutex<AppMode>>;

/// Tool for entering Plan mode
pub struct EnterPlanModeTool {
    app_mode: SharedAppMode,
}

impl EnterPlanModeTool {
    pub fn new(app_mode: SharedAppMode) -> Self {
        Self { app_mode }
    }
}

#[async_trait]
impl ToolSpec for EnterPlanModeTool {
    fn name(&self) -> &'static str {
        "enter_plan_mode"
    }

    fn description(&self) -> &'static str {
        "Enter Plan mode to design and investigate before implementing. In Plan mode, you can read files, search code, and build a thorough plan using update_plan, but all write operations and shell execution are blocked. The user will review your plan and decide whether to proceed with implementation."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Optional reason for entering plan mode (e.g., 'Need to understand the codebase before making changes')"
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
        let reason = input
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Investigating and planning");

        let mut mode = self.app_mode.lock().await;
        let current_mode = *mode;

        if current_mode == AppMode::Plan {
            return Ok(ToolResult::success(
                "Already in Plan mode. Use update_plan to design your approach.".to_string(),
            ));
        }

        // Switch to Plan mode
        *mode = AppMode::Plan;

        Ok(ToolResult::success(format!(
            "Entered Plan mode.\n\
             Reason: {reason}\n\n\
             In Plan mode you can:\n\
             - Read files and search code\n\
             - Use update_plan to design your approach\n\
             - Use checklist_write for granular progress tracking\n\
             - Spawn read-only sub-agents for parallel investigation\n\n\
             All write operations and shell execution are blocked.\n\
             When your plan is ready, the user will review it and decide whether to proceed."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_enter_plan_mode() {
        let app_mode = Arc::new(Mutex::new(AppMode::Agent));
        let tool = EnterPlanModeTool::new(app_mode.clone());

        let result = tool
            .execute(json!({"reason": "Testing"}), &ToolContext::new("."))
            .await
            .unwrap();

        assert!(result.content.contains("Entered Plan mode"));
        assert_eq!(*app_mode.lock().await, AppMode::Plan);
    }

    #[tokio::test]
    async fn test_enter_plan_mode_already_in_plan() {
        let app_mode = Arc::new(Mutex::new(AppMode::Plan));
        let tool = EnterPlanModeTool::new(app_mode.clone());

        let result = tool
            .execute(json!({}), &ToolContext::new("."))
            .await
            .unwrap();

        assert!(result.content.contains("Already in Plan mode"));
    }
}
