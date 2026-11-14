# Roadmap

Ideas that are not planned yet. Nothing here is promised; an issue that makes
the case for one is welcome.

- `--model` for the phases of the other agent in a plan that mixes agents.
- Phases in parallel, using git worktrees.
- A separate diff per subagent. Today subagent activity is only labelled in the
  log.
- More agents, such as Gemini CLI or opencode.
- A spending limit per phase, and limits for agents that report tokens instead of cost.
- Sanitized captures of real agent output as test fixtures.
- Windows support.
- Take the run lock with `File::lock` (stable since Rust 1.89, above the current
  MSRV). It would close a narrow race in the stale-lock takeover: three
  processes in a window of microseconds can end up holding the run together.

phasewise will never add telemetry, update checks or any other network access
of its own.
