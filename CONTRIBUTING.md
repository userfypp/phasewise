# Contributing

Thanks for helping. phasewise is small on purpose: one Rust binary, no async
runtime, no network access of its own. Changes that keep it that way are the
easiest to merge.

## Issues

Search the existing issues first, then use the bug or feature template. For a
bug, include:

- `phasewise --version`, the output of `phasewise doctor` and your OS;
- what you ran and what you expected;
- for a failed phase, the relevant files from `.phasewise/runs/<run-id>/`
  (agent output, `stderr.log`, verification logs). Remove anything private
  first: paths, account names, tokens.

### Labels

Every issue has one `type:` label (`bug`, `feature`, `test`, `docs`, `chore` or
`design`, a decision to make before building), usually one or two `area:`
labels (`cli`, `tui`, `engine`, `agents`, `git`, `plan`, `state`, `release`)
and, once planned, a `priority:` label (`high` is next up, `medium` planned,
`low` no rush). `agent: claude` and `agent: codex` mark issues specific to one
agent. `status: needs info` and `status: blocked` say what an issue is waiting
for. The type labels match the Conventional Commit types of pull request
titles. Issues with `good first issue` or `help wanted` are open to anyone.

### Groups

Every issue belongs to exactly one group: an `epic` issue that holds it as a
sub-issue, so GitHub shows the progress of each group. There are eight, one per
area of the code (plus one for what goes beyond a single agent). A group is
long-lived and general; it is never closed because its issues are done, since
new ones keep arriving.

| Group | Holds | Label |
|---|---|---|
| [#86 Run safety: git, state and recovery](https://github.com/userfypp/phasewise/issues/86) | branch and checkpoints, `retry`, `rollback`, `abort`, `merge`, `state.json`, the lock, recovery | `area: git`, `area: state` |
| [#84 Plan format, validation and settings](https://github.com/userfypp/phasewise/issues/84) | parsing and checking plans, drafted plans, settings file, examples | `area: plan` |
| [#83 Terminal UI](https://github.com/userfypp/phasewise/issues/83) | home and run screens, keys, popups, TUI tests | `area: tui` |
| [#59 Agents and output fidelity](https://github.com/userfypp/phasewise/issues/59) | Claude Code and Codex command lines and parsers, fixtures, what reaches the terminal | `area: agents` |
| [#57 Engine: checks, limits and spending](https://github.com/userfypp/phasewise/issues/57) | the phase cycle, checks and their output, timeouts, cost and usage limits | `area: engine` |
| [#60 CLI, scripts and integrations](https://github.com/userfypp/phasewise/issues/60) | commands, flags, exit codes, error messages, `--json`, completions, `doctor` | `area: cli` |
| [#61 Beyond one agent](https://github.com/userfypp/phasewise/issues/61) | mixed plans, other agents, parallel phases | `area: engine`, `area: agents` |
| [#62 Distribution and project health](https://github.com/userfypp/phasewise/issues/62) | binaries, packaging, platforms, release tooling, CI checks | `area: release` |

Pick the group by what the change touches, not by its type: a TUI bug goes to
#83 even if it is about git output. When two groups fit, take the one whose
code has to change; mention the other in the issue. A new issue gets its
labels and its group when it is triaged. Closed issues stay in their group, so
a group also shows what was done. Do not open a new group for one issue: if
nothing fits, say so in the issue and the maintainer decides.

### Where to start

- [`good first issue`](https://github.com/userfypp/phasewise/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22): one module, a clear symptom, and an existing test that shows where a new one goes. Start here.
- [`help wanted`](https://github.com/userfypp/phasewise/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22): well specified, no rush on the maintainer's side; a bit more to read first.
- Anything else, including `type: design`: open a comment before you start, because the answer is not settled yet.

Comment on the issue you pick so nobody else starts it; a maintainer assigns it. Read [AGENTS.md](AGENTS.md#rules) for the code rules and run the quality gate before opening a pull request.

Report vulnerabilities privately, as described in the
[security policy](SECURITY.md), never in a public issue.

## Development

You need Rust 1.88 or later (edition 2024), git, and macOS or Linux. Neither
Claude Code nor Codex is needed: the tests use fake agents.

```sh
cargo build
cargo run -- --help
```

Before opening a pull request, run the quality gate. CI runs the same three
commands on Linux and macOS, and all of them must pass with no warnings:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

The code rules (dependencies, errors, git, state files, parsers, scope) are in
[AGENTS.md](AGENTS.md#rules). They apply to people and coding agents alike.
For orientation, read [Architecture](docs/ARCHITECTURE.md).

## Tests

- **Unit tests** live next to the code (`#[cfg(test)]`).
- **Integration tests** in `tests/` run the real binary against temporary git
  repositories with a repository-local identity, and use the fake agents in
  `tests/fakes/` instead of `claude` and `codex`. `FAKE_MODE` and
  `FAKE_PLAN_MODE` choose what a fake does (success, failure, slow, crash, …);
  each script lists its modes at the top.
- **Property tests** (`proptest`, in `tests/plan_properties.rs`) feed the plan
  parser arbitrary and plan-like text and check that it never panics, that line
  numbers fall inside the file and that a rendered plan parses back to what was
  rendered. A fixed number of cases keeps them fast; to look harder, run them
  with `PROPTEST_CASES=20000`.
- **Agent output fixtures** are in `tests/fixtures/`. A `.synthetic` file was
  written by hand; anything copied from a real run must be cleaned of personal
  paths, account data and MCP or plugin names first. `tests/parser_mutations.rs`
  feeds every fixture line back to both parsers with values changed, keys
  removed and the line cut short: a new fixture gets that check for free.
- **TUI snapshots** (`insta`) are in `src/tui/snapshots/`. After an intended
  change, update them with `INSTA_UPDATE=always cargo test --all-features` and
  read `git diff src/tui/snapshots` line by line before committing.
- Tests never call a real agent, never use the network and never depend on
  your global git configuration.

To try a change by hand without spending anything, point phasewise at the
fakes from a scratch repository:

```sh
export PHASEWISE_CLAUDE_BIN=/path/to/phasewise/tests/fakes/fake-claude.sh
export PHASEWISE_CODEX_BIN=/path/to/phasewise/tests/fakes/fake-codex.sh
export FAKE_LOG_DIR=$(mktemp -d)
cargo run --manifest-path /path/to/phasewise/Cargo.toml
```

### Real agents

After `cargo build`, `scripts/smoke-test.sh claude` (or `codex`) runs a
two-phase plan with the real, logged-in CLI in a throwaway repository and
checks the result. It costs a few cents. Run it when an adapter changes or a
new agent version misbehaves, and record the version that passed in
[docs/agents.md](docs/agents.md).

### README graphics

The screen captures in `assets/readme/` come from the real rendering:

```sh
cargo test --all-features -- --ignored readme_screenshots
```

The title card and the diagram of a run are drawn by
`scripts/readme-graphics.py`, which needs `pip install fonttools` and a folder
with `SourceSansPro-Regular.ttf`, `Inconsolata-Regular.ttf` and
`DejaVuSansMono.ttf` (all free to embed), passed as `--fonts DIR`. Regenerate
them after changing the look of the screens, and read the result in a browser,
in light and dark mode.

## Pull requests

- Keep each pull request focused on one change; leave out unrelated edits and
  generated files.
- Use a [Conventional Commits](https://www.conventionalcommits.org/) title:
  `feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `test:`, `ci:` or `chore:`,
  with `!` for a breaking change. Pull requests are squash-merged and the
  title becomes the commit message, so it decides the next version and the
  changelog entry. A check fails when the title does not follow the format.
  Do not edit `CHANGELOG.md` or the version in `Cargo.toml`; see
  [Releasing](docs/RELEASING.md).
- If you want credit, add yourself to [CONTRIBUTORS.md](CONTRIBUTORS.md) in the
  same pull request. It is optional.
- Add or update tests for every change in behaviour.
- Update the documentation the change affects: the
  [user guide](docs/user-guide.md) for anything a user sees, the
  [plan format](docs/plan-format.md), [Architecture](docs/ARCHITECTURE.md) for
  structure or invariants, and the reason for any choice others would ask
  about (in the code comment or the pull request description).
- New dependencies are added with `cargo add` and justified in the pull
  request description.
- Describe what you checked by hand, especially for the TUI and for anything
  that touches git, processes or signals.

## AI-assisted contributions

Welcome, under the same rules as any other: you are responsible for the
change. Understand and test what you submit, and keep it focused. Reviews
judge the result, not the tool.
