# Roadmap

Ideas that are not planned yet. Nothing here is promised; an issue that makes
the case for one is welcome.

- Merge a completed run from the TUI. Today phasewise shows the
  `git merge --squash` command and leaves the merge to you.
- A different agent per phase.
- Phases in parallel, using git worktrees.
- A separate diff per subagent. Today subagent activity is only labelled in the
  log.
- More agents, such as Gemini CLI or opencode.
- Spending limits per phase or per run.
- A monochrome TUI when `NO_COLOR` is set. Today `NO_COLOR` only affects
  command output.
- Sanitized captures of real agent output as test fixtures.
- Windows support.

phasewise will never add telemetry, update checks or any other network access
of its own.
