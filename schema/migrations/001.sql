CREATE TABLE episodes (
    video_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    url TEXT NOT NULL,
    upload_date TEXT,
    duration INTEGER,
    thumbnail_path TEXT,
    mode TEXT NOT NULL,
    mc_version TEXT NOT NULL,
    review_count INTEGER NOT NULL CHECK (review_count >= 0),
    manifest_json TEXT NOT NULL
);
CREATE INDEX episodes_recency ON episodes(upload_date DESC, video_id);
CREATE INDEX episodes_mode ON episodes(mode, upload_date DESC, video_id);
CREATE INDEX episodes_version ON episodes(mc_version, upload_date DESC, video_id);
CREATE TABLE episode_tags (
    video_id TEXT NOT NULL REFERENCES episodes(video_id) ON DELETE CASCADE,
    tag TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY(video_id, tag)
) WITHOUT ROWID;
CREATE INDEX episode_tags_lookup ON episode_tags(tag, video_id);
