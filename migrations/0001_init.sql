-- MAW initial schema. State tables are authoritative; `events` is the append-only audit log.

CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    path        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE goals (
    id               TEXT PRIMARY KEY,
    project_id       TEXT NOT NULL REFERENCES projects(id),
    text             TEXT NOT NULL,
    success_criteria TEXT NOT NULL DEFAULT '[]',
    status           TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1,
    leader_agent_id  TEXT,
    current_milestone TEXT,
    current_focus    TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX idx_goals_project ON goals(project_id);

CREATE TABLE goal_revisions (
    goal_id          TEXT NOT NULL REFERENCES goals(id),
    revision         INTEGER NOT NULL,
    text             TEXT NOT NULL,
    success_criteria TEXT NOT NULL DEFAULT '[]',
    reason           TEXT NOT NULL,
    author           TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    PRIMARY KEY (goal_id, revision)
);

CREATE TABLE plan_revisions (
    goal_id      TEXT NOT NULL REFERENCES goals(id),
    revision     INTEGER NOT NULL,
    summary      TEXT NOT NULL,
    milestones   TEXT NOT NULL DEFAULT '[]',
    next_actions TEXT NOT NULL DEFAULT '[]',
    reason       TEXT NOT NULL,
    author       TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (goal_id, revision)
);

CREATE TABLE agents (
    id                TEXT PRIMARY KEY,
    project_id        TEXT NOT NULL REFERENCES projects(id),
    name              TEXT NOT NULL,
    provider          TEXT NOT NULL,
    model             TEXT,
    role              TEXT NOT NULL,
    state             TEXT NOT NULL,
    permission        TEXT NOT NULL,
    session_id        TEXT,
    native_session_id TEXT,
    workspace         TEXT,
    current_task_id   TEXT,
    capabilities      TEXT NOT NULL DEFAULT '{}',
    last_heartbeat    TEXT,
    last_error        TEXT,
    usage             TEXT NOT NULL DEFAULT '{}',
    created_at        TEXT NOT NULL,
    UNIQUE (project_id, name)
);

CREATE TABLE tasks (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL REFERENCES projects(id),
    key_num             INTEGER NOT NULL,
    goal_id             TEXT NOT NULL REFERENCES goals(id),
    parent_task_id      TEXT REFERENCES tasks(id),
    title               TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    acceptance_criteria TEXT NOT NULL DEFAULT '[]',
    priority            TEXT NOT NULL,
    status              TEXT NOT NULL,
    owner_agent_id      TEXT,
    estimated_weight    REAL NOT NULL DEFAULT 1.0,
    progress            INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    started_at          TEXT,
    completed_at        TEXT,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    workspace_id        TEXT,
    review_status       TEXT NOT NULL DEFAULT 'not_required',
    reviewer_agent_id   TEXT,
    blocked_reason      TEXT,
    lease_expires_at    TEXT,
    last_heartbeat_at   TEXT,
    plan_revision       INTEGER NOT NULL DEFAULT 1,
    created_by          TEXT NOT NULL,
    no_file_changes_expected INTEGER NOT NULL DEFAULT 0,
    idempotency_key     TEXT,
    UNIQUE (project_id, key_num)
);
CREATE UNIQUE INDEX idx_tasks_idem ON tasks(project_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX idx_tasks_status ON tasks(project_id, status);
CREATE INDEX idx_tasks_owner ON tasks(owner_agent_id);

CREATE TABLE task_deps (
    task_id    TEXT NOT NULL REFERENCES tasks(id),
    depends_on TEXT NOT NULL REFERENCES tasks(id),
    PRIMARY KEY (task_id, depends_on)
);
CREATE INDEX idx_task_deps_rev ON task_deps(depends_on);

CREATE TABLE evidence (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    kind        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    passed      INTEGER NOT NULL,
    detail      TEXT NOT NULL DEFAULT 'null',
    recorded_by TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE INDEX idx_evidence_task ON evidence(task_id);

CREATE TABLE artifacts (
    id            TEXT PRIMARY KEY,
    task_id       TEXT REFERENCES tasks(id),
    kind          TEXT NOT NULL,
    path          TEXT,
    description   TEXT NOT NULL DEFAULT '',
    registered_by TEXT NOT NULL,
    registered_at TEXT NOT NULL
);
CREATE INDEX idx_artifacts_task ON artifacts(task_id);

CREATE TABLE messages (
    id                 TEXT PRIMARY KEY,
    project_id         TEXT NOT NULL REFERENCES projects(id),
    sender_agent_id    TEXT NOT NULL,
    recipient_agent_id TEXT NOT NULL,
    task_id            TEXT,
    message_type       TEXT NOT NULL,
    subject            TEXT NOT NULL,
    content            TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    delivered_at       TEXT,
    acknowledged_at    TEXT,
    correlation_id     TEXT,
    idempotency_key    TEXT
);
CREATE INDEX idx_messages_recipient ON messages(recipient_agent_id, delivered_at);
CREATE UNIQUE INDEX idx_messages_idem ON messages(project_id, sender_agent_id, idempotency_key) WHERE idempotency_key IS NOT NULL;

CREATE TABLE events (
    sequence       INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id       TEXT NOT NULL UNIQUE,
    schema_version INTEGER NOT NULL,
    ts             TEXT NOT NULL,
    project_id     TEXT NOT NULL,
    run_id         TEXT,
    agent_id       TEXT,
    task_id        TEXT,
    correlation_id TEXT,
    event_type     TEXT NOT NULL,
    verbose        INTEGER NOT NULL DEFAULT 0,
    payload        TEXT NOT NULL
);
CREATE INDEX idx_events_project ON events(project_id, sequence);
CREATE INDEX idx_events_task ON events(task_id);
CREATE INDEX idx_events_type ON events(event_type);

CREATE TABLE context_entries (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    kind       TEXT NOT NULL,
    key        TEXT NOT NULL,
    content    TEXT NOT NULL,
    tags       TEXT NOT NULL DEFAULT '[]',
    task_id    TEXT,
    author     TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_context_project ON context_entries(project_id, kind);

CREATE TABLE workspaces (
    id                 TEXT PRIMARY KEY,
    project_id         TEXT NOT NULL REFERENCES projects(id),
    agent_id           TEXT,
    task_id            TEXT,
    path               TEXT NOT NULL,
    branch             TEXT,
    base_revision      TEXT,
    kind               TEXT NOT NULL,
    status             TEXT NOT NULL,
    changed_files      TEXT NOT NULL DEFAULT '[]',
    integration_status TEXT NOT NULL DEFAULT 'none',
    conflict           TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

CREATE TABLE attempts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    TEXT NOT NULL REFERENCES tasks(id),
    agent_id   TEXT NOT NULL,
    attempt_no INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at   TEXT,
    outcome    TEXT,
    note       TEXT
);
CREATE INDEX idx_attempts_task ON attempts(task_id);

CREATE TABLE approvals (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL,
    agent_id     TEXT NOT NULL,
    task_id      TEXT,
    kind         TEXT NOT NULL,
    summary      TEXT NOT NULL,
    reason       TEXT,
    details      TEXT NOT NULL DEFAULT 'null',
    request_ref  TEXT NOT NULL,
    status       TEXT NOT NULL,
    decision     TEXT,
    requested_at TEXT NOT NULL,
    resolved_at  TEXT
);
CREATE INDEX idx_approvals_status ON approvals(project_id, status);

CREATE TABLE runs (
    id             TEXT PRIMARY KEY,
    project_id     TEXT NOT NULL,
    started_at     TEXT NOT NULL,
    stopped_at     TEXT,
    clean_shutdown INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE kv (
    project_id TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (project_id, key)
);
