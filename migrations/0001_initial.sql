-- BEASTUBE initial schema.
--
-- Conventions, applied uniformly:
--
--   * Every table is STRICT. SQLite's default type affinity silently accepts a string into an
--     INTEGER column; STRICT makes that an error, which turns a whole class of storage-layer bugs
--     into immediate failures instead of rows that read back wrong months later.
--   * Timestamps are INTEGER milliseconds since the Unix epoch, UTC (see `beastube-core::time_util`).
--     They sort chronologically, index tightly, and reach JavaScript as a `Date` with no parsing.
--   * Booleans are INTEGER 0/1 with a CHECK constraint, since STRICT has no BOOLEAN type.
--   * Library tables denormalize the metadata they display (title, channel, thumbnails). This is
--     deliberate duplication: history, playlists and bookmarks must render offline and after the
--     metadata cache is cleared, so they cannot depend on a join to a cache table (§72).
--   * Cache tables carry `expires_at` and are safe to delete wholesale. User-created data
--     (history, playlists, bookmarks, settings) never is (§71, §101).

-- ---------------------------------------------------------------------------------------------
-- Metadata cache: provider data, disposable, rebuilt on demand.
-- ---------------------------------------------------------------------------------------------

CREATE TABLE channels (
    id                TEXT    NOT NULL PRIMARY KEY,
    name              TEXT    NOT NULL,
    handle            TEXT,
    avatar_json       TEXT,
    banner_json       TEXT,
    subscriber_count  INTEGER,
    video_count       INTEGER,
    description       TEXT,
    is_verified       INTEGER NOT NULL DEFAULT 0 CHECK (is_verified IN (0, 1)),
    available_tabs    TEXT,
    canonical_url     TEXT,
    fetched_at        INTEGER NOT NULL,
    expires_at        INTEGER
) STRICT;

CREATE INDEX idx_channels_expires_at ON channels (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE videos (
    id               TEXT    NOT NULL PRIMARY KEY,
    title            TEXT    NOT NULL,
    -- ON DELETE SET NULL, not CASCADE: evicting a channel from the cache must never delete the
    -- videos that reference it.
    channel_id       TEXT    REFERENCES channels (id) ON DELETE SET NULL,
    channel_name     TEXT,
    thumbnails_json  TEXT,
    duration_ms      INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    published_at     INTEGER,
    published_text   TEXT,
    view_count       INTEGER CHECK (view_count IS NULL OR view_count >= 0),
    live_status      TEXT    NOT NULL DEFAULT 'not_live'
                             CHECK (live_status IN ('not_live', 'live', 'upcoming', 'was_live')),
    is_short         INTEGER NOT NULL DEFAULT 0 CHECK (is_short IN (0, 1)),
    -- The watch-page-only fields (description, chapters, captions, …) as one JSON document. They
    -- are read together, only on the watch page, and never queried by field, so columns would add
    -- width to every list query for no benefit.
    details_json     TEXT,
    fetched_at       INTEGER NOT NULL,
    expires_at       INTEGER
) STRICT;

CREATE INDEX idx_videos_channel_id ON videos (channel_id) WHERE channel_id IS NOT NULL;
CREATE INDEX idx_videos_expires_at ON videos (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE remote_playlists (
    id               TEXT    NOT NULL PRIMARY KEY,
    title            TEXT    NOT NULL,
    channel_id       TEXT    REFERENCES channels (id) ON DELETE SET NULL,
    channel_name     TEXT,
    thumbnails_json  TEXT,
    video_count      INTEGER CHECK (video_count IS NULL OR video_count >= 0),
    description      TEXT,
    fetched_at       INTEGER NOT NULL,
    expires_at       INTEGER
) STRICT;

CREATE INDEX idx_remote_playlists_expires_at
    ON remote_playlists (expires_at) WHERE expires_at IS NOT NULL;

-- ---------------------------------------------------------------------------------------------
-- Local library: user-owned, never evicted, never synchronized anywhere.
-- ---------------------------------------------------------------------------------------------

CREATE TABLE history (
    video_id          TEXT    NOT NULL PRIMARY KEY,
    title             TEXT    NOT NULL,
    channel_id        TEXT,
    channel_name      TEXT,
    thumbnails_json   TEXT,
    duration_ms       INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    first_watched_at  INTEGER NOT NULL,
    last_watched_at   INTEGER NOT NULL,
    play_count        INTEGER NOT NULL DEFAULT 1 CHECK (play_count >= 0),
    -- Lowercased "title \n channel" maintained on write, so search is one scan over a narrow
    -- column instead of a scan plus per-row case folding. Revisit with FTS5 if measurement on a
    -- 100k-row table shows this is not enough (see docs/performance.md).
    search_text       TEXT    NOT NULL DEFAULT ''
) STRICT;

CREATE INDEX idx_history_last_watched ON history (last_watched_at DESC);
CREATE INDEX idx_history_channel ON history (channel_id) WHERE channel_id IS NOT NULL;
CREATE INDEX idx_history_search ON history (search_text);

-- Split from `history` because it is written far more often (every playback checkpoint) and read
-- on a different path (resume). Keeping the hot table narrow keeps checkpoint writes small in WAL.
CREATE TABLE playback_positions (
    video_id     TEXT    NOT NULL PRIMARY KEY,
    position_ms  INTEGER NOT NULL CHECK (position_ms >= 0),
    duration_ms  INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    updated_at   INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_playback_positions_updated ON playback_positions (updated_at DESC);

CREATE TABLE bookmarks (
    video_id         TEXT    NOT NULL PRIMARY KEY,
    title            TEXT    NOT NULL,
    channel_id       TEXT,
    channel_name     TEXT,
    thumbnails_json  TEXT,
    note             TEXT,
    timestamp_ms     INTEGER CHECK (timestamp_ms IS NULL OR timestamp_ms >= 0),
    created_at       INTEGER NOT NULL,
    search_text      TEXT    NOT NULL DEFAULT ''
) STRICT;

CREATE INDEX idx_bookmarks_created ON bookmarks (created_at DESC);
CREATE INDEX idx_bookmarks_search ON bookmarks (search_text);

CREATE TABLE bookmark_tags (
    video_id  TEXT NOT NULL REFERENCES bookmarks (video_id) ON DELETE CASCADE,
    tag       TEXT NOT NULL,
    PRIMARY KEY (video_id, tag)
) STRICT;

CREATE INDEX idx_bookmark_tags_tag ON bookmark_tags (tag);

CREATE TABLE playlists (
    id           INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    -- Stable identifier for built-in lists ('watch_later', 'favorites'); NULL for user-created
    -- ones. Matching on a slug rather than a display name keeps translations from orphaning rows.
    slug         TEXT    UNIQUE,
    name         TEXT    NOT NULL,
    description  TEXT,
    is_system    INTEGER NOT NULL DEFAULT 0 CHECK (is_system IN (0, 1)),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    -- A system playlist must have a slug, and a user playlist must not.
    CHECK ((is_system = 1) = (slug IS NOT NULL))
) STRICT;

CREATE INDEX idx_playlists_updated ON playlists (updated_at DESC);

CREATE TABLE playlist_items (
    playlist_id      INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    video_id         TEXT    NOT NULL,
    -- Sparse sort key (see `beastube-core::model::playlist::position_between`) so an insertion
    -- rewrites one row rather than renumbering the tail.
    position         INTEGER NOT NULL,
    added_at         INTEGER NOT NULL,
    title            TEXT    NOT NULL,
    channel_id       TEXT,
    channel_name     TEXT,
    thumbnails_json  TEXT,
    duration_ms      INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0),
    PRIMARY KEY (playlist_id, video_id)
) STRICT;

-- Enforces a total order per playlist: two items cannot occupy the same slot, so a failed
-- midpoint computation surfaces as a constraint violation rather than a nondeterministic order.
CREATE UNIQUE INDEX idx_playlist_items_position ON playlist_items (playlist_id, position);
CREATE INDEX idx_playlist_items_video ON playlist_items (video_id);

CREATE TABLE search_history (
    query             TEXT    NOT NULL PRIMARY KEY,
    last_searched_at  INTEGER NOT NULL,
    search_count      INTEGER NOT NULL DEFAULT 1 CHECK (search_count >= 0)
) STRICT;

CREATE INDEX idx_search_history_recent ON search_history (last_searched_at DESC);

-- ---------------------------------------------------------------------------------------------
-- Application state.
-- ---------------------------------------------------------------------------------------------

CREATE TABLE settings (
    key         TEXT    NOT NULL PRIMARY KEY,
    value_json  TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

-- Index of the on-disk blob cache (thumbnails and other byte payloads). Rows are disposable; the
-- files they point at are removed with them.
CREATE TABLE cache_entries (
    key               TEXT    NOT NULL PRIMARY KEY,
    namespace         TEXT    NOT NULL,
    -- Relative to the cache root and validated by `security::resolve_within` before use, so a
    -- corrupted row cannot direct a read or a delete outside the cache directory.
    relative_path     TEXT    NOT NULL,
    size_bytes        INTEGER NOT NULL CHECK (size_bytes >= 0),
    content_type      TEXT,
    -- Content hash, used to detect a file that was truncated or replaced under us (§71).
    checksum          TEXT,
    created_at        INTEGER NOT NULL,
    last_accessed_at  INTEGER NOT NULL,
    expires_at        INTEGER
) STRICT;

CREATE INDEX idx_cache_entries_namespace ON cache_entries (namespace);
-- Drives LRU eviction: ordering by last access within a namespace is the eviction scan.
CREATE INDEX idx_cache_entries_lru ON cache_entries (namespace, last_accessed_at);
CREATE INDEX idx_cache_entries_expires ON cache_entries (expires_at) WHERE expires_at IS NOT NULL;

-- Crash recovery and incognito bookkeeping. An incognito session writes nothing else to the
-- database; this row exists so an unclean shutdown can be detected and its scratch state discarded.
CREATE TABLE local_sessions (
    id                TEXT    NOT NULL PRIMARY KEY,
    kind              TEXT    NOT NULL CHECK (kind IN ('normal', 'incognito')),
    started_at        INTEGER NOT NULL,
    ended_at          INTEGER,
    last_video_id     TEXT,
    last_position_ms  INTEGER CHECK (last_position_ms IS NULL OR last_position_ms >= 0),
    -- Set on a clean shutdown. A row with 0 on startup is evidence of a crash (§81).
    clean_shutdown    INTEGER NOT NULL DEFAULT 0 CHECK (clean_shutdown IN (0, 1))
) STRICT;

CREATE INDEX idx_local_sessions_started ON local_sessions (started_at DESC);

-- ---------------------------------------------------------------------------------------------
-- Content filtering.
-- ---------------------------------------------------------------------------------------------

-- One row per installed rule set. Retaining superseded sets is what makes rollback possible
-- without a network round trip when a bad set is detected (§8).
CREATE TABLE filtering_rule_sets (
    version         TEXT    NOT NULL PRIMARY KEY,
    state           TEXT    NOT NULL
                            CHECK (state IN ('candidate', 'active', 'superseded', 'rolled_back')),
    rule_count      INTEGER NOT NULL CHECK (rule_count >= 0),
    checksum        TEXT    NOT NULL,
    source          TEXT    NOT NULL CHECK (source IN ('builtin', 'update', 'user')),
    installed_at    INTEGER NOT NULL,
    activated_at    INTEGER,
    rolled_back_at  INTEGER,
    -- i18n key explaining a rejection or rollback, surfaced on the diagnostics screen.
    reason_key      TEXT
) STRICT;

-- At most one rule set may be active at a time.
CREATE UNIQUE INDEX idx_filtering_rule_sets_active
    ON filtering_rule_sets (state) WHERE state = 'active';

CREATE TABLE filtering_rules (
    id            INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    set_version   TEXT    NOT NULL REFERENCES filtering_rule_sets (version) ON DELETE CASCADE,
    kind          TEXT    NOT NULL
                          CHECK (kind IN ('block_host', 'block_url', 'allow_host', 'allow_url',
                                          'hide_channel', 'hide_keyword', 'skip_segment')),
    pattern       TEXT    NOT NULL,
    -- Higher wins. Allow rules are evaluated before block rules regardless of priority.
    priority      INTEGER NOT NULL DEFAULT 0,
    -- Minimum filtering mode at which this rule applies: a rule marked 'strict' is inert in
    -- Standard mode, which is how the two modes differ without maintaining two rule sets.
    min_mode      TEXT    NOT NULL DEFAULT 'standard' CHECK (min_mode IN ('standard', 'strict')),
    enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    expires_at    INTEGER,
    created_at    INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_filtering_rules_set ON filtering_rules (set_version, enabled);
CREATE INDEX idx_filtering_rules_kind ON filtering_rules (kind, enabled);

-- User-authored entries, kept separate from downloaded rule sets so a rule-set update or rollback
-- can never discard something the user wrote.
CREATE TABLE filtering_preferences (
    id          INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    kind        TEXT    NOT NULL
                        CHECK (kind IN ('allow_host', 'block_host', 'hide_channel',
                                        'hide_keyword', 'custom_rule')),
    pattern     TEXT    NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at  INTEGER NOT NULL,
    UNIQUE (kind, pattern)
) STRICT;

CREATE INDEX idx_filtering_preferences_kind ON filtering_preferences (kind, enabled);

-- ---------------------------------------------------------------------------------------------
-- Seed data.
-- ---------------------------------------------------------------------------------------------

-- The built-in lists. Created here rather than at first launch so that the invariant "a system
-- playlist always exists" holds from the first query, with no startup ordering to get wrong.
INSERT INTO playlists (slug, name, is_system, created_at, updated_at)
VALUES ('watch_later', 'Watch Later', 1, 0, 0),
       ('favorites',   'Favorites',   1, 0, 0);
