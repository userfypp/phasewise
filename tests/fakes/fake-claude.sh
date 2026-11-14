#!/bin/sh
# Fake Claude Code for tests. Reads the prompt from stdin and emits stream-json.
# FAKE_MODE (or FAKE_MODE_<N> per phase): success, fail, fix_on_resume,
# never_fixes, broken_once, commit, slow, branch, index_lock, stubborn.
# broken_once: the first fresh session (and its resume) writes broken output; a later
# fresh session succeeds (needs FAKE_LOG_DIR for its marker file). FAKE_LOG_DIR: append calls to calls.log.

if [ "$1" = "--version" ]; then
    echo "0.0.0-fake (Claude Code)"
    exit 0
fi

# `claude auth status`: succeeds, or exits 1 when FAKE_LOGIN=out.
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
    [ "${FAKE_LOGIN:-in}" = "out" ] && exit 1
    echo '{"loggedIn": true}'
    exit 0
fi


# Planning session (read-only flags in argv). FAKE_PLAN_MODE: valid (default),
# invalid_then_valid (duplicate title; valid after a resume), wrong_agent_then_valid,
# fenced (plan wrapped in a ```markdown fence), no_markers, always_invalid, fail, slow,
# crash (exits 2 with a message on stderr and no output), creates_output (claude only:
# writes phasewise.md itself, as if the user did, then answers a valid plan).
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
    if [ "$prev" = "--resume" ]; then
        resume=$arg
    fi
    prev=$arg
done

if [ -n "$resume" ]; then
    n=${resume#fake-session-}
else
    n=$(printf '%s\n' "$prompt" | sed -n 's/^## Your task: phase \([0-9][0-9]*\) of .*/\1/p' | head -n 1)
fi
# Planning sessions have no phase number.
case " $* " in *" Read,Grep,Glob "*) n=plan ;; esac
sid="fake-session-$n"

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
    *" Read,Grep,Glob "*)
        if [ "${FAKE_PLAN_MODE:-valid}" = "crash" ]; then
            echo "error: fake option rejected" >&2
            exit 2
        fi
        psid=${resume:-fake-plan-session}
        printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$psid"
        if [ "${FAKE_PLAN_MODE:-valid}" = "slow" ]; then
            exec sleep 600
        fi
        if [ "${FAKE_PLAN_MODE:-valid}" = "creates_output" ]; then
            # The user writes the plan file by hand while the agent works.
            printf 'mine\n' > phasewise.md
        fi
        printf '{"type":"assistant","session_id":"%s","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Reading the project."},{"type":"tool_use","id":"toolu_p","name":"Read","input":{"file_path":"README.md"}}]}}\n' "$psid"
        if [ "${FAKE_PLAN_MODE:-valid}" = "fail" ]; then
            printf '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"fake planning failure","session_id":"%s","total_cost_usd":0.001}\n' "$psid"
            exit 1
        fi
        printf '{"type":"result","subtype":"success","is_error":false,"result":"%s","session_id":"%s","total_cost_usd":0.02}\n' "$(plan_answer claude codex "$resume")" "$psid"
        exit 0
        ;;
esac

# Like Claude Code, report the model in use: the one passed with --model, else
# a default.
model=claude-sonnet-5-5
prev=
for arg in "$@"; do
    if [ "$prev" = "--model" ]; then
        model=$arg
    fi
    prev=$arg
done
printf '{"type":"system","subtype":"init","session_id":"%s","model":"%s"}\n' "$sid" "$model"

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
    broken_once)
        marker="$FAKE_LOG_DIR/broken-once-$n"
        if [ -n "$resume" ] || [ ! -e "$marker" ]; then
            : > "$marker"
            printf 'broken\n' > "$file"
            printf 'leftover\n' > "leftover-$n.txt"
        else
            printf 'phase %s\n' "$n" > "$file"
        fi
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

printf '{"type":"assistant","session_id":"%s","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Working on phase %s."},{"type":"tool_use","id":"toolu_%s","name":"Write","input":{"file_path":"%s"}}]}}\n' "$sid" "$n" "$n" "$file"

if [ "$mode" = "fail" ]; then
    printf '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"fake failure","session_id":"%s","total_cost_usd":0.001}\n' "$sid"
    exit 1
fi

printf '{"type":"result","subtype":"success","is_error":false,"result":"Phase %s done.","session_id":"%s","total_cost_usd":0.01}\n' "$n" "$sid"
