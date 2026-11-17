+++
agent = "claude"
# These commands run after EVERY phase; all must pass before the checkpoint.
verify = ["ruff check .", "pytest -q"]
max_retries = 1
+++

# Add a `--format json` option to a Python CLI

The project is a Python command-line tool (the package is called `mytool`) built
with `argparse`, tested with pytest and linted with ruff. Follow the existing
module layout and the style of the current tests. Use only the standard
library: do not add dependencies.

## Phase 1: Separate the report from its printing

The `report` command builds a text report and prints it in one function. Split
it in two: a function that returns the report as plain data (a dict) and one
that renders that data as text. The output of `report` must not change.

```verify
python -m mytool report
```

## Phase 2: Add the option

Add `--format` to `report` with the choices `text` (the default) and `json`.
With `json`, print the data from phase 1 as one JSON object on stdout, with
sorted keys and no trailing spaces. Errors keep going to stderr with the same
exit codes.

### Constraints

- Reject any other value with the error `argparse` already gives.
- Do not change the default output.

```verify
python -m mytool report --format json
```

## Phase 3: Tests and documentation

Add tests for both formats: the JSON parses and has the same numbers as the
text, `--format text` equals the old output, and an unknown format fails with
exit code 2. Describe the option in the README and in the command's `--help`.

```verify
pytest -q
grep -q -- "--format" README.md
```
