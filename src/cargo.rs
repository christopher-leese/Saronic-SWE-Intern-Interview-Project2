//! The cargo service: load, unload, and swap operations pending until the next tick, the
//! per-location throughput budget, and how pending operations change the planned world.

use crate::error::EngineError;
use crate::world::{DockCargo, World};

/// One cargo command issued since the last tick, by a vessel docked (committed) at `location`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingOp {
    pub vessel: usize,
    pub location: usize,
    pub op: CargoOp,
}

/// Each variant costs one operation against the dock's throughput (FLW1-3-00, 3-01, 3-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoOp {
    Load(usize),
    Unload(usize),
    /// A load and an unload together (FLW1-1-14).
    Swap {
        load: usize,
        unload: usize,
    },
    /// A load and an unload of the same cargo that cancelled each other out (FLW1-3-06). Moves
    /// nothing, but still costs the one operation of the command that overwrote the other.
    Cancelled,
}

/// Records a new cargo command. Satisfies FLW1-3-06 and FLW1-3-03: a load and an unload of the
/// same destination by the same vessel overwrite each other in either order, so the pair cancels
/// out and counts as one operation instead of two.
pub fn record(ops: &mut Vec<PendingOp>, new: PendingOp) {
    let opposite = match new.op {
        CargoOp::Load(destination) => Some(CargoOp::Unload(destination)),
        CargoOp::Unload(destination) => Some(CargoOp::Load(destination)),
        CargoOp::Swap { .. } | CargoOp::Cancelled => None,
    };
    let earlier = ops
        .iter_mut()
        .rev()
        .find(|op| op.vessel == new.vessel && Some(op.op) == opposite);
    match earlier {
        Some(earlier) => earlier.op = CargoOp::Cancelled,
        None => ops.push(new),
    }
}

/// Applies pending operations, in the order issued, to `world` (the committed world with planned
/// vessel states). Checks the throughput budget (FLW1-3-04, 3-05, 3-07), then that each moved
/// unit was in last tick's inventory and is not already moved by another pending operation, so
/// cancelling one operation never invalidates another.
pub fn apply(
    world: &mut World,
    committed: &World,
    ops: &[PendingOp],
    dock_throughput: &[u64],
) -> Result<(), EngineError> {
    for (location, &throughput) in dock_throughput.iter().enumerate() {
        let operations = ops.iter().filter(|op| op.location == location).count();
        if operations as u64 > throughput {
            return Err(EngineError::ThroughputExceeded {
                operations,
                throughput,
                location,
            });
        }
    }
    // Last tick's inventory, minus units already claimed by earlier pending operations.
    let mut unclaimed = committed.clone();
    for op in ops {
        match op.op {
            CargoOp::Load(destination) => load(world, &mut unclaimed, op, destination)?,
            CargoOp::Unload(destination) => unload(world, &mut unclaimed, op, destination)?,
            CargoOp::Swap { load: l, unload: u } => {
                load(world, &mut unclaimed, op, l)?;
                unload(world, &mut unclaimed, op, u)?;
            }
            CargoOp::Cancelled => {}
        }
    }
    Ok(())
}

/// Moves one unit from the dock to the vessel. Satisfies FLW1-1-02, FLW1-2-09.
fn load(
    world: &mut World,
    unclaimed: &mut World,
    op: &PendingOp,
    destination: usize,
) -> Result<(), EngineError> {
    if !unclaimed.take_dock_cargo(op.location, destination) {
        return Err(EngineError::NoDockCargo {
            location: op.location,
            destination,
        });
    }
    // The planned dock holds every unclaimed unit, so this take always succeeds.
    world.take_dock_cargo(op.location, destination);
    // `op.vessel` is a valid index: it came from `World::vessel_index`.
    world.vessels[op.vessel].add_cargo(destination)
}

/// Moves one unit off the vessel: delivered if it is at its destination (FLW1-2-12), otherwise
/// into this dock's inventory (FLW1-2-13). Satisfies FLW1-1-03, FLW1-2-11.
fn unload(
    world: &mut World,
    unclaimed: &mut World,
    op: &PendingOp,
    destination: usize,
) -> Result<(), EngineError> {
    let vessel = &mut unclaimed.vessels[op.vessel];
    if !vessel.take_cargo(destination) {
        return Err(EngineError::NoVesselCargo {
            vessel: vessel.id.clone(),
            destination,
        });
    }
    // The planned vessel holds every unclaimed unit, so this take always succeeds.
    world.vessels[op.vessel].take_cargo(destination);
    if destination == op.location {
        return Ok(());
    }
    world.add_dock_cargo(DockCargo {
        location: op.location,
        destination,
        quantity: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Cargo, Vessel, VesselState};

    fn op(vessel: usize, op: CargoOp) -> PendingOp {
        PendingOp {
            vessel,
            location: 0,
            op,
        }
    }

    /// Two vessels docked at location 0; dock 0 holds one unit for 1 and one for 2; v0 carries
    /// one unit for 2.
    fn committed() -> World {
        let vessel = |id: &str, cargo| Vessel {
            id: id.into(),
            state: VesselState::Docked { location: 0 },
            cargo,
        };
        World {
            tick: 3,
            vessels: vec![
                vessel(
                    "v0",
                    vec![Cargo {
                        destination: 2,
                        quantity: 1,
                    }],
                ),
                vessel("v1", vec![]),
            ],
            cargo: vec![
                DockCargo {
                    location: 0,
                    destination: 1,
                    quantity: 1,
                },
                DockCargo {
                    location: 0,
                    destination: 2,
                    quantity: 1,
                },
            ],
        }
    }

    fn planned(ops: &[PendingOp], throughput: u64) -> Result<World, EngineError> {
        let committed = committed();
        let mut world = committed.clone();
        apply(&mut world, &committed, ops, &[throughput])?;
        Ok(world)
    }

    fn vessel_cargo(world: &World, vessel: usize) -> Vec<(usize, u64)> {
        world.vessels[vessel]
            .cargo
            .iter()
            .map(|c| (c.destination, c.quantity))
            .collect()
    }

    fn dock_cargo(world: &World) -> Vec<(usize, usize, u64)> {
        world
            .cargo
            .iter()
            .map(|c| (c.location, c.destination, c.quantity))
            .collect()
    }

    #[test]
    fn flw1_1_02_load_moves_one_unit_from_dock_to_vessel() {
        let world = planned(&[op(1, CargoOp::Load(1))], 4).unwrap();
        assert_eq!(vessel_cargo(&world, 1), [(1, 1)]);
        assert_eq!(dock_cargo(&world), [(0, 2, 1)]);
    }

    #[test]
    fn flw1_2_09_load_without_dock_cargo_is_an_error() {
        assert_eq!(
            planned(&[op(1, CargoOp::Load(0))], 4),
            Err(EngineError::NoDockCargo {
                location: 0,
                destination: 0
            })
        );
    }

    #[test]
    fn flw1_2_12_unload_at_destination_delivers() {
        let mut committed = committed();
        committed.vessels[0].state = VesselState::Docked { location: 2 };
        let mut world = committed.clone();
        let deliver = PendingOp {
            vessel: 0,
            location: 2,
            op: CargoOp::Unload(2),
        };
        apply(&mut world, &committed, &[deliver], &[1, 1, 1]).unwrap();
        assert!(world.vessels[0].cargo.is_empty());
        assert_eq!(world.cargo, committed.cargo, "not added to any dock");
    }

    #[test]
    fn flw1_2_13_unload_elsewhere_adds_to_dock() {
        // Also FLW1-1-03: a vessel can unload at any dock.
        let mut committed = committed();
        committed.vessels[0].cargo[0].destination = 1;
        let mut world = committed.clone();
        apply(&mut world, &committed, &[op(0, CargoOp::Unload(1))], &[4]).unwrap();
        assert!(world.vessels[0].cargo.is_empty());
        assert_eq!(dock_cargo(&world), [(0, 1, 2), (0, 2, 1)]);
    }

    #[test]
    fn flw1_2_11_unload_without_vessel_cargo_is_an_error() {
        assert_eq!(
            planned(&[op(1, CargoOp::Unload(2))], 4),
            Err(EngineError::NoVesselCargo {
                vessel: "v1".into(),
                destination: 2
            })
        );
    }

    #[test]
    fn flw1_1_14_swap_loads_and_unloads_together() {
        // Also FLW1-3-02 (one operation): throughput 1 is enough.
        let world = planned(&[op(0, CargoOp::Swap { load: 1, unload: 2 })], 1).unwrap();
        assert_eq!(vessel_cargo(&world, 0), [(1, 1)]);
        assert_eq!(dock_cargo(&world), [(0, 2, 2)]);
    }

    #[test]
    fn flw1_2_16_swap_without_either_cargo_is_an_error() {
        let swap = |load, unload| [op(1, CargoOp::Swap { load, unload })];
        assert!(matches!(
            planned(&swap(0, 2), 4),
            Err(EngineError::NoDockCargo { .. })
        ));
        assert!(matches!(
            planned(&swap(1, 2), 4),
            Err(EngineError::NoVesselCargo { .. })
        ));
    }

    #[test]
    fn flw1_3_04_operations_beyond_throughput_are_rejected() {
        // Also FLW1-3-00, 3-01, 3-07: each operation at the dock counts once, across vessels.
        let ops = [op(1, CargoOp::Load(1)), op(0, CargoOp::Unload(2))];
        assert!(planned(&ops, 2).is_ok());
        assert_eq!(
            planned(&ops, 1),
            Err(EngineError::ThroughputExceeded {
                operations: 2,
                throughput: 1,
                location: 0
            })
        );
    }

    #[test]
    fn flw1_3_05_throughput_message_matches_the_spec() {
        let error = EngineError::ThroughputExceeded {
            operations: 3,
            throughput: 2,
            location: 0,
        };
        assert_eq!(
            error.to_string(),
            "transfer of 3 operations exceeds dock throughput of 2 at location 0"
        );
    }

    #[test]
    fn flw1_3_04_throughput_is_checked_before_inventory() {
        let ops = [op(1, CargoOp::Load(1)), op(1, CargoOp::Load(1))];
        assert!(matches!(
            planned(&ops, 1),
            Err(EngineError::ThroughputExceeded { operations: 2, .. })
        ));
    }

    #[test]
    fn flw1_1_02_units_moved_this_tick_are_not_moved_again() {
        // v0 unloads a unit for 2 into dock 0; v1 cannot load that unit until the next tick, but
        // can load the unit that was there at the last tick.
        let ops = [
            op(0, CargoOp::Unload(2)),
            op(1, CargoOp::Load(2)),
            op(1, CargoOp::Load(2)),
        ];
        assert!(matches!(
            planned(&ops, 4),
            Err(EngineError::NoDockCargo { destination: 2, .. })
        ));
        assert!(planned(&ops[..2], 4).is_ok());
    }

    #[test]
    fn flw1_3_06_load_and_unload_of_the_same_cargo_cancel_out() {
        // Also FLW1-3-03: the pair costs one operation.
        for (first, second) in [
            (CargoOp::Load(1), CargoOp::Unload(1)),
            (CargoOp::Unload(2), CargoOp::Load(2)),
        ] {
            let mut ops = Vec::new();
            record(&mut ops, op(0, first));
            record(&mut ops, op(0, second));
            assert_eq!(ops, [op(0, CargoOp::Cancelled)]);
            assert_eq!(planned(&ops, 1).unwrap(), committed());
        }
    }

    #[test]
    fn flw1_3_06_other_vessels_and_destinations_are_not_overwritten() {
        let mut ops = Vec::new();
        record(&mut ops, op(0, CargoOp::Load(1)));
        record(&mut ops, op(1, CargoOp::Unload(1)));
        record(&mut ops, op(0, CargoOp::Unload(2)));
        record(&mut ops, op(0, CargoOp::Swap { load: 2, unload: 1 }));
        assert_eq!(ops.len(), 4);
        assert!(!ops.iter().any(|op| op.op == CargoOp::Cancelled));
    }
}
