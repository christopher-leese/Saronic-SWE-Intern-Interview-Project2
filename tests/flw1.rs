//! Black-box verification and validation of the FLW1 (cargo and dock) requirements through the
//! binary.

mod common;

use common::{assert_transcript, run_commands};
use serde_json::{json, Value};

/// The spec's FLW1 world: throughput [2, 4, 1]; v1 at 0, v2 at 2; dock 0 holds one unit each for
/// 2 and 1, dock 2 one unit for 0.
const INIT: &str = r#"{"command":"init","parameters":{"travel_times":[[0,4,7],[4,0,3],[7,3,0]],"dock_throughput":[2,4,1],"vessels":[{"id":"v1","location":0},{"id":"v2","location":2}],"cargo_manifest":[{"location":0,"destination":2,"quantity":1},{"location":0,"destination":1,"quantity":1},{"location":2,"destination":0,"quantity":1}]}}"#;
const TICK: &str = r#"{"command":"tick","parameters":{}}"#;
const GET_STATE: &str = r#"{"command":"get_state","parameters":{}}"#;

fn command(name: &str, parameters: Value) -> String {
    json!({"command": name, "parameters": parameters}).to_string()
}

fn dock(id: &str) -> String {
    command("dock", json!({"vessel_id": id}))
}

fn undock(id: &str) -> String {
    command("undock", json!({"vessel_id": id}))
}

fn load(id: &str, destination: usize) -> String {
    command("load", json!({"vessel_id": id, "destination": destination}))
}

fn unload(id: &str, destination: usize) -> String {
    command(
        "unload",
        json!({"vessel_id": id, "destination": destination}),
    )
}

fn swap(id: &str, load: usize, unload: usize) -> String {
    command(
        "swap",
        json!({"vessel_id": id, "load_destination": load, "unload_destination": unload}),
    )
}

fn set_destination(id: &str, destination: usize) -> String {
    command(
        "set_destination",
        json!({"vessel_id": id, "destination": destination}),
    )
}

fn tick_with(manifest: Value) -> String {
    command("tick", json!({ "cargo_manifest": manifest }))
}

/// Runs `INIT`, docks v1 and v2, ticks, then runs `commands`. Returns only their responses.
fn docked(commands: &[&str]) -> Vec<Value> {
    let (dock_v1, dock_v2) = (dock("v1"), dock("v2"));
    let mut all = vec![INIT, dock_v1.as_str(), dock_v2.as_str(), TICK];
    all.extend_from_slice(commands);
    run_commands(&all).split_off(4)
}

fn vessel<'a>(response: &'a Value, id: &str) -> &'a Value {
    &response["payload"]["vessels"][id]
}

fn docks(response: &Value) -> &Value {
    &response["payload"]["cargo"]
}

fn message(response: &Value) -> &Value {
    &response["payload"]["message"]
}

// --- validation against the spec ---

/// Validation: the interview spec's Follow-on 1 example, verbatim, with omitted ticks inserted.
#[test]
fn validation_flw1_spec_example() {
    assert_transcript(include_str!("fixtures/flw1_example.txt"));
}

#[test]
fn validation_flw1_spec_init_with_cargo_manifest() {
    let out = run_commands(&[INIT]);
    assert_eq!(
        out[0],
        json!({"status": "ok", "payload": {"tick": 0, "vessels": {
            "v1": {"state": "idle", "location": 0, "cargo": []},
            "v2": {"state": "idle", "location": 2, "cargo": []}},
            "cargo": [{"location": 0, "destination": 2, "quantity": 1},
                      {"location": 0, "destination": 1, "quantity": 1},
                      {"location": 2, "destination": 0, "quantity": 1}]}})
    );
}

#[test]
fn validation_flw1_spec_tick_with_cargo_manifest() {
    let init = r#"{"command":"init","parameters":{"travel_times":[[0,4,7],[4,0,3],[7,3,0]],"dock_throughput":[2,4,1],"vessels":[{"id":"v1","location":0},{"id":"v2","location":2}]}}"#;
    let tick = r#"{"command": "tick", "parameters": {"cargo_manifest": [{"location": 0, "destination": 2, "quantity": 1}, {"location": 0, "destination": 1, "quantity": 1}, {"location": 2, "destination": 0, "quantity": 1}]}}"#;
    let out = run_commands(&[init, tick]);
    assert_eq!(
        out[1],
        json!({"status": "ok", "payload": {"tick": 1, "vessels": {
            "v1": {"state": "idle", "location": 0, "cargo": []},
            "v2": {"state": "idle", "location": 2, "cargo": []}},
            "cargo": [{"location": 0, "destination": 2, "quantity": 1},
                      {"location": 0, "destination": 1, "quantity": 1},
                      {"location": 2, "destination": 0, "quantity": 1}]}})
    );
}

#[test]
fn validation_flw1_spec_swap() {
    // The spec's state: tick 2, v1 docked at 0 with cargo for 2; dock 0 has cargo for 1.
    let init = r#"{"command":"init","parameters":{"travel_times":[[0,4,7],[4,0,3],[7,3,0]],"dock_throughput":[2,4,1],"vessels":[{"id":"v1","location":0},{"id":"v2","location":2}],"cargo_manifest":[{"location":0,"destination":2,"quantity":1},{"location":0,"destination":1,"quantity":1}]}}"#;
    let swap = r#"{"command": "swap", "parameters": {"vessel_id": "v1", "load_destination": 1, "unload_destination": 2}}"#;
    let out = run_commands(&[init, &dock("v1"), TICK, &load("v1", 2), TICK, swap]);
    assert_eq!(
        out[5],
        json!({"status": "ok", "payload": {"tick": 2, "vessels": {
            "v1": {"state": "docked", "location": 0, "cargo": [{"destination": 1, "quantity": 1}]},
            "v2": {"state": "idle", "location": 2, "cargo": []}},
            "cargo": [{"location": 0, "destination": 2, "quantity": 1}]}})
    );
}

#[test]
fn validation_flw1_spec_failure_messages() {
    let out = docked(&[
        &dock("v1"),
        &undock("v1"),
        &undock("v1"),
        &load("v1", 1),
        &load("v1", 2),
        &load("v1", 1),
    ]);
    assert_eq!(*message(&out[0]), "vessel v1 is not idle");
    assert_eq!(*message(&out[2]), "vessel v1 is not docked");
    assert_eq!(out[3]["status"], "ok", "load overrides the pending undock");
    assert_eq!(
        *message(&out[5]),
        "transfer of 3 operations exceeds dock throughput of 2 at location 0"
    );
    let out = run_commands(&[INIT, &load("v1", 1), &unload("v1", 1)]);
    assert_eq!(*message(&out[1]), "vessel v1 is not docked");
    assert_eq!(*message(&out[2]), "vessel v1 is not docked");
}

#[test]
fn validation_flw1_spec_get_state() {
    // The spec's state: tick 3 of the FLW1 example, after undock.
    let init = r#"{"command":"init","parameters":{"travel_times":[[0,4,7],[4,0,3],[7,3,0]],"dock_throughput":[2,4,1],"vessels":[{"id":"v1","location":0},{"id":"v2","location":2}]}}"#;
    let manifest = json!([{"location": 0, "destination": 2, "quantity": 1},
                          {"location": 0, "destination": 1, "quantity": 1}]);
    let out = run_commands(&[
        init,
        &tick_with(manifest),
        &dock("v1"),
        TICK,
        &load("v1", 2),
        &load("v1", 1),
        TICK,
        &undock("v1"),
        r#"{"command": "get_state", "parameters": {}}"#,
    ]);
    assert_eq!(
        out[8],
        json!({"status": "ok", "payload": {"tick": 3, "vessels": {
            "v1": {"state": "idle", "location": 0, "cargo": [
                {"destination": 2, "quantity": 1}, {"destination": 1, "quantity": 1}]},
            "v2": {"state": "idle", "location": 2, "cargo": []}},
            "cargo": []}})
    );
}

// --- verification of FLW1-0-00 and its L1 children through the binary ---

#[test]
fn flw1_0_00_every_cargo_and_dock_command_is_supported() {
    let out = docked(&[
        &load("v1", 2),
        TICK,
        &swap("v1", 1, 2),
        TICK,
        &unload("v1", 1),
        &undock("v1"),
        TICK,
        &dock("v1"),
    ]);
    for response in &out {
        assert_eq!(response["status"], "ok", "{response}");
    }
}

#[test]
fn flw1_1_00_each_location_has_a_dock_inventory() {
    let out = run_commands(&[INIT]);
    let locations: Vec<_> = docks(&out[0])
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["location"].clone())
        .collect();
    assert_eq!(locations, [json!(0), json!(0), json!(2)]);
}

#[test]
fn flw1_1_01_each_dock_limits_operations_per_tick() {
    // Dock 0 allows 2 operations per tick; dock 2's budget is separate; a tick resets both.
    let out = docked(&[
        &load("v1", 2),
        &load("v1", 1),
        &load("v1", 2),
        &load("v2", 0),
        TICK,
        &unload("v1", 2),
    ]);
    let statuses: Vec<_> = out.iter().map(|r| r["status"].clone()).collect();
    assert_eq!(statuses, ["ok", "ok", "error", "ok", "ok", "ok"]);
    let out = docked(&[&load("v2", 0), &swap("v2", 0, 0)]);
    assert_eq!(
        *message(&out[1]),
        "transfer of 2 operations exceeds dock throughput of 1 at location 2"
    );
}

#[test]
fn flw1_1_02_vessels_load_from_the_dock_where_they_are() {
    let out = docked(&[&load("v1", 0), &load("v2", 0)]);
    assert_eq!(out[0]["status"], "error", "the unit for 0 is at dock 2");
    assert_eq!(
        vessel(&out[1], "v2")["cargo"],
        json!([{"destination": 0, "quantity": 1}])
    );
}

#[test]
fn flw1_1_03_vessels_unload_at_any_dock() {
    let out = docked(&[&load("v1", 1), TICK, &unload("v1", 1)]);
    assert_eq!(vessel(&out[2], "v1")["cargo"], json!([]));
    assert_eq!(
        *docks(&out[2]),
        json!([{"location": 0, "destination": 2, "quantity": 1},
               {"location": 2, "destination": 0, "quantity": 1},
               {"location": 0, "destination": 1, "quantity": 1}])
    );
}

#[test]
fn flw1_1_04_every_world_state_reports_dock_and_vessel_cargo() {
    // v1 carries its unit for 1 through the docked, idle, and transit states.
    let out = docked(&[
        &load("v1", 1),
        TICK,
        &undock("v1"),
        TICK,
        &set_destination("v1", 1),
        TICK,
        GET_STATE,
    ]);
    let states = [
        "docked", "docked", "idle", "idle", "transit", "transit", "transit",
    ];
    let dock_cargo = json!([{"location": 0, "destination": 2, "quantity": 1},
                            {"location": 2, "destination": 0, "quantity": 1}]);
    assert_eq!(out.len(), states.len());
    for (response, state) in out.iter().zip(states) {
        assert_eq!(response["status"], "ok", "{response}");
        assert_eq!(vessel(response, "v1")["state"], state);
        assert_eq!(
            vessel(response, "v1")["cargo"],
            json!([{"destination": 1, "quantity": 1}])
        );
        assert_eq!(vessel(response, "v2")["cargo"], json!([]));
        assert_eq!(*docks(response), dock_cargo);
    }
}

#[test]
fn flw1_1_05_cargo_enters_the_docks_through_manifests() {
    let manifest = json!([{"location": 1, "destination": 0, "quantity": 3}]);
    let out = run_commands(&[INIT, &tick_with(manifest)]);
    assert_eq!(
        docks(&out[1])[3],
        json!({"location": 1, "destination": 0, "quantity": 3})
    );
}

#[test]
fn flw1_1_06_an_idle_vessel_can_dock() {
    let out = run_commands(&[INIT, &dock("v1"), TICK]);
    assert_eq!(
        *vessel(&out[1], "v1"),
        json!({"state": "docked", "location": 0, "cargo": []})
    );
    assert_eq!(vessel(&out[2], "v1")["state"], "docked");
}

#[test]
fn flw1_1_07_set_destination_overrides_a_planned_dock() {
    let out = run_commands(&[INIT, &dock("v1"), &set_destination("v1", 1), TICK]);
    assert_eq!(vessel(&out[3], "v1")["state"], "transit");
}

#[test]
fn flw1_1_08_dock_overrides_a_planned_set_destination() {
    let out = run_commands(&[INIT, &set_destination("v1", 1), &dock("v1"), TICK]);
    assert_eq!(vessel(&out[3], "v1")["state"], "docked");
}

#[test]
fn flw1_1_09_a_docked_vessel_can_load() {
    let out = docked(&[&load("v1", 2), TICK]);
    assert_eq!(
        vessel(&out[1], "v1")["cargo"],
        json!([{"destination": 2, "quantity": 1}])
    );
}

#[test]
fn flw1_1_10_a_docked_vessel_can_unload() {
    let out = docked(&[&load("v1", 2), TICK, &unload("v1", 2), TICK]);
    assert_eq!(vessel(&out[3], "v1")["cargo"], json!([]));
}

#[test]
fn flw1_1_11_a_docked_vessel_can_undock() {
    let out = docked(&[&undock("v1"), TICK]);
    assert_eq!(
        *vessel(&out[1], "v1"),
        json!({"state": "idle", "location": 0, "cargo": []})
    );
}

#[test]
fn flw1_1_12_dock_after_dock_is_an_error() {
    let out = run_commands(&[INIT, &dock("v1"), &dock("v1")]);
    assert_eq!(*message(&out[2]), "vessel v1 is already docked");
    let out = docked(&[&dock("v1")]);
    assert_eq!(*message(&out[0]), "vessel v1 is not idle");
}

#[test]
fn flw1_3_14_dock_after_undock_returns_planned_state_to_docked() {
    // Also FLW1-1-15: the load the undock dropped stays dropped.
    let out = docked(&[GET_STATE, &load("v1", 2), &undock("v1"), &dock("v1"), TICK]);
    assert_eq!(out[3], out[0], "back to the state after the last tick");
    assert_eq!(
        *vessel(&out[4], "v1"),
        json!({"state": "docked", "location": 0, "cargo": []})
    );
}

#[test]
fn flw1_1_13_undock_when_not_docked_is_an_error() {
    let out = run_commands(&[INIT, &undock("v1")]);
    assert_eq!(*message(&out[1]), "vessel v1 is not docked");
    let out = docked(&[&undock("v1"), &undock("v1")]);
    assert_eq!(*message(&out[1]), "vessel v1 is not docked");
}

#[test]
fn flw1_1_13_undock_cancels_a_planned_dock() {
    let out = run_commands(&[INIT, &dock("v1"), &undock("v1"), TICK]);
    assert_eq!(out[2], out[0], "back to the state at init");
    assert_eq!(vessel(&out[3], "v1")["state"], "idle");
}

#[test]
fn flw1_1_14_swap_loads_and_unloads_at_once() {
    let out = docked(&[&load("v1", 2), TICK, &swap("v1", 1, 2)]);
    assert_eq!(
        vessel(&out[2], "v1")["cargo"],
        json!([{"destination": 1, "quantity": 1}])
    );
}

#[test]
fn flw1_1_15_undock_overrides_pending_cargo_operations() {
    let out = docked(&[GET_STATE, &load("v1", 2), &load("v1", 1), &undock("v1")]);
    assert_eq!(vessel(&out[3], "v1")["state"], "idle");
    assert_eq!(vessel(&out[3], "v1")["cargo"], json!([]));
    assert_eq!(docks(&out[3]), docks(&out[0]));
}

#[test]
fn flw1_1_16_cargo_operations_override_a_pending_undock() {
    let out = docked(&[&undock("v1"), &load("v1", 2), TICK]);
    assert_eq!(vessel(&out[1], "v1")["state"], "docked");
    assert_eq!(
        *vessel(&out[2], "v1"),
        json!({"state": "docked", "location": 0, "cargo": [{"destination": 2, "quantity": 1}]})
    );
}

#[test]
fn flw1_1_17_get_state_includes_planned_cargo_and_docks() {
    let out = docked(&[&load("v1", 1), GET_STATE]);
    assert_eq!(out[1], out[0]);
    assert_eq!(
        vessel(&out[1], "v1")["cargo"],
        json!([{"destination": 1, "quantity": 1}])
    );
}
