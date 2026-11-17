# Security

## Reporting a vulnerability

Report it privately through
[GitHub private vulnerability reporting](https://github.com/userfypp/phasewise/security/advisories/new),
not in a public issue. Include the phasewise version (`phasewise --version`),
the agent and its version, your OS, the steps to reproduce and the impact you
expect. Use a minimal plan and repository; never send credentials, private code
or personal data. Keep the details private until a fix is released. Ordinary
bugs go to the public issue tracker.

phasewise is young: fixes go into the latest release only.

## Security model

phasewise runs a coding agent unattended. Read this before you use it with a
plan you did not write or a repository you do not trust.

### What the agent may do

The agent runs headless, with nobody to answer permission prompts:

- **Claude Code** gets `--permission-mode acceptEdits --permission-prompts none
  --allowedTools Read,Edit,Write,Bash --disallowedTools "Bash(git push *)"`.
  Allowing `Bash` means any shell command. The `git push` rule is best effort:
  it matches that command, not every way to reach a remote or the network.
- **Codex** runs with `--sandbox workspace-write`. The confinement is Codex's
  own sandbox, not phasewise's.

The agent runs in your repository, as your user, and inherits your environment
variables, including any tokens in them. The plan's `verify` commands run
through `sh -c` in the same environment.

Planning sessions (`phasewise plan`, or `p` on the home screen) are read-only:
Claude Code gets only the Read, Grep and Glob tools and no MCP servers, and
Codex runs with `--sandbox read-only`. The plan file is written by phasewise,
not by the agent. The exact command lines are in [docs/agents.md](docs/agents.md).

### What protects you

Each run works on its own branch, `phasewise/<run-id>`, and every phase ends in
a checkpoint commit. Your original branch is not modified; you can inspect each
phase and go back to any checkpoint. This protects your repository history. It
does not limit what a command run by the agent can do outside the repository.

phasewise itself:

- never runs `git push`, never deletes branches and never uses `--force`;
- commits only on the run branch, except the plan file when you ask for it
  (`c` on the home screen), which honours your hooks and signing;
- never deletes git's lock files: if `.git/index.lock` stays taken, it tells
  you and stops;
- makes no network connections of its own (no telemetry, no update checks);
  the agents do;
- never commits `.phasewise/`, which it adds to `.git/info/exclude`;
- never logs in to an agent and never reads its credentials.

phasewise is not a sandbox. A plan is a set of instructions for an agent that
can run commands: treat it like a script you are about to run. That includes the
plan's `agent_args`, which go last on the agent's command line and so can
override the permission or sandbox options phasewise sets.

### Recommendations

- Use a disposable clone, container or VM for untrusted plans or repositories.
- Keep secrets out of the repository and out of the environment the agent
  inherits.
- Read the `verify` commands of a plan (`phasewise check` lists them): they run
  on your machine.
- Review every phase before approving it, and the whole run before merging it.
- Keep `max_retries` and the timeouts (`phase_timeout_minutes`,
  `verify_timeout_seconds`) at sensible values.
