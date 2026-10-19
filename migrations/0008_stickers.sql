CREATE TABLE sticker_images (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL UNIQUE,
    mime TEXT NOT NULL,
    data BLOB NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL
);
CREATE TABLE stickers (
    id TEXT PRIMARY KEY,
    image_id TEXT NOT NULL REFERENCES sticker_images(id),
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
CREATE TABLE message_stickers (
    message_id TEXT PRIMARY KEY REFERENCES messages(id),
    image_id TEXT NOT NULL REFERENCES sticker_images(id),
    name TEXT NOT NULL
);
ALTER TABLE qq_inbox ADD COLUMN image_urls TEXT NOT NULL DEFAULT '[]';
ALTER TABLE qq_inbox ADD COLUMN image_assets TEXT NOT NULL DEFAULT '[]';
