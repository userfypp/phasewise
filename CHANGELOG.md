# Changelog

All notable changes to this project are documented in this file. From version
0.2.0 on it is written by [Release Please](docs/RELEASING.md) from the titles of
the merged pull requests. The project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1](https://github.com/userfypp/phasewise/compare/v0.3.0...v0.3.1) (2026-11-18)


### Fixed

* **engine:** cap the agent's stdout and stderr logs at 64 MiB ([#207](https://github.com/userfypp/phasewise/issues/207)) ([de00c1e](https://github.com/userfypp/phasewise/commit/de00c1ee8bdbdb4ee4bfc6f97a3e17ba2e6965f5))
* **plan:** reject control characters in titles, near-miss fences, misplaced front matter and empty checks ([#206](https://github.com/userfypp/phasewise/issues/206)) ([25afa7d](https://github.com/userfypp/phasewise/commit/25afa7d3a3cc977367cfd1c8e125ae20343490bb))
* **tui:** drop every key but Ctrl-C while the terminal is too small ([#187](https://github.com/userfypp/phasewise/issues/187)) ([57c5a44](https://github.com/userfypp/phasewise/commit/57c5a44d6f6c58dd147a9256f18f777067d0ad8d))


### Documentation

* explain scripting and CI with phasewise ([#208](https://github.com/userfypp/phasewise/issues/208)) ([9323080](https://github.com/userfypp/phasewise/commit/932308019b2846069471efcd8dc85602c23d8279))

## [0.3.0](https://github.com/userfypp/phasewise/compare/v0.2.0...v0.3.0) (2026-11-17)


### Added

* **cli:** add phasewise accept for a failed phase fixed by hand ([#135](https://github.com/userfypp/phasewise/issues/135)) ([0772386](https://github.com/userfypp/phasewise/commit/077238667b5eb1c279968b24b3f215a65a212c5b))
* **cli:** add phasewise report for a pull request description ([#113](https://github.com/userfypp/phasewise/issues/113)) ([b97815e](https://github.com/userfypp/phasewise/commit/b97815e0979b7961bb8f0c319a9e8c57b326a02f))
* **plan:** let a phase set its own timeout ([#112](https://github.com/userfypp/phasewise/issues/112)) ([5aafaa6](https://github.com/userfypp/phasewise/commit/5aafaa641d4adf6d9fdf2addedc05d1ef8c51ff7))
* **tui:** let the agent setting choose for a plan that names no agent ([#137](https://github.com/userfypp/phasewise/issues/137)) ([4286e2b](https://github.com/userfypp/phasewise/commit/4286e2b5d979695f7bc26511adf0017e70cf4ef0))
* **tui:** open a phase's changes in git difftool with d ([#139](https://github.com/userfypp/phasewise/issues/139)) ([3dc3144](https://github.com/userfypp/phasewise/commit/3dc314451acd4a3ee36fb5322d8905210be10eea))
* **tui:** run several phases from the run screen with u ([#151](https://github.com/userfypp/phasewise/issues/151)) ([64b4a6e](https://github.com/userfypp/phasewise/commit/64b4a6ef8d57b769556b13e615f8326eb62aaa2f))
* **tui:** scroll the right panel by line and column, jump to its ends and search it ([#111](https://github.com/userfypp/phasewise/issues/111)) ([c9faf09](https://github.com/userfypp/phasewise/commit/c9faf09278d09a5f6f4c39aa6987511a8956a257))
* **tui:** show the whole failure of a phase and its log paths on f ([#134](https://github.com/userfypp/phasewise/issues/134)) ([6abacc3](https://github.com/userfypp/phasewise/commit/6abacc38df34cdc8fa3eaf08a52110a9909934ca))


### Fixed

* **agent:** strip the repository path only at the start of a path ([#94](https://github.com/userfypp/phasewise/issues/94)) ([a685637](https://github.com/userfypp/phasewise/commit/a6856379612c6a73e7c56fe9db1d03be506f09d3))
* **cli:** count only the phases a rollback reset ([#108](https://github.com/userfypp/phasewise/issues/108)) ([798baee](https://github.com/userfypp/phasewise/commit/798baeee2618bdf83ad4d2ea0b46a22d8615c8c7))
* **cli:** make doctor fail on repository problems and warn about the plan's agent ([#96](https://github.com/userfypp/phasewise/issues/96)) ([c60471b](https://github.com/userfypp/phasewise/commit/c60471bf4dc92f9e72d7fc862ef0185511c7393a))
* **cli:** make init refuse a plan path that is a directory or a dangling link ([#109](https://github.com/userfypp/phasewise/issues/109)) ([5d10389](https://github.com/userfypp/phasewise/commit/5d103891f05e43451aefdefb0684fa08660cae5c))
* **cli:** warn when a report file would get in the way of the run ([#127](https://github.com/userfypp/phasewise/issues/127)) ([22aa58c](https://github.com/userfypp/phasewise/commit/22aa58cb1b38b10c348c73f34caf0189c4b83321))
* **engine:** bound a line of agent output and ignore a negative cost ([#143](https://github.com/userfypp/phasewise/issues/143)) ([ad32d7a](https://github.com/userfypp/phasewise/commit/ad32d7a1838408a124e95658cde610d5c1cc3209))
* **engine:** compare the cost limit in micro-dollars ([#101](https://github.com/userfypp/phasewise/issues/101)) ([08a7853](https://github.com/userfypp/phasewise/commit/08a7853ca177d565791a766115e376cfcb3a47d7))
* **git:** do not call a repository git refuses "not a git repository" ([#157](https://github.com/userfypp/phasewise/issues/157)) ([85d87c6](https://github.com/userfypp/phasewise/commit/85d87c63e6ea08e01f581d51f3760aebbfcf1459))
* **git:** keep the user's hooks out of checkpoints and out of branch switches ([#158](https://github.com/userfypp/phasewise/issues/158)) ([e8cfc37](https://github.com/userfypp/phasewise/commit/e8cfc37b4114a6b3be95399636980232148cd949))
* **git:** take the path of the plan literally when committing it ([#120](https://github.com/userfypp/phasewise/issues/120)) ([37b6fea](https://github.com/userfypp/phasewise/commit/37b6fea8b89a087b94cd61b0e8709072a73f6193))
* **ops:** count a checkpoint commit the state never recorded as phasewise's own ([#145](https://github.com/userfypp/phasewise/issues/145)) ([b68269d](https://github.com/userfypp/phasewise/commit/b68269d0e71acfd71fb29cbc68f975b34b66b3d2))
* **ops:** do not advise phasewise init when a run's plan copy is missing ([#156](https://github.com/userfypp/phasewise/issues/156)) ([05c97c6](https://github.com/userfypp/phasewise/commit/05c97c69ddd5b44d8b8f0b8d37cc2032e2ddd2be))
* **ops:** let abort finish when nothing can be staged ([#93](https://github.com/userfypp/phasewise/issues/93)) ([9cb94ab](https://github.com/userfypp/phasewise/commit/9cb94abac918b8cf5b801e257532b82b46d183c1))
* **ops:** let retry and rollback drop the commits the agent made in a phase ([#121](https://github.com/userfypp/phasewise/issues/121)) ([18eedf5](https://github.com/userfypp/phasewise/commit/18eedf5a972ff2cc47727aa414d4895d2d5e0752))
* **ops:** name a submodule or nested repository that blocks the run ([#136](https://github.com/userfypp/phasewise/issues/136)) ([7346c9e](https://github.com/userfypp/phasewise/commit/7346c9e444f7adc769595dce751c01330ca9ca2b))
* **ops:** refuse 0 in the timeout environment variables ([#90](https://github.com/userfypp/phasewise/issues/90)) ([a61ceaf](https://github.com/userfypp/phasewise/commit/a61ceaf125ff420376934a55d52aa9b8955955b0))
* **ops:** refuse retry and rollback when they would drop the user's commits ([#114](https://github.com/userfypp/phasewise/issues/114)) ([0932cac](https://github.com/userfypp/phasewise/commit/0932cac980a851ba0ec440fc262a254f917d2158))
* **ops:** refuse to go on when the run branch lost a checkpoint ([#144](https://github.com/userfypp/phasewise/issues/144)) ([1b30523](https://github.com/userfypp/phasewise/commit/1b30523bb459e97c755bc69fc9211221330f4d82))
* **ops:** say when a hook changed the plan after it was committed ([#154](https://github.com/userfypp/phasewise/issues/154)) ([f10069b](https://github.com/userfypp/phasewise/commit/f10069bbfaaeb7d23327a669208b48f65179c120))
* **plan:** bound the timeouts to one year ([#95](https://github.com/userfypp/phasewise/issues/95)) ([1e76b8d](https://github.com/userfypp/phasewise/commit/1e76b8d463863a0bf70cdeae3c93e528d7fd862c))
* **plan:** keep an answer without markers as a draft; init names the real path ([#148](https://github.com/userfypp/phasewise/issues/148)) ([9df51d6](https://github.com/userfypp/phasewise/commit/9df51d61073969291d8f874c5c24a725658f6687))
* **plan:** read the front matter of a plan that starts with a byte order mark ([#88](https://github.com/userfypp/phasewise/issues/88)) ([27f1461](https://github.com/userfypp/phasewise/commit/27f14611659a6352e24aa2393d5eb3d2e6ceed93))
* **plan:** refuse an output that cannot be written before the agent runs ([#147](https://github.com/userfypp/phasewise/issues/147)) ([3e8fe56](https://github.com/userfypp/phasewise/commit/3e8fe56e34858a9c5399440d2d60d0e769ee182f))
* **plan:** reject a phase heading with an empty title ([#89](https://github.com/userfypp/phasewise/issues/89)) ([d9d0adb](https://github.com/userfypp/phasewise/commit/d9d0adb0344949ccd49bf889d79dae9c7dc75a42))
* **plan:** reject control characters in a model and NUL in agent_args ([#153](https://github.com/userfypp/phasewise/issues/153)) ([b5919a8](https://github.com/userfypp/phasewise/commit/b5919a85833d4018b7bfe27509fe0f3d0b72bdf4))
* **plan:** say that a plan file must be UTF-8 when it is not ([#92](https://github.com/userfypp/phasewise/issues/92)) ([4d47205](https://github.com/userfypp/phasewise/commit/4d472053dc9623f49bdb7b320b0eb5836ac4a878))
* **state:** serialize concurrent settings updates with a lock file ([#104](https://github.com/userfypp/phasewise/issues/104)) ([d24dc8f](https://github.com/userfypp/phasewise/commit/d24dc8f89929105c5ef36c39f63840fdce69cae2))
* **state:** stop an agent that a killed phasewise left running ([#126](https://github.com/userfypp/phasewise/issues/126)) ([b161a75](https://github.com/userfypp/phasewise/commit/b161a75526ef12e02b58acad2a77c7c48e414c03))
* **state:** take over a lock file that has stayed empty ([#146](https://github.com/userfypp/phasewise/issues/146)) ([0d8b403](https://github.com/userfypp/phasewise/commit/0d8b4030f68c5257d7fc2bc86b0d53ed1d7d0764))
* **state:** take over a lock that names PID 0 or an impossible PID ([#110](https://github.com/userfypp/phasewise/issues/110)) ([bdf44ad](https://github.com/userfypp/phasewise/commit/bdf44ad463d82876d2d39bc07468472173a4eb02))
* **state:** write through a symbolic link instead of replacing it ([#130](https://github.com/userfypp/phasewise/issues/130)) ([cded998](https://github.com/userfypp/phasewise/commit/cded998e3633a62854e4cfbbbbf9e1b8e8e43bc1))
* **tui:** follow changes made to the run by another process ([#118](https://github.com/userfypp/phasewise/issues/118)) ([bde80ed](https://github.com/userfypp/phasewise/commit/bde80ed930b71374587532e487643d3ee2d74526))
* **tui:** ignore control keys on the run screen except Ctrl-C ([#131](https://github.com/userfypp/phasewise/issues/131)) ([27753ea](https://github.com/userfypp/phasewise/commit/27753ea75d3aeef10c13eb985bc78aed6e82bf31))
* **tui:** keep the end of a typed text in view and sanitize the error popup ([#123](https://github.com/userfypp/phasewise/issues/123)) ([628a221](https://github.com/userfypp/phasewise/commit/628a2218bdb769c909443175cea7621e3b754db7))
* **tui:** read only the end of a long agent log on each refresh ([#142](https://github.com/userfypp/phasewise/issues/142)) ([811e493](https://github.com/userfypp/phasewise/commit/811e493394cbd2da9b794a9b614ff130770e9a44))
* **tui:** scroll a diff of more than 65,535 lines to its last line ([#103](https://github.com/userfypp/phasewise/issues/103)) ([f83805c](https://github.com/userfypp/phasewise/commit/f83805cd2c03acb09d2493bc996d391409c69b27))
* **tui:** show tabs and control characters in the diff, log and verify views ([#97](https://github.com/userfypp/phasewise/issues/97)) ([260824e](https://github.com/userfypp/phasewise/commit/260824e9d5d705364f5dfe38d50a7adef0630f6f))
* **tui:** type a large paste in linear time ([#155](https://github.com/userfypp/phasewise/issues/155)) ([605519e](https://github.com/userfypp/phasewise/commit/605519e0db16d050593e0eb7848f90720a134a02))
* **tui:** type a pasted text into the input instead of submitting it ([#122](https://github.com/userfypp/phasewise/issues/122)) ([8452594](https://github.com/userfypp/phasewise/commit/8452594a22113b7b64a1df8b71f1bfe3a545dad5))
* **verify:** keep the exit code line off the end of the command's output ([#91](https://github.com/userfypp/phasewise/issues/91)) ([57e84f7](https://github.com/userfypp/phasewise/commit/57e84f75e7fa71bee407da3ccd24436733e4d5bd))
* **verify:** read only the end of a check's output for the retry prompt ([#102](https://github.com/userfypp/phasewise/issues/102)) ([10c47be](https://github.com/userfypp/phasewise/commit/10c47bef120bdc40c4a16624fb09b8b2aa4ade10))
* **verify:** send the agent a check's output without escapes or progress bars ([#140](https://github.com/userfypp/phasewise/issues/140)) ([0e8f3e0](https://github.com/userfypp/phasewise/commit/0e8f3e0e5d14bc2ae4482eb99c7d6fc930e330d3))
* **verify:** stop a check that writes more than 64 MiB ([#141](https://github.com/userfypp/phasewise/issues/141)) ([4e703c3](https://github.com/userfypp/phasewise/commit/4e703c376cb7a12e22561b9180a6c9dfc49fcf6f))


### Documentation

* add an example plan for a Go project ([#119](https://github.com/userfypp/phasewise/issues/119)) ([b37099d](https://github.com/userfypp/phasewise/commit/b37099dd591f4212638565aab52c2a7970c95727))
* add an example plan for a Python project ([#116](https://github.com/userfypp/phasewise/issues/116)) ([97c3e33](https://github.com/userfypp/phasewise/commit/97c3e334f619617ba40f0baf95a4f6514fd2278a))
* add rate-limits.json to the file layout and screenshot.rs to the module map ([#149](https://github.com/userfypp/phasewise/issues/149)) ([72b6a50](https://github.com/userfypp/phasewise/commit/72b6a5054d1eca5cc2b4df6b66e92acf506b300d))
* describe the issue labels and update the template labels ([#28](https://github.com/userfypp/phasewise/issues/28)) ([a1b6a5c](https://github.com/userfypp/phasewise/commit/a1b6a5c2c8e7fb676eb79c19e5586321d766e2f1))
* drop the finished items from the roadmap ([#132](https://github.com/userfypp/phasewise/issues/132)) ([58e619b](https://github.com/userfypp/phasewise/commit/58e619b2f125f723a5b42b827e32685324ce4876))
* explain how to install the shell completions ([#115](https://github.com/userfypp/phasewise/issues/115)) ([ccf4f37](https://github.com/userfypp/phasewise/commit/ccf4f3720f608c35bace7ff1e2eb3f6c8e2b84c6))
* list the roadmap as issues and add an optional CONTRIBUTORS file ([#56](https://github.com/userfypp/phasewise/issues/56)) ([7bb93fb](https://github.com/userfypp/phasewise/commit/7bb93fb952ef086cf134cbb4873a7132cfa2de77))
* **roadmap:** add the issues found in the audit ([#87](https://github.com/userfypp/phasewise/issues/87)) ([0fd617e](https://github.com/userfypp/phasewise/commit/0fd617e6ec2e9e5027945745aa8c765e71a4438d))
* **roadmap:** drop [#37](https://github.com/userfypp/phasewise/issues/37), which is done, and list [#150](https://github.com/userfypp/phasewise/issues/150) ([#152](https://github.com/userfypp/phasewise/issues/152)) ([eb3fb69](https://github.com/userfypp/phasewise/commit/eb3fb699f1d4bc7e3d39c30c82d8ca1963fff8e0))
* **roadmap:** drop the issues that are closed ([#138](https://github.com/userfypp/phasewise/issues/138)) ([efb8119](https://github.com/userfypp/phasewise/commit/efb81198cd41e83d18895022a1533cebcb60490c))

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
