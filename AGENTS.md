# AGENTS.md

Instructions for coding agents working on phasewise. People: see
[CONTRIBUTING.md](CONTRIBUTING.md); the rules below apply to you too.

## What this is

phasewise is a Rust CLI/TUI that runs a Markdown plan with Claude Code or Codex
one phase at a time: it launches the agent headless for one phase, runs the
plan's checks, commits a checkpoint on its own branch (`phasewise/<run-id>`)
and stops for the user's review.

## Where to look

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): module map, data flow, on-disk
  layout and invariants. Binding: change it together with the code.
- [docs/agents.md](docs/agents.md): the exact command lines phasewise gives
  Claude Code and Codex, and the output formats it parses.
- [docs/RELEASING.md](docs/RELEASING.md): how versions, the changelog and releases
  are made (by tooling, from pull request titles; `scripts/release-notes.py` writes the notes).
- [docs/user-guide.md](docs/user-guide.md) and
  [docs/plan-format.md](docs/plan-format.md): the behaviour users rely on.

Source: `src/` — `cli` (clap, dispatch, exit codes), `ops` (run-level
operations shared by CLI and TUI), `engine` (one phase cycle), `plan`,
`prompt` (every prompt text), `agent/` (`claude`, `codex`), `verify`, `git`
(the only module that runs git), `state` (`state.json`, lock, recovery),
`process` (process groups, signals), `display` and `style` (text shared by CLI
and TUI), `doctor`, and `tui/`: one loop for the home and run screens in
`mod.rs`, pure state machines in `home.rs` and `app.rs`, what they share in
`shared.rs`, rendering in `home_view.rs`, `view.rs` and `widgets.rs`, and
loaders in `data.rs`. The engine and `ops` never print or draw: they send
events through an `mpsc` channel that the CLI and the TUI consume.

## Quality gate

All three must pass with no errors and no warnings before any commit:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Rules

1. Do not invent interfaces: check agent CLI options with `--help` and crate
   APIs against the documentation of the installed version.
2. Add dependencies with `cargo add` (latest stable) and justify each one in the
   commit message.
3. No `unsafe` (`#![forbid(unsafe_code)]`). No `unwrap()`/`expect()` outside
   tests, except provable invariants with a comment. No `#[allow]` without a
   comment that justifies it.
4. No async runtime: threads and `std::sync::mpsc`.
5. git runs only through the `git` binary, only from `src/git.rs`. phasewise
   never pushes, never deletes branches and never uses `--force`.
6. State files are written atomically (temporary file in the same directory,
   then `rename`).
7. Every user-facing error says what happened and what to do. No panics as UI.
8. Agent output parsers ignore unknown event types and fields; they never fail.
9. Do not widen the scope: new ideas go to `docs/ROADMAP.md`.
10. Tests never call a real agent, never use the network and never depend on
    the global git configuration.
11. The repository is in English: code, comments, docs, commit messages
    (Conventional Commits) and program output.

## Testing and trying changes

- Unit tests sit next to the code; integration tests are in `tests/` (helpers in
  `tests/common/`) and run against temporary repositories with the fake agents
  in `tests/fakes/` (`FAKE_MODE`, `FAKE_PLAN_MODE`).
- TUI rendering is covered by `insta` snapshots in `src/tui/snapshots/`. Update
  them with `INSTA_UPDATE=always cargo test --all-features` and read every
  changed snapshot; leave no `.snap.new` behind.
- `claude` and `codex` may be installed on the machine. Never run them: manual
  tries set `PHASEWISE_CLAUDE_BIN` / `PHASEWISE_CODEX_BIN` to the fakes and
  `GIT_CONFIG_GLOBAL=/dev/null`, in a scratch repository outside this one.
  `scripts/smoke-test.sh` calls a real agent and costs money: run it only when
  asked.
- README screenshots are generated from the real rendering:
  `cargo test --all-features -- --ignored readme_screenshots`. The title card
  and the diagram of a run come from `python3 scripts/readme-graphics.py`
  (needs `fonttools`).

## When behaviour changes

Update, in the same change: the tests and snapshots, the
[user guide](docs/user-guide.md) (and [plan format](docs/plan-format.md) if
plans are affected), [Architecture](docs/ARCHITECTURE.md) for structure or
invariants, and the README screenshots if a screen changed. The changelog is
written by the release tooling from commit titles; do not edit it by hand.
