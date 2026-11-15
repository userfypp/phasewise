# Changelog

All notable changes to this project are documented in this file. From version
0.2.0 on it is written by [Release Please](docs/RELEASING.md) from the titles of
the merged pull requests. The project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0](https://github.com/userfypp/phasewise/compare/v0.1.0...v0.2.0) (2026-11-15)


### Added

* add a Settings menu and phasewise settings ([#26](https://github.com/userfypp/phasewise/issues/26)) ([ff5e392](https://github.com/userfypp/phasewise/commit/ff5e392a5533e006e6efb9039293fa2cb8483534))
* add a spending limit per run with max_cost_usd ([#13](https://github.com/userfypp/phasewise/issues/13)) ([94521f6](https://github.com/userfypp/phasewise/commit/94521f608afe761824b429969af5581dab505618))
* **cli:** add phasewise completions for bash, zsh, fish, elvish and powershell ([#15](https://github.com/userfypp/phasewise/issues/15)) ([7fb7c84](https://github.com/userfypp/phasewise/commit/7fb7c8463928fa38b5c04e7c4b9e48d74e633e24))
* **cli:** add phasewise logs to see what happened in a phase ([#17](https://github.com/userfypp/phasewise/issues/17)) ([427f8b1](https://github.com/userfypp/phasewise/commit/427f8b124a17562e5c7aceee8ee22d6a56fdf0e1))
* **cli:** add status --json for scripts ([#16](https://github.com/userfypp/phasewise/issues/16)) ([bc84c2c](https://github.com/userfypp/phasewise/commit/bc84c2c8dfca4037d83ecc3a1d4fcacabf264e3a))
* **cli:** note in check that Codex phases do not count against the limit ([#21](https://github.com/userfypp/phasewise/issues/21)) ([367d59f](https://github.com/userfypp/phasewise/commit/367d59f616c749793f04a02febab3622dae54cd5))
* **cli:** show the run's cost against its limit in status ([#14](https://github.com/userfypp/phasewise/issues/14)) ([8e1845b](https://github.com/userfypp/phasewise/commit/8e1845b6bd804d0fe4fa2ca68948f0c5e5b52e6e))
* **doctor:** check that the agents are logged in ([#7](https://github.com/userfypp/phasewise/issues/7)) ([928057b](https://github.com/userfypp/phasewise/commit/928057bd1ff78c6abe69351bd5a581e61c685487))
* let a phase run with a different agent ([#19](https://github.com/userfypp/phasewise/issues/19)) ([2d7ae07](https://github.com/userfypp/phasewise/commit/2d7ae0789a86f9f3416968d0681e695b1ea951d8))
* merge a completed run with phasewise merge or the m key ([#8](https://github.com/userfypp/phasewise/issues/8)) ([7db47e9](https://github.com/userfypp/phasewise/commit/7db47e957d68bbdaf94e8e07778305b1dac9e22f))
* show the agent's usage limits on the screens and in status ([#25](https://github.com/userfypp/phasewise/issues/25)) ([23e3877](https://github.com/userfypp/phasewise/commit/23e387778ba398d251ebe0cd8d33855527114bd6))
* show the time of each phase and add --until N ([#27](https://github.com/userfypp/phasewise/issues/27)) ([3c755c1](https://github.com/userfypp/phasewise/commit/3c755c1f5a8e3e98bd925c88f49bab7c5841377b))
* **tui:** honour NO_COLOR on the screens ([#11](https://github.com/userfypp/phasewise/issues/11)) ([732f00d](https://github.com/userfypp/phasewise/commit/732f00d790b45373fe80428cbf4231669a78f029))


### Fixed

* **git:** keep git's own error when it exits before reading its input ([#6](https://github.com/userfypp/phasewise/issues/6)) ([83e5d86](https://github.com/userfypp/phasewise/commit/83e5d869ce6c00b29524a9ee289a1c9acb745199))
* **ops:** check the git identity before approving in next ([#12](https://github.com/userfypp/phasewise/issues/12)) ([58427e5](https://github.com/userfypp/phasewise/commit/58427e58696120d1552c2c5613befb24cc276e3c))
* **ops:** count the cost of discarded attempts against max_cost_usd ([#18](https://github.com/userfypp/phasewise/issues/18)) ([b79a32b](https://github.com/userfypp/phasewise/commit/b79a32bd29413092ad04b319402899368fe1878d))
* **ops:** refuse run --model for a first phase of the other agent ([#20](https://github.com/userfypp/phasewise/issues/20)) ([282fbe5](https://github.com/userfypp/phasewise/commit/282fbe5127f6daed808227e575145fae500831b8))
* say how to recover from a damaged run and from an abort that cannot switch back ([#5](https://github.com/userfypp/phasewise/issues/5)) ([554977c](https://github.com/userfypp/phasewise/commit/554977cfffb94a156bd6d12a6c4e7c27a718cb13))
* **state:** do not delete a lock that another process took over ([#10](https://github.com/userfypp/phasewise/issues/10)) ([2f01f38](https://github.com/userfypp/phasewise/commit/2f01f384aab445e6e0c2a8b77e096890956f87bf))
* **tui:** let c commit the plan from the branch of a finished run ([#23](https://github.com/userfypp/phasewise/issues/23)) ([fb51488](https://github.com/userfypp/phasewise/commit/fb51488217fab6be3d624fc68d62a88e8c594764))


### Documentation

* **readme:** add the crates.io badge ([#9](https://github.com/userfypp/phasewise/issues/9)) ([fc5573c](https://github.com/userfypp/phasewise/commit/fc5573c6c944a82109093fe5667c838dc1231132))
* **roadmap:** note the stale-lock takeover race and its fix ([#22](https://github.com/userfypp/phasewise/issues/22)) ([4adf92c](https://github.com/userfypp/phasewise/commit/4adf92c6c8d2a4ab421e2ff9802e38da13df4324))
* say that retry discards uncommitted changes and that the push rule is best effort ([#4](https://github.com/userfypp/phasewise/issues/4)) ([2806443](https://github.com/userfypp/phasewise/commit/2806443714af0aeecaebceadf3822837ca626a9d))
* simplify the issue templates ([#2](https://github.com/userfypp/phasewise/issues/2)) ([c44bd36](https://github.com/userfypp/phasewise/commit/c44bd362757ad5df3423817e06eca75400c1bd6d))

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
