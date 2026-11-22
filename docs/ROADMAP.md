# Roadmap

What may come. Each item is a GitHub issue, which is where the discussion
happens and where progress is tracked; this page is only the map. Nothing here
is promised. An issue that makes the case for another idea is welcome (see
[Labels](../CONTRIBUTING.md#labels)).

## How issues are organised

Every issue belongs to one of eight groups, shown below, and carries one
`type:` label, one or two `area:` labels and, once planned, a `priority:`
label. The group is the `epic` issue that holds it as a sub-issue; GitHub shows
how many of its issues are done. The rules for choosing a group are in
[CONTRIBUTING.md](../CONTRIBUTING.md#groups).

Priority decides the order, whatever the group:

- [`priority: high`](https://github.com/userfypp/phasewise/issues?q=is%3Aissue+is%3Aopen+label%3A%22priority%3A+high%22): next up.
- [`priority: medium`](https://github.com/userfypp/phasewise/issues?q=is%3Aissue+is%3Aopen+label%3A%22priority%3A+medium%22): planned.
- [`priority: low`](https://github.com/userfypp/phasewise/issues?q=is%3Aissue+is%3Aopen+label%3A%22priority%3A+low%22): no rush.

Bugs are not listed here: they live in their group and are ordered by priority.
Open one with the bug template and it will be placed in a group when triaged.

## Groups

### Run safety: git, state and recovery ([#86](https://github.com/userfypp/phasewise/issues/86))

The run branch, checkpoints, `retry`, `rollback`, `abort`, `merge`, the lock
and recovery after a crash.

- [Take the run lock with `File::lock` (blocked on a newer MSRV)](https://github.com/userfypp/phasewise/issues/45) (#45)
- [Warn when a checkpoint contains a huge number of files](https://github.com/userfypp/phasewise/issues/164) (#164)

### Plan format, validation and settings ([#84](https://github.com/userfypp/phasewise/issues/84))

- [Commands to run before a phase](https://github.com/userfypp/phasewise/issues/38) (#38)
- [`check`: warn about plans that are sure to run badly](https://github.com/userfypp/phasewise/issues/159) (#159)
- [Update the plan of a run that is under way](https://github.com/userfypp/phasewise/issues/162) (#162)

### Terminal UI ([#83](https://github.com/userfypp/phasewise/issues/83))

- [A separate diff per subagent](https://github.com/userfypp/phasewise/issues/47) (#47)
- [Real-terminal tests with a pty](https://github.com/userfypp/phasewise/issues/51) (#51)
- [Move the run screen's git and log loading off the UI thread](https://github.com/userfypp/phasewise/issues/184) (#184)
- [Move the cursor and delete by grapheme in text inputs](https://github.com/userfypp/phasewise/issues/185) (#185)

### Agents and output fidelity ([#59](https://github.com/userfypp/phasewise/issues/59))

- [Sanitized captures of real agent output as test fixtures](https://github.com/userfypp/phasewise/issues/50) (#50)
- [Check the agent CLIs for changed flags on a schedule](https://github.com/userfypp/phasewise/issues/53) (#53)
- Open design question: [what it means when an agent exits 0 without a final event](https://github.com/userfypp/phasewise/issues/41) (#41)

### Engine: checks, limits and spending ([#57](https://github.com/userfypp/phasewise/issues/57))

- [A spending limit per phase](https://github.com/userfypp/phasewise/issues/34) (#34)
- [Usage limits for agents that report tokens, not cost](https://github.com/userfypp/phasewise/issues/35) (#35)

### CLI, scripts and integrations ([#60](https://github.com/userfypp/phasewise/issues/60))

- [Run a user command when a phase ends](https://github.com/userfypp/phasewise/issues/32) (#32)
- [Machine-readable event stream: `run --json`](https://github.com/userfypp/phasewise/issues/33) (#33)
- [List past runs and read one by id](https://github.com/userfypp/phasewise/issues/160) (#160)
- [Clean up old runs](https://github.com/userfypp/phasewise/issues/161) (#161)
- [Man page and the missing completion instructions](https://github.com/userfypp/phasewise/issues/163) (#163)
- [Document how to install the shell completions](https://github.com/userfypp/phasewise/issues/100) (#100)

### Beyond one agent: mixed plans, other agents, parallel runs ([#61](https://github.com/userfypp/phasewise/issues/61))

- [`--model` for the phases of the other agent in a mixed plan](https://github.com/userfypp/phasewise/issues/39) (#39)
- Open design questions: [should `run --agent` naming the plan's own agent force every phase](https://github.com/userfypp/phasewise/issues/42) (#42), [parallel phases using git worktrees](https://github.com/userfypp/phasewise/issues/46) (#46), [support another agent (Gemini CLI, opencode)](https://github.com/userfypp/phasewise/issues/48) (#48)

### Distribution and project health ([#62](https://github.com/userfypp/phasewise/issues/62))

- [Dependency and licence checks in CI](https://github.com/userfypp/phasewise/issues/54) (#54)
- [Prebuilt binaries and a Homebrew formula](https://github.com/userfypp/phasewise/issues/55) (#55)
- Open design question: [Windows support](https://github.com/userfypp/phasewise/issues/49) (#49)

phasewise will never add telemetry, update checks or any other network access
of its own.
