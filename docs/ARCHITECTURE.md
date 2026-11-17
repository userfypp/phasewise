# Architecture

How phasewise is built. This page is binding: a change to module boundaries,
on-disk formats or the invariants below updates it in the same commit. For what users see, read the
[user guide](user-guide.md); for the exact agent command lines,
[agents](agents.md).

## The big picture

```text
          phasewise <command>              phasewise (no command)
               cli.rs                              tui/
                  │                                  │
                  └────────────────┬─────────────────┘
                                   ▼
                                ops.rs            start, next, retry, accept, rollback, merge,
                                   │              abort, commit the plan, plan
                                   ▼
                              engine.rs           one phase: agent → checks → checkpoint
                                   │
      ┌──────────────┬─────────────┼─────────────┬──────────────┐
      ▼              ▼             ▼             ▼              ▼
   agent/        verify.rs      git.rs       state.rs      process.rs
   the agent     the plan's     the only     state.json,   process groups,
   CLIs          checks         git caller   lock          signals
```

Below these sit pure helpers with no I/O worth mentioning: `plan` (parser),
`prompt` (every prompt text), `display` and `style` (text shared by the CLI and
the TUI).

The engine and `ops` never print or draw. They report through a
`std::sync::mpsc::Sender` of `EngineEvent` (a phase) or `PlanEvent` (a planning
session). The CLI turns the events into lines on stdout; the TUI turns the same
events into its screens. Both use `display` for the words, so they always say
the same thing.

There is no async runtime: long work runs on plain threads, and every agent or
check runs as a child process in its own process group.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | Only calls `cli::main()`. |
| `cli.rs` | clap definitions, dispatch, printing events, exit codes. |
| `ops.rs` | Run-level operations shared by the CLI and the TUI: `start`, `next`, `approve`, `retry`, `accept`, `rollback`, `merge`, `abort`, `execute`, `commit_plan`, loading the current run, and `create_plan` for `phasewise plan`. `run_one` picks the agent of each phase with `engine::phase_agent`. |
| `engine.rs` | One phase cycle (`run_phase`) and the agent-following loop shared with planning (`run_agent_session`): spawn, stdin, reader thread, logs, timeout, cancel, leftovers. |
| `plan.rs` | The plan model and parser (`pulldown-cmark` and `toml`), validation with line numbers, model and effort values. |
| `prompt.rs` | Every text sent to an agent, and the extraction of a plan from an answer. Pure functions. |
| `agent/mod.rs` | The `Agent` and `AgentParser` traits, `AgentEvent`, `LaunchOptions`, `ModelInfo`. |
| `agent/claude.rs`, `agent/codex.rs` | Command lines, model lists and output parsers, one file per agent. |
| `verify.rs` | Runs the checks with a timeout and an output limit (64 MiB each) and writes `verify.log`. |
| `git.rs` | The only module that runs git. |
| `state.rs` | `state.json`, the `.phasewise` layout, atomic writes, the run lock and recovery. |
| `process.rs` | Process groups, termination, signals, the panic hook, the closed-terminal watch. |
| `display.rs` | Events into text: `EventLine`s, phase headers and summaries, status and failure labels, model names, paths, and the Markdown of `phasewise report` (`report_markdown`, pure). |
| `style.rs` | `Styler`: ANSI colour for the CLI, only on a terminal with `NO_COLOR` unset, `TERM` not `dumb` and the `color` setting not `never`. The TUI follows the same rule: when colour is off, `widgets::strip_color` clears every colour of a drawn frame and turns the dim grey into the dim attribute. |
| `settings.rs` | User settings: the registry (one table entry per setting), `settings.toml` in the user's configuration directory, validation, atomic save, the bell decision. |
| `doctor.rs` | `phasewise doctor`. |
| `tui/mod.rs` | The TUI's event loop: terminal setup and restore, the two screens, worker threads, refreshes. |
| `tui/home.rs`, `tui/app.rs` | Pure state machines for the home and run screens: a key in, a command out, no I/O. |
| `tui/shared.rs` | State and helpers both screens use (popups, footer items, spinner timing, the settings popup's state machine). |
| `tui/data.rs` | Read-only loaders: home data, run, diff, log and checks. The agent log and the verify log are read from their end (4 MiB per attempt, 1 MiB), because the run screen reloads them several times a second. |
| `tui/home_view.rs`, `tui/view.rs`, `tui/widgets.rs` | Rendering with ratatui: the home screen, the run screen, and the pieces they share. Covered by `insta` snapshots. |

Library code returns `anyhow::Result`, or a `thiserror` type where the caller
must tell errors apart (plan errors). Every message that reaches the user says
what happened and what to do next.

## A run, step by step

### Starting

`ops::start` loads and validates the plan, refuses if a run is already active,
and checks the repository: it has commits, `HEAD` is on a branch, and the
working tree is clean (the error lists up to ten dirty paths and says to commit
or stash them). It then adds `.phasewise/` to `.git/info/exclude`, creates the
branch `phasewise/<run-id>` at `HEAD`, copies the plan to the run directory and
writes `state.json` and `current`.

The run id is the UTC time plus four random hex characters:
`20261114-153012-a3f9`.

### One phase

`engine::run_phase` runs with the run lock held:

1. Build the prompt: the plan's context, the phase, one line per completed
   phase, the checks, and the reviewer's note if any.
2. Launch the phase's agent ([command lines](agents.md)). The agent is chosen per
   phase in `ops::run_one`: a phase block's `agent`, else the plan's, unless
   `run --agent` switched the run to another one, which then runs every phase.
   It is derived from the plan copy and the run's agent, never stored. Every launch counts as an
   attempt, and attempts are never reset, so no log is ever overwritten. The
   status is `running`. Events stream to the UI as they come. The attempt
   stops on the phase timeout (`phase_timeout_minutes`) or on cancel.
3. The agent failed, timed out, was interrupted, or left `HEAD` off the run
   branch: the phase is `failed`, with no automatic retry.
4. Otherwise the status is `verifying` and the checks run: the plan's global
   checks, then the phase's, in order, stopping at the first failure.
5. A check failed and retries are left (`max_retries`): the failure report
   (command, exit code, last 200 lines of output, with escape sequences
   removed and a carriage return counted as a line break) goes back to the
   agent, in the same session when its id is known, and the cycle repeats from
   step 2.
   No retries left, or the run has reached `max_cost_usd`: `failed` with
   `verify_failed`, and the tree is left as it is for the user to inspect.
   `ops` also refuses to start a phase (and `--auto` to approve one) once the
   limit is reached.
6. All checks passed: `git add -A`, then a checkpoint commit on the run branch,
   even when nothing changed. The phase is `awaiting_review`.

| From | To | When |
|---|---|---|
| `pending` | `running` | The phase starts. |
| `running` | `verifying` | The agent finished. |
| `running` | `failed` | Agent error, timeout, interruption, or `HEAD` left the run branch. |
| `verifying` | `running` | A check failed and retries are left. |
| `verifying` | `failed` | A check failed with no retries left, or the checkpoint failed. |
| `verifying` | `awaiting_review` | Every check passed and the checkpoint was committed. |
| `awaiting_review` | `done` | The user approved it. |
| `failed`, `awaiting_review` | `running` | The user retried it. |
| `failed` | `verifying` | The user accepted it (`accept`): no agent runs. |
| any later phase | `pending` | The user rolled back to an earlier phase. |

`accept` (`ops::accept`, `engine::accept_phase`) takes a `failed` phase of any
kind that the user fixed by hand: it keeps attempts, cost and session, runs the
same checks on the working tree as it is, and on success makes the checkpoint
as in step 6 (`awaiting_review`); otherwise the phase is `failed` again, with
`verify_failed` and the verify log. It resets nothing and launches no agent.

A retry resets the tree to the previous checkpoint and runs the phase again in
a fresh session, with the user's note if there is one. A rollback to phase k
resets the tree to its checkpoint and clears everything recorded for the later
phases.

### Reviewing and finishing

`ops::next` approves the phase awaiting review (it becomes `done`) and runs the
next pending one. With `--auto` it keeps going until a phase fails (`ops::Chain::All`); with
`--until N` (`Chain::Until`) it stops when phase N awaits review. The last
phase always waits for review. When the last phase is approved, the run is
`completed`. Integrating it is left to the user, who is told `phasewise merge`
and the equivalent commands.

`ops::merge` integrates a completed run, and only when the user asks
(`phasewise merge`, `m` on the run screen; the TUI runs it on a worker thread).
Under the run lock it requires status `completed` and a clean tree outside
`.phasewise/`, switches to the original branch, runs `git merge --squash <run
branch>` and commits the result (`phasewise: <plan title>` with a
`Phasewise-Run` trailer; the user's hooks and signing apply). If nothing is
staged, the work is already in the branch and no commit is made. On a conflict
it runs `git reset --hard HEAD` (safe: the tree was verified clean), stays on
the original branch and fails naming the files. The run branch is kept, nothing
is pushed and `state.json` is unchanged: a merged run stays `completed`. The
manual route still works:

```sh
git switch <original-branch> && git merge --squash phasewise/<run-id> && git commit
```

`ops::abort` ends a run at any point. If the run branch has uncommitted
changes, they are committed there first (`phasewise: abort — uncommitted
changes`), so nothing is lost. phasewise then switches back to the original
branch. The run branch is never deleted.

`next`, `retry`, `accept` and `abort` require the run branch to be checked out; `next`
also requires a clean tree.

## Writing a plan

`ops::create_plan` drives `phasewise plan` and the home screen's `p`:

1. Refuse if the output file exists and replacing it was not confirmed.
2. Run a read-only agent session ([command lines](agents.md#writing-a-plan))
   with `prompt::plan_prompt`: the plan format, how to write good phases, the
   agent's model list, and the rule that the answer must hold the plan between
   a `<<<PHASEWISE-PLAN` line and a `PHASEWISE-PLAN>>>` line.
3. Extract the text between the last pair of markers, remove a code fence
   around it, and validate it with `plan::parse`. Its `agent` must be the
   chosen one.
4. On any problem, resume the same session with the list of problems, at most
   twice.
5. Write the plan atomically, or nothing if it is still invalid. Every answer
   and draft is kept under `.phasewise/plans/<plan-id>/`.

The plan is not committed. The user reads it, then commits it, by hand or with
`c` on the home screen (`ops::commit_plan`, which commits that one file and
nothing else).

## On disk

```text
<repo>/.phasewise/                     excluded through .git/info/exclude
├── current                            id of the current run
├── rate-limits.json                   latest usage limits the agent reported
├── runs/<run-id>/
│   ├── state.json                     the run's state
│   ├── plan.md                        the plan as it was when the run started
│   ├── lock                           PID of the process changing the run
│   ├── agent.group                    process group of the running agent (see below)
│   ├── phase-NN.attempt-K.jsonl       agent output, one event per line
│   ├── phase-NN.attempt-K.stderr.log  agent stderr
│   └── phase-NN.verify.log            check output, every attempt
└── plans/<plan-id>/                   one per planning session
    ├── attempt-K.jsonl
    ├── attempt-K.stderr.log
    └── draft-K.md                     each plan extracted from an answer
```

`NN` is the phase number with two digits; `K` counts from 1.

`state.json` holds the run (`run_id`, plan path and title, agent, original
branch, run branch, `base_sha`, status `active`, `completed` or `aborted`,
timestamps, model and effort overrides) and one entry per phase: status,
attempts, session id, `start_sha` (the `HEAD` when it started), checkpoint SHA, the agent's final message (at most 2000
characters), cost, start and end times, the failure (`kind` and `detail`), the
model and effort requested, and the model the agent reported. New fields are
optional, so older files keep loading; `schema_version` stays 1.

`rate-limits.json` is per repository, not per run:
`{"agent": "claude", "seen_at": <unix seconds>, "windows": [{"name", "utilization", "resets_at", "status"}]}`.
The engine writes it (atomically, errors ignored) when a `RateLimits` agent
event differs from what is stored or the stored copy is over a minute old; the
CLI and the TUI only read it.

## Invariants

These hold everywhere. A change that breaks one is a design change.

- **git runs only from `git.rs`**, always as `git -C <root>` with the output
  captured, so every failure can name the command and its stderr.
- **phasewise never pushes, never deletes a branch, never uses `--force`, and
  never resets or commits on the original branch.** The one commit it makes
  outside the run branch is the plan file, when the user asks for it with `c`.
  That commit runs the user's hooks and signing like any commit of theirs;
  checkpoints skip both (`--no-verify --no-gpg-sign`) because they are
  phasewise's own bookkeeping.
- **The only file written outside `.phasewise/` is the user settings file**
  (`settings.toml`, see `settings.rs`), and only when the user changes a
  setting. Changes are read-modify-write under a lock file next to it
  (`settings.toml.lock`, created exclusively and removed afterwards; a lock
  older than ten seconds is the leftover of a crash and is removed; reading
  never creates it). It is written atomically; a file that is not valid TOML is never
  overwritten without keeping a copy as `settings.toml.bak`. Settings only
  fill in what environment variables, command-line flags and the plan leave
  open. The TUI keeps the settings in force in its loop and copies them to
  both screens; a change is a command (`SaveSettings`) the loop executes.
- **Reads never block writes.** Every git command runs with
  `GIT_OPTIONAL_LOCKS=0`, so the TUI's `status` and `diff` refreshes never hold
  `index.lock` while a checkpoint is committed. A command that still meets a
  held lock is retried for about 3.5 seconds before the error, which names the
  lock file and what to do. phasewise never deletes a lock file.
- **A path is a path.** `commit_file` (the plan's commit) passes
  `--literal-pathspecs`, so a plan named `a*.md` or `plan[v2].md` is not a
  pattern that also stages and commits its neighbours.
- **`retry` and `rollback` never drop a commit that is only on the run
  branch and is not the run's own.** Before `git reset --hard`,
  `Git::commits_that_would_be_lost` lists the commits between the target and
  `HEAD`, minus the run's own and minus those reachable from another ref; if
  any remain, the operation refuses before changing anything. The run's own
  are the checkpoints and, for each phase with a checkpoint, the commits from
  its `start_sha` (the `HEAD` when it started) to that checkpoint: the ones the
  agent made inside the phase. A commit the user makes between phases is
  before the next `start_sha`, so it stays protected. A failed phase has no
  checkpoint, so commits the agent made in it count as the user's.
- **`git clean` never touches `.phasewise/`**: it runs with
  `-e /.phasewise/`, and the directory is also excluded.
- **State is written atomically:** a temporary file in the same directory,
  `sync_all`, then `rename`. A path that is a symbolic link (a settings file
  kept in a dotfiles repository) is written through: the file it points to
  is replaced and the link stays.
- **One process changes a run at a time.** `runs/<id>/lock` holds the PID of
  the process running a mutating operation, created with `create_new`. A lock
  whose process is gone is stale and replaced. With a live lock, read-only
  commands show the state as it is and mutating commands refuse.
- **Interrupted work is recovered on load.** Without a live lock, a phase left
  in `running` or `verifying` becomes `failed` with `interrupted`. While an
  agent runs, `runs/<id>/agent.group` holds its process group and the start
  time of its leader (`ps -o lstart=`); the engine removes it when the agent is
  gone. If phasewise was killed without a chance to clean up, recovery stops
  that group (`TERM`, 2 seconds, `KILL`) before anything else touches the tree.
  A PID can come back for an unrelated process, so the group is signalled only
  when its leader started at the recorded time, or the leader is gone while
  other members still hold the id; when that cannot be told (`ps` missing, or
  its start time for the leader differs, which a changed boot clock can cause),
  nothing is signalled and the phase's failure names the group and the `kill`
  to run by hand. The start time is read with `TZ=UTC`. The record is written a moment after the spawn, so a kill in that moment
  leaves the agent unrecorded.
- **Every child runs in its own process group** and is stopped as a group:
  `TERM`, a grace period (5 seconds for an agent, 2 for a check), then
  `KILL`. A child dropped without being stopped
  (an early return, a panic) is killed. After a normal exit, whatever it left
  running in its group is stopped too.
- **Signals:** Ctrl-C, `SIGTERM` and `SIGHUP` set a shared cancel flag; a
  second one kills every group and exits with 130. The TUI also treats a
  changed parent process as a closed terminal, because then no signal may
  arrive.
- **Agent parsers never fail.** Invalid JSON, unknown events and unknown fields
  are ignored, and so is a known field with a value of the wrong type
  (`tests/parser_mutations.rs` mutates every fixture line to check it).
- **No network.** phasewise itself makes no connections; only the agents do.
- **Tests never run a real agent**, never use the network and never read the
  global git configuration.

## The TUI

`phasewise` without a command, on a terminal inside a git repository, opens the
TUI: the run screen when the current run is active, the home screen otherwise.
`phasewise review` opens the run screen directly.

- **One loop, two screens.** `tui/mod.rs` owns the terminal and a single event
  loop. The home state always exists; the run state exists while the
  repository has a current run, and keeps its worker and caches while the home
  screen is shown. `esc` goes from the run screen to home; `enter` or `o` goes
  back.
- **The one suspension.** The `d` key of the run screen gives the terminal to
  `git difftool` (`Git::difftool`, inherited stdio): the loop leaves raw mode
  and the alternate screen, runs the tool on a scoped thread while it keeps
  beating for the watchdog, and always enters them again, showing a failure as
  a notice.
- **State machines are pure.** `home.rs` and `app.rs` take a key and return a
  command; `mod.rs` carries the command out: it starts a worker thread, calls
  `ops`, or reloads data. This is what makes the screens testable without a
  terminal.
- **Workers.** A phase or a planning session runs on a thread that sends events
  through a channel. Every turn, the loop drains the channels, joins finished
  workers, reloads what changed (run data at most every 250 ms; the home data
  every 2 seconds, on a thread) and redraws only when something changed, or
  every 120 ms while a spinner shows. While no worker of its own runs, the
  run screen also compares a `RunStamp` (the current run and the time and
  size of its `state.json`) once a second and reloads when another process
  changed the run; if the run is gone, the screen closes. A periodic home reload is dropped if,
  when it arrives, a popup is open, a commit is running or an explicit reload
  happened since it started.
- **Pasting** uses the terminal's bracketed paste mode (on while the TUI runs,
  off on every exit path with the rest of the terminal state). Without it, a
  pasted line break is an Enter key and would submit the input. The loop turns
  an `Event::Paste` into characters typed into the text being edited, if any;
  see `shared::paste_keys`.
- **Quitting** with work in progress asks first; yes cancels every worker and
  waits for them to stop. A second Ctrl-C or `q` while stopping kills every
  process group and exits with 130, like a second signal. The terminal is
  restored on every exit path, including panics.
- **Widths are terminal columns** (`widgets::text_width`): wide characters
  take two, and truncation, padding and wrapping never split one.
- **Rendering** is snapshot-tested with `insta` and ratatui's `TestBackend`.
  The README screenshots come from the same rendering:
  `cargo test --all-features -- --ignored readme_screenshots` writes the SVGs
  in `assets/readme/`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | A usage or environment error, or a failed planning session. |
| 3 | A phase ended `failed`. |
| 130 | Stopped by a second Ctrl-C or signal. |

## Where to change what

| To… | Change |
|---|---|
| Support a new version of an agent CLI | `agent/claude.rs` or `agent/codex.rs`, their fixtures in `tests/fixtures/`, and [agents](agents.md). |
| Add a model to the Claude list | `CLAUDE_MODELS` in `agent/claude.rs`, the [user guide](user-guide.md#models-and-effort) and [agents](agents.md#models-and-effort). |
| Change what the agent is told | `prompt.rs` and its tests. |
| Add plan syntax | `plan.rs`, [plan format](plan-format.md), and the plan prompt in `prompt.rs`, which teaches the format to the agent. |
| Add or change a command | `cli.rs`, the operation in `ops.rs`, the integration tests and the [user guide](user-guide.md#commands). |
| Change how an event reads | `display.rs`; the CLI and the TUI both follow. |
| Add a key | `tui/home.rs` or `tui/app.rs`, the footer and shortcuts lists, and the [user guide](user-guide.md#keys). |
| Change the look | `tui/widgets.rs`, `tui/home_view.rs`, `tui/view.rs`, the snapshots and the README screenshots. |
