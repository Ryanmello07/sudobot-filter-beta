CREATE TABLE guild_state (
    guild_id   INTEGER PRIMARY KEY,
    mode       TEXT NOT NULL DEFAULT 'observe',
    updated_by INTEGER,
    updated_at TEXT NOT NULL
);

CREATE TABLE members (
    guild_id     INTEGER NOT NULL,
    user_id      INTEGER NOT NULL,
    clean_scans  INTEGER NOT NULL DEFAULT 0,
    messages     INTEGER NOT NULL DEFAULT 0,
    last_flag_at TEXT,
    join_flag    INTEGER NOT NULL DEFAULT 0,
    allowlisted  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE strikes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    incident_id INTEGER
);
CREATE INDEX strikes_user ON strikes (guild_id, user_id, expires_at);

CREATE TABLE recent_messages (
    guild_id   INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (guild_id, message_id)
);
CREATE INDEX recent_user ON recent_messages (guild_id, user_id, created_at);

CREATE TABLE incidents (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id       INTEGER NOT NULL,
    user_id        INTEGER NOT NULL,
    channel_id     INTEGER NOT NULL,
    message_id     INTEGER NOT NULL,
    tier           TEXT NOT NULL,
    label          TEXT NOT NULL,
    score          REAL NOT NULL,
    local_signals  TEXT,
    paxmod_json    TEXT,
    jev_json       TEXT,
    ocr_json       TEXT,
    verdict_json   TEXT,
    action_planned TEXT NOT NULL,
    action_taken   TEXT NOT NULL,
    outcome        TEXT NOT NULL,
    text_excerpt   TEXT,
    created_at     TEXT NOT NULL,
    UNIQUE (guild_id, message_id)
);
CREATE INDEX incidents_created ON incidents (created_at);

CREATE TABLE api_usage (
    day      TEXT NOT NULL,
    provider TEXT NOT NULL,
    calls    INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, provider)
);
