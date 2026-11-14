# Changelog

All notable changes to this project are documented in this file. From version
0.2.0 on it is written by [Release Please](docs/RELEASING.md) from the titles of
the merged pull requests. The project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0](https://github.com/userfypp/phasewise/releases/tag/v0.1.0) (2026-11-14)


### Added

* Run a plan one phase at a time: each phase runs the agent headless on the branch `phasewise/<run-id>`, then your checks, and becomes a checkpoint commit with `Phasewise-Run` and `Phasewise-Phase` trailers.
* Review each phase: approve and continue, retry it with a note, roll back to any checkpoint, or abandon the run.
* A terminal interface with a home screen and a run screen in one session.
* Plans written by the agent with `phasewise plan "<goal>"`, which explores the project read-only and chooses the model and effort of every phase.
* Models and effort per plan, per phase or per run, and `phasewise models` to list what an agent offers.
* A Markdown plan format with optional TOML front matter and `verify` blocks.
* Commands `init`, `check`, `doctor`, `plan`, `run`, `next`, `retry`, `rollback`, `status`, `diff`, `review`, `models` and `abort`.
* Atomic state writes, a run lock, recovery of interrupted phases, and a process group for every agent and check.
* Support for Claude Code and Codex, with Claude Code's cost shown per phase.


### Documentation

* README, user guide, plan format, agents, architecture, roadmap, contributing guide, security policy and code of conduct.
