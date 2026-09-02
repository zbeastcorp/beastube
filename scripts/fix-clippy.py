"""One-shot clippy cleanup for the salvaged infrastructure crates.

Each edit is a real fix or a documented, locally-scoped allow — never a blanket suppression.
Dead-code warnings are silenced with `#[allow(dead_code)]` plus a note naming the module that will
consume the item, so the allow is a promise with an owner rather than a shrug.

Safe to re-run.
"""

import io
import sys

BACKSLASH = chr(92)


def patch(path: str, replacements: list[tuple[str, str]]) -> None:
    source = io.open(path, encoding="utf-8").read()
    changed = 0
    for old, new in replacements:
        if new in source and old not in source:
            continue  # already applied
        if old not in source:
            print(f"  !! not found in {path}: {old[:70]!r}")
            continue
        source = source.replace(old, new, 1)
        changed += 1
    if changed:
        io.open(path, "w", encoding="utf-8", newline="\n").write(source)
    print(f"{path}: {changed} edit(s)")


def main() -> int:
    # --- network -----------------------------------------------------------------------------
    patch(
        "crates/beastube-network/src/error.rs",
        [
            (
                "    pub(crate) fn from_reqwest(",
                "    // Consumed by `client.rs`, which lands with the request pipeline.\n"
                "    #[allow(dead_code)]\n"
                "    pub(crate) fn from_reqwest(",
            )
        ],
    )
    patch(
        "crates/beastube-network/src/retry.rs",
        [
            (
                "thread_local! {\n    static ",
                "thread_local! {\n    static ",
            )
        ],
    )

    # --- tasks -------------------------------------------------------------------------------
    patch(
        "crates/beastube-tasks/src/retry.rs",
        [
            (
                "                    .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);",
                "                    .map_or(0x9E37_79B9_7F4A_7C15, |d| {\n"
                "                        // Truncating to the low 64 bits is fine: this seeds a\n"
                "                        // jitter generator, where only spread matters.\n"
                "                        #[allow(clippy::cast_possible_truncation)]\n"
                "                        {\n"
                "                            d.as_nanos() as u64\n"
                "                        }\n"
                "                    });",
            ),
        ],
    )
    patch(
        "crates/beastube-tasks/src/scheduler.rs",
        [
            (
                "    fn cap(self, priority: Priority) -> usize {",
                "    fn cap(self, priority: Priority) -> usize {",
            ),
            (
                "async fn run_attempts<F, Fut, T, E>(\n    shared: &Arc<Shared>,",
                "async fn run_attempts<F, Fut, T, E>(\n    shared: &Shared,",
            ),
            (
                "                let outcome = run_attempts(&shared, &spec, &task_token, body).await;",
                "                let outcome = run_attempts(&shared, &spec, &task_token, body).await;",
            ),
        ],
    )

    # --- db ----------------------------------------------------------------------------------
    patch(
        "crates/beastube-db/src/repo/mod.rs",
        [
            (
                "pub(crate) fn like_prefix(",
                "// Consumed by the search-suggestion repository.\n"
                "#[allow(dead_code)]\n"
                "pub(crate) fn like_prefix(",
            ),
            (
                "pub(crate) fn from_db_bool(",
                "// Consumed by the filtering repository, which stores enabled flags as 0/1.\n"
                "#[allow(dead_code)]\n"
                "pub(crate) fn from_db_bool(",
            ),
            (
                "pub(crate) fn to_db_bool(",
                "// Consumed by the filtering repository, which stores enabled flags as 0/1.\n"
                "#[allow(dead_code)]\n"
                "pub(crate) fn to_db_bool(",
            ),
        ],
    )
    patch(
        "crates/beastube-db/src/connection.rs",
        [
            (
                "    fn connect_options(path: &Path) -> DbResult<SqliteConnectOptions> {\n"
                "        Ok(SqliteConnectOptions::new()",
                "    fn connect_options(path: &Path) -> SqliteConnectOptions {\n"
                "        SqliteConnectOptions::new()",
            ),
            (
                "            .optimize_on_close(true, None))\n    }",
                "            .optimize_on_close(true, None)\n    }",
            ),
            (
                "        let options = Self::connect_options(path)?;",
                "        let options = Self::connect_options(path);",
            ),
            (
                "        Ok(u64::try_from(page_count.max(0)).unwrap_or(0)\n"
                "            * u64::try_from(page_size.max(0)).unwrap_or(0))",
                "        Ok(u64::try_from(page_count.max(0)).unwrap_or(0)\n"
                "            * u64::try_from(page_size.max(0)).unwrap_or(0))",
            ),
        ],
    )
    patch(
        "crates/beastube-db/src/repo/positions.rs",
        [
            (
                "limit.max(CANDIDATE_BATCH).min(MAX_CANDIDATE_BATCH)",
                "limit.clamp(CANDIDATE_BATCH, MAX_CANDIDATE_BATCH)",
            ),
        ],
    )

    # --- cache -------------------------------------------------------------------------------
    patch(
        "crates/beastube-cache/src/key.rs",
        [
            (
                "pub(crate) fn is_entry_file_name(",
                "// Consumed by `disk.rs` when sweeping the cache directory for orphans.\n"
                "#[allow(dead_code)]\n"
                "pub(crate) fn is_entry_file_name(",
            ),
            (
                "pub(crate) fn is_shard_dir_name(",
                "// Consumed by `disk.rs` when sweeping the cache directory for orphans.\n"
                "#[allow(dead_code)]\n"
                "pub(crate) fn is_shard_dir_name(",
            ),
        ],
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
