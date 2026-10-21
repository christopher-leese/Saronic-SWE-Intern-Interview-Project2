//! The JSON protocol: one command per input line, one response per output line.

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::world::{DockCargo, World};

/// One input command, e.g. `{"command": "tick", "parameters": {}}`. Satisfies BASE-0-04.
/// Unknown fields are rejected rather than ignored, so a misspelled field is reported.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "command",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Command {
    Init {
        travel_times: Vec<Vec<u64>>,
        dock_throughput: Vec<u64>,
        vessels: Vec<VesselSpec>,
        /// Optional (FLW1-2-03).
        #[serde(default)]
        cargo_manifest: Vec<DockCargo>,
    },
    Tick {
        /// Optional (FLW1-2-04).
        #[serde(default)]
        cargo_manifest: Vec<DockCargo>,
    },
    SetDestination {
        vessel_id: String,
        destination: usize,
    },
    GetState {},
    /// Satisfies FLW2-1-02. `tick` is unsigned, so a negative or non-integer tick is an invalid
    /// command (FLW2-2-20).
    GetStateAt {
        tick: u64,
    },
    Dock {
        vessel_id: String,
    },
    Undock {
        vessel_id: String,
    },
    Load {
        vessel_id: String,
        destination: usize,
    },
    Unload {
        vessel_id: String,
        destination: usize,
    },
    Swap {
        vessel_id: String,
        load_destination: usize,
        unload_destination: usize,
    },
}

/// One entry of init's `vessels` list. Satisfies BASE-3-07.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VesselSpec {
    pub id: String,
    pub location: usize,
}

/// One output line. Satisfies BASE-1-00 and BASE-3-00 (`{"status": "ok", "payload":
/// <WorldState>}`), BASE-1-01 and BASE-3-01 (`{"status": "error", "payload": {"message":
/// "<reason>"}}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "payload", rename_all = "lowercase")]
pub enum Response {
    Ok(World),
    Error { message: String },
}

impl From<Result<World, EngineError>> for Response {
    fn from(result: Result<World, EngineError>) -> Self {
        match result {
            Ok(world) => Self::Ok(world),
            Err(error) => Self::Error {
                message: error.to_string(),
            },
        }
    }
}

/// Parses one input line. Satisfies BASE-1-05 (with `run`).
pub fn parse(line: &[u8]) -> Result<Command, EngineError> {
    serde_json::from_slice(line).map_err(|error| EngineError::InvalidCommand(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Vessel, VesselState};

    #[test]
    fn base_3_00_success_response_shape() {
        let world = World {
            tick: 0,
            vessels: vec![Vessel {
                id: "v1".into(),
                state: VesselState::Idle { location: 0 },
                cargo: vec![],
            }],
            cargo: vec![],
        };
        let text = serde_json::to_string(&Response::from(Ok(world))).unwrap();
        assert_eq!(
            text,
            r#"{"status":"ok","payload":{"tick":0,"vessels":{"v1":{"state":"idle","location":0,"cargo":[]}},"cargo":[]}}"#
        );
    }

    #[test]
    fn base_3_01_error_response_shape() {
        let response = Response::from(Err(EngineError::VesselNotIdle("v1".into())));
        let text = serde_json::to_string(&response).unwrap();
        assert_eq!(
            text,
            r#"{"status":"error","payload":{"message":"vessel v1 is not idle"}}"#
        );
    }

    #[test]
    fn base_3_07_init_vessels_are_a_list_of_id_and_location() {
        let line = br#"{"command": "init", "parameters": {"travel_times": [[0,4,7],[4,0,3],[7,3,0]], "dock_throughput": [2,4,1], "vessels": [{"id": "v1", "location": 0}, {"id": "v2", "location": 2}]}}"#;
        let Command::Init { vessels, .. } = parse(line).unwrap() else {
            panic!("expected init");
        };
        let ids: Vec<_> = vessels
            .iter()
            .map(|v| (v.id.as_str(), v.location))
            .collect();
        assert_eq!(ids, [("v1", 0), ("v2", 2)]);
    }

    #[test]
    fn base_3_07_vessel_without_location_is_rejected() {
        let line =
            br#"{"command":"init","parameters":{"travel_times":[[0]],"dock_throughput":[1],"vessels":[{"id":"v1"}]}}"#;
        assert!(matches!(parse(line), Err(EngineError::InvalidCommand(_))));
    }

    #[test]
    fn base_1_05_parses_each_spec_command() {
        assert_eq!(
            parse(br#"{"command": "tick", "parameters": {}}"#),
            Ok(Command::Tick {
                cargo_manifest: vec![]
            })
        );
        assert_eq!(
            parse(br#"{"command": "get_state", "parameters": {}}"#),
            Ok(Command::GetState {})
        );
        assert_eq!(
            parse(br#"{"command": "set_destination", "parameters": {"vessel_id": "v1", "destination": 1}}"#),
            Ok(Command::SetDestination {
                vessel_id: "v1".into(),
                destination: 1
            })
        );
    }

    #[test]
    fn base_1_05_malformed_lines_are_invalid_commands() {
        for line in [
            &b"not json"[..],
            br#"{"command": "fly", "parameters": {}}"#,
            br#"{"command": "set_destination", "parameters": {"vessel_id": "v1"}}"#,
            br#"{"command": "set_destination", "parameters": {"vessel_id": "v1", "destination": -1}}"#,
            br#"{"command":"init","parameters":{"travel_times":[[0,-4],[4,0]],"vessels":[]}}"#,
        ] {
            assert!(matches!(parse(line), Err(EngineError::InvalidCommand(_))));
        }
    }

    #[test]
    fn base_1_05_unknown_fields_are_invalid_commands() {
        for line in [
            &br#"{"command": "tick", "parameters": {"x": 1}}"#[..],
            br#"{"command": "tick", "parameters": {}, "extra": 1}"#,
            br#"{"command": "set_destination", "parameters": {"vessel_id": "v1", "destination": 1, "speed": 2}}"#,
            br#"{"command":"init","parameters":{"travel_times":[[0]],"dock_throughput":[1],"vessels":[{"id":"v1","location":0,"cargo":1}]}}"#,
            br#"{"command":"tick","parameters":{"cargo_manifest":[{"location":0,"destination":0,"quantity":1,"x":0}]}}"#,
        ] {
            assert!(
                matches!(parse(line), Err(EngineError::InvalidCommand(_))),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn flw1_2_00_cargo_manifest_is_a_field_of_init_and_tick() {
        // Also FLW1-2-03, FLW1-2-04 (optional) and FLW1-2-05 (entry format).
        let entry = DockCargo {
            location: 0,
            destination: 2,
            quantity: 1,
        };
        let Ok(Command::Init { cargo_manifest, .. }) = parse(
            br#"{"command":"init","parameters":{"travel_times":[[0]],"dock_throughput":[1],"vessels":[],"cargo_manifest":[{"location":0,"destination":2,"quantity":1}]}}"#,
        ) else {
            panic!("expected init");
        };
        assert_eq!(cargo_manifest, [entry]);
        assert_eq!(
            parse(br#"{"command":"tick","parameters":{"cargo_manifest":[{"location":0,"destination":2,"quantity":1}]}}"#),
            Ok(Command::Tick {
                cargo_manifest: vec![entry]
            })
        );
    }

    #[test]
    fn flw1_2_03_init_without_cargo_manifest_has_an_empty_one() {
        let Ok(Command::Init {
            cargo_manifest,
            dock_throughput,
            ..
        }) = parse(
            br#"{"command":"init","parameters":{"travel_times":[[0]],"dock_throughput":[3],"vessels":[]}}"#,
        )
        else {
            panic!("expected init");
        };
        assert!(cargo_manifest.is_empty());
        assert_eq!(dock_throughput, [3]);
    }

    #[test]
    fn flw1_2_01_init_requires_dock_throughput() {
        let line = br#"{"command":"init","parameters":{"travel_times":[[0]],"vessels":[]}}"#;
        assert!(matches!(parse(line), Err(EngineError::InvalidCommand(_))));
    }

    #[test]
    fn flw1_2_05_manifest_entries_need_location_destination_and_quantity() {
        for line in [
            &br#"{"command":"tick","parameters":{"cargo_manifest":[{"location":0,"destination":1}]}}"#[..],
            br#"{"command":"tick","parameters":{"cargo_manifest":[{"location":0,"quantity":1}]}}"#,
            br#"{"command":"tick","parameters":{"cargo_manifest":[{"location":0,"destination":1,"quantity":-1}]}}"#,
        ] {
            assert!(matches!(parse(line), Err(EngineError::InvalidCommand(_))));
        }
    }

    #[test]
    fn flw2_1_02_parses_get_state_at() {
        assert_eq!(
            parse(br#"{"command": "get_state_at", "parameters": {"tick": 5}}"#),
            Ok(Command::GetStateAt { tick: 5 })
        );
    }

    #[test]
    fn flw2_2_20_malformed_ticks_are_invalid_commands() {
        for line in [
            &br#"{"command": "get_state_at", "parameters": {"tick": -1}}"#[..],
            br#"{"command": "get_state_at", "parameters": {"tick": 1.5}}"#,
            br#"{"command": "get_state_at", "parameters": {}}"#,
        ] {
            assert!(matches!(parse(line), Err(EngineError::InvalidCommand(_))));
        }
    }

    #[test]
    fn flw1_0_00_parses_each_cargo_and_dock_command() {
        let id = || "v1".to_string();
        let cases: [(&[u8], Command); 5] = [
            (
                br#"{"command": "dock", "parameters": {"vessel_id": "v1"}}"#,
                Command::Dock { vessel_id: id() },
            ),
            (
                br#"{"command": "undock", "parameters": {"vessel_id": "v1"}}"#,
                Command::Undock { vessel_id: id() },
            ),
            (
                br#"{"command": "load", "parameters": {"vessel_id": "v1", "destination": 2}}"#,
                Command::Load {
                    vessel_id: id(),
                    destination: 2,
                },
            ),
            (
                br#"{"command": "unload", "parameters": {"vessel_id": "v1", "destination": 2}}"#,
                Command::Unload {
                    vessel_id: id(),
                    destination: 2,
                },
            ),
            (
                br#"{"command": "swap", "parameters": {"vessel_id": "v1", "load_destination": 1, "unload_destination": 2}}"#,
                Command::Swap {
                    vessel_id: id(),
                    load_destination: 1,
                    unload_destination: 2,
                },
            ),
        ];
        for (line, command) in cases {
            assert_eq!(parse(line), Ok(command));
        }
    }
}
