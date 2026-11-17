+++
agent = "claude"
# These commands run after EVERY phase; all must pass before the checkpoint.
verify = ["gofmt -l . | (! grep .)", "go vet ./...", "go test ./..."]
max_retries = 1
+++

# Add a `version` subcommand to a Go CLI

The project is a Go command-line tool (module `example.com/mytool`) with one
`main` package under `cmd/mytool` and its logic in `internal/`. It uses only the
standard library: keep it that way and follow the existing error handling
(return errors, print them once in `main`).

## Phase 1: Wire the subcommand

Add a `version` subcommand to the command dispatch in `cmd/mytool/main.go`.
For now it prints `dev` and exits with code 0. Unknown subcommands must keep
failing as they do today.

```verify
go run ./cmd/mytool version
```

## Phase 2: Report the real version

Replace `dev` with a package-level variable `version` in `internal/buildinfo`
that defaults to `dev` and can be set at build time with
`-ldflags "-X example.com/mytool/internal/buildinfo.version=1.2.3"`. Add a
`--short` flag that prints only the number, without the tool name.

### Constraints

- Print to stdout; errors go to stderr with exit code 1.
- Do not read the version from a file at run time.

```verify
go build -ldflags "-X example.com/mytool/internal/buildinfo.version=1.2.3" -o /dev/null ./cmd/mytool
```

## Phase 3: Tests and documentation

Add a table-driven test for the output of `version` and `version --short`, and
one test that an unknown subcommand still fails. Document the subcommand and
the build flag in the README.

```verify
go test ./...
grep -q "ldflags" README.md
```
