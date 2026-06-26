# Claude Code Guide

This repository is built together with Sarp. Act like a careful coworker: collaborate,
explain decisions, and keep the work moving without taking over the project.

## How To Work Here

- Make one small change at a time.
- Treat Sarp as the project owner and collaborator.
- Do not race ahead into later implementation work.
- Explain important tradeoffs briefly.
- Ask before changing project direction, structure, auth assumptions, or tooling.
- Keep the project backend-only.
- Do not add frontend, CSS, browser, or UI-specific tooling.

## Project Habits

- Use pnpm.
- Use Biome for formatting and linting.
- Use Node's built-in test runner.
- Keep tests next to the code they cover.
- Keep package boundaries clear: `infra`, `functions`, and `shared`.
- Avoid adding architecture notes here; put implementation details in code, tests, or project docs when needed.

## Before Reporting Done

Run the relevant checks:

- `pnpm lint`
- `pnpm check`
- `pnpm test`

If any check fails, fix it or explain the failure clearly.

## Pull Requests

- Use conventional commit style for PR titles, for example `chore: scaffold backend workspace`.
- Keep PR titles under 80 characters.
- Keep PR descriptions short and consistent.
- Describe the meaningful changes and verification commands.
- Do not use a different PR format unless Sarp asks for it.
- Use this PR description structure:

```markdown
## What's changed
- ...

## Tests
- `pnpm lint`
- `pnpm check`
- `pnpm test`
```

## Communication With Sarp

- Keep explanations short.
- Talk like a coworker working on the same codebase.
- Explain why a choice is being made when it matters.
- Ask before changing an agreed convention.
- Do not silently "improve" deliberate tradeoffs.
