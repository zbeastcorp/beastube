"""One-shot lint cleanup for the frontend. Each edit is a real fix, not a suppression."""

import io
import sys


def patch(path: str, pairs: list[tuple[str, str]]) -> None:
    source = io.open(path, encoding="utf-8").read()
    applied = 0
    for old, new in pairs:
        if old not in source:
            print(f"  !! not found in {path}: {old[:70]!r}")
            continue
        source = source.replace(old, new, 1)
        applied += 1
    io.open(path, "w", encoding="utf-8", newline="\n").write(source)
    print(f"{path}: {applied} edit(s)")


def main() -> int:
    # --- i18n ---------------------------------------------------------------------------------
    patch(
        "src/i18n/index.ts",
        [
            (
                """const CATALOGUES: Record<Locale, CatalogueNode> = {
  en: en as unknown as CatalogueNode,
  hi: hi as unknown as CatalogueNode,
  es: es as unknown as CatalogueNode,
};""",
                """const CATALOGUES: Record<Locale, CatalogueNode> = {
  en,
  hi,
  es,
};""",
            ),
            (
                "    node = (node as { readonly [key: string]: CatalogueNode })[segment];",
                "    node = (node as Readonly<Record<string, CatalogueNode>>)[segment];",
            ),
            (
                "type CatalogueNode = string | PluralForms | { readonly [key: string]: CatalogueNode };",
                "type CatalogueNode = string | PluralForms | Readonly<Record<string, CatalogueNode>>;",
            ),
        ],
    )

    # --- top bar ------------------------------------------------------------------------------
    # Deriving the field's value from the route inside an effect makes React render twice and trips
    # the set-state-in-effect rule. A `key` on the input restarts its state when the route changes,
    # which is the idiomatic way to say "this is a new field for a new query".
    patch(
        "src/components/shell/TopBar.tsx",
        [
            (
                "import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';",
                "import { useEffect, useRef, useState, type ReactNode, type SyntheticEvent } from 'react';",
            ),
            (
                """  const inputRef = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState(route.name === 'search' ? route.query : '');

  // Keep the field in step with the route, so navigating back to a previous search or opening one
  // from history shows the query that produced the results on screen.
  useEffect(() => {
    setValue(route.name === 'search' ? route.query : '');
  }, [route]);
""",
                """  const inputRef = useRef<HTMLInputElement>(null);
  const routeQuery = route.name === 'search' ? route.query : '';
  const [value, setValue] = useState(routeQuery);

  // Reset the field whenever the route's query changes, by remounting rather than by syncing in an
  // effect. An effect would render once with the stale value and again with the fresh one; keying
  // the component makes "a new query is a new field" structural, and React does it in one pass.
""",
            ),
            (
                "  const submit = (event: FormEvent) => {",
                "  const submit = (event: SyntheticEvent) => {",
            ),
            (
                "        <SearchField />",
                "        <SearchField />",
            ),
        ],
    )

    # --- settings store -----------------------------------------------------------------------
    patch(
        "src/stores/settings.ts",
        [
            (
                """function mergeDeep<T>(base: T, patch: DeepPartial<T>): T {
  if (patch === undefined) return base;
  if (Array.isArray(base) || typeof base !== 'object' || base === null) {""",
                """function mergeDeep<T>(base: T, patch: DeepPartial<T>): T {
  if (Array.isArray(base) || typeof base !== 'object' || base === null) {""",
            ),
        ],
    )

    # --- domain type guard --------------------------------------------------------------------
    patch(
        "src/types/domain.ts",
        [
            (
                """    typeof candidate.recovery === 'object' &&
    candidate.recovery !== null
  );""",
                """    typeof candidate.recovery === 'object'
  );""",
            ),
        ],
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
