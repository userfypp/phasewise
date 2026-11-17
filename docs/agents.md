# Agents

The exact command lines phasewise gives Claude Code and Codex, what it reads
from their output, and how to check a new version of either CLI. The source of
truth is `src/agent/claude.rs` and `src/agent/codex.rs`; their unit tests pin
every argument listed here.

| CLI | Tested version |
|---|---|
| Claude Code (`claude --version`) | 2.1.325 |
| Codex (`codex --version`) | codex-cli 0.160.0 |

Agent CLIs change often. If a flag below disappears or changes meaning,
phasewise breaks in a way its own tests cannot catch, because the tests use
fake agents. Re-check this page whenever you upgrade either CLI
([how](#checking-a-new-cli-version)).

## What both agents have in common

- **One process per attempt.** The agent runs headless in the repository root,
  in its own process group, so phasewise can stop it and everything it started.
- **The prompt goes on stdin**, which is then closed. No prompt is passed as
  an argument.
- **Output is one JSON event per line.** Each raw line is appended to
  `.phasewise/runs/<run-id>/phase-NN.attempt-K.jsonl` (planning sessions use
  `.phasewise/plans/<plan-id>/attempt-K.jsonl`) and translated into
  phasewise's own events. stderr goes to the matching `.stderr.log`.
- **The parsers never fail.** Lines that are not JSON objects, unknown event
  types and unknown fields are ignored, so a newer CLI that adds events keeps
  working. A line over 16 MiB is saved to the log cut and not parsed, and a
  negative or non-numeric `total_cost_usd` counts as no cost.
- **The environment is inherited unchanged.** The agent sees your `PATH`, your
  credentials and your configuration, exactly as when you run it yourself.
- **Which program runs:** `claude` and `codex` from `PATH`, or the paths in
  `PHASEWISE_CLAUDE_BIN` and `PHASEWISE_CODEX_BIN`.
- **Model and effort** are passed only when set (in the plan, a phase or the
  run). `default` means no flag, so the agent uses its own default.
- **`agent_args`** from the plan's front matter go last, as given (so they can
  override the permission or sandbox options phasewise sets; read them as you
  read `verify`), on phase
  sessions of the run's agent only: a phase that a `phasewise` block gives to
  the other agent gets none. Planning sessions run before there is a plan, so they take
  none.

## Claude Code

### Running a phase

```sh
claude -p --output-format stream-json --verbose \
  --permission-mode acceptEdits --permission-prompts none \
  --allowedTools Read,Edit,Write,Bash \
  --disallowedTools "Bash(git push *)" \
  [--model <model>] [--effort <effort>] [--resume <session-id>] [agent_args...]
```

| Flag | Why |
|---|---|
| `-p --output-format stream-json --verbose` | Headless, with one JSON event per line. |
| `--permission-mode acceptEdits` | File edits are accepted without asking. |
| `--permission-prompts none` | Anything that would ask for permission is denied instead, so a phase never waits for an answer nobody will give. |
| `--allowedTools Read,Edit,Write,Bash` | The tools a phase needs, pre-approved. |
| `--disallowedTools "Bash(git push *)"` | A best-effort rule against `git push`. The agent has `Bash`, so this is not a guarantee; see [SECURITY.md](../SECURITY.md). |
| `--resume <session-id>` | Only when a check failed and the phase continues in the same session. |

### Writing a plan

`phasewise plan` runs a read-only session:

```sh
claude -p --output-format stream-json --verbose \
  --permission-mode dontAsk --permission-prompts none \
  --tools Read,Grep,Glob --strict-mcp-config \
  --allowedTools Read,Grep,Glob \
  --disallowedTools Edit,Write,NotebookEdit,Bash,WebFetch,WebSearch \
  [--model <model>] [--effort <effort>] [--resume <session-id>]
```

`--tools` leaves only those three built-in tools in the session, and
`--strict-mcp-config` without `--mcp-config` loads no MCP server. Without them,
a real session also offered tools that need no permission, such as subagents,
worktrees and schedulers. `--resume` is used for the repair rounds when the
first answer has no valid plan.

### What phasewise reads

| Event | Use |
|---|---|
| The first event with a `session_id` (normally `system`/`init`) | The session id, for `--resume`, and `model`: the model the session really uses, shown in the run screen and in `phasewise status`. |
| `assistant`, `text` blocks | Agent messages. |
| `assistant`, `tool_use` blocks | Tool lines. The detail is the first of `file_path`, `path`, `command` (first line), `pattern`, `url` or `description` in the input, at most 200 characters. A non-null `parent_tool_use_id` marks a subagent. |
| `system`/`api_retry` | A retry notice (`attempt`, `max_retries`, `error`). |
| `system`/`permission_denied` and the result's `permission_denials` | A permission-denied notice, once per tool use id. |
| `result` | The end of the attempt. It succeeded when `subtype` is `success` and `is_error` is false. The summary is `result`, or the `errors` list on error subtypes. The cost is `total_cost_usd`. |
| `rate_limit_event` | The usage limits. From `rate_limit_info.unifiedWindows` (a map of window name, such as `five_hour` or `seven_day`, to `utilization` from 0 to 1, `resetsAt` in Unix seconds and `status`), or, when that is absent, the single window `rateLimitType` with the top-level `resetsAt` and no utilization. A `status` that is not `allowed` (for example `allowed_warning`) is shown as it comes. phasewise keeps the latest values in `.phasewise/rate-limits.json`; the home screen, the run screen and `phasewise status` show them. |

Everything else is ignored. Real runs also emitted `system`/`thinking_tokens`.

### Login

`phasewise doctor` runs `claude auth status` and `codex login status`, with a
10-second timeout and no model call. Exit status 0 means logged in; anything
else shows a warning with the command that signs in. Claude Code documents exit
status 1 for "not logged in"; Codex documents only 0 for "logged in". The
warning never makes `doctor` fail, because an API key or a cloud provider can
work without a login.

### Models and effort

- `--model` takes an alias or a full model name. phasewise offers the full
  names of the current models, so the choice names one exact model:

  | Name | `--model` | Offered as |
  |---|---|---|
  | Fable 5.2 | `claude-fable-5-2` | Most capable, for the hardest work |
  | Opus 5.5 | `claude-opus-5-5` | Complex, multi-step work |
  | Sonnet 5.5 | `claude-sonnet-5-5` | Fast and capable, for most coding |
  | Haiku 5.5 | `claude-haiku-5-5` | Fastest and cheapest, for simple phases |

  Aliases (`opus`, `sonnet`, …) and `[1m]` variants still work when typed.
- Claude Code has no non-interactive command that lists models, so this list
  is built into phasewise (`CLAUDE_MODELS` in `src/agent/claude.rs`). Update
  it when new models are released. Settings managed by an organization can
  restrict which models an account may use.
- `--effort` takes `low`, `medium`, `high`, `xhigh` or `max`. A model without
  the chosen level uses the closest lower one.

## Codex

### Running a phase

```sh
codex exec --json --sandbox workspace-write \
  [-m <model>] [-c model_reasoning_effort="<effort>"] - [agent_args...]
```

When a check failed and the phase continues in the same session:

```sh
codex exec --json --sandbox workspace-write \
  [-m <model>] [-c model_reasoning_effort="<effort>"] \
  resume <thread-id> - [agent_args...]
```

| Flag | Why |
|---|---|
| `exec --json` | Headless, with one JSON event per line. |
| `--sandbox workspace-write` | The agent can write in the repository. |
| `-` | Read the prompt from stdin. |

`--sandbox`, `-m` and `-c` must come before the `resume` subcommand, which
rejects them after it. For the same reason, `agent_args` that only `exec`
accepts (such as `--sandbox`) fail when a phase is resumed: they come after
`resume`.

### Writing a plan

```sh
codex exec --json --sandbox read-only \
  [-m <model>] [-c model_reasoning_effort="<effort>"] [resume <thread-id>] -
```

Codex explores the project with shell commands such as `rg --files` and `cat`,
which the read-only sandbox allows.

### What phasewise reads

| Event | Use |
|---|---|
| The first event with a `thread_id` (`thread.started`) | The session id, for `resume`. Codex reports no model, so phasewise shows the requested one. |
| `item.started`, `item.updated`, `item.completed` | Reported once per item id. `agent_message`: a message, once completed and not empty; the last one is the summary. `command_execution`: a `Shell` line. `file_change`: an `Edit` line with the paths. `mcp_tool_call`: `MCP server/tool`. `web_search`: the query. `collab_tool_call`: a subagent line. `error`: an error line. |
| `turn.completed` | The end of the attempt, successful. Codex reports tokens, not cost, so no cost is shown. |
| `turn.failed` | The end of the attempt, failed, with `error.message`. |
| `error` | An error line. |

`reasoning`, `todo_list` and everything else are ignored. Codex reports no
usage limits in its event stream, so phasewise shows none for it.

### Models and effort

- `codex debug models` prints the model catalog as JSON. phasewise runs it with
  a 10-second timeout and lists the entries whose `visibility` is `list`, in
  catalog order: `slug` (the value for `-m`), `display_name`, `description`
  and `supported_reasoning_levels`. If the command fails, any name `codex -m`
  accepts can still be typed.
- Effort goes through `-c model_reasoning_effort="<level>"`.

## Checking a new CLI version

1. Note the versions: `claude --version`, `codex --version`.
2. Read `claude --help`, `codex exec --help` and `codex exec resume --help`,
   and confirm every flag on this page still exists with the same meaning.
3. Run these with an empty stdin. They parse the arguments and exit before
   any model call, so they cost nothing:

   ```sh
   claude -p --output-format stream-json --verbose --permission-mode acceptEdits \
     --permission-prompts none --allowedTools Read,Edit,Write,Bash \
     --disallowedTools "Bash(git push *)" < /dev/null
   codex exec --json --sandbox workspace-write - < /dev/null
   codex exec --json --sandbox workspace-write -m <model> \
     -c model_reasoning_effort="high" resume --help
   ```

   The first must fail only because no prompt was given, the second only
   because no prompt came on stdin, and the third must print the help of
   `resume`. An error about an argument means a flag changed.
4. Run the real smoke test for that agent, as described in
   [CONTRIBUTING](../CONTRIBUTING.md#real-agents). It costs a few
   cents.
5. Update the tested versions at the top of this page and in the README.

## Checks so far

- **2026-11-13:** every flag above checked with `--help` and parse-only runs
  (Claude Code 2.1.296, Codex 0.160.0).
- **2026-11-14:** models, effort and planning sessions checked with Claude
  Code 2.1.325 and Codex 0.160.0. Real planning sessions with both agents
  produced valid plans at the first answer, with no permission denials.
- **2026-11-14:** the real smoke test passed with both agents: two phases each,
  8 of 8 checks, about $0.13 in total for Claude Code. The streams contained
  only the events documented above, plus the ignored ones.
