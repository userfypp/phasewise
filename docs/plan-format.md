# Plan format

A plan is one Markdown file, by default `phasewise.md` at the repository root.
`phasewise init` creates a starter one and `phasewise check` validates it
without running anything.

## Structure

```text
+++ ... +++        optional TOML front matter (settings)
# Plan title       optional level-1 heading
shared context     everything before the first `##` heading
## Phase heading   each level-2 heading opens a phase
phase text         instructions for the agent
```verify ...```   optional checks for that phase
```

## Front matter

Optional. It only counts if line 1 of the file is exactly `+++`; it ends at the
next line that is exactly `+++`. The content is TOML. Unknown keys are errors.
Every key is optional.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `agent` | string | `"claude"` | Agent that runs the phases: `"claude"` or `"codex"`. |
| `verify` | array of strings | `[]` | Commands run after every phase. |
| `max_retries` | integer >= 0 | `1` | Automatic retries when verification fails. |
| `phase_timeout_minutes` | integer, 1 to 525600 | `60` | Time limit for the agent in one phase; a phase's settings block can override it. |
| `verify_timeout_seconds` | integer, 1 to 31536000 | `600` | Time limit for each verification command. A command that writes more than 64 MiB is stopped and fails. |
| `max_cost_usd` | number > 0 | unset | Spending limit for the whole run, in US dollars; see [Spending limit](#spending-limit). |
| `agent_args` | array of strings | `[]` | Extra arguments for the agent command line. |
| `model` | string | unset | Model for every phase; see [Model and effort](#model-and-effort). |
| `effort` | string | unset | Effort for every phase: `low`, `medium`, `high`, `xhigh` or `max`. |

## Spending limit

`max_cost_usd` caps what a run may spend. phasewise adds up the cost each
phase's agent reports and checks it at two points: it does not retry a failed
check once the run has reached the limit, and it does not start the next phase
(in `--auto`, it stops before approving the one under review). Cost is checked
between agent sessions, so a session that is already running can go over the
limit. What a discarded attempt spent (`retry`, `rollback`) stays counted;
`retry` and `next` refuse before changing anything once the limit is reached.
Only Claude Code reports a cost; with Codex the limit never triggers. In a plan
that gives some phases to Codex (see [Agent per phase](#agent-per-phase)), the
limit bounds only the phases that report a cost.

`PHASEWISE_MAX_COST_USD` overrides the limit for one command, for example to
finish a run that stopped at its limit.

## Phases and titles

- Every level-2 heading (`## Title`, or a setext heading underlined with
  `---`) opens a phase. Phases are numbered 1, 2, 3... by order of appearance.
- The title is the heading text without the `#` markers. A leading
  `Phase <number>:` (any case, any spacing) is removed, so
  `## Phase 2: Convert utils` becomes `Convert utils`. A word for "phase",
  "step", "stage" or "part" in a common language (`Fase`, `Paso`, `Étape`,
  `Schritt`, `フェーズ`...) plus the phase's own number and a separator is
  removed too: `:`, `：`, `-`, `–` or `—`, or `.` or `)` followed by a space
  (`## Fase 2: X`, `## Étape 2 — X`, `## Step 2) X`). The bare number
  (`## 2. X`, `## 2) X`) is removed as well. Other words are never removed
  (`## Python 3: migrate` stays as written), nor is a prefix whose number is
  not the phase's position, or one that would leave nothing (`## Phase 1:`).
- A `##` inside a fenced code block is not a heading.
- A title must not be empty (`##` with nothing after it) and must be unique, ignoring case.
- Level-3 and deeper headings (`###`) stay inside the phase body.
- The body is everything after the heading up to the next phase, without the
  `verify` blocks. It must not be empty.

## Shared context and plan title

Everything between the front matter and the first phase is the shared context
and is sent to the agent in every phase. The first level-1 heading in that
region is the plan title; it is not part of the context.

## Verify blocks

A fenced code block whose info string is exactly `verify` lists commands that
must succeed for the phase to pass: one command per line. Empty lines and lines
starting with `#` are ignored. The block is removed from the text the agent
receives. A `verify` block before the first phase is an error.

The commands of a phase run after the global `verify` commands from the front
matter.

## Model and effort

`model` and `effort` can be set for the whole plan (front matter) and for one
phase, with a fenced block whose info string is exactly `phasewise`:

````markdown
## Phase 3: Hard refactor

```phasewise
model = "claude-opus-5-5"
effort = "high"
```

Refactor the parser.
````

The block is TOML with only the keys `agent`, `model`, `effort` and
`phase_timeout_minutes`, all optional (`agent` is described in
[Agent per phase](#agent-per-phase)). `phase_timeout_minutes` (integer, 1 to
525600, like the front matter key) sets the agent's time limit for that phase
only; `PHASEWISE_PHASE_TIMEOUT_SECONDS` still overrides it. It is
removed from the text the agent receives, like `verify` blocks. A phase can have
one block, and it must come after a phase heading.

- A model is a non-empty name of at most 100 characters, without whitespace or
  control characters and not starting with `-`. `agent_args` entries must not
  contain a NUL byte. List the names with `phasewise models`.
- `default` is not a value in a plan: remove the key to use the agent's own
  default.
- Precedence, highest first: `--model` / `--effort` of `run`, `next` or `retry`
  (stored in the run), the phase block, the front matter, nothing (no flag is
  passed).
- The plan's values (block and front matter) apply only when the run uses the
  plan's `agent`. With `run --agent codex` on a Claude plan they are ignored,
  because a model name belongs to one agent.

## Agent per phase

A phase's `phasewise` block may also set `agent` (`"claude"` or `"codex"`) to run
that phase with a different agent than the plan's:

````markdown
## Phase 2: Review the change

```phasewise
agent = "codex"
```
````

Every phase without `agent` uses the plan's `agent`. The agent of a phase is
derived from the plan; it is not stored in the run.

- `run --agent` beats the phase blocks: when it picks an agent other than the
  plan's, that agent runs every phase.
- A model name belongs to one agent, so each setting applies only to its owner.
  A phase block's `model` and `effort` apply to the phase's own agent (its
  `agent`, else the plan's). The front matter's `model` and `effort` apply only
  to phases run by the plan's `agent`. `agent_args` go only to phases run by the
  run's agent.
- `--effort` applies to every phase. `--model` applies only to phases run by the
  run's agent: `next` and `retry` refuse it when the phase to run uses another
  agent, and nothing is stored.
- Codex phases report no cost, so `max_cost_usd` bounds only the phases that do.
- A plan that sets `agent` in a phase block is rejected by older phasewise
  versions (unknown key); remove those lines before going back to one.
- `phasewise plan` drafts use a single agent: a draft with a phase `agent` that
  differs from the requested one is sent back for a fix.

## What the agent receives

For each phase the agent gets the shared context (title excluded), a summary of
the phases already completed, the phase body (title and instructions, without
`verify` blocks) and the list of checks it will have to pass. It does not
receive the other phases' instructions.

## Validation errors

`phasewise check` reports every problem as `<file>:<line>: <message>` (lines are
1-based, counted in the original file including the front matter), sorted by
line, and exits with code 1:

- front matter that is not closed (line 1);
- unknown key (named in the message), invalid TOML or wrong value type;
- unknown `agent`, in the front matter or in a phase block (the valid values are listed);
- `phase_timeout_minutes` (in the front matter or a phase block) or `verify_timeout_seconds` below 1 or above a year, or `max_cost_usd` not above 0;
- a plan with no phases;
- a phase with an empty title or an empty body;
- duplicate phase titles (reported at the second one, with the first one's line);
- a `verify` block before the first phase;
- an invalid `model` or `effort`, or the value `default`;
- a `phasewise` block before the first phase, a second one in the same phase,
  an unknown key in it (named in the message) or an invalid value.

## Full example

````markdown
+++
agent = "claude"
verify = ["cargo test"]
max_retries = 1
phase_timeout_minutes = 60
verify_timeout_seconds = 600
+++

# Add a stats subcommand

Shared context: a clap-based Rust CLI. Do not add dependencies.

## Phase 1: Define the subcommand

Add `stats` with a `--json` flag that prints `not implemented`.

```verify
cargo run -q -- stats --help
```

## Phase 2: Implement it

Print a summary, or one JSON object with `--json`.

### Constraints

- Output goes to stdout.
````

More plans are in [`examples/plans/`](../examples/plans/).

## Environment variables

`PHASEWISE_PHASE_TIMEOUT_SECONDS` overrides `phase_timeout_minutes` for every
phase. It and the other variables phasewise reads are listed in the
[user guide](user-guide.md#files-and-settings).
