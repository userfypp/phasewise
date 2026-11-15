# Roadmap

What may come, in order of priority. Each item is a GitHub issue, which is where
the discussion happens and where progress is tracked; this page is only the map.
Nothing here is promised. An issue that makes the case for another idea is
welcome (see [Labels](../CONTRIBUTING.md#labels)).

## Next up

- [Accept the working tree of a failed phase after fixing it by hand](https://github.com/userfypp/phasewise/issues/30) (#30)

## Planned

- [Report a run as Markdown for a pull request description](https://github.com/userfypp/phasewise/issues/31) (#31)
- [Run a user command when a phase ends](https://github.com/userfypp/phasewise/issues/32) (#32)
- [Machine-readable event stream: `run --json`](https://github.com/userfypp/phasewise/issues/33) (#33)
- [A spending limit per phase](https://github.com/userfypp/phasewise/issues/34) (#34)
- [Usage limits for agents that report tokens, not cost](https://github.com/userfypp/phasewise/issues/35) (#35)
- [Sanitized captures of real agent output as test fixtures](https://github.com/userfypp/phasewise/issues/50) (#50)
- [Real-terminal tests with a pty](https://github.com/userfypp/phasewise/issues/51) (#51)
- [Dependency and licence checks in CI](https://github.com/userfypp/phasewise/issues/54) (#54)
- [Prebuilt binaries and a Homebrew formula](https://github.com/userfypp/phasewise/issues/55) (#55)

## Later

Features:

- [Per-phase timeout in the phase settings block](https://github.com/userfypp/phasewise/issues/36) (#36)
- [Open a phase's diff in the user's editor or difftool](https://github.com/userfypp/phasewise/issues/37) (#37)
- [Commands to run before a phase](https://github.com/userfypp/phasewise/issues/38) (#38)
- [`--model` for the phases of the other agent in a mixed plan](https://github.com/userfypp/phasewise/issues/39) (#39)
- [Settings: the default agent should apply to valid plans without an explicit agent](https://github.com/userfypp/phasewise/issues/40) (#40)
- [A separate diff per subagent](https://github.com/userfypp/phasewise/issues/47) (#47)

Open design questions:

- [What it means when an agent exits 0 without a final event](https://github.com/userfypp/phasewise/issues/41) (#41)
- [Should `run --agent` naming the plan's own agent force every phase](https://github.com/userfypp/phasewise/issues/42) (#42)
- [Parallel phases using git worktrees](https://github.com/userfypp/phasewise/issues/46) (#46)
- [Support another agent (Gemini CLI, opencode)](https://github.com/userfypp/phasewise/issues/48) (#48)
- [Windows support](https://github.com/userfypp/phasewise/issues/49) (#49)

Fixes and housekeeping:

- [`doctor` does not check that the plan's own agent is installed](https://github.com/userfypp/phasewise/issues/43) (#43)
- [The cost limit compares floats without tolerance and the message rounds](https://github.com/userfypp/phasewise/issues/44) (#44)
- [Take the run lock with `File::lock` (blocked on a newer MSRV)](https://github.com/userfypp/phasewise/issues/45) (#45)
- [Fuzz or property tests for the plan parser](https://github.com/userfypp/phasewise/issues/52) (#52)
- [Check the agent CLIs for changed flags on a schedule](https://github.com/userfypp/phasewise/issues/53) (#53)

phasewise will never add telemetry, update checks or any other network access
of its own.
