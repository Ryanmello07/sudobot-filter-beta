CREATE TABLE guild_overrides (
    guild_id   INTEGER PRIMARY KEY,
    toml       TEXT NOT NULL,
    updated_by INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);
