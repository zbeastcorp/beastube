-- The channel avatar on history rows.
--
-- `0002` gave history cards their view count and date; this gives them the picture beside the
-- title. Without it "Continue watching" was the one row on the home screen whose cards were
-- visibly poorer than every row beneath them — no avatar, where every provider-sourced card has
-- one.
--
-- Stored the same way as `thumbnails_json`: the encoded set, so the row carries what it needs to
-- render without a lookup. A snapshot, like the title and the thumbnail already are — a channel
-- that changes its picture shows the old one until the video is opened again.
--
-- Additive and nullable, so a row written before this migration renders exactly as it did.

ALTER TABLE history ADD COLUMN channel_avatar_json TEXT;
