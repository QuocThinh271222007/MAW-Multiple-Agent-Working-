# Task lifecycle

Source of truth: `maw-core/src/task.rs` (state machine, evidence rules, progress) and `maw-orchestrator/src/tasks.rs` (operations).

## States

```
BACKLOG → READY → ASSIGNED → RUNNING → IN_REVIEW ─┐
                     ▲          │  ▲                ├→ VERIFYING → DONE
                     │          ▼  │                │       │
                     └──────── BLOCKED       (reject)       └─(fail)→ ASSIGNED (back to owner)
any non-final state → FAILED | CANCELED
```

| Transition | Who/what |
|---|---|
| BACKLOG → READY | automatic when every dependency is DONE |
| READY → ASSIGNED | Leader `maw_task_assign`, worker `maw_task_claim` (atomic), or orchestrator auto-assign to an idle worker |
| ASSIGNED → RUNNING | **orchestrator only**, when it dispatches the task to the owner's session (attempt+lease start) |
| RUNNING → BLOCKED | owner `maw_task_block`; orchestrator on lease expiry / owner disconnect / restart / two turns without a result |
| RUNNING → IN_REVIEW or VERIFYING | owner `maw_task_submit_result` (IN_REVIEW if a review is required) |
| IN_REVIEW → VERIFYING / ASSIGNED | Reviewer or Leader `maw_review_submit` approve / reject (owner cannot review own work) |
| VERIFYING → DONE / ASSIGNED | Leader (or user) `maw_task_verify` pass / fail |

`DONE` is reachable **only from VERIFYING**; this is enforced by the state machine (and tested exhaustively).

## What "done" requires

`Task::completion_blockers()` must be empty at verification time, evaluated **before** the verifier's own note is added, so a Leader sign-off alone cannot complete a task:

- at least one *passing* evidence item;
- the most recent test/build evidence must pass;
- file-changing tasks need `file_changes` evidence or an owner-provided `verification`; tasks that legitimately change nothing are flagged `no_file_changes_expected` and then need any passing evidence (e.g. a verification note);
- if review is required, it must be approved (not pending/rejected).

An agent finishing its turn, exiting, or reporting "100%" never completes a task. Reported progress is capped at 99 and displayed separately from verified progress.

## Dependencies

Stored as a DAG; additions are cycle-checked (`would_create_cycle`) and canceled prerequisites are rejected. Dispatch re-checks that every dependency is DONE. When a prerequisite completes, `refresh_ready` promotes its dependents automatically.

## Concurrency, leases, retries

- One running task per agent; `start_task` refuses to overwrite another running task's slot. The scheduler dispatches at most `max_concurrency` tasks and one task per owner per pass.
- Assignment/claim/creation are single transactions; creation is idempotent via `idempotency_key`.
- Each RUNNING task has a lease (`lease_secs`, default 15 min) renewed by `maw_progress_report`. Expiry → BLOCKED with an event and a Leader notification. **Tasks are never silently re-executed** after uncertainty (lease expiry, owner disconnect, MAW restart): their side effects are unknown, so the Leader/user decides (`release` to retry, or cancel).
- Attempts are counted and bounded (`max_attempts`, default 3); exceeding it fails the task. Each attempt is recorded in `attempts`.
- A Leader may requeue a FAILED/BLOCKED task, which grants one more attempt (an explicit human/Leader decision).

## Progress

```
verified % = Σ weight(DONE) / Σ weight(non-canceled tasks of the goal)
```

Shown alongside reported-unverified, blocked, failed and unknown weight. Adding tasks grows the denominator, so progress can fall; the plan revision that introduced each task is recorded (`plan_revision`, `plan.revised` event with the new total weight).

## Bounds against runaway planning

`max_tasks_per_goal` (200) and `max_task_depth` (4), enforced at creation.
