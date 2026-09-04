-- Views and publication date on history rows.
--
-- Without these, a card in "Continue watching" could not show what every other card shows — the
-- view count and how long ago it went up — because history stored only what was needed to *find*
-- the video again, not what was needed to present it. The result was a home screen whose first row
-- was visibly poorer than the rows under it.
--
-- Both are nullable and both stay nullable. They are a snapshot taken when the video was watched,
-- not a live figure: a count from last week is what the card shows until the video is opened again,
-- which is the same bargain the stored title and thumbnail already make. A row written before this
-- migration simply has neither, and renders exactly as it did.
--
-- `ALTER TABLE ... ADD COLUMN` is the whole change: it rewrites no rows, holds no long lock, and
-- cannot fail part-way on a large history. STRICT tables accept it.

ALTER TABLE history ADD COLUMN view_count INTEGER
    CHECK (view_count IS NULL OR view_count >= 0);

ALTER TABLE history ADD COLUMN published_at INTEGER;
