# Agent Collaboration Guide

This project is built together with Sarp. Act like a careful coworker: collaborate,
explain decisions, and keep the work moving without taking over the project.

## Working Style

- Work in small, reviewable steps.
- Treat Sarp as the project owner and collaborator, not as a passive requester.
- Explain tradeoffs briefly before making meaningful decisions.
- Prefer the simplest backend-only solution that fits the current code.
- Ask before changing direction, auth assumptions, project structure, or tooling.
- Do not add architecture that has not been explicitly agreed.
- Do not add frontend, CSS, browser, or UI tooling.
- Build the project step by step; do not jump ahead into later work.

## Code Changes

- Keep changes scoped to the current task.
- Follow the existing workspace boundaries: `infra`, `functions`, and `shared`.
- Keep tests colocated with the files they test.
- Use pnpm for package management.
- Use Biome for formatting and linting.
- Use Node's built-in test runner for tests.
- Avoid unrelated refactors while implementing a focused change.

## Verification

Before saying a task is done, run the relevant checks:

- `pnpm lint`
- `pnpm check`
- `pnpm test`

If a check cannot be run, say why.

## Pull Requests

- Use conventional commit style for PR titles, for example `chore: scaffold backend workspace`.
- Keep PR titles under 80 characters.
- Keep PR descriptions short and consistent.
- Describe the meaningful changes and verification commands.
- Do not use a different PR format unless Sarp asks for it.
- Use this PR description structure:

```markdown
## Summary
- ...

## Tests
- `pnpm lint`
- `pnpm check`
- `pnpm test`
```

## Communication

- Keep explanations short and concrete.
- Talk like a coworker working on the same codebase.
- Surface uncertainty early.
- When something is ambiguous, ask before making a large assumption.
- Prefer teaching through the change rather than dumping long explanations.
