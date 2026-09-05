## What this changes

<!-- The change, and the reason it was needed. If a measurement motivated it, put the number in. -->

## How it was verified

<!--
Not "it builds". What you actually observed: the window size you tested at, the before and after
numbers, the case that used to fail. This project's history is full of fixes that were correct in
principle and wrong in the application, and measuring is what separates them.
-->

## Checklist

- [ ] `pnpm typecheck`, `pnpm lint`, `pnpm format:check` and `pnpm test` pass
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] Any user-facing string goes through `src/i18n/locales/en.ts`
- [ ] No control was added that does not yet do anything
- [ ] Comments explain _why_, and any measurement quoted is one I took
