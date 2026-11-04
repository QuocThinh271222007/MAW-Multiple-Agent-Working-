-- What each agent said and did, so the conversation survives closing MAW (like resuming a
-- Claude Code or Codex session). Free text is redacted before it is written. Kept per agent
-- up to a fixed number of lines; no foreign key, so history outlives a removed agent.
CREATE TABLE transcript_lines (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id TEXT NOT NULL,
    at       TEXT NOT NULL,
    kind     TEXT NOT NULL,
    text     TEXT NOT NULL
);
CREATE INDEX idx_transcript_lines_agent ON transcript_lines(agent_id, id);
