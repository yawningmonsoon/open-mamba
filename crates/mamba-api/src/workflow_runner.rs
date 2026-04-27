//! Workflow advancer.
//!
//! Polls DuckDB for running workflow runs whose `current_task_id` has
//! status='done' (or 'failed'), then either:
//!   - dispatches the next step, injecting the previous output as
//!     `__previous` in the payload JSON, OR
//!   - marks the run completed if no more steps remain, OR
//!   - marks the run failed if the task failed.
//!
//! Symmetric with the queue worker: simple poll loop, no in-process
//! channels. DuckDB is the source of truth.

use mamba_bus::Bus;
use mamba_lake::{Lake, WorkflowStep};
use mamba_types::envelope::{TaskEnvelope, TaskSource};
use std::time::Duration;
use tracing::{info, warn};

const TICK_INTERVAL: Duration = Duration::from_secs(2);

pub fn spawn(lake: Lake, bus: Bus) {
    tokio::spawn(async move {
        info!(
            tick_secs = TICK_INTERVAL.as_secs(),
            "workflow advancer started"
        );
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            if let Err(e) = tick(&lake, &bus).await {
                warn!("workflow tick failed: {e}");
            }
        }
    });
}

async fn tick(lake: &Lake, bus: &Bus) -> anyhow::Result<()> {
    // Look at every running run; find ones whose current task has
    // settled. List is small (workflow runs are not high-cardinality).
    let runs = lake.list_runs(None, 100)?;
    for run in runs {
        if run.status != "running" {
            continue;
        }
        let Some(task_id) = run.current_task_id else {
            continue;
        };
        let task = match lake.get_task(task_id) {
            Ok(Some(t)) => t,
            _ => continue,
        };
        match task.status.as_str() {
            "done" => {
                if let Err(e) = advance(lake, bus, &run, &task).await {
                    warn!(run_id = %run.run_id, "advance failed: {e}");
                }
            }
            "failed" => {
                let mut failed = run.clone();
                failed.status = "failed".into();
                failed.error = Some(format!("step {} task failed", run.current_step));
                failed.completed_at = Some(chrono::Utc::now());
                let _ = lake.update_run(&failed);
                info!(run_id = %run.run_id, "workflow failed at step {}", run.current_step);
            }
            _ => {} // pending / running / dispatched — keep waiting
        }
    }
    Ok(())
}

async fn advance(
    lake: &Lake,
    bus: &Bus,
    run: &mamba_lake::WorkflowRun,
    finished_task: &mamba_lake::TaskRow,
) -> anyhow::Result<()> {
    let response_text = lake.get_response(uuid::Uuid::parse_str(&finished_task.id)?)?
        .unwrap_or_default();

    // Capture the output of the finished step.
    let mut updated = run.clone();
    updated.outputs.push(serde_json::json!({
        "step":     run.current_step,
        "task_id":  finished_task.id,
        "response": response_text,
        "tokens_in":  finished_task.tokens_in,
        "tokens_out": finished_task.tokens_out,
        "cost_usd":   finished_task.cost_usd,
    }));

    // Look up the workflow definition to know if there's a next step.
    let workflow = match lake.get_workflow(&run.workflow_id)? {
        Some(w) => w,
        None => {
            updated.status = "failed".into();
            updated.error = Some("workflow definition deleted mid-run".into());
            updated.completed_at = Some(chrono::Utc::now());
            updated.current_task_id = None;
            lake.update_run(&updated)?;
            return Ok(());
        }
    };

    let next_step_idx = (run.current_step + 1) as usize;
    if next_step_idx >= workflow.steps.len() {
        // Run complete.
        updated.status = "done".into();
        updated.completed_at = Some(chrono::Utc::now());
        updated.current_task_id = None;
        updated.current_step = next_step_idx as i32;
        lake.update_run(&updated)?;
        info!(
            run_id = %run.run_id,
            steps = workflow.steps.len(),
            "workflow complete"
        );
        return Ok(());
    }

    // Dispatch the next step.
    let next = &workflow.steps[next_step_idx];
    let envelope = build_step_envelope(next, &updated)?;
    let task_id = envelope.id;
    bus.submit(envelope).await?;

    updated.current_step = next_step_idx as i32;
    updated.current_task_id = Some(task_id);
    lake.update_run(&updated)?;
    info!(
        run_id = %run.run_id,
        step = next_step_idx,
        task_id = %task_id,
        agent = %next.assigned_agent,
        "workflow advanced"
    );
    Ok(())
}

/// Build the envelope for a single step. Injects the run history as
/// `__workflow` and the most recent output as `__previous` in the
/// payload JSON, so the agent has full context.
pub fn build_step_envelope(
    step: &WorkflowStep,
    run: &mamba_lake::WorkflowRun,
) -> anyhow::Result<TaskEnvelope> {
    let previous = run.outputs.last().cloned().unwrap_or(serde_json::Value::Null);
    let payload = serde_json::json!({
        "template":   step.payload,
        "__previous": previous,
        "__workflow": {
            "run_id":       run.run_id,
            "workflow_id":  run.workflow_id,
            "step_index":   run.current_step + 1,
            "step_name":    step.name,
            "initial_input": run.initial_input,
        },
    });
    Ok(TaskEnvelope::new(
        step.project.clone(),
        TaskSource::Webhook, // workflows use the webhook source so they're easy to filter
        step.assigned_agent.clone(),
        step.skill.clone(),
        step.model.clone(),
        payload.to_string(),
        step.priority,
    ))
}
