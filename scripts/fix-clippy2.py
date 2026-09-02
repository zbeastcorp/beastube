"""Second clippy cleanup pass. Each edit is a real fix or a locally-justified allow."""

import io
import re
import sys


def patch(path: str, replacements: list[tuple[str, str]]) -> None:
    source = io.open(path, encoding="utf-8").read()
    changed = 0
    for old, new in replacements:
        if old not in source:
            if new not in source:
                print(f"  !! not found in {path}: {old[:70]!r}")
            continue
        source = source.replace(old, new, 1)
        changed += 1
    if changed:
        io.open(path, "w", encoding="utf-8", newline="\n").write(source)
    print(f"{path}: {changed} edit(s)")


def allow_on_match(path: str, marker: str, lint: str, note: str) -> None:
    """Insert `#[allow(lint)]` with a rationale immediately above the line containing `marker`."""
    source = io.open(path, encoding="utf-8").read()
    if note in source:
        print(f"{path}: allow already present")
        return
    lines = source.split("\n")
    for index, line in enumerate(lines):
        if marker in line:
            indent = re.match(r"\s*", line).group(0)
            lines.insert(index, f"{indent}// {note}")
            lines.insert(index + 1, f"{indent}#[allow({lint})]")
            io.open(path, "w", encoding="utf-8", newline="\n").write("\n".join(lines))
            print(f"{path}: allow({lint}) inserted")
            return
    print(f"  !! marker not found in {path}: {marker!r}")


def main() -> int:
    # Distinct error variants that happen to share a recovery today; merging them would couple
    # rules that are expected to diverge.
    for path in (
        "crates/beastube-filtering/src/error.rs",
        "crates/beastube-tasks/src/error.rs",
        "crates/beastube-db/src/error.rs",
    ):
        allow_on_match(
            path,
            "fn recovery(&self) -> Recovery {",
            "clippy::match_same_arms",
            "Variants sharing a recovery today are still distinct failures; merging the arms"
            " would couple rules that are expected to diverge.",
        )

    patch(
        "crates/beastube-network/src/retry.rs",
        [
            (
                "static LAST_JITTER: Cell<u64> = Cell::new(0);",
                "static LAST_JITTER: Cell<u64> = const { Cell::new(0) };",
            ),
        ],
    )
    allow_on_match(
        "crates/beastube-network/src/retry.rs",
        "as f64",
        "clippy::cast_precision_loss",
        "Only the spread matters here; losing low bits of a jitter seed is immaterial.",
    )

    patch(
        "crates/beastube-tasks/src/scheduler.rs",
        [
            (
                "        body: F,\n    ) -> Result<TaskHandle<T, E>, RejectReason>\n    where\n"
                "        F: Fn(CancellationToken) -> Fut + Send + 'static,\n"
                "        Fut: Future<Output = Result<T, E>> + Send,\n"
                "        T: Send + 'static,\n"
                "        E: Retryable + Send + 'static,\n    {\n        if self.is_shutting_down()",
                "        body: F,\n    ) -> Result<TaskHandle<T, E>, RejectReason>\n    where\n"
                "        F: Fn(CancellationToken) -> Fut + Send + 'static,\n"
                "        Fut: Future<Output = Result<T, E>> + Send,\n"
                "        T: Send + 'static,\n"
                "        E: Retryable + Send + 'static,\n    {\n        if self.is_shutting_down()",
            ),
        ],
    )
    allow_on_match(
        "crates/beastube-tasks/src/scheduler.rs",
        "    fn spawn_in<F, Fut, T, E>(",
        "clippy::needless_pass_by_value",
        "`parent` is cloned into the child token rather than consumed, but taking it by value"
        " keeps every caller free to hand over a temporary.",
    )

    allow_on_match(
        "crates/beastube-db/src/connection.rs",
        "let nanos = std::time::SystemTime::now()",
        "clippy::cast_possible_truncation",
        "Truncating to the low 64 bits is fine: this only needs to be unique per test process.",
    )

    patch(
        "crates/beastube-cache/src/error.rs",
        [
            ("            _ => ", "            other @ FetchError::Failed { .. } => "),
        ],
    )
    allow_on_match(
        "crates/beastube-cache/src/memory.rs",
        "impl std::fmt::Debug for MemoryCache {",
        "clippy::missing_fields_in_debug",
        "The moka cache holds arbitrary entry values; printing them would dump cached media"
        " bytes into a log.",
    )

    # Float comparisons in tests are against exact literals the code returns verbatim.
    allow_on_match(
        "crates/beastube-tasks/src/retry.rs",
        "fn a_nonsensical_jitter_fraction_cannot_extend_the_delay()",
        "clippy::float_cmp",
        "These compare against literals the clamp returns bit-for-bit, not computed values.",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
