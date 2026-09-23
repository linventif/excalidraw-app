-- Scenes: mirrors the Firestore doc scenes/{roomId} shape used by upstream Excalidraw.
-- The server never sees plaintext: iv/ciphertext are base64 strings produced client-side.
CREATE TABLE IF NOT EXISTS scenes (
    room_id       TEXT PRIMARY KEY,
    scene_version INTEGER NOT NULL,
    iv            TEXT NOT NULL,
    ciphertext    TEXT NOT NULL,
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Files: metadata for encrypted file blobs stored on disk under DATA_DIR/files/...
-- scope is either 'rooms' or 'share-links', scope_id is the room_id / share_id.
CREATE TABLE IF NOT EXISTS files (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    scope      TEXT NOT NULL,
    scope_id   TEXT NOT NULL,
    file_id    TEXT NOT NULL,
    path       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (scope, scope_id, file_id)
);
