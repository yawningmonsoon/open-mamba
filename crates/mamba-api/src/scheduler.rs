//! Cron schedule worker.
//!
//! Wakes on a fixed tick (30s by default), finds all schedules whose
//! `next_run_at <= now AND enabled`, fires each as a fresh task envelope,
//! and advances `next_run_at` using the schedule's cron expression.
//!
//! Crash safety: if the worker is killed mid-tick, schedules that
//! already had a task envelope inserted but no `next_run_at` advance
//! will fire again on the next tick. That can yield up to one extra
//! invocation per crash, but never silently drops a schedule. We keep
//! it that way deliberately — duplicates are recoverable, missed runs
//! are not.

use crate::cron_util;
use mamba_bus::Bus;
use mamba_lake::Lake;
use mamba_types::envelope::{TaskEnvelope, TaskSource};
use std::time::Duration;
use tracing::{info, warn};

const TICK_INTERVAL: Duration = Duration::from_secs(30);

pub fn spawn(lake: Lake, bus: Bus) {
    tokio::spawn(async move {
        info!(
            tick_secs = TICK_INTERVAL.as_secs(),
            "schedule worker started"
        );
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            if let Err(e) = tick(&lake, &bus).await {
                warn!("schedule tick failed: {e}");
            }
        }
    });
}

async fn tick(lake: &Lake, bus: &Bus) -> anyhow::Result<()> {
    let due = lake.due_schedules()?;
    if due.is_empty() {
        return Ok(());
    }
    info!(count = due.len(), "firing due schedules");

    for sched in due {
        // Build the envelope from the schedule template.
        let envelope = TaskEnvelope::new(
            sched.project.clone(),
            TaskSource::Cron,
            sched.assigned_agent.clone(),
            sched.skill.clone(),
            sched.model.clone(),
            sched.payload.clone(),
            sched.priority.max(0) as u8,
        );
        let task_id = envelope.id;

        // Submit before advancing — if submit fails we want the next tick
        // to retry, not lose the firing.
        if let Err(e) = bus.submit(envelope).await {
            warn!(schedule_id = %sched.id, "schedule submit failed: {e}");
            continue;
        }

        // Compute the next firing time and persist.
        let next = match cron_util::next_after(&sched.cron_expr, chrono::Utc::now()) {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    schedule_id = %sched.id,
                    cron = %sched.cron_expr,
                    "cannot compute next_run_at, disabling: {e}"
                );
                // Disable a broken schedule so it doesn't loop forever.
                let mut bad = sched.clone();
                bad.enabled = false;
                let _ = lake.upsert_schedule(&bad);
                continue;
            }
        };

        if let Err(e) = lake.advance_schedule(&sched.id, next) {
            warn!(schedule_id = %sched.id, "advance_schedule failed: {e}");
            continue;
        }

        info!(
            schedule_id = %sched.id,
            task_id = %task_id,
            next_run_at = %next.to_rfc3339(),
            "scheduled task fired"
        );
    }
    Ok(())
}
