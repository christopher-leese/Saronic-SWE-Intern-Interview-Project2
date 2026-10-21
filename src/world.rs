//! World state: what the engine tracks and reports at each point in time.

use serde::{Deserialize, Serialize, Serializer};

use crate::error::EngineError;

/// A snapshot of the world at one tick. Planned and committed states share this type.
/// Satisfies BASE-0-02 and BASE-0-03 (vessel states tracked and reported per tick), and FLW1-1-04
/// (dock and vessel cargo inventories reported in every world state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct World {
    pub tick: u64,
    #[serde(serialize_with = "vessels_as_map")]
    pub vessels: Vec<Vessel>,
    /// Every dock's inventory in one list, in first-insertion order (FLW1-1-00).
    pub cargo: Vec<DockCargo>,
}

/// A vessel, its state, and its cargo. Kept in a `Vec` so output follows init order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Vessel {
    #[serde(skip)]
    pub id: String,
    #[serde(flatten)]
    pub state: VesselState,
    pub cargo: Vec<Cargo>,
}

/// Serializes as `{"state": "idle", "location": 0}`, `{"state": "docked", "location": 0}`, or
/// `{"state": "transit", "from": 0, "to": 1, "arrives_at_tick": 4}`, as in the spec examples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum VesselState {
    Idle {
        location: usize,
    },
    Transit {
        from: usize,
        to: usize,
        arrives_at_tick: u64,
    },
    Docked {
        location: usize,
    },
}

/// Cargo on a vessel. Cargo is represented only by its destination (FLW1-2-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Cargo {
    pub destination: usize,
    pub quantity: u64,
}

/// Cargo at a dock, and also the format of a `cargo_manifest` entry (FLW1-2-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockCargo {
    pub location: usize,
    pub destination: usize,
    pub quantity: u64,
}

impl World {
    /// Index of a vessel by id. Planned and committed worlds hold the same vessels in the same
    /// order, so one index is valid in both.
    pub fn vessel_index(&self, id: &str) -> Option<usize> {
        self.vessels.iter().position(|vessel| vessel.id == id)
    }

    pub fn add_dock_cargo(&mut self, units: DockCargo) -> Result<(), EngineError> {
        add(&mut self.cargo, units)
    }

    /// Removes one unit from a dock; false if it has none.
    pub fn take_dock_cargo(&mut self, location: usize, destination: usize) -> bool {
        take_one(
            &mut self.cargo,
            DockCargo {
                location,
                destination,
                quantity: 1,
            },
        )
    }
}

impl Vessel {
    pub fn add_cargo(&mut self, destination: usize) -> Result<(), EngineError> {
        add(
            &mut self.cargo,
            Cargo {
                destination,
                quantity: 1,
            },
        )
    }

    /// Removes one unit from the vessel; false if it has none.
    pub fn take_cargo(&mut self, destination: usize) -> bool {
        take_one(
            &mut self.cargo,
            Cargo {
                destination,
                quantity: 1,
            },
        )
    }
}

/// An inventory entry: units that are indistinguishable from each other, and how many.
/// Implemented by vessel cargo and dock cargo so both inventories share `add` and `take_one`.
trait Units {
    fn same_units(&self, other: &Self) -> bool;
    fn quantity(&mut self) -> &mut u64;
}

impl Units for Cargo {
    fn same_units(&self, other: &Self) -> bool {
        self.destination == other.destination
    }
    fn quantity(&mut self) -> &mut u64 {
        &mut self.quantity
    }
}

impl Units for DockCargo {
    fn same_units(&self, other: &Self) -> bool {
        (self.location, self.destination) == (other.location, other.destination)
    }
    fn quantity(&mut self) -> &mut u64 {
        &mut self.quantity
    }
}

/// Adds units to a matching entry, or appends a new entry, so entries keep first-insertion order.
fn add<T: Units>(entries: &mut Vec<T>, mut units: T) -> Result<(), EngineError> {
    match entries.iter_mut().find(|entry| entry.same_units(&units)) {
        Some(entry) => {
            let total = entry.quantity().checked_add(*units.quantity());
            *entry.quantity() = total.ok_or(EngineError::CargoOverflow)?;
        }
        None => entries.push(units),
    }
    Ok(())
}

/// Removes one unit; an entry whose quantity reaches zero is dropped, as in the spec examples.
fn take_one<T: Units>(entries: &mut Vec<T>, like: T) -> bool {
    let Some(index) = entries.iter().position(|entry| entry.same_units(&like)) else {
        return false;
    };
    let entry = &mut entries[index];
    *entry.quantity() -= 1; // Entries are never stored with quantity 0, so this cannot underflow.
    if *entry.quantity() == 0 {
        entries.remove(index);
    }
    true
}

fn vessels_as_map<S: Serializer>(vessels: &[Vessel], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_map(vessels.iter().map(|vessel| (&vessel.id, vessel)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn world() -> World {
        World {
            tick: 1,
            vessels: vec![
                Vessel {
                    id: "v1".into(),
                    state: VesselState::Transit {
                        from: 0,
                        to: 1,
                        arrives_at_tick: 4,
                    },
                    cargo: vec![Cargo {
                        destination: 1,
                        quantity: 2,
                    }],
                },
                Vessel {
                    id: "v2".into(),
                    state: VesselState::Docked { location: 2 },
                    cargo: vec![],
                },
            ],
            cargo: vec![DockCargo {
                location: 2,
                destination: 0,
                quantity: 1,
            }],
        }
    }

    fn units(location: usize, destination: usize, quantity: u64) -> DockCargo {
        DockCargo {
            location,
            destination,
            quantity,
        }
    }

    #[test]
    fn base_0_03_world_serializes_as_spec_world_state() {
        // Also FLW1-1-04: dock and vessel cargo are part of every world state.
        let expected = json!({"tick": 1, "vessels": {
            "v1": {"state": "transit", "from": 0, "to": 1, "arrives_at_tick": 4,
                   "cargo": [{"destination": 1, "quantity": 2}]},
            "v2": {"state": "docked", "location": 2, "cargo": []}},
            "cargo": [{"location": 2, "destination": 0, "quantity": 1}]});
        assert_eq!(serde_json::to_value(world()).unwrap(), expected);
    }

    #[test]
    fn base_0_03_key_order_matches_spec_examples() {
        let text = serde_json::to_string(&world()).unwrap();
        assert_eq!(
            text,
            r#"{"tick":1,"vessels":{"v1":{"state":"transit","from":0,"to":1,"arrives_at_tick":4,"cargo":[{"destination":1,"quantity":2}]},"v2":{"state":"docked","location":2,"cargo":[]}},"cargo":[{"location":2,"destination":0,"quantity":1}]}"#
        );
    }

    #[test]
    fn base_0_02_vessel_index_finds_vessels_by_id() {
        let world = world();
        assert_eq!(world.vessel_index("v2"), Some(1));
        assert_eq!(world.vessel_index("v9"), None);
    }

    #[test]
    fn flw1_2_02_cargo_with_the_same_destination_is_one_entry() {
        let mut vessel = world().vessels.remove(1);
        vessel.add_cargo(2).unwrap();
        vessel.add_cargo(0).unwrap();
        vessel.add_cargo(2).unwrap();
        let entries: Vec<_> = vessel
            .cargo
            .iter()
            .map(|c| (c.destination, c.quantity))
            .collect();
        assert_eq!(entries, [(2, 2), (0, 1)], "first-insertion order");
    }

    #[test]
    fn flw1_1_00_dock_inventory_adds_and_removes_units() {
        let mut world = world();
        world.add_dock_cargo(units(0, 1, 2)).unwrap();
        world.add_dock_cargo(units(2, 0, 3)).unwrap();
        assert_eq!(world.cargo, [units(2, 0, 4), units(0, 1, 2)]);
        assert!(world.take_dock_cargo(0, 1));
        assert!(world.take_dock_cargo(0, 1));
        assert!(!world.take_dock_cargo(0, 1), "none left");
        assert_eq!(world.cargo, [units(2, 0, 4)], "emptied entries are dropped");
        assert_eq!(
            world.add_dock_cargo(units(2, 0, u64::MAX)),
            Err(EngineError::CargoOverflow)
        );
    }

    #[test]
    fn flw1_1_00_vessel_inventory_take_reports_missing_cargo() {
        let mut vessel = world().vessels.remove(0);
        assert!(vessel.take_cargo(1));
        assert!(!vessel.take_cargo(0));
        assert!(vessel.take_cargo(1));
        assert!(vessel.cargo.is_empty());
    }
}
