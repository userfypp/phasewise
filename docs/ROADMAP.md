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

phasewise will never add telemetry, update checks or any other network access
of its own.
