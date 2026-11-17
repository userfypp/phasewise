//! The agent output parsers never fail, whatever the agent sends: every line
//! of the fixtures is fed back with one value replaced, one key removed, or cut
//! short. A panic anywhere is a failed test; the events are not compared.
//!
//! There is no random source: a fixed xorshift sequence makes every run, on
//! every machine, try the same mutations.

use std::path::Path;

use phasewise::agent::claude::ClaudeParser;
use phasewise::agent::codex::CodexParser;
use phasewise::agent::{AgentEvent, AgentParser};
use serde_json::{Map, Value, json};

const FIXTURES: &[&str] = &[
    include_str!("fixtures/claude-basic.synthetic.jsonl"),
    include_str!("fixtures/claude-error.synthetic.jsonl"),
    include_str!("fixtures/codex-basic.synthetic.jsonl"),
    include_str!("fixtures/codex-failed.synthetic.jsonl"),
];

/// Mutations tried on every line of every fixture.
const ROUNDS: usize = 150;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Values of every JSON type, including the extremes a number can take.
fn replacements() -> Vec<Value> {
    vec![
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(u64::MAX),
        json!(i64::MIN),
        json!(1e308),
        json!(-0.0),
        json!(f64::MIN_POSITIVE),
        json!(""),
        json!("x"),
        json!("é\u{0}\u{1b}[31m\n\t日本語"),
        json!("a".repeat(10_000)),
        json!([]),
        json!([null]),
        json!([[[]]]),
        json!({}),
        json!({"a": 1}),
        json!({"type": "assistant", "message": 5}),
        json!([{"type": "text", "text": 5}, {"type": "tool_use", "name": [], "input": "x"}]),
    ]
}

/// Every path to a value inside `value`, the root included.
fn paths(value: &Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    out.push(at.clone());
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                at.push(key.clone());
                paths(child, at, out);
                at.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                at.push(i.to_string());
                paths(child, at, out);
                at.pop();
            }
        }
        _ => {}
    }
}

fn slot<'a>(value: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    let mut at = value;
    for step in path {
        at = match at {
            Value::Object(map) => map.get_mut(step)?,
            Value::Array(items) => items.get_mut(step.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(at)
}

/// Removes the value at `path` from its parent. The root cannot be removed.
fn remove(value: &mut Value, path: &[String]) {
    let Some((last, parent)) = path.split_last() else {
        return;
    };
    match slot(value, parent) {
        Some(Value::Object(map)) => {
            map.remove(last);
        }
        Some(Value::Array(items)) => {
            if let Ok(i) = last.parse::<usize>()
                && i < items.len()
            {
                items.remove(i);
            }
        }
        _ => {}
    }
}

fn feed_everywhere(line: &str) -> usize {
    let mut events = 0;
    let root = Path::new("/work/repo");
    let mut claude = ClaudeParser::new(root);
    let mut codex = CodexParser::new(root);
    events += claude.feed(line).len();
    events += codex.feed(line).len();
    events
}

#[test]
fn the_fixtures_are_understood_before_they_are_mutated() {
    // Otherwise the test below could pass by feeding lines nobody reads.
    let root = Path::new("/work/repo");
    for (fixture, mut parser) in [
        (
            FIXTURES[0],
            Box::new(ClaudeParser::new(root)) as Box<dyn AgentParser>,
        ),
        (FIXTURES[2], Box::new(CodexParser::new(root))),
    ] {
        let events: Vec<AgentEvent> = fixture.lines().flat_map(|l| parser.feed(l)).collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Finished { .. })),
            "{events:?}"
        );
    }
}

#[test]
fn a_changed_value_or_a_missing_key_never_makes_a_parser_fail() {
    let replacements = replacements();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut tried = 0;
    for line in FIXTURES.iter().flat_map(|f| f.lines()) {
        let Ok(original) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let mut all = Vec::new();
        paths(&original, &mut Vec::new(), &mut all);
        for _ in 0..ROUNDS {
            let mut value = original.clone();
            // One to three changes on top of each other.
            for _ in 0..=rng.below(3) {
                let path = &all[rng.below(all.len())];
                if rng.below(4) == 0 {
                    remove(&mut value, path);
                } else if let Some(at) = slot(&mut value, path) {
                    *at = replacements[rng.below(replacements.len())].clone();
                }
            }
            feed_everywhere(&value.to_string());
            tried += 1;
        }
    }
    assert!(tried > 1000, "only {tried} mutations were tried");
}

#[test]
fn a_line_cut_anywhere_never_makes_a_parser_fail() {
    for line in FIXTURES.iter().flat_map(|f| f.lines()) {
        for (end, _) in line.char_indices() {
            feed_everywhere(&line[..end]);
        }
    }
}

#[test]
fn nesting_and_size_never_make_a_parser_fail() {
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    feed_everywhere(&deep);
    let wide: Map<String, Value> = (0..5000).map(|i| (format!("k{i}"), json!(i))).collect();
    feed_everywhere(&Value::Object(wide).to_string());
    feed_everywhere(&"x".repeat(1_000_000));
    feed_everywhere("");
    feed_everywhere("null");
    feed_everywhere("\u{feff}{\"type\":\"result\"}");
}
