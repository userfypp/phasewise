# User guide

[README](../README.md) · [Plan format](plan-format.md) · [Security](../SECURITY.md)

On this page: [Install](#install) · [How it works](#how-it-works) ·
[Home screen](#the-home-screen) · [Writing a plan](#writing-a-plan) ·
[Running phases](#running-phases) · [Reviewing](#reviewing-a-phase) ·
[Finishing a run](#finishing-a-run) · [Models and effort](#models-and-effort) ·
[Commands](#commands) · [Files and settings](#files-and-settings) ·
[Troubleshooting](#when-something-goes-wrong) · [Keys](#keys)

## Install

You need git, macOS or Linux, and at least one agent installed and logged in:
[Claude Code](https://code.claude.com/docs) (`claude`) or
[Codex](https://github.com/openai/codex) (`codex`). phasewise never logs in for
you.

With [Rust](https://rustup.rs) 1.88 or later, from a clone of the repository:

```sh
cargo install --path .
```

Then run `phasewise doctor` inside your project. It checks git, both agents
(installed, version and whether they are logged in) and whether the repository
is ready (a first commit, a branch checked out, a clean working tree). A login
that is not confirmed is a warning, not an error.

## How it works

1. **Plan.** A plan is one Markdown file, `phasewise.md`: some shared context
   and a list of phases, each with its instructions and the commands that
   prove it worked. The agent can write it for you from one sentence.
2. **Run.** phasewise creates the branch `phasewise/<run-id>` from your current
   commit and runs the agent headless on phase 1 only. Then it runs the
   phase's checks. If they fail, the agent gets the failure and tries again.
3. **Checkpoint.** When the checks pass, phasewise commits the phase on the run
   branch and stops.
4. **Review.** You look at the diff, the agent's log and the check output, then
   approve the phase (the next one starts), retry it with a note, or roll back.
5. **Merge.** When every phase is done, you merge the run branch into yours:
   phasewise can do it when you ask (`merge`, or `m` on the run screen).

Your own branch never changes during a run, and every phase is a commit you can
go back to.

## The home screen

Run `phasewise` with no arguments in your repository. If a run is in progress,
it opens that run; otherwise it opens the home screen:

- **Header**: the repository and the current branch.
- **Plan**: the plan's title and phases, with the number of checks of each
  phase and its own model when it sets one. A missing plan says how to create
  one; an invalid plan lists its errors with their lines. The screen notices
  when you edit the file and updates on its own.
- **Agent, model and effort** used to write the plan and to run it. Values that
  come from the plan say `from the plan`.
- **Notices**: a plan that is not committed yet (a run needs a clean working
  tree), with the git command to commit it yourself; or a warning when you are
  still on the branch of an earlier run.

The line at the bottom shows the keys that make sense right now; `?` lists all
of them.

| Key | Action |
|---|---|
| `enter` | Start a run with the plan (asks to commit it first if needed). With a run in progress: open it. |
| `c` | Commit the plan file alone, with the message `Add phasewise plan`. Your hooks and signing apply. |
| `p` | Write a new plan with the agent (asks before replacing an existing file). |
| `a` | Switch the agent between Claude Code and Codex. |
| `m` | Choose the model. |
| `e` | Choose the effort. |
| `o` | Open the last run (completed or abandoned) to look at it. |
| `?` | Show every key. |
| `q` | Quit (asks first if an agent is working). |

The agent, model and effort you choose here apply to the plan you write and to
the run you start, on top of what the plan says. They are not saved to the
plan file.

## Writing a plan

### With the agent

On the home screen press `p` and describe the goal in one sentence, or run:

```sh
phasewise plan "add a dark mode toggle to the settings page"
```

The agent reads the project and writes `phasewise.md`. It works read-only: it
can read and search files but cannot change, build or run anything. phasewise
checks the result and, if it is not a valid plan, asks the same session to fix
it (twice at most). The file is written only when the plan is valid. Writing a
plan has a 20-minute limit; `esc` stops it.

Read the plan before running it, especially the `verify` commands: they run on
your machine. Edit anything you like; the home screen picks up the changes.

The agent also chooses the model and effort of every phase, aiming for the
cheapest that will do the job: a cheap model and low effort for mechanical
work, the strongest for design or risky changes. It writes the choice in each
phase's `phasewise` block, so you see it when you review the plan, and you can
change it there or override it from the home screen or the command line.

`phasewise plan` options: `--agent`, `--model`, `--effort` (these two choose
the model and effort of the planning session itself, not of the phases),
`--output PATH` (default `phasewise.md`) and `--force` to replace an existing
file. The agent's raw output and every draft stay in `.phasewise/plans/<id>/`.

### By hand

`phasewise init` creates an example `phasewise.md` to edit. The short version
of the format:

````markdown
+++
verify = ["cargo test"]
+++

# Add a stats subcommand

Shared context sent with every phase: what the project is, the rules.

## Phase 1: Define the subcommand

Add `stats` with a `--json` flag that prints `not implemented`.

```verify
cargo run -q -- stats --help
```

## Phase 2: Implement it

Print a summary, or one JSON object with `--json`.
````

Every `##` heading starts a phase. Its `verify` block lists the commands that
must succeed after that phase; `verify` in the front matter runs after every
phase. The full reference, with every setting and validation error, is
[Plan format](plan-format.md). `phasewise check` validates a plan and lists its
phases and checks without running anything.

## Running phases

A run needs a git repository with at least one commit, a branch checked out and
a clean working tree (phasewise's own `.phasewise/` folder does not count).
Start it with `enter` on the home screen or with `phasewise run`.

For each phase:

1. The agent gets the shared context, a summary of the phases already done, the
   phase's instructions and the list of checks it will have to pass. It works
   without asking for permission: Claude Code may read and edit files and run
   shell commands; Codex runs in its `workspace-write` sandbox.
2. When it finishes, the global checks and then the phase's checks run, in the
   repository root, each with a time limit (10 minutes by default).
3. If a check fails, the agent gets the failing command and the end of its
   output and tries again in the same session, up to `max_retries` times (1 by
   default).
4. When everything passes, phasewise commits the phase on the run branch and
   stops: the phase is **awaiting review**.

A phase **fails** when the checks still fail after the retries, the agent
reports an error, the phase takes longer than `phase_timeout_minutes` (60 by
default), you interrupt it, or the checkpoint commit cannot be created. The
working tree is left as the agent left it, so you can look at what happened.

## Reviewing a phase

The run screen lists the phases on the left and shows the selected one on the
right: its state, attempts, changed files, cost (Claude Code only) and the
model the agent used, then three views:

- **Diff**: the changed files with their line counts, then the diff. While a
  phase runs, the diff is live.
- **Log**: what the agent did, attempt by attempt: messages, files edited,
  commands run, errors.
- **Verify**: the output of every check.

The phase that starts running is selected by itself, so its progress shows
without a key. Then decide:

| Key | Action |
|---|---|
| `n` | Approve the phase and run the next one. |
| `r` | Retry: go back to the checkpoint before the phase and run it again in a new session. You can add a note for the agent. |
| `b` | Roll back to the selected phase's checkpoint: later phases are discarded and become pending again. |
| `m` | Merge a completed run into your branch (see Finishing a run). |
| `x` | Interrupt the phase that is running. |
| `A` | Abandon the run (see below). |
| `esc` | Back to the home screen. The run keeps going if a phase is running. |

The same actions exist as commands: `phasewise next`, `retry --note "…"`,
`rollback N`, `merge`, `diff [N]` and `status`.

**Abandoning a run** stops it for good. Uncommitted changes are saved in a
commit on the run branch, which is kept, and you go back to your original
branch. From the command line: `phasewise abort`.

## Finishing a run

When the last phase is approved, the run is complete. Press `m` on the run
screen, or run `phasewise merge` (`--yes` skips the question), to bring the work
into your branch. phasewise switches to your original branch, squashes the run
branch into it and commits the result as `phasewise: <plan title>` with a
`Phasewise-Run: <run-id>` trailer. Your git hooks and signing apply, as for any
commit of yours.

- The working tree must be clean (outside `.phasewise/`): commit or stash first.
- If the work is already in your branch (you merged it before), phasewise says
  there is nothing to merge.
- If the squash conflicts, phasewise restores the tree, stays on your branch and
  names the conflicting files. Resolve it by hand:

```sh
git switch main && git merge --squash phasewise/<run-id> && git commit
```

(with your original branch instead of `main`). phasewise only merges when you
ask with `merge` or `m`, and it never pushes or deletes branches. The run
branch is kept: delete it yourself when you no longer need it.

## Models and effort

`m` and `e` on the home screen, `--model` and `--effort` on `run`, `next`,
`retry` and `plan`, or `model` and `effort` in the plan (for the whole plan or
one phase) choose them. The most specific wins: the command line or home screen
choice, then the phase, then the plan, then the agent's own default.

For Claude Code, phasewise offers:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../assets/readme/home-models-dark.svg">
  <img src="../assets/readme/home-models-light.svg" alt="The model picker on the home screen: Default from the plan, the four Claude models with what each is for, and Other… to type any name.">
</picture>

| Model | ID | Use it for |
|---|---|---|
| Fable 5.2 | `claude-fable-5-2` | The hardest work; the most capable and the most expensive. |
| Opus 5.5 | `claude-opus-5-5` | Complex, multi-step work. |
| Sonnet 5.5 | `claude-sonnet-5-5` | Most coding: fast and capable. |
| Haiku 5.5 | `claude-haiku-5-5` | Simple phases: the fastest and cheapest. |

Any other name `claude --model` accepts works too: aliases such as `opus`
(which follow Claude Code's latest version), or `claude-opus-5-5[1m]` for a
1M-token context. Choose **Other…** in the list to type one. Your account
decides which models you can use.

For Codex, the list comes from your Codex installation (`codex debug models`),
with the effort levels each model supports.

Effort is `low`, `medium`, `high`, `xhigh` or `max`: more effort means more
thinking, time and cost. `phasewise models` prints both lists. After a phase
runs, the run screen and `phasewise status` show the model Claude Code actually
used.

## Commands

Everything the screens do is also a command, for scripts and for use without a
terminal UI.

| Command | What it does |
|---|---|
| `phasewise` | Opens the home screen, or the run in progress. |
| `phasewise plan [GOAL]` | The agent writes the plan for GOAL (read-only). |
| `phasewise init` | Creates an example `phasewise.md` (never overwrites). |
| `phasewise check [PLAN]` | Validates the plan and lists phases and checks. |
| `phasewise doctor` | Checks git, the agents and the repository. |
| `phasewise run [PLAN]` | Starts a run and runs phase 1. `--agent`, `--model`, `--effort`, `--auto`. |
| `phasewise next` | Approves the phase awaiting review and runs the next one. `--auto`, `--model`, `--effort`. |
| `phasewise retry` | Runs the current phase again from its starting checkpoint, discarding uncommitted changes. `--note`, `--model`, `--effort`. |
| `phasewise rollback N` | Goes back to the checkpoint of phase N (0 = the start). `--yes` skips the question. |
| `phasewise merge` | Squashes a completed run into the original branch and commits it. `--yes` skips the question. |
| `phasewise status` | The run's cost so far (against `max_cost_usd`, if set) and a table of phases: state, attempts, files, cost, model. |
| `phasewise diff [N]` | Prints the diff of phase N (default: the last one with a checkpoint). |
| `phasewise review` | Opens the run screen. |
| `phasewise models` | Lists the models and effort levels of each agent. `--agent`. |
| `phasewise abort` | Abandons the run and returns to the original branch. |

`--auto` approves every phase that passes its checks and continues until the
last phase or the first failure; the last phase still waits for your review.
`--model default` and `--effort default` remove a stored choice.

Exit codes: `0` success, `1` usage or environment error, `3` a phase ended
failed, `130` stopped by a second Ctrl-C.

## Files and settings

phasewise keeps everything in `.phasewise/` at the repository root and adds it
to `.git/info/exclude`, so it is never committed:

```text
.phasewise/
  current                  id of the latest run
  runs/<run-id>/
    state.json             phases, attempts, checkpoints, costs
    plan.md                the plan as it was when the run started
    phase-01.attempt-1.jsonl        the agent's raw output
    phase-01.attempt-1.stderr.log   the agent's error output
    phase-01.verify.log             output of the checks
  plans/<id>/              raw output and drafts of each `phasewise plan`
```

You can delete `.phasewise/` when no run is in progress.

Environment variables:

| Variable | Effect |
|---|---|
| `PHASEWISE_CLAUDE_BIN`, `PHASEWISE_CODEX_BIN` | Use another binary for the agent. |
| `PHASEWISE_PHASE_TIMEOUT_SECONDS` | Overrides `phase_timeout_minutes` for every phase. |
| `PHASEWISE_MAX_COST_USD` | Overrides `max_cost_usd` (the run's spending limit). |
| `PHASEWISE_PLAN_TIMEOUT_SECONDS` | Time limit for writing a plan (default 1200). |
| `NO_COLOR` | Turns off colour in command output and in the screens (bold and dim stay). |

## When something goes wrong

- **"The working tree has uncommitted changes"**: commit or stash them; a run
  must start from a clean tree. On the home screen, `c` commits the plan file.
- **A phase failed**: read the Log and Verify views, then `r` to retry with a
  note. `phasewise status` shows the full failure message. A retry starts again
  from the checkpoint before the phase and discards everything uncommitted,
  including edits you made by hand, so put a fix you want to keep in the note.
- **"another git process is using the repository"**: an editor or another
  terminal is running git. phasewise waits a few seconds; if the lock stays,
  close that tool, or delete `.git/index.lock` if no git command is running,
  then retry.
- **phasewise or the terminal was closed during a phase**: the agent is
  stopped, and the next phasewise command marks the phase as failed
  (interrupted). Retry it.
- **Ctrl-C**: in commands, the first one stops the agent and marks the phase
  interrupted; a second one kills everything phasewise started and exits with
  130. On the screens, Ctrl-C works like `q`; while the screen says
  "Stopping…", a second Ctrl-C or `q` does the same as in commands.
- **"is not installed"** next to an agent: install it, or point phasewise at it
  with `PHASEWISE_CLAUDE_BIN` / `PHASEWISE_CODEX_BIN`. Then log in with the
  agent's own command.

## Keys

**Home screen**: `enter` start or open the run · `c` commit the plan · `p` new
plan · `a` agent · `m` model · `e` effort · `o` last run · `?` all keys · `q`
quit. While a plan is being written: `esc` stops it.

**Run screen**: `j`/`k` or arrows select a phase · `tab` next view ·
`PgUp`/`PgDn` scroll · `n` approve · `r` retry · `b` roll back · `m` merge a
completed run · `x` interrupt · `A` abandon · `esc` home · `?` all keys · `q` quit.

**Lists and questions**: arrows or `j`/`k` move, `enter` or the row's number
chooses, `esc` cancels; `y`/`n` answer confirmations. In text fields, `Ctrl-U`
clears and `Ctrl-W` deletes a word.
