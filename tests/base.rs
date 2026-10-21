//! Black-box verification and validation of the BASE requirements through the binary, in the
//! FLW1 input and output format (init requires dock_throughput; world states include cargo).

mod common;

use common::{assert_transcript, run, run_commands};
use serde_json::json;

const INIT: &str = r#"{"command":"init","parameters":{"travel_times":[[0,4,7],[4,0,3],[7,3,0]],"dock_throughput":[2,4,1],"vessels":[{"id":"v1","location":0},{"id":"v2","location":2}]}}"#;
const TICK: &str = r#"{"command":"tick","parameters":{}}"#;
const GET_STATE: &str = r#"{"command":"get_state","parameters":{}}"#;

fn set_destination(id: &str, destination: usize) -> String {
    format!(
        r#"{{"command":"set_destination","parameters":{{"vessel_id":"{id}","destination":{destination}}}}}"#
    )
}

/// Validation: the interview spec's Base Problem example with omitted ticks inserted, in the FLW1
/// format (see the fixture's header for the exact changes).
#[test]
fn validation_base_spec_example_in_flw1_format() {
    assert_transcript(include_str!("fixtures/base_example_flw1.txt"));
}

#[test]
fn base_0_00_time_advances_in_discrete_unit_steps() {
    let out = run_commands(&[INIT, TICK, TICK, TICK]);
    let ticks: Vec<_> = out.iter().map(|r| r["payload"]["tick"].clone()).collect();
    assert_eq!(ticks, [json!(0), json!(1), json!(2), json!(3)]);
}

#[test]
fn base_0_01_engine_acts_only_on_external_commands() {
    assert!(run("").is_empty(), "no commands, no output");
    let out = run_commands(&[INIT, GET_STATE, GET_STATE]);
    assert_eq!(out[1], out[2], "state does not change without commands");
}

#[test]
fn base_0_02_vessel_state_is_tracked_through_every_tick() {
    let set = set_destination("v2", 1);
    let out = run_commands(&[INIT, &set, TICK, TICK, TICK]);
    assert_eq!(out[2]["payload"]["vessels"]["v2"]["state"], "transit");
    assert_eq!(out[3]["payload"]["vessels"]["v2"]["state"], "transit");
    assert_eq!(
        out[4]["payload"]["vessels"]["v2"],
        json!({"state": "idle", "location": 1, "cargo": []})
    );
}

#[test]
fn base_0_03_every_success_reports_all_vessel_states() {
    let set = set_destination("v1", 1);
    for response in run_commands(&[INIT, &set, TICK, GET_STATE]) {
        let vessels = response["payload"]["vessels"].as_object().unwrap();
        assert_eq!(vessels.len(), 2);
    }
}

#[test]
fn base_0_04_input_and_output_are_json() {
    let out = run(&format!("{INIT}\n{TICK}\n"));
    for line in out {
        serde_json::from_str::<serde_json::Value>(&line).expect("output line is JSON");
    }
}

#[test]
fn base_0_05_exactly_one_output_line_per_command() {
    let bad = set_destination("v9", 1);
    let out = run(&format!(
        "{TICK}\n{INIT}\n{INIT}\nnot json\n{bad}\n{GET_STATE}\n"
    ));
    assert_eq!(out.len(), 6);
}

#[test]
fn base_0_06_commands_are_processed_in_given_order() {
    let first = set_destination("v1", 2);
    let second = set_destination("v1", 1);
    let out = run_commands(&[INIT, &first, &second, GET_STATE]);
    assert_eq!(out[3]["payload"]["vessels"]["v1"]["to"], 1);
    let out = run_commands(&[INIT, &second, &first, GET_STATE]);
    assert_eq!(out[3]["payload"]["vessels"]["v1"]["to"], 2);
}

#[test]
fn base_0_07_planned_and_committed_states_are_separate() {
    let first = set_destination("v1", 2);
    let second = set_destination("v1", 1);
    let out = run_commands(&[INIT, &first, &second, TICK, &first]);
    assert_eq!(
        out[1]["status"], "ok",
        "planned change while committed idle"
    );
    assert_eq!(
        out[2]["status"], "ok",
        "still committed idle, so override allowed"
    );
    assert_eq!(out[3]["payload"]["vessels"]["v1"]["to"], 1);
    assert_eq!(out[4]["status"], "error", "committed now in transit");
}

#[test]
fn base_1_00_successes_are_reported_with_status_ok() {
    for response in run_commands(&[INIT, TICK, GET_STATE]) {
        assert_eq!(response["status"], "ok");
        assert!(response["payload"]["tick"].is_u64());
    }
}

#[test]
fn base_1_01_errors_are_reported_with_status_error_and_message() {
    let out = run_commands(&[TICK, "not json", INIT, INIT]);
    for index in [0, 1, 3] {
        assert_eq!(out[index]["status"], "error");
        let payload = out[index]["payload"].as_object().unwrap();
        assert_eq!(payload.len(), 1, "payload holds only the message");
        assert!(payload["message"].is_string());
    }
}

#[test]
fn base_1_04_responses_go_to_stdout() {
    // `run` fails the test if anything is written to stderr.
    assert_eq!(run(&format!("{INIT}\n")).len(), 1);
}

#[test]
fn base_1_05_reads_newline_delimited_json_including_crlf() {
    let out = run(&format!("{INIT}\r\n{TICK}\r\n\n{GET_STATE}"));
    assert_eq!(out.len(), 3);
    assert!(out.iter().all(|line| line.contains(r#""status":"ok""#)));
}

#[test]
fn base_1_11_init_must_come_first_and_only_once() {
    let out = run_commands(&[GET_STATE, INIT, INIT, GET_STATE]);
    assert_eq!(out[0]["status"], "error");
    assert_eq!(out[1]["payload"]["tick"], 0);
    assert_eq!(out[2]["status"], "error");
    assert_eq!(out[3]["status"], "ok");
}

fn vessel<'a>(response: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    &response["payload"]["vessels"][id]
}

#[test]
fn base_1_02_commands_take_effect_in_order_received() {
    let go = set_destination("v1", 1);
    let out = run_commands(&[INIT, TICK, &go]);
    assert_eq!(
        vessel(&out[2], "v1")["arrives_at_tick"],
        5,
        "issued at tick 1"
    );
    let out = run_commands(&[INIT, &go, TICK]);
    assert_eq!(
        vessel(&out[2], "v1")["arrives_at_tick"],
        4,
        "issued at tick 0"
    );
}

#[test]
fn base_1_03_response_already_shows_the_planned_update() {
    let out = run_commands(&[INIT, &set_destination("v1", 1)]);
    assert_eq!(
        *vessel(&out[1], "v1"),
        json!({"state": "transit", "from": 0, "to": 1, "arrives_at_tick": 4, "cargo": []})
    );
}

#[test]
fn base_1_06_tick_reports_a_snapshot_of_the_planned_state() {
    let out = run_commands(&[INIT, &set_destination("v1", 1), GET_STATE, TICK]);
    assert_eq!(out[3]["payload"]["vessels"], out[2]["payload"]["vessels"]);
    assert_eq!(out[3]["payload"]["tick"], 1);
}

#[test]
fn base_1_07_planned_state_starts_from_the_last_tick() {
    let out = run_commands(&[INIT, TICK, GET_STATE, &set_destination("v2", 0), GET_STATE]);
    assert_eq!(out[2], out[1], "no command since the tick");
    assert_eq!(out[4]["payload"]["tick"], 1);
    assert_eq!(*vessel(&out[4], "v1"), *vessel(&out[1], "v1"));
    assert_eq!(vessel(&out[4], "v2")["arrives_at_tick"], 8);
}

#[test]
fn base_1_08_planned_state_updates_with_each_command() {
    let out = run_commands(&[
        INIT,
        &set_destination("v1", 1),
        GET_STATE,
        &set_destination("v1", 2),
        GET_STATE,
    ]);
    assert_eq!(vessel(&out[2], "v1")["to"], 1);
    assert_eq!(vessel(&out[4], "v1")["to"], 2);
    assert_eq!(out[4]["payload"]["tick"], 0);
}

#[test]
fn base_1_09_tick_commits_the_planned_state() {
    let go = set_destination("v1", 1);
    let out = run_commands(&[INIT, &go, TICK, GET_STATE, &go]);
    assert_eq!(vessel(&out[3], "v1")["state"], "transit");
    assert_eq!(out[4]["status"], "error", "committed state is now transit");
}

#[test]
fn base_1_10_each_tick_advances_one_step() {
    let out = run_commands(&[INIT, TICK, TICK, GET_STATE, TICK]);
    let ticks: Vec<_> = out.iter().map(|r| r["payload"]["tick"].clone()).collect();
    assert_eq!(ticks, [json!(0), json!(1), json!(2), json!(2), json!(3)]);
}

#[test]
fn base_1_12_only_idle_vessels_travel_and_only_to_a_new_location() {
    let out = run_commands(&[
        INIT,
        &set_destination("v1", 0),
        &set_destination("v1", 1),
        TICK,
        &set_destination("v1", 2),
    ]);
    let statuses: Vec<_> = out.iter().map(|r| r["status"].clone()).collect();
    assert_eq!(statuses, ["ok", "error", "ok", "ok", "error"]);
    assert_eq!(
        out[1]["payload"]["message"],
        "vessel v1 is already at location 0"
    );
    assert_eq!(out[4]["payload"]["message"], "vessel v1 is not idle");
}

#[test]
fn base_1_13_get_state_returns_the_planned_state() {
    let go = set_destination("v2", 1);
    let out = run_commands(&[INIT, &go, GET_STATE]);
    assert_eq!(out[2], out[1]);
    assert_eq!(vessel(&out[2], "v2")["state"], "transit");
}

#[test]
fn base_3_07_init_vessels_are_a_list_of_id_and_location() {
    let missing_location = r#"{"command":"init","parameters":{"travel_times":[[0]],"dock_throughput":[1],"vessels":[{"id":"v1"}]}}"#;
    let out = run_commands(&[missing_location, INIT]);
    assert_eq!(out[0]["status"], "error");
    assert_eq!(
        *vessel(&out[1], "v2"),
        json!({"state": "idle", "location": 2, "cargo": []})
    );
}
