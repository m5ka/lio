-- Lio library schema v1.

-- Library mirror -------------------------------------------------------------

CREATE TABLE artist (
    rowid        INTEGER PRIMARY KEY,
    id           TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    sort_name    TEXT,
    album_count  INTEGER NOT NULL DEFAULT 0,
    cover_art_id TEXT,
    starred_at   INTEGER,
    rating       INTEGER CHECK (rating BETWEEN 1 AND 5),
    synced_at    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX artist_sort ON artist (sort_name);

CREATE TABLE album (
    rowid            INTEGER PRIMARY KEY,
    id               TEXT NOT NULL UNIQUE,
    name             TEXT NOT NULL,
    sort_name        TEXT,
    artist_display   TEXT,
    year             INTEGER,
    genre            TEXT,
    cover_art_id     TEXT,
    song_count       INTEGER NOT NULL DEFAULT 0,
    duration_s       INTEGER NOT NULL DEFAULT 0,
    play_count       INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL DEFAULT 0,
    played_at        INTEGER,
    starred_at       INTEGER,
    rating           INTEGER CHECK (rating BETWEEN 1 AND 5),
    tracks_synced_at INTEGER,
    is_compilation   INTEGER NOT NULL DEFAULT 0,
    synced_at        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX album_created ON album (created_at);
CREATE INDEX album_sort ON album (sort_name);
CREATE INDEX album_year ON album (year);
CREATE INDEX album_play_count ON album (play_count);
CREATE INDEX album_played ON album (played_at);
CREATE INDEX album_artist_display ON album (artist_display);

CREATE TABLE album_artist (
    album_rowid  INTEGER NOT NULL REFERENCES album (rowid) ON DELETE CASCADE,
    artist_rowid INTEGER NOT NULL REFERENCES artist (rowid) ON DELETE CASCADE,
    role         TEXT NOT NULL DEFAULT 'albumartist',
    PRIMARY KEY (album_rowid, artist_rowid, role)
);
CREATE INDEX album_artist_by_artist ON album_artist (artist_rowid);

CREATE TABLE album_genre (
    album_rowid INTEGER NOT NULL REFERENCES album (rowid) ON DELETE CASCADE,
    genre       TEXT NOT NULL,
    PRIMARY KEY (album_rowid, genre)
);
CREATE INDEX album_genre_by_genre ON album_genre (genre);

CREATE TABLE track (
    rowid          INTEGER PRIMARY KEY,
    id             TEXT NOT NULL UNIQUE,
    album_rowid    INTEGER REFERENCES album (rowid) ON DELETE CASCADE,
    title          TEXT NOT NULL,
    artist_display TEXT,
    track_no       INTEGER,
    disc_no        INTEGER,
    duration_s     INTEGER NOT NULL DEFAULT 0,
    suffix         TEXT,
    content_type   TEXT,
    bitrate_kbps   INTEGER,
    sample_rate    INTEGER,
    bit_depth      INTEGER,
    channels       INTEGER,
    size_bytes     INTEGER NOT NULL DEFAULT 0,
    rg_track_gain  REAL,
    rg_track_peak  REAL,
    rg_album_gain  REAL,
    rg_album_peak  REAL,
    play_count     INTEGER NOT NULL DEFAULT 0,
    starred_at     INTEGER,
    rating         INTEGER CHECK (rating BETWEEN 1 AND 5),
    cover_art_id   TEXT,
    synced_at      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX track_order ON track (album_rowid, disc_no, track_no);
CREATE INDEX track_starred ON track (starred_at) WHERE starred_at IS NOT NULL;

CREATE TABLE track_artist (
    track_rowid  INTEGER NOT NULL REFERENCES track (rowid) ON DELETE CASCADE,
    artist_rowid INTEGER NOT NULL REFERENCES artist (rowid) ON DELETE CASCADE,
    role         TEXT NOT NULL DEFAULT 'artist',
    PRIMARY KEY (track_rowid, artist_rowid, role)
);
CREATE INDEX track_artist_by_artist ON track_artist (artist_rowid);

CREATE TABLE playlist (
    rowid        INTEGER PRIMARY KEY,
    id           TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    comment      TEXT,
    owner        TEXT,
    public       INTEGER NOT NULL DEFAULT 0,
    song_count   INTEGER NOT NULL DEFAULT 0,
    duration_s   INTEGER NOT NULL DEFAULT 0,
    changed_at   INTEGER NOT NULL DEFAULT 0,
    cover_art_id TEXT,
    readonly     INTEGER NOT NULL DEFAULT 0,
    tracks_synced_at INTEGER
);

CREATE TABLE playlist_track (
    playlist_rowid INTEGER NOT NULL REFERENCES playlist (rowid) ON DELETE CASCADE,
    position       INTEGER NOT NULL,
    track_rowid    INTEGER NOT NULL REFERENCES track (rowid) ON DELETE CASCADE,
    PRIMARY KEY (playlist_rowid, position)
);
CREATE INDEX playlist_track_by_track ON playlist_track (track_rowid);

CREATE VIRTUAL TABLE search_fts USING fts5 (
    kind UNINDEXED,
    ref_rowid UNINDEXED,
    text,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TABLE sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Play queue -----------------------------------------------------------------

CREATE TABLE queue_item (
    position    INTEGER PRIMARY KEY,
    track_rowid INTEGER NOT NULL REFERENCES track (rowid) ON DELETE CASCADE,
    origin_kind TEXT,
    origin_id   TEXT
);

CREATE TABLE queue_state (
    key   TEXT PRIMARY KEY,
    value BLOB
);

-- Audio cache index ----------------------------------------------------------

CREATE TABLE cache_entry (
    track_rowid    INTEGER NOT NULL REFERENCES track (rowid) ON DELETE CASCADE,
    quality        TEXT NOT NULL,
    path           TEXT NOT NULL,
    size_bytes     INTEGER NOT NULL DEFAULT 0,
    complete       INTEGER NOT NULL DEFAULT 0,
    last_played_at INTEGER NOT NULL DEFAULT 0,
    play_count     INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (track_rowid, quality)
);

-- Pinned downloads -----------------------------------------------------------

CREATE TABLE download (
    rowid          INTEGER PRIMARY KEY,
    kind           TEXT NOT NULL CHECK (kind IN ('album', 'playlist', 'favourites')),
    ref_id         TEXT NOT NULL,
    quality        TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'queued',
    created_at     INTEGER NOT NULL DEFAULT 0,
    last_synced_at INTEGER,
    error          TEXT,
    UNIQUE (kind, ref_id)
);

CREATE TABLE download_track (
    download_rowid INTEGER NOT NULL REFERENCES download (rowid) ON DELETE CASCADE,
    track_rowid    INTEGER NOT NULL REFERENCES track (rowid) ON DELETE CASCADE,
    PRIMARY KEY (download_rowid, track_rowid)
);
CREATE INDEX download_track_by_track ON download_track (track_rowid);

CREATE TABLE download_file (
    track_rowid INTEGER NOT NULL REFERENCES track (rowid) ON DELETE CASCADE,
    quality     TEXT NOT NULL,
    path        TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL DEFAULT 0,
    complete    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (track_rowid, quality)
);

-- Pending scrobbles ----------------------------------------------------------

CREATE TABLE scrobble (
    rowid     INTEGER PRIMARY KEY,
    track_id  TEXT NOT NULL,
    played_at INTEGER NOT NULL,
    submitted INTEGER NOT NULL DEFAULT 0,
    attempts  INTEGER NOT NULL DEFAULT 0,
    submitted_at INTEGER
);
CREATE INDEX scrobble_pending ON scrobble (submitted, played_at);
