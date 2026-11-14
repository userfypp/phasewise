#!/bin/sh
# Fake Codex for tests. Reads the prompt from stdin and emits `--json` events.
# FAKE_MODE (or FAKE_MODE_<N> per phase): success, fail, fix_on_resume,
# never_fixes, commit, slow, branch. FAKE_LOG_DIR: append calls to calls.log.
# FAKE_MODELS_MODE: behaviour of `debug models` (see below).

if [ "$1" = "--version" ]; then
    echo "codex-cli 0.0.0-fake"
    exit 0
fi

# `codex login status`: succeeds, or exits 1 when FAKE_LOGIN=out.
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
    [ "${FAKE_LOGIN:-in}" = "out" ] && exit 1
    echo "Logged in using ChatGPT"
    exit 0
fi

# `codex debug models`: prints the synthetic catalog. FAKE_MODELS_MODE: ok (default),
# fail (exit 1 with a message on stderr), invalid (prints text that is not JSON).
if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
    case "${FAKE_MODELS_MODE:-ok}" in
        fail)
            echo "fake codex: cannot load the catalog" >&2
            exit 1
            ;;
        invalid)
            echo "this is not json"
            exit 0
            ;;
    esac
    cat "$(dirname "$0")/../fixtures/codex-models.synthetic.json"
    exit 0
fi


# Planning session (read-only flags in argv). FAKE_PLAN_MODE: valid (default),
# invalid_then_valid (duplicate title; valid after a resume), wrong_agent_then_valid,
# fenced (plan wrapped in a ```markdown fence), no_markers, always_invalid, fail, slow,
# crash (exits 2 with a message on stderr and no output).
# A resume keeps the same session id.
plan_text() {
    # $1: agent line value, $2: second phase title. JSON-escaped, markers included.
    printf '%s' '<<<PHASEWISE-PLAN\n+++\nagent = \"'"$1"'\"\n+++\n\n# Fake plan\n\nShared context.\n\n## Phase 1: First\n\nDo one.\n\n```verify\ntrue\n```\n\n## Phase 2: '"$2"'\n\nDo two.\nPHASEWISE-PLAN>>>'
}

plan_answer() {
    # $1: this agent, $2: the other agent, $3: "resume" when resuming.
    case "${FAKE_PLAN_MODE:-valid}" in
        invalid_then_valid)
            if [ -n "$3" ]; then plan_text "$1" Second; else plan_text "$1" First; fi
            ;;
        wrong_agent_then_valid)
            if [ -n "$3" ]; then plan_text "$1" Second; else plan_text "$2" Second; fi
            ;;
        always_invalid) plan_text "$1" First ;;
        fenced)
            printf '%s' '<<<PHASEWISE-PLAN\n```markdown\n+++\nagent = \"'"$1"'\"\n+++\n\n## Phase 1: First\n\nDo one.\n\n```verify\ntrue\n```\n\n## Phase 2: Second\n\nDo two.\n```\nPHASEWISE-PLAN>>>'
            ;;
        no_markers) printf '%s' 'Here is my plan, but I forgot the markers.' ;;
        *) plan_text "$1" Second ;;
    esac
}

prompt=$(cat)

resume=""
prev=""
for arg in "$@"; do
    if [ "$prev" = "resume" ]; then
        resume=$arg
    fi
    prev=$arg
done

if [ -n "$resume" ]; then
    n=${resume#fake-thread-}
else
    n=$(printf '%s\n' "$prompt" | sed -n 's/^## Your task: phase \([0-9][0-9]*\) of .*/\1/p' | head -n 1)
fi
# Planning sessions have no phase number.
case " $* " in *" read-only "*) n=plan ;; esac
tid="fake-thread-$n"

eval "mode=\${FAKE_MODE_$n:-}"
if [ -z "$mode" ]; then
    mode=${FAKE_MODE:-success}
fi

if [ -n "$FAKE_LOG_DIR" ]; then
    {
        echo "=== call ==="
        echo "args: $*"
        echo "prompt:"
        printf '%s\n' "$prompt"
    } >> "$FAKE_LOG_DIR/calls.log"
fi

case " $* " in
    *" read-only "*)
        if [ "${FAKE_PLAN_MODE:-valid}" = "crash" ]; then
            echo "error: fake option rejected" >&2
            exit 2
        fi
        ptid=${resume:-fake-plan-thread}
        printf '{"type":"thread.started","thread_id":"%s"}\n' "$ptid"
        echo '{"type":"turn.started"}'
        if [ "${FAKE_PLAN_MODE:-valid}" = "slow" ]; then
            exec sleep 600
        fi
        if [ "${FAKE_PLAN_MODE:-valid}" = "fail" ]; then
            echo '{"type":"turn.failed","error":{"message":"fake planning failure"}}'
            exit 1
        fi
        printf '{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"%s"}}\n' "$(plan_answer codex claude "$resume")"
        echo '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
        exit 0
        ;;
esac

printf '{"type":"thread.started","thread_id":"%s"}\n' "$tid"
echo '{"type":"turn.started"}'

if [ "$mode" = "slow" ]; then
    exec sleep 600
fi

file="phase-$n.txt"
case "$mode" in
    fix_on_resume)
        if [ -n "$resume" ]; then
            printf 'phase %s\n' "$n" > "$file"
        else
            printf 'broken\n' > "$file"
        fi
        ;;
    never_fixes)
        printf 'broken\n' > "$file"
        ;;
    commit)
        printf 'phase %s\n' "$n" > "$file"
        git add "$file"
        git commit -q -m "agent commit for phase $n"
        ;;
    branch)
        git switch -q -c "fake-agent-branch-$n"
        printf 'phase %s\n' "$n" > "$file"
        ;;
    stubborn)
        trap '' TERM
        echo $$ > "$FAKE_LOG_DIR/agent.pid"
        sleep 30
        ;;
    index_lock)
        printf 'phase %s\n' "$n" > "$file"
        : > "$(git rev-parse --git-dir)/index.lock"
        ;;
    fail)
        ;;
    *)
        printf 'phase %s\n' "$n" > "$file"
        ;;
esac

printf '{"type":"item.completed","item":{"id":"item_1","type":"file_change","changes":[{"path":"%s","kind":"add"}],"status":"completed"}}\n' "$file"

if [ "$mode" = "fail" ]; then
    echo '{"type":"turn.failed","error":{"message":"fake failure"}}'
    exit 1
fi

printf '{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Phase %s done."}}\n' "$n"
echo '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
