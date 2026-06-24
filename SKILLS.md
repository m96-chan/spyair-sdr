# SKILLS.md — agents, skills & workflow for `spyair-sdr`

Companion to `CLAUDE.md` (the rules). This file documents the **project-specific sub-agents** and
the repeatable workflow used to build `spyair-sdr`. Keep it in sync as new agents/skills are added.

---

## Sub-agents

Defined under `.claude/agents/`. Invoke an agent for a scoped unit of work; it runs with its own
context and returns a report.

### `rust-tdd-engineer`
`.claude/agents/rust-tdd-engineer.md`

- **Purpose:** implement one scoped slice (typically one GitHub issue) **test-first**, strictly
  following `CLAUDE.md` — TDD red→green→refactor, the mock policy, traits-at-boundaries, and
  `Error::NotImplemented` for non-runnable hardware/network backends.
- **Use when:** an issue defines a testable Rust module/feature ready to build.
- **Guarantees:** mocks confined to `#[cfg(test)]`; no fabricated data in production; clippy+fmt
  clean; returns the test count and the implementable-vs-stubbed split. Does not open PRs.
- **Inputs it expects:** the issue scope + acceptance criteria; access to `src/` for conventions.

> Planned: a `mock-policy-reviewer` agent that audits a diff to prove no mock/fake can reach a
> release build and that every boundary is a trait with a real impl or `NotImplemented`. Not yet
> added — cut an issue before relying on it.

---

## The build workflow (one slice at a time)

This is how every feature is delivered. It mirrors §2 of `CLAUDE.md`.

1. **Issue first.** Cut a GitHub issue under epic #1 with scope, acceptance criteria, and
   explicit *out of scope*. Record any hardware/network blockers in the epic's
   implementable/not-implementable matrix.
2. **TDD.** Hand the issue to `rust-tdd-engineer` (or follow the same loop manually):
   write failing tests → minimal implementation → refactor.
3. **Verify.** `cargo fmt && cargo clippy --all-targets && cargo test` — all green.
4. **Push.** Commit to the feature branch (updates the open PR). Do not open new PRs unasked.
5. **Report.** Post one concise PR comment: what landed, test count, implementable-vs-stubbed
   split. Close the issue.

---

## Mock policy at a glance

The single most important rule (full text in `CLAUDE.md` §1.3):

| Context | Mocks/fakes allowed? | What production does instead |
|---------|----------------------|------------------------------|
| `#[cfg(test)] mod tests` | ✅ yes | — |
| Library / binary (release) | ❌ **never** | Real impl, or `Error::NotImplemented` for non-runnable hardware/network backends |

Boundaries that are currently **stubbed** (`NotImplemented`, tracked in epic #1):
RTL-SDR I/O, audio output, GPS/`gpsd`, IP geolocation, Whisper, RepeaterBook fetch, live ADS-B
feed, and the OurAirports network download (offline `build-db` reads local CSVs instead).

---

## Built-in Claude Code skills used here

These ship with the harness (not project files) but fit this repo's workflow:

- **`/code-review`** — review the working diff for correctness + cleanups before pushing.
- **`/security-review`** — security pass on pending changes (relevant given the RF/legal context).
- **`/init`** — (re)generate baseline `CLAUDE.md` docs if the structure drifts.

Use them to supplement — they do not replace the issue-first, test-first rules above.
