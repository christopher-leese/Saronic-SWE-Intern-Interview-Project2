//! Black-box verification and validation of the FLW2 (historical state query) requirements
//! through the binary.

mod common;

use common::{assert_transcript, run_commands};
use serde_json::{json, Value};

/// The spec's FLW1 world, with one unit for location 2 at dock 0.
const INIT: &str = r#"{"command":"init","parameters":{"travel_times":[[0,4,7],[4,0,3],[7,3,0]],"dock_throughput":[2,4,1],"vessels":[{"id":"v1","location":0},{"id":"v2","location":2}],"cargo_manifest":[{"location":0,"destination":2,"quantity":1}]}}"#;
const TICK: &str = r#"{"command":"tick","parameters":{}}"#;
const GET_STATE: &str = r#"{"command":"get_state","parameters":{}}"#;
const DOCK_V1: &str = r#"{"command":"dock","parameters":{"vessel_id":"v1"}}"#;
const LOAD_V1: &str = r#"{"command":"load","parameters":{"vessel_id":"v1","destination":2}}"#;
/// v2 travels from 2 to 1 and arrives on tick 3 when sent at tick 0.
const TRIP_V2: &str =
    r#"{"command":"set_destination","parameters":{"vessel_id":"v2","destination":1}}"#;

fn get_state_at(tick: i64) -> String {
    json!({"command": "get_state_at", "parameters": {"tick": tick}}).to_string()
}

fn message(response: &Value) -> &Value {
    &response["payload"]["message"]
}

/// `transcript` with a `get_state_at` query for tick 0 after every expected response. The queries
/// have no expected lines, so only the original commands' responses are compared.
fn with_queries(transcript: &str) -> String {
    let query = get_state_at(0);
    let mut out = String::new();
    for line in transcript.lines() {
        out.push_str(line);
        out.push('\n');
        if line.trim_start().starts_with('→') {
            out.push_str(&query);
            out.push('\n');
        }
    }
    out
}

// --- validation against the spec ---

/// Validation: the interview spec's Follow-on 2 example, verbatim. It continues the Follow-on 1
/// example after tick 13, so both are replayed in order.
#[test]
fn validation_flw2_spec_example() {
    assert_transcript(
        &[
            include_str!("fixtures/flw1_example.txt"),
            include_str!("fixtures/flw2_example.txt"),
        ]
        .concat(),
    );
}

#[test]
fn validation_flw2_spec_failure_message() {
    let query = get_state_at(99);
    let mut commands = vec![INIT];
    commands.extend([TICK; 36]);
    commands.push(&query);
    let out = run_commands(&commands);
    assert_eq!(
        *message(&out[37]),
        "tick 99 is in the future (current tick: 36)"
    );
}

// --- verification ---

#[test]
fn flw2_2_19_tick_0_and_the_current_tick_are_valid() {
    let (at_0, at_2) = (get_state_at(0), get_state_at(2));
    let out = run_commands(&[INIT, TICK, TICK, &at_0, &at_2]);
    assert_eq!(out[3], out[0]);
    assert_eq!(out[4], out[2]);
}

#[test]
fn flw2_2_20_future_and_negative_ticks_are_errors() {
    let (future, negative) = (get_state_at(2), get_state_at(-1));
    let out = run_commands(&[INIT, TICK, &future, &negative, GET_STATE]);
    assert_eq!(
        *message(&out[2]),
        "tick 2 is in the future (current tick: 1)"
    );
    assert_eq!(out[3]["status"], "error");
    let negative_message = message(&out[3]).as_str().unwrap();
    assert!(
        negative_message.starts_with("invalid command: "),
        "{negative_message}"
    );
    assert_eq!(out[4], out[1], "state unchanged");
}

#[test]
fn flw2_1_00_init_records_tick_0() {
    let at_0 = get_state_at(0);
    let out = run_commands(&[INIT, TRIP_V2, TICK, TICK, &at_0]);
    assert_eq!(out[4], out[0]);
}

#[test]
fn flw2_1_01_each_tick_records_the_world_after_all_transitions() {
    let manifest_tick = r#"{"command":"tick","parameters":{"cargo_manifest":[{"location":1,"destination":0,"quantity":2}]}}"#;
    let at_3 = get_state_at(3);
    let out = run_commands(&[INIT, TRIP_V2, TICK, TICK, manifest_tick, TICK, &at_3]);
    assert_eq!(out[6], out[4]);
    assert_eq!(
        out[6]["payload"]["vessels"]["v2"],
        json!({"state": "idle", "location": 1, "cargo": []}),
        "the arrival on tick 3"
    );
    assert_eq!(
        out[6]["payload"]["cargo"],
        json!([{"location": 0, "destination": 2, "quantity": 1},
               {"location": 1, "destination": 0, "quantity": 2}]),
        "tick 3's manifest"
    );
}

#[test]
fn flw2_1_01_a_rejected_tick_records_nothing() {
    let bad_tick = r#"{"command":"tick","parameters":{"cargo_manifest":[{"location":9,"destination":0,"quantity":1}]}}"#;
    let (at_1, at_2) = (get_state_at(1), get_state_at(2));
    let out = run_commands(&[INIT, TRIP_V2, TICK, bad_tick, TICK, TICK, &at_1, &at_2]);
    assert_eq!(out[3]["status"], "error");
    assert_eq!(out[6], out[2]);
    assert_eq!(out[7], out[4]);
}

#[test]
fn flw2_1_02_get_state_at_returns_each_committed_state() {
    let queries: Vec<String> = (0..=3).map(get_state_at).collect();
    let mut commands = vec![INIT, DOCK_V1, TICK, LOAD_V1, TICK, TICK];
    commands.extend(queries.iter().map(String::as_str));
    let out = run_commands(&commands);
    for (query, tick_response) in [6, 7, 8, 9].into_iter().zip([0, 2, 4, 5]) {
        assert_eq!(out[query], out[tick_response]);
    }
}

#[test]
fn flw2_1_03_states_between_ticks_are_not_queryable() {
    let at_1 = get_state_at(1);
    let out = run_commands(&[INIT, TICK, DOCK_V1, &at_1, GET_STATE]);
    assert_eq!(out[3], out[1], "the committed world at tick 1");
    assert_eq!(
        out[4]["payload"]["vessels"]["v1"]["state"], "docked",
        "planned"
    );
}

#[test]
fn flw2_0_00_past_committed_states_are_queryable() {
    // Also FLW2-1-02: after the spec's FLW1 run, every tick returns what init or that tick reported.
    let queries: Vec<String> = (0..=13).map(get_state_at).collect();
    let fixture = include_str!("fixtures/flw1_example.txt");
    let run: Vec<&str> = fixture
        .lines()
        .filter(|line| line.starts_with('{'))
        .collect();
    let commands: Vec<&str> = run
        .iter()
        .copied()
        .chain(queries.iter().map(String::as_str))
        .collect();
    let out = run_commands(&commands);
    let recorded: Vec<&Value> = run
        .iter()
        .zip(&out)
        .filter(|(command, _)| {
            command.contains(r#""command":"init""#) || command.contains(r#""command":"tick""#)
        })
        .map(|(_, response)| response)
        .collect();
    assert_eq!(recorded.len(), queries.len(), "init and 13 ticks");
    for (answer, recorded) in out[run.len()..].iter().zip(recorded) {
        assert_eq!(answer, recorded);
    }
}

#[test]
fn flw2_0_01_base_and_flw1_transcripts_pass_with_queries_interleaved() {
    // Also FLW2-0-00: get_state_at between commands changes no response of the original runs.
    assert_transcript(&with_queries(include_str!(
        "fixtures/base_example_flw1.txt"
    )));
    assert_transcript(&with_queries(include_str!("fixtures/flw1_example.txt")));
}
