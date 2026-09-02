"""One-shot migration: turn shared SQL `const`s into literal-expanding macros.

sqlx 0.9 requires `&'static str` for `query()`, which is what makes SQL injection structurally
impossible rather than merely unlikely. A `const` cannot be spliced by `concat!`, but a macro
expanding to a string literal can — so the shared column lists stay DRY *and* provably static,
with no `AssertSqlSafe` escape hatch anywhere in the codebase.

Safe to re-run: a file whose const has already been converted is skipped.
"""

import io
import sys

BACKSLASH = chr(92)
QUOTE = chr(34)


def to_macro(path: str, const_name: str, macro_name: str, blurb: str) -> None:
    source = io.open(path, encoding="utf-8").read()

    if f"macro_rules! {macro_name}" in source:
        print(f"skip  {path}: {macro_name}!() already present")
        return

    decl = f"const {const_name}: &str = " + QUOTE
    if decl not in source:
        print(f"skip  {path}: const {const_name} not found")
        return

    start = source.index(decl)
    literal_start = start + len(decl) - 1  # index of the opening quote

    # Scan to the closing quote, honouring escapes.
    i = literal_start + 1
    while True:
        ch = source[i]
        if ch == BACKSLASH:
            i += 2
            continue
        if ch == QUOTE:
            break
        i += 1
    literal_end = i + 1

    if source[literal_end : literal_end + 1] != ";":
        raise SystemExit(f"{path}: expected ';' after the {const_name} literal")

    literal = source[literal_start:literal_end]
    indented = literal.replace("\n", "\n        ")

    replacement = (
        f"/// {blurb}\n"
        "///\n"
        "/// A macro rather than a `const` so call sites can assemble their full statement with\n"
        "/// `concat!`, keeping the SQL a compile-time literal. sqlx 0.9 refuses a runtime `String`\n"
        "/// as a query, and rightly so: that refusal is what makes SQL injection structurally\n"
        "/// impossible here rather than merely unlikely.\n"
        f"macro_rules! {macro_name} {{\n"
        "    () => {\n"
        f"        {indented}\n"
        "    };\n"
        "}"
    )

    source = source[:start] + replacement + source[literal_end + 1 :]
    io.open(path, "w", encoding="utf-8", newline="\n").write(source)
    print(f"ok    {path}: {const_name} -> {macro_name}!()")


def main() -> int:
    to_macro(
        "crates/beastube-db/src/repo/history.rs",
        "SELECT_ENTRY",
        "select_entry",
        "Columns and joins shared by every history query.",
    )
    to_macro(
        "crates/beastube-db/src/repo/bookmarks.rs",
        "SELECT_BOOKMARK",
        "select_bookmark",
        "Columns shared by every bookmark query.",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
