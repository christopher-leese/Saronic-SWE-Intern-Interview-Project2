//! The engine: commands over a committed world (the last tick) and a planned world (the
//! committed world with every command since then applied).

use std::collections::HashSet;

use crate::cargo::{self, CargoOp, PendingOp};
use crate::error::EngineError;
use crate::protocol::{Command, VesselSpec};
use crate::world::{DockCargo, Vessel, VesselState, World};

/// Holds no simulation until a valid `init`. Satisfies BASE-1-11, BASE-2-00, BASE-2-01.
#[derive(Debug, Default)]
pub struct Engine {
    sim: Option<Simulation>,
}

/// Satisfies BASE-0-07: committed and planned states are kept separately.
#[derive(Debug)]
struct Simulation {
    travel_times: Vec<Vec<u64>>,
    dock_throughput: Vec<u64>,
    /// The world at the last tick (BASE-1-06).
    committed: World,
    /// The committed world of every earlier tick, indexed by tick, so `history.len()` is the
    /// current tick (FLW2-1-00, FLW2-1-01).
    history: Vec<World>,
    /// Every command since the last tick (BASE-1-07).
    plan: Plan,
    /// `committed` with `plan` applied, kept so each response and get_state is a copy.
    planned: World,
}

/// What commands since the last tick have changed: each vessel's planned state (set_destination,
/// dock, undock), and the cargo operations in the order issued.
#[derive(Debug, Clone)]
struct Plan {
    states: Vec<VesselState>,
    ops: Vec<PendingOp>,
}

impl From<&World> for Plan {
    /// No commands yet since `world`'s tick.
    fn from(world: &World) -> Self {
        Self {
            states: world.vessels.iter().map(|vessel| vessel.state).collect(),
            ops: Vec::new(),
        }
    }
}

impl Engine {
    /// Applies one command and returns the world state to report. The planned state is updated
    /// before this returns (BASE-1-03, BASE-1-08).
    pub fn handle(&mut self, command: Command) -> Result<World, EngineError> {
        let Some(sim) = &mut self.sim else {
            let Command::Init {
                travel_times,
                dock_throughput,
                vessels,
                cargo_manifest,
            } = command
            else {
                return Err(EngineError::NotInitialized);
            };
            let sim = Simulation::new(travel_times, dock_throughput, &vessels, &cargo_manifest)?;
            let world = sim.committed.clone();
            self.sim = Some(sim);
            return Ok(world);
        };
        match command {
            Command::Init { .. } => Err(EngineError::AlreadyInitialized),
            Command::Tick { cargo_manifest } => sim.tick(&cargo_manifest),
            Command::SetDestination {
                vessel_id,
                destination,
            } => sim.set_destination(&vessel_id, destination),
            // Satisfies BASE-1-13 and FLW1-1-17 (the planned world includes cargo and docks).
            Command::GetState {} => Ok(sim.planned.clone()),
            Command::GetStateAt { tick } => sim.state_at(tick),
            Command::Dock { vessel_id } => sim.dock(&vessel_id),
            Command::Undock { vessel_id } => sim.undock(&vessel_id),
            Command::Load {
                vessel_id,
                destination,
            } => sim.cargo_command(&vessel_id, CargoOp::Load(destination)),
            Command::Unload {
                vessel_id,
                destination,
            } => sim.cargo_command(&vessel_id, CargoOp::Unload(destination)),
            Command::Swap {
                vessel_id,
                load_destination,
                unload_destination,
            } => sim.cargo_command(
                &vessel_id,
                CargoOp::Swap {
                    load: load_destination,
                    unload: unload_destination,
                },
            ),
        }
    }
}

impl Simulation {
    /// Validates init's parameters and builds the world at tick 0: every vessel idle and empty,
    /// docks holding the optional cargo_manifest (FLW1-2-03).
    fn new(
        travel_times: Vec<Vec<u64>>,
        dock_throughput: Vec<u64>,
        vessels: &[VesselSpec],
        cargo_manifest: &[DockCargo],
    ) -> Result<Self, EngineError> {
        validate_travel_times(&travel_times)?;
        let locations = travel_times.len();
        validate_dock_throughput(&dock_throughput, locations)?;
        let mut seen = HashSet::new();
        for spec in vessels {
            if !seen.insert(spec.id.as_str()) {
                return Err(EngineError::DuplicateVessel(spec.id.clone()));
            }
            check_location("location", spec.location, locations)?;
        }
        let mut world = World {
            tick: 0,
            vessels: vessels
                .iter()
                .map(|spec| Vessel {
                    id: spec.id.clone(),
                    state: VesselState::Idle {
                        location: spec.location,
                    },
                    cargo: Vec::new(),
                })
                .collect(),
            cargo: Vec::new(),
        };
        add_manifest(&mut world, cargo_manifest, locations)?;
        Ok(Self {
            travel_times,
            dock_throughput,
            plan: Plan::from(&world),
            committed: world.clone(),
            // The tick 0 world is `committed` until the first tick moves it here (FLW2-1-00).
            history: Vec::new(),
            planned: world,
        })
    }

    /// Advances one time step: the planned world becomes the committed world at the next tick,
    /// vessels arriving at that tick become idle at their destination, and the optional
    /// cargo_manifest is added to the docks of that newly committed world (FLW1-2-04). Time
    /// changes only here, in whole ticks. The world it replaces joins the history, so each tick
    /// is recorded after all its transitions (FLW2-1-01).
    /// Satisfies BASE-0-00, BASE-1-09, BASE-1-10, BASE-2-11.
    fn tick(&mut self, cargo_manifest: &[DockCargo]) -> Result<World, EngineError> {
        let mut next = self.planned.clone();
        next.tick = next.tick.checked_add(1).ok_or(EngineError::TickOverflow)?;
        for vessel in &mut next.vessels {
            if let VesselState::Transit {
                to,
                arrives_at_tick,
                ..
            } = vessel.state
            {
                if arrives_at_tick == next.tick {
                    vessel.state = VesselState::Idle { location: to };
                }
            }
        }
        add_manifest(&mut next, cargo_manifest, self.travel_times.len())?;
        self.history
            .push(std::mem::replace(&mut self.committed, next.clone()));
        self.plan = Plan::from(&next);
        self.planned = next.clone();
        Ok(next)
    }

    /// The committed world at `tick`, for any tick from 0 to the current tick (FLW2-2-19). The
    /// current tick gives the committed world, not the planned one, so the states between ticks
    /// cannot be queried (FLW2-1-03). Satisfies FLW2-1-02, FLW2-2-20.
    fn state_at(&self, tick: u64) -> Result<World, EngineError> {
        if tick == self.committed.tick {
            return Ok(self.committed.clone());
        }
        usize::try_from(tick)
            .ok()
            .and_then(|index| self.history.get(index))
            .cloned()
            .ok_or(EngineError::FutureTick {
                tick,
                current: self.committed.tick,
            })
    }

    /// Plans travel for a vessel that is idle in the committed state, replacing any earlier plan,
    /// including a planned dock (FLW1-1-07).
    /// Satisfies BASE-1-12, BASE-2-02..BASE-2-06, BASE-2-09, BASE-2-10, BASE-2-12, BASE-3-02.
    fn set_destination(&mut self, id: &str, destination: usize) -> Result<World, EngineError> {
        let index = self.vessel(id)?;
        check_location("destination", destination, self.travel_times.len())?;
        // Indexing cannot panic: `index` came from `vessel_index`; `location` is an init location
        // or an earlier destination, both range-checked; `destination` was just checked; and the
        // matrix is square.
        let location = self.committed_idle(index)?;
        if location == destination {
            return Err(EngineError::AlreadyAtDestination {
                vessel: id.to_string(),
                location,
            });
        }
        let arrives_at_tick = self
            .committed
            .tick
            .checked_add(self.travel_times[location][destination])
            .ok_or_else(|| EngineError::ArrivalOverflow(id.to_string()))?;
        let mut plan = self.plan.clone();
        plan.states[index] = VesselState::Transit {
            from: location,
            to: destination,
            arrives_at_tick,
        };
        self.replan(plan)
    }

    /// Plans docking at the current location for a vessel idle in the committed state, replacing
    /// a planned set_destination (FLW1-1-08). For a vessel docked in the committed state, cancels
    /// a planned undock, so the vessel stays docked. Docking a vessel that is already docked, or
    /// already planned to dock, is an error. Satisfies FLW1-1-06, FLW1-1-12, FLW1-2-07, FLW1-3-14.
    fn dock(&mut self, id: &str) -> Result<World, EngineError> {
        let index = self.vessel(id)?;
        let location = match (self.committed.vessels[index].state, self.plan.states[index]) {
            (VesselState::Idle { .. }, VesselState::Docked { .. }) => {
                return Err(EngineError::AlreadyDocked(id.to_string()))
            }
            (VesselState::Idle { location }, _)
            | (VesselState::Docked { location }, VesselState::Idle { .. }) => location,
            _ => return Err(EngineError::VesselNotIdle(id.to_string())),
        };
        let mut plan = self.plan.clone();
        plan.states[index] = VesselState::Docked { location };
        self.replan(plan)
    }

    /// Plans undocking for a vessel docked in both the committed and planned states. Its pending
    /// cargo operations are dropped and their cost refunded (FLW1-1-15, FLW1-3-03). For a vessel
    /// idle in the committed state, cancels a planned dock, so the vessel stays idle. Any vessel
    /// not planned to be docked is an error. Satisfies FLW1-1-11, FLW1-1-13.
    fn undock(&mut self, id: &str) -> Result<World, EngineError> {
        let index = self.vessel(id)?;
        let location = match (self.committed.vessels[index].state, self.plan.states[index]) {
            (
                VesselState::Docked { location } | VesselState::Idle { location },
                VesselState::Docked { .. },
            ) => location,
            _ => return Err(EngineError::NotDocked(id.to_string())),
        };
        let mut plan = self.plan.clone();
        plan.states[index] = VesselState::Idle { location };
        plan.ops.retain(|op| op.vessel != index);
        self.replan(plan)
    }

    /// Load, unload, or swap for a vessel docked in the committed state. A pending undock is
    /// replaced: the vessel stays docked (FLW1-1-16). Satisfies FLW1-1-09, FLW1-1-10, FLW1-1-14,
    /// FLW1-2-08, FLW1-2-10, FLW1-2-14, FLW1-2-15, FLW1-2-17, FLW1-2-18, FLW1-3-12, FLW1-3-13.
    fn cargo_command(&mut self, id: &str, op: CargoOp) -> Result<World, EngineError> {
        let index = self.vessel(id)?;
        let locations = self.travel_times.len();
        match op {
            CargoOp::Load(destination) | CargoOp::Unload(destination) => {
                check_location("destination", destination, locations)?;
            }
            CargoOp::Swap { load, unload } => {
                check_location("load_destination", load, locations)?;
                check_location("unload_destination", unload, locations)?;
            }
            CargoOp::Cancelled => {}
        }
        let location = self.committed_docked(index)?;
        let mut plan = self.plan.clone();
        plan.states[index] = VesselState::Docked { location };
        cargo::record(
            &mut plan.ops,
            PendingOp {
                vessel: index,
                location,
                op,
            },
        );
        self.replan(plan)
    }

    /// Builds the planned world for `plan` and keeps both only if every check passes, so a
    /// rejected command leaves the state unchanged.
    fn replan(&mut self, plan: Plan) -> Result<World, EngineError> {
        let mut world = self.committed.clone();
        for (vessel, &state) in world.vessels.iter_mut().zip(&plan.states) {
            vessel.state = state;
        }
        cargo::apply(
            &mut world,
            &self.committed,
            &plan.ops,
            &self.dock_throughput,
        )?;
        self.plan = plan;
        self.planned = world.clone();
        Ok(world)
    }

    /// Satisfies BASE-2-05 (unknown vessel_id) for every command that names a vessel.
    fn vessel(&self, id: &str) -> Result<usize, EngineError> {
        self.committed
            .vessel_index(id)
            .ok_or_else(|| EngineError::UnknownVessel(id.to_string()))
    }

    /// The location of a vessel idle in the committed state (set_destination).
    fn committed_idle(&self, index: usize) -> Result<usize, EngineError> {
        let vessel = &self.committed.vessels[index];
        match vessel.state {
            VesselState::Idle { location } => Ok(location),
            _ => Err(EngineError::VesselNotIdle(vessel.id.clone())),
        }
    }

    /// The location of a vessel docked in the committed state (load, unload, swap).
    fn committed_docked(&self, index: usize) -> Result<usize, EngineError> {
        let vessel = &self.committed.vessels[index];
        match vessel.state {
            VesselState::Docked { location } => Ok(location),
            _ => Err(EngineError::NotDocked(vessel.id.clone())),
        }
    }
}

/// Satisfies BASE-3-03 (NxN), BASE-3-05 (zero diagonal), BASE-3-06 (positive off-diagonal).
/// Values are non-negative integers by type (`u64`).
fn validate_travel_times(travel_times: &[Vec<u64>]) -> Result<(), EngineError> {
    let n = travel_times.len();
    for (from, row) in travel_times.iter().enumerate() {
        if row.len() != n {
            return Err(EngineError::TravelTimesNotSquare);
        }
        for (to, &ticks) in row.iter().enumerate() {
            if from == to && ticks != 0 {
                return Err(EngineError::NonZeroDiagonal { location: from });
            }
            if from != to && ticks == 0 {
                return Err(EngineError::NonPositiveTravelTime { from, to });
            }
        }
    }
    Ok(())
}

/// Satisfies FLW1-2-01 (one entry per location, at least one location) and FLW1-3-08 (positive
/// values).
fn validate_dock_throughput(dock_throughput: &[u64], locations: usize) -> Result<(), EngineError> {
    if locations == 0 {
        return Err(EngineError::NoLocations);
    }
    if dock_throughput.len() != locations {
        return Err(EngineError::DockThroughputLength { locations });
    }
    match dock_throughput.iter().position(|&limit| limit == 0) {
        Some(location) => Err(EngineError::NonPositiveThroughput { location }),
        None => Ok(()),
    }
}

/// Adds manifest entries to the docks, after checking every entry, so an out-of-range entry
/// rejects the whole command (FLW1-2-06). Satisfies FLW1-1-05, FLW1-2-00, FLW1-3-09..FLW1-3-11.
///
/// Also rejects a manifest that would bring the world's total units past `u64::MAX`. Cargo
/// operations only move or deliver units, so after this check no inventory entry can overflow,
/// and undock or a cancelling cargo command is never rejected for overflow.
fn add_manifest(
    world: &mut World,
    cargo_manifest: &[DockCargo],
    locations: usize,
) -> Result<(), EngineError> {
    for entry in cargo_manifest {
        check_location("cargo_manifest location", entry.location, locations)?;
        check_location("cargo_manifest destination", entry.destination, locations)?;
        if entry.quantity == 0 {
            return Err(EngineError::NonPositiveQuantity);
        }
    }
    let on_vessels = world.vessels.iter().flat_map(|vessel| &vessel.cargo);
    on_vessels
        .map(|cargo| cargo.quantity)
        .chain(world.cargo.iter().map(|cargo| cargo.quantity))
        .chain(cargo_manifest.iter().map(|entry| entry.quantity))
        .try_fold(0u64, u64::checked_add)
        .ok_or(EngineError::CargoOverflow)?;
    for &entry in cargo_manifest {
        world.add_dock_cargo(entry)?;
    }
    Ok(())
}

/// Satisfies BASE-2-06 for destinations; also used for every other location parameter.
fn check_location(field: &'static str, value: usize, locations: usize) -> Result<(), EngineError> {
    if value < locations {
        Ok(())
    } else {
        Err(EngineError::OutOfRange {
            field,
            value,
            locations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_with(travel_times: Vec<Vec<u64>>, vessels: &[(&str, usize)]) -> Command {
        Command::Init {
            dock_throughput: vec![1; travel_times.len()],
            travel_times,
            cargo_manifest: vec![],
            vessels: vessels
                .iter()
                .map(|&(id, location)| VesselSpec {
                    id: id.into(),
                    location,
                })
                .collect(),
        }
    }

    /// The spec's example world: v1 at 0, v2 at 2.
    fn spec_engine() -> Engine {
        let mut engine = Engine::default();
        let matrix = vec![vec![0, 4, 7], vec![4, 0, 3], vec![7, 3, 0]];
        engine
            .handle(init_with(matrix, &[("v1", 0), ("v2", 2)]))
            .unwrap();
        engine
    }

    fn set_destination(
        engine: &mut Engine,
        id: &str,
        destination: usize,
    ) -> Result<World, EngineError> {
        engine.handle(Command::SetDestination {
            vessel_id: id.into(),
            destination,
        })
    }

    fn tick(engine: &mut Engine) -> World {
        engine
            .handle(Command::Tick {
                cargo_manifest: vec![],
            })
            .unwrap()
    }

    fn get_state(engine: &mut Engine) -> World {
        engine.handle(Command::GetState {}).unwrap()
    }

    fn state_of(world: &World, id: &str) -> VesselState {
        world.vessels[world.vessel_index(id).unwrap()].state
    }

    fn committed(engine: &Engine) -> &World {
        &engine.sim.as_ref().unwrap().committed
    }

    fn transit(from: usize, to: usize, arrives_at_tick: u64) -> VesselState {
        VesselState::Transit {
            from,
            to,
            arrives_at_tick,
        }
    }

    // --- init ---

    #[test]
    fn base_1_11_init_returns_world_at_tick_0_with_idle_vessels() {
        let mut engine = Engine::default();
        let world = engine
            .handle(init_with(
                vec![vec![0, 1], vec![1, 0]],
                &[("a", 1), ("b", 0)],
            ))
            .unwrap();
        assert_eq!(world.tick, 0);
        assert_eq!(state_of(&world, "a"), VesselState::Idle { location: 1 });
        assert_eq!(state_of(&world, "b"), VesselState::Idle { location: 0 });
        let ids: Vec<_> = world.vessels.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"], "vessels keep init order");
    }

    #[test]
    fn base_2_00_commands_before_init_are_errors() {
        let mut engine = Engine::default();
        for command in [
            Command::Tick {
                cargo_manifest: vec![],
            },
            Command::GetState {},
            Command::SetDestination {
                vessel_id: "v1".into(),
                destination: 1,
            },
        ] {
            assert_eq!(engine.handle(command), Err(EngineError::NotInitialized));
        }
    }

    #[test]
    fn base_2_01_second_init_is_an_error_and_changes_nothing() {
        let mut engine = spec_engine();
        let before = get_state(&mut engine);
        let again = engine.handle(init_with(vec![vec![0]], &[("x", 0)]));
        assert_eq!(again, Err(EngineError::AlreadyInitialized));
        assert_eq!(get_state(&mut engine), before);
    }

    #[test]
    fn base_2_01_rejected_init_leaves_engine_uninitialized() {
        let mut engine = Engine::default();
        let bad = engine.handle(init_with(vec![vec![1]], &[]));
        assert_eq!(bad, Err(EngineError::NonZeroDiagonal { location: 0 }));
        assert_eq!(
            engine.handle(Command::Tick {
                cargo_manifest: vec![],
            }),
            Err(EngineError::NotInitialized)
        );
        assert!(engine.handle(init_with(vec![vec![0]], &[])).is_ok());
    }

    #[test]
    fn base_3_03_travel_times_must_be_square() {
        for matrix in [
            vec![vec![0, 1]],
            vec![vec![0, 1], vec![1]],
            vec![vec![0], vec![1, 0]],
        ] {
            let mut engine = Engine::default();
            assert_eq!(
                engine.handle(init_with(matrix, &[])),
                Err(EngineError::TravelTimesNotSquare)
            );
        }
    }

    #[test]
    fn base_3_05_travel_times_diagonal_must_be_zero() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.handle(init_with(vec![vec![0, 1], vec![1, 2]], &[])),
            Err(EngineError::NonZeroDiagonal { location: 1 })
        );
    }

    #[test]
    fn base_3_06_travel_times_off_diagonal_must_be_positive() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.handle(init_with(vec![vec![0, 1], vec![0, 0]], &[])),
            Err(EngineError::NonPositiveTravelTime { from: 1, to: 0 })
        );
    }

    #[test]
    fn base_3_07_init_rejects_duplicate_ids_and_bad_locations() {
        let matrix = || vec![vec![0, 1], vec![1, 0]];
        let mut engine = Engine::default();
        assert_eq!(
            engine.handle(init_with(matrix(), &[("v1", 0), ("v1", 1)])),
            Err(EngineError::DuplicateVessel("v1".into()))
        );
        assert_eq!(
            engine.handle(init_with(matrix(), &[("v1", 2)])),
            Err(EngineError::OutOfRange {
                field: "location",
                value: 2,
                locations: 2
            })
        );
        assert!(
            engine.handle(init_with(matrix(), &[])).is_ok(),
            "empty fleet is allowed"
        );
    }

    // --- set_destination ---

    #[test]
    fn base_3_02_arrival_is_issue_tick_plus_travel_time() {
        let mut engine = spec_engine();
        let world = set_destination(&mut engine, "v1", 2).unwrap();
        assert_eq!(state_of(&world, "v1"), transit(0, 2, 7));
        tick(&mut engine);
        tick(&mut engine);
        let world = set_destination(&mut engine, "v2", 1).unwrap();
        assert_eq!(state_of(&world, "v2"), transit(2, 1, 2 + 3));
    }

    #[test]
    fn base_3_02_arrival_tick_overflow_is_an_error_not_a_panic() {
        let mut engine = Engine::default();
        let matrix = vec![vec![0, u64::MAX], vec![1, 0]];
        engine.handle(init_with(matrix, &[("a", 0)])).unwrap();
        tick(&mut engine);
        let before = get_state(&mut engine);
        let error = set_destination(&mut engine, "a", 1).unwrap_err();
        assert_eq!(error, EngineError::ArrivalOverflow("a".into()));
        assert_eq!(
            error.to_string(),
            "vessel a would arrive after the tick limit"
        );
        assert_eq!(get_state(&mut engine), before);
    }

    #[test]
    fn base_3_04_travel_time_is_read_from_row_i_column_j() {
        let mut engine = Engine::default();
        engine
            .handle(init_with(
                vec![vec![0, 2], vec![9, 0]],
                &[("a", 0), ("b", 1)],
            ))
            .unwrap();
        assert_eq!(
            state_of(&set_destination(&mut engine, "a", 1).unwrap(), "a"),
            transit(0, 1, 2)
        );
        assert_eq!(
            state_of(&set_destination(&mut engine, "b", 0).unwrap(), "b"),
            transit(1, 0, 9)
        );
    }

    #[test]
    fn base_2_02_set_destination_requires_committed_idle() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        assert_eq!(
            set_destination(&mut engine, "v1", 2),
            Err(EngineError::VesselNotIdle("v1".into()))
        );
    }

    #[test]
    fn base_2_03_destination_equal_to_current_location_is_an_error() {
        let mut engine = spec_engine();
        assert_eq!(
            set_destination(&mut engine, "v2", 2),
            Err(EngineError::AlreadyAtDestination {
                vessel: "v2".into(),
                location: 2
            })
        );
    }

    #[test]
    fn base_3_08_redundant_destination_message_states_it() {
        let mut engine = spec_engine();
        let error = set_destination(&mut engine, "v2", 2).unwrap_err();
        assert_eq!(error.to_string(), "vessel v2 is already at location 2");
    }

    #[test]
    fn base_2_04_set_destination_for_vessel_in_transit_is_an_error() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 2).unwrap();
        for _ in 0..3 {
            tick(&mut engine);
            assert_eq!(
                set_destination(&mut engine, "v1", 1),
                Err(EngineError::VesselNotIdle("v1".into()))
            );
        }
        assert_eq!(
            EngineError::VesselNotIdle("v1".into()).to_string(),
            "vessel v1 is not idle"
        );
    }

    #[test]
    fn base_2_05_unknown_vessel_is_an_error() {
        let mut engine = spec_engine();
        assert_eq!(
            set_destination(&mut engine, "v9", 1),
            Err(EngineError::UnknownVessel("v9".into()))
        );
    }

    #[test]
    fn base_2_06_destination_out_of_range_is_an_error() {
        let mut engine = spec_engine();
        assert_eq!(
            set_destination(&mut engine, "v1", 3),
            Err(EngineError::OutOfRange {
                field: "destination",
                value: 3,
                locations: 3
            })
        );
    }

    #[test]
    fn base_2_06_input_checks_come_before_state_checks() {
        // Also BASE-2-02 and BASE-2-05: unknown vessel, then range, then not idle.
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        let message = |engine: &mut Engine, id: &str, destination| {
            set_destination(engine, id, destination)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(message(&mut engine, "v9", 9), "vessel v9 does not exist");
        assert_eq!(
            message(&mut engine, "v1", 9),
            "destination 9 is out of range (location count: 3)"
        );
        assert_eq!(message(&mut engine, "v1", 1), "vessel v1 is not idle");
    }

    #[test]
    fn base_2_09_set_destination_overrides_earlier_plan() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 2).unwrap();
        let world = set_destination(&mut engine, "v1", 1).unwrap();
        assert_eq!(state_of(&world, "v1"), transit(0, 1, 4));
        assert_eq!(state_of(&tick(&mut engine), "v1"), transit(0, 1, 4));
    }

    #[test]
    fn base_2_10_set_destination_updates_planned_not_committed() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        assert_eq!(state_of(&get_state(&mut engine), "v1"), transit(0, 1, 4));
        assert_eq!(
            state_of(committed(&engine), "v1"),
            VesselState::Idle { location: 0 }
        );
    }

    #[test]
    fn base_2_10_rejected_command_leaves_planned_unchanged() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        let before = get_state(&mut engine);
        assert!(set_destination(&mut engine, "v1", 0).is_err());
        assert!(set_destination(&mut engine, "v1", 5).is_err());
        assert_eq!(get_state(&mut engine), before);
    }

    // --- tick ---

    #[test]
    fn base_2_11_vessel_reports_idle_on_its_arrival_tick() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        for expected_tick in 1..=3 {
            let world = tick(&mut engine);
            assert_eq!(world.tick, expected_tick);
            assert_eq!(state_of(&world, "v1"), transit(0, 1, 4));
        }
        let world = tick(&mut engine);
        assert_eq!(world.tick, 4);
        assert_eq!(state_of(&world, "v1"), VesselState::Idle { location: 1 });
    }

    #[test]
    fn base_2_12_arrived_vessel_can_be_commanded_on_arrival_tick() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        for _ in 0..4 {
            tick(&mut engine);
        }
        let world = set_destination(&mut engine, "v1", 2).unwrap();
        assert_eq!(world.tick, 4);
        assert_eq!(state_of(&world, "v1"), transit(1, 2, 7));
    }

    #[test]
    fn base_1_09_tick_commits_the_planned_state() {
        let mut engine = spec_engine();
        let planned = set_destination(&mut engine, "v1", 1).unwrap();
        let world = tick(&mut engine);
        assert_eq!(world.vessels, planned.vessels);
        assert_eq!(committed(&engine), &world);
        assert_eq!(
            get_state(&mut engine),
            world,
            "planned restarts from the new committed state"
        );
    }

    #[test]
    fn base_1_10_each_tick_advances_exactly_one_step() {
        let mut engine = spec_engine();
        for expected in 1..=5 {
            assert_eq!(tick(&mut engine).tick, expected);
        }
    }

    #[test]
    fn base_1_13_get_state_returns_planned_and_changes_nothing() {
        let mut engine = spec_engine();
        let planned = set_destination(&mut engine, "v1", 2).unwrap();
        assert_eq!(get_state(&mut engine), planned);
        assert_eq!(get_state(&mut engine), planned);
    }

    #[test]
    fn base_1_12_only_idle_vessels_travel_and_only_to_a_new_location() {
        let mut engine = spec_engine();
        assert!(set_destination(&mut engine, "v1", 0).is_err());
        assert!(set_destination(&mut engine, "v1", 1).is_ok());
        tick(&mut engine);
        assert!(set_destination(&mut engine, "v1", 2).is_err());
    }

    #[test]
    fn base_1_06_committed_state_is_a_snapshot_at_the_tick() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        let world = tick(&mut engine);
        set_destination(&mut engine, "v2", 1).unwrap();
        assert_eq!(
            committed(&engine),
            &world,
            "later commands do not alter the committed snapshot"
        );
    }

    #[test]
    fn base_1_07_planned_is_committed_plus_commands_since_tick() {
        let mut engine = spec_engine();
        tick(&mut engine);
        set_destination(&mut engine, "v2", 0).unwrap();
        let planned = get_state(&mut engine);
        let committed = committed(&engine).clone();
        assert_eq!(planned.tick, committed.tick);
        assert_eq!(state_of(&planned, "v1"), state_of(&committed, "v1"));
        assert_eq!(state_of(&planned, "v2"), transit(2, 0, 1 + 7));
    }

    #[test]
    fn base_1_03_planned_state_reflects_command_in_its_own_response() {
        let mut engine = spec_engine();
        let response = set_destination(&mut engine, "v1", 1).unwrap();
        assert_eq!(state_of(&response, "v1"), transit(0, 1, 4));
    }

    #[test]
    fn base_0_07_planned_and_committed_are_separate() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        let sim = engine.sim.as_ref().unwrap();
        assert_ne!(sim.planned, sim.committed);
    }

    #[test]
    fn base_1_10_tick_overflow_is_an_error_not_a_panic() {
        let mut engine = spec_engine();
        engine.sim.as_mut().unwrap().planned.tick = u64::MAX;
        let error = engine
            .handle(Command::Tick {
                cargo_manifest: vec![],
            })
            .unwrap_err();
        assert_eq!(error, EngineError::TickOverflow);
        assert_eq!(error.to_string(), "tick limit reached");
    }

    #[test]
    fn base_1_08_planned_state_updates_immediately_with_each_command() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        assert_eq!(state_of(&get_state(&mut engine), "v1"), transit(0, 1, 4));
        set_destination(&mut engine, "v2", 0).unwrap();
        let planned = get_state(&mut engine);
        assert_eq!(state_of(&planned, "v1"), transit(0, 1, 4));
        assert_eq!(state_of(&planned, "v2"), transit(2, 0, 7));
        assert_eq!(planned.tick, 0, "no tick was needed");
    }

    #[test]
    fn base_0_00_time_changes_only_by_whole_ticks() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v1", 1).unwrap();
        assert_eq!(
            get_state(&mut engine).tick,
            0,
            "commands do not advance time"
        );
        assert_eq!(tick(&mut engine).tick, 1);
        assert_eq!(tick(&mut engine).tick, 2);
    }

    #[test]
    fn base_0_02_vessel_states_are_tracked_at_every_tick() {
        let mut engine = spec_engine();
        set_destination(&mut engine, "v2", 1).unwrap();
        let mut history = Vec::new();
        for _ in 0..3 {
            let world = tick(&mut engine);
            history.push((world.tick, state_of(committed(&engine), "v2")));
        }
        assert_eq!(
            history,
            [
                (1, transit(2, 1, 3)),
                (2, transit(2, 1, 3)),
                (3, VesselState::Idle { location: 1 })
            ]
        );
    }

    // --- FLW1: docks, cargo, and cargo operations ---

    fn units(location: usize, destination: usize, quantity: u64) -> DockCargo {
        DockCargo {
            location,
            destination,
            quantity,
        }
    }

    /// The spec's FLW1 world (throughput [2, 4, 1]) with docks holding `manifest`.
    fn flw1_init(vessels: &[(&str, usize)], manifest: Vec<DockCargo>) -> Command {
        Command::Init {
            travel_times: vec![vec![0, 4, 7], vec![4, 0, 3], vec![7, 3, 0]],
            dock_throughput: vec![2, 4, 1],
            vessels: vessels
                .iter()
                .map(|&(id, location)| VesselSpec {
                    id: id.into(),
                    location,
                })
                .collect(),
            cargo_manifest: manifest,
        }
    }

    /// Tick 1: v1 docked at 0 and v2 docked at 2 (committed). Dock 0 holds one unit each for 2
    /// and 1; dock 2 holds one unit for 0.
    fn docked_engine() -> Engine {
        let mut engine = Engine::default();
        let manifest = vec![units(0, 2, 1), units(0, 1, 1), units(2, 0, 1)];
        engine
            .handle(flw1_init(&[("v1", 0), ("v2", 2)], manifest))
            .unwrap();
        dock(&mut engine, "v1").unwrap();
        dock(&mut engine, "v2").unwrap();
        tick(&mut engine);
        engine
    }

    fn dock(engine: &mut Engine, id: &str) -> Result<World, EngineError> {
        engine.handle(Command::Dock {
            vessel_id: id.into(),
        })
    }

    fn undock(engine: &mut Engine, id: &str) -> Result<World, EngineError> {
        engine.handle(Command::Undock {
            vessel_id: id.into(),
        })
    }

    fn load(engine: &mut Engine, id: &str, destination: usize) -> Result<World, EngineError> {
        engine.handle(Command::Load {
            vessel_id: id.into(),
            destination,
        })
    }

    fn unload(engine: &mut Engine, id: &str, destination: usize) -> Result<World, EngineError> {
        engine.handle(Command::Unload {
            vessel_id: id.into(),
            destination,
        })
    }

    fn swap(
        engine: &mut Engine,
        id: &str,
        load: usize,
        unload: usize,
    ) -> Result<World, EngineError> {
        engine.handle(Command::Swap {
            vessel_id: id.into(),
            load_destination: load,
            unload_destination: unload,
        })
    }

    fn tick_with(engine: &mut Engine, manifest: Vec<DockCargo>) -> Result<World, EngineError> {
        engine.handle(Command::Tick {
            cargo_manifest: manifest,
        })
    }

    fn cargo_of(world: &World, id: &str) -> Vec<(usize, u64)> {
        world.vessels[world.vessel_index(id).unwrap()]
            .cargo
            .iter()
            .map(|c| (c.destination, c.quantity))
            .collect()
    }

    fn docks(world: &World) -> Vec<(usize, usize, u64)> {
        world
            .cargo
            .iter()
            .map(|c| (c.location, c.destination, c.quantity))
            .collect()
    }

    fn docked(location: usize) -> VesselState {
        VesselState::Docked { location }
    }

    fn idle(location: usize) -> VesselState {
        VesselState::Idle { location }
    }

    #[test]
    fn flw1_2_03_init_manifest_fills_the_docks() {
        // Also FLW1-1-00 (a dock with inventory at each location) and FLW1-1-05.
        let mut engine = Engine::default();
        let manifest = vec![units(0, 2, 1), units(0, 1, 1), units(2, 0, 1)];
        let world = engine
            .handle(flw1_init(&[("v1", 0)], manifest.clone()))
            .unwrap();
        assert_eq!(world.cargo, manifest);
        assert!(world.vessels[0].cargo.is_empty());
    }

    #[test]
    fn flw1_2_05_manifest_entries_are_location_destination_quantity() {
        let mut engine = Engine::default();
        let world = engine.handle(flw1_init(&[], vec![units(1, 2, 5)])).unwrap();
        assert_eq!(docks(&world), [(1, 2, 5)]);
    }

    #[test]
    fn flw1_2_00_cargo_enters_through_init_and_tick_manifests() {
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[], vec![units(0, 2, 1)])).unwrap();
        let world = tick_with(&mut engine, vec![units(1, 0, 2)]).unwrap();
        assert_eq!(docks(&world), [(0, 2, 1), (1, 0, 2)]);
    }

    #[test]
    fn flw1_1_05_cargo_units_can_be_entered_into_any_dock_at_any_tick() {
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[], vec![units(2, 1, 3)])).unwrap();
        tick(&mut engine);
        let world = tick_with(&mut engine, vec![units(0, 1, 1), units(2, 1, 2)]).unwrap();
        assert_eq!(world.tick, 2);
        assert_eq!(docks(&world), [(2, 1, 5), (0, 1, 1)]);
    }

    #[test]
    fn flw1_2_04_tick_manifest_adds_to_the_newly_committed_state() {
        // Also FLW1-2-02: units with the same location and destination are one entry.
        let mut engine = docked_engine();
        let world = tick_with(&mut engine, vec![units(0, 2, 2), units(1, 1, 1)]).unwrap();
        assert_eq!(world.tick, 2);
        assert_eq!(docks(&world), [(0, 2, 3), (0, 1, 1), (2, 0, 1), (1, 1, 1)]);
        assert_eq!(committed(&engine), &world);
        assert_eq!(get_state(&mut engine), world);
    }

    #[test]
    fn flw1_2_06_a_bad_manifest_entry_rejects_the_whole_command() {
        let mut engine = Engine::default();
        let bad = vec![units(0, 2, 1), units(3, 0, 1)];
        assert!(engine.handle(flw1_init(&[], bad.clone())).is_err());
        assert_eq!(
            tick_with(&mut engine, vec![]),
            Err(EngineError::NotInitialized),
            "rejected init leaves the engine uninitialized"
        );
        let mut engine = docked_engine();
        let before = get_state(&mut engine);
        assert!(tick_with(&mut engine, bad).is_err());
        assert_eq!(get_state(&mut engine), before, "tick did not advance");
    }

    #[test]
    fn flw1_2_06_manifest_beyond_the_cargo_limit_is_rejected() {
        // The world's total units may not pass u64::MAX, so no later move can overflow.
        let mut engine = Engine::default();
        let full = vec![units(0, 1, u64::MAX - 1), units(2, 0, 1)];
        assert!(engine.handle(flw1_init(&[], full)).is_ok());
        let before = get_state(&mut engine);
        assert_eq!(
            tick_with(&mut engine, vec![units(1, 2, 1)]).map_err(|e| e.to_string()),
            Err("cargo quantity limit reached".into())
        );
        assert_eq!(get_state(&mut engine), before, "tick did not advance");
        let mut engine = Engine::default();
        let over = vec![units(0, 1, u64::MAX), units(2, 0, 1)];
        assert_eq!(
            engine.handle(flw1_init(&[], over)),
            Err(EngineError::CargoOverflow)
        );
        // Units on vessels count too.
        let mut engine = Engine::default();
        let full = vec![units(0, 1, u64::MAX)];
        engine.handle(flw1_init(&[("v1", 0)], full)).unwrap();
        dock(&mut engine, "v1").unwrap();
        tick(&mut engine);
        load(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        assert_eq!(
            tick_with(&mut engine, vec![units(1, 2, 1)]),
            Err(EngineError::CargoOverflow)
        );
    }

    #[test]
    fn flw1_3_09_manifest_location_must_be_valid() {
        let mut engine = docked_engine();
        assert_eq!(
            tick_with(&mut engine, vec![units(3, 0, 1)]).map_err(|e| e.to_string()),
            Err("cargo_manifest location 3 is out of range (location count: 3)".into())
        );
    }

    #[test]
    fn flw1_3_10_manifest_destination_must_be_valid() {
        let mut engine = docked_engine();
        assert_eq!(
            tick_with(&mut engine, vec![units(0, 7, 1)]).map_err(|e| e.to_string()),
            Err("cargo_manifest destination 7 is out of range (location count: 3)".into())
        );
    }

    #[test]
    fn flw1_3_11_manifest_quantity_must_be_positive() {
        let mut engine = Engine::default();
        assert_eq!(
            engine.handle(flw1_init(&[], vec![units(0, 1, 0)])),
            Err(EngineError::NonPositiveQuantity)
        );
    }

    #[test]
    fn flw1_2_01_dock_throughput_has_one_entry_per_location() {
        let mut engine = Engine::default();
        let Command::Init { travel_times, .. } = flw1_init(&[], vec![]) else {
            unreachable!()
        };
        for throughput in [vec![2, 4], vec![2, 4, 1, 1]] {
            let init = Command::Init {
                travel_times: travel_times.clone(),
                dock_throughput: throughput,
                vessels: vec![],
                cargo_manifest: vec![],
            };
            assert_eq!(
                engine.handle(init),
                Err(EngineError::DockThroughputLength { locations: 3 })
            );
        }
    }

    #[test]
    fn flw1_2_01_at_least_one_location_is_required() {
        // N is a positive integer (FLW1-2-01), so the BASE acceptance of an empty matrix ends here.
        let mut engine = Engine::default();
        let error = engine.handle(init_with(vec![], &[])).unwrap_err();
        assert_eq!(error, EngineError::NoLocations);
        assert_eq!(
            error.to_string(),
            "travel_times and dock_throughput must have at least one location"
        );
        assert!(engine.handle(init_with(vec![vec![0]], &[])).is_ok());
    }

    #[test]
    fn flw1_3_08_dock_throughput_values_must_be_positive() {
        let mut engine = Engine::default();
        let init = Command::Init {
            travel_times: vec![vec![0, 1], vec![1, 0]],
            dock_throughput: vec![1, 0],
            vessels: vec![],
            cargo_manifest: vec![],
        };
        let error = engine.handle(init).unwrap_err();
        assert_eq!(error, EngineError::NonPositiveThroughput { location: 1 });
        assert_eq!(
            error.to_string(),
            "dock_throughput[1] must be a positive integer"
        );
    }

    // --- dock and undock ---

    #[test]
    fn flw1_1_06_idle_vessel_can_dock() {
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        let world = dock(&mut engine, "v1").unwrap();
        assert_eq!(state_of(&world, "v1"), docked(0));
        assert_eq!(state_of(committed(&engine), "v1"), idle(0));
        assert_eq!(state_of(&tick(&mut engine), "v1"), docked(0));
    }

    #[test]
    fn flw1_2_07_dock_requires_an_idle_vessel() {
        let mut engine = docked_engine();
        assert_eq!(
            dock(&mut engine, "v1"),
            Err(EngineError::VesselNotIdle("v1".into())),
            "committed docked"
        );
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        set_destination(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        assert_eq!(
            dock(&mut engine, "v1"),
            Err(EngineError::VesselNotIdle("v1".into())),
            "committed in transit"
        );
    }

    #[test]
    fn flw1_1_12_dock_after_dock_is_an_error() {
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        dock(&mut engine, "v1").unwrap();
        let before = get_state(&mut engine);
        let error = dock(&mut engine, "v1").unwrap_err();
        assert_eq!(error.to_string(), "vessel v1 is already docked");
        assert_eq!(get_state(&mut engine), before);
        let mut engine = docked_engine();
        assert_eq!(
            dock(&mut engine, "v1"),
            Err(EngineError::VesselNotIdle("v1".into())),
            "committed docked"
        );
        // Also FLW1-1-16: a cargo operation replaced the undock, so the vessel stays docked.
        undock(&mut engine, "v1").unwrap();
        load(&mut engine, "v1", 2).unwrap();
        assert_eq!(
            dock(&mut engine, "v1"),
            Err(EngineError::VesselNotIdle("v1".into())),
            "undock replaced by a load"
        );
    }

    #[test]
    fn flw1_1_07_set_destination_overrides_a_planned_dock() {
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        dock(&mut engine, "v1").unwrap();
        let world = set_destination(&mut engine, "v1", 2).unwrap();
        assert_eq!(state_of(&world, "v1"), transit(0, 2, 7));
        assert_eq!(state_of(&tick(&mut engine), "v1"), transit(0, 2, 7));
    }

    #[test]
    fn flw1_1_08_dock_overrides_a_planned_set_destination() {
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        set_destination(&mut engine, "v1", 2).unwrap();
        let world = dock(&mut engine, "v1").unwrap();
        assert_eq!(state_of(&world, "v1"), docked(0));
        assert_eq!(state_of(&tick(&mut engine), "v1"), docked(0));
    }

    #[test]
    fn flw1_1_11_docked_vessel_can_undock() {
        let mut engine = docked_engine();
        let world = undock(&mut engine, "v1").unwrap();
        assert_eq!(state_of(&world, "v1"), idle(0));
        assert_eq!(state_of(committed(&engine), "v1"), docked(0));
        assert_eq!(state_of(&tick(&mut engine), "v1"), idle(0));
    }

    #[test]
    fn flw1_1_13_undock_when_not_docked_is_an_error() {
        let error = EngineError::NotDocked("v1".into());
        let not_docked = Err(error.clone());
        let mut engine = Engine::default();
        engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        assert_eq!(undock(&mut engine, "v1"), not_docked, "committed idle");
        set_destination(&mut engine, "v1", 1).unwrap();
        assert_eq!(undock(&mut engine, "v1"), not_docked, "planned transit");
        tick(&mut engine);
        assert_eq!(undock(&mut engine, "v1"), not_docked, "committed transit");
        let mut engine = docked_engine();
        undock(&mut engine, "v1").unwrap();
        assert_eq!(undock(&mut engine, "v1"), not_docked, "already undocking");
        assert_eq!(error.to_string(), "vessel v1 is not docked");
    }

    #[test]
    fn flw1_1_13_undock_cancels_a_planned_dock() {
        let mut engine = Engine::default();
        let before = engine.handle(flw1_init(&[("v1", 0)], vec![])).unwrap();
        dock(&mut engine, "v1").unwrap();
        assert_eq!(undock(&mut engine, "v1").unwrap(), before);
        assert_eq!(state_of(&tick(&mut engine), "v1"), idle(0));
        // Also FLW1-1-08: a dock that replaced a planned trip cancels to idle, not to the trip.
        set_destination(&mut engine, "v1", 2).unwrap();
        dock(&mut engine, "v1").unwrap();
        assert_eq!(state_of(&undock(&mut engine, "v1").unwrap(), "v1"), idle(0));
        assert_eq!(state_of(&tick(&mut engine), "v1"), idle(0));
    }

    #[test]
    fn flw1_3_14_dock_after_undock_returns_planned_state_to_docked() {
        // Also FLW1-1-15: the cargo operations the undock dropped stay dropped and refunded.
        let mut engine = docked_engine();
        let before = get_state(&mut engine);
        load(&mut engine, "v1", 2).unwrap();
        undock(&mut engine, "v1").unwrap();
        assert_eq!(dock(&mut engine, "v1").unwrap(), before);
        assert_eq!(
            dock(&mut engine, "v1"),
            Err(EngineError::VesselNotIdle("v1".into())),
            "docked again, so another dock is redundant"
        );
        // Dock 2 allows one operation per tick, so a new load shows the refund stands.
        load(&mut engine, "v2", 0).unwrap();
        undock(&mut engine, "v2").unwrap();
        dock(&mut engine, "v2").unwrap();
        load(&mut engine, "v2", 0).unwrap();
        let world = tick(&mut engine);
        assert_eq!(state_of(&world, "v1"), docked(0));
        assert_eq!(cargo_of(&world, "v2"), [(0, 1)]);
    }

    // --- load, unload, swap ---

    #[test]
    fn flw1_1_09_docked_vessel_can_load() {
        let mut engine = docked_engine();
        let world = load(&mut engine, "v1", 2).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(2, 1)]);
        assert_eq!(docks(&world), [(0, 1, 1), (2, 0, 1)]);
        assert!(
            cargo_of(committed(&engine), "v1").is_empty(),
            "planned only"
        );
    }

    #[test]
    fn flw1_1_02_load_only_from_the_dock_at_the_vessels_location() {
        let mut engine = docked_engine();
        assert_eq!(
            load(&mut engine, "v1", 0),
            Err(EngineError::NoDockCargo {
                location: 0,
                destination: 0
            }),
            "the unit for 0 is at dock 2"
        );
        assert_eq!(
            cargo_of(&load(&mut engine, "v2", 0).unwrap(), "v2"),
            [(0, 1)]
        );
    }

    #[test]
    fn flw1_2_08_load_on_a_vessel_not_docked_is_an_error() {
        let mut engine = Engine::default();
        engine
            .handle(flw1_init(&[("v1", 0)], vec![units(0, 1, 1)]))
            .unwrap();
        assert_eq!(
            load(&mut engine, "v1", 1),
            Err(EngineError::NotDocked("v1".into()))
        );
        dock(&mut engine, "v1").unwrap();
        assert_eq!(
            load(&mut engine, "v1", 1),
            Err(EngineError::NotDocked("v1".into())),
            "planned docked is not enough"
        );
    }

    #[test]
    fn flw1_2_02_vessel_cargo_is_one_entry_per_destination() {
        let mut engine = docked_engine();
        tick_with(&mut engine, vec![units(0, 2, 1)]).unwrap();
        load(&mut engine, "v1", 2).unwrap();
        let world = load(&mut engine, "v1", 2).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(2, 2)]);
        assert_eq!(docks(&world), [(0, 1, 1), (2, 0, 1)]);
    }

    #[test]
    fn flw1_2_09_load_without_matching_dock_cargo_is_an_error() {
        let mut engine = docked_engine();
        load(&mut engine, "v1", 1).unwrap();
        let error = load(&mut engine, "v1", 1).unwrap_err();
        assert_eq!(
            error.to_string(),
            "no cargo for destination 1 is available to load at location 0"
        );
    }

    fn loaded_engine() -> Engine {
        // Tick 2: v1 docked at 0 carrying one unit each for 2 and 1; dock 0 empty.
        let mut engine = docked_engine();
        load(&mut engine, "v1", 2).unwrap();
        load(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        engine
    }

    #[test]
    fn flw1_1_10_docked_vessel_can_unload() {
        let mut engine = loaded_engine();
        let world = unload(&mut engine, "v1", 1).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(2, 1)]);
        assert_eq!(docks(&world), [(2, 0, 1), (0, 1, 1)]);
    }

    #[test]
    fn flw1_2_10_unload_on_a_vessel_not_docked_is_an_error() {
        let mut engine = loaded_engine();
        undock(&mut engine, "v1").unwrap();
        tick(&mut engine);
        assert_eq!(
            unload(&mut engine, "v1", 1),
            Err(EngineError::NotDocked("v1".into()))
        );
    }

    #[test]
    fn flw1_2_11_unload_without_matching_vessel_cargo_is_an_error() {
        let mut engine = loaded_engine();
        let error = unload(&mut engine, "v1", 0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "vessel v1 has no cargo for destination 0 available to unload"
        );
    }

    /// v1 carrying units for 2 and 1, docked at location 2 at tick 10.
    fn at_location_2() -> Engine {
        let mut engine = loaded_engine();
        undock(&mut engine, "v1").unwrap();
        tick(&mut engine);
        set_destination(&mut engine, "v1", 2).unwrap();
        for _ in 0..7 {
            tick(&mut engine);
        }
        dock(&mut engine, "v1").unwrap();
        tick(&mut engine);
        engine
    }

    #[test]
    fn flw1_2_12_unload_at_the_cargo_destination_delivers_it() {
        let mut engine = at_location_2();
        let world = unload(&mut engine, "v1", 2).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(1, 1)]);
        assert_eq!(docks(&world), [(2, 0, 1)], "not added to the dock");
    }

    #[test]
    fn flw1_2_13_unload_elsewhere_adds_the_cargo_to_the_dock() {
        // Also FLW1-1-03: a vessel can unload at any dock.
        let mut engine = at_location_2();
        let world = unload(&mut engine, "v1", 1).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(2, 1)]);
        assert_eq!(docks(&world), [(2, 0, 1), (2, 1, 1)]);
    }

    #[test]
    fn flw1_1_03_vessels_unload_at_any_dock() {
        // The same unit for 1 unloads at dock 0, or at dock 2 after travelling there.
        for (mut engine, dock) in [(loaded_engine(), 0), (at_location_2(), 2)] {
            let world = unload(&mut engine, "v1", 1).unwrap();
            assert_eq!(cargo_of(&world, "v1"), [(2, 1)]);
            assert!(docks(&world).contains(&(dock, 1, 1)));
        }
    }

    #[test]
    fn flw1_2_14_unload_destination_must_be_valid() {
        let mut engine = loaded_engine();
        assert_eq!(
            unload(&mut engine, "v1", 3).map_err(|e| e.to_string()),
            Err("destination 3 is out of range (location count: 3)".into())
        );
    }

    #[test]
    fn flw1_1_14_swap_loads_and_unloads_in_one_command() {
        // The spec's swap example: v1 docked at 0 with cargo for 2; dock 0 has cargo for 1.
        let mut engine = Engine::default();
        engine
            .handle(flw1_init(
                &[("v1", 0), ("v2", 2)],
                vec![units(0, 2, 1), units(0, 1, 1)],
            ))
            .unwrap();
        dock(&mut engine, "v1").unwrap();
        tick(&mut engine);
        load(&mut engine, "v1", 2).unwrap();
        tick(&mut engine);
        let world = swap(&mut engine, "v1", 1, 2).unwrap();
        assert_eq!(world.tick, 2);
        assert_eq!(cargo_of(&world, "v1"), [(1, 1)]);
        assert_eq!(docks(&world), [(0, 2, 1)]);
    }

    #[test]
    fn flw1_2_15_swap_on_a_vessel_not_docked_is_an_error() {
        let mut engine = loaded_engine();
        undock(&mut engine, "v1").unwrap();
        tick(&mut engine);
        assert_eq!(
            swap(&mut engine, "v1", 1, 2),
            Err(EngineError::NotDocked("v1".into()))
        );
    }

    #[test]
    fn flw1_2_16_swap_needs_both_cargo_units() {
        let mut engine = loaded_engine();
        assert!(matches!(
            swap(&mut engine, "v1", 1, 2),
            Err(EngineError::NoDockCargo { .. })
        ));
        let mut engine = docked_engine();
        assert!(matches!(
            swap(&mut engine, "v1", 1, 2),
            Err(EngineError::NoVesselCargo { .. })
        ));
    }

    #[test]
    fn flw1_3_12_swap_load_destination_must_be_valid() {
        // Also FLW1-2-17.
        let mut engine = loaded_engine();
        assert_eq!(
            swap(&mut engine, "v1", 3, 2).map_err(|e| e.to_string()),
            Err("load_destination 3 is out of range (location count: 3)".into())
        );
    }

    #[test]
    fn flw1_3_13_swap_unload_destination_must_be_valid() {
        // Also FLW1-2-18.
        let mut engine = loaded_engine();
        assert_eq!(
            swap(&mut engine, "v1", 1, 4).map_err(|e| e.to_string()),
            Err("unload_destination 4 is out of range (location count: 3)".into())
        );
    }

    #[test]
    fn flw1_2_17_swap_accepts_the_highest_load_destination() {
        let mut engine = docked_engine();
        load(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        let world = swap(&mut engine, "v1", 2, 1).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(2, 1)]);
        assert_eq!(docks(&world), [(2, 0, 1), (0, 1, 1)]);
    }

    #[test]
    fn flw1_2_18_swap_accepts_the_highest_unload_destination() {
        let mut engine = docked_engine();
        load(&mut engine, "v1", 2).unwrap();
        tick(&mut engine);
        let world = swap(&mut engine, "v1", 1, 2).unwrap();
        assert_eq!(cargo_of(&world, "v1"), [(1, 1)]);
        assert_eq!(docks(&world), [(2, 0, 1), (0, 2, 1)]);
    }

    // --- throughput ---

    /// v2 is docked at location 2, whose throughput is 1.
    fn one_op_spent(first: fn(&mut Engine) -> Result<World, EngineError>) -> Engine {
        let mut engine = docked_engine();
        tick_with(&mut engine, vec![units(2, 1, 3)]).unwrap();
        load(&mut engine, "v2", 1).unwrap();
        tick(&mut engine);
        first(&mut engine).unwrap();
        engine
    }

    fn second_op_is_rejected(engine: &mut Engine) {
        let before = get_state(engine);
        assert_eq!(
            load(engine, "v2", 0),
            Err(EngineError::ThroughputExceeded {
                operations: 2,
                throughput: 1,
                location: 2
            })
        );
        assert_eq!(get_state(engine), before);
    }

    #[test]
    fn flw1_3_00_load_counts_as_one_operation() {
        second_op_is_rejected(&mut one_op_spent(|e| load(e, "v2", 0)));
    }

    #[test]
    fn flw1_3_01_unload_counts_as_one_operation() {
        second_op_is_rejected(&mut one_op_spent(|e| unload(e, "v2", 1)));
    }

    #[test]
    fn flw1_3_02_swap_counts_as_one_operation() {
        second_op_is_rejected(&mut one_op_spent(|e| swap(e, "v2", 0, 1)));
    }

    #[test]
    fn flw1_1_01_each_location_has_its_own_budget() {
        // Dock 2 (throughput 1) is spent while dock 0 (throughput 2) still accepts operations.
        let mut engine = docked_engine();
        load(&mut engine, "v2", 0).unwrap();
        assert!(matches!(
            load(&mut engine, "v2", 0),
            Err(EngineError::ThroughputExceeded { location: 2, .. })
        ));
        load(&mut engine, "v1", 2).unwrap();
        load(&mut engine, "v1", 1).unwrap();
    }

    #[test]
    fn flw1_3_07_throughput_is_shared_by_all_vessels_at_a_dock_per_tick() {
        // Also FLW1-1-01 and FLW1-3-04: the third operation at dock 0 (throughput 2) is rejected,
        // the budget is per location, and it resets at the next tick.
        let mut engine = Engine::default();
        let manifest = vec![units(0, 1, 4), units(2, 0, 1)];
        engine
            .handle(flw1_init(&[("a", 0), ("b", 0), ("c", 2)], manifest))
            .unwrap();
        for id in ["a", "b", "c"] {
            dock(&mut engine, id).unwrap();
        }
        tick(&mut engine);
        load(&mut engine, "a", 1).unwrap();
        load(&mut engine, "b", 1).unwrap();
        load(&mut engine, "c", 0).unwrap();
        let error = load(&mut engine, "a", 1).unwrap_err();
        assert_eq!(
            error.to_string(),
            "transfer of 3 operations exceeds dock throughput of 2 at location 0"
        );
        tick(&mut engine);
        load(&mut engine, "a", 1).unwrap();
        load(&mut engine, "b", 1).unwrap();
    }

    #[test]
    fn flw1_3_05_rejected_operation_reports_the_spec_error() {
        let mut engine = docked_engine();
        tick_with(&mut engine, vec![units(0, 1, 1)]).unwrap();
        load(&mut engine, "v1", 1).unwrap();
        load(&mut engine, "v1", 1).unwrap();
        assert_eq!(
            load(&mut engine, "v1", 2).map_err(|e| e.to_string()),
            Err("transfer of 3 operations exceeds dock throughput of 2 at location 0".into())
        );
    }

    #[test]
    fn flw1_3_04_rejected_operation_leaves_the_state_unchanged() {
        let mut engine = docked_engine();
        load(&mut engine, "v1", 1).unwrap();
        load(&mut engine, "v1", 2).unwrap();
        let before = get_state(&mut engine);
        assert!(load(&mut engine, "v1", 2).is_err());
        assert_eq!(get_state(&mut engine), before);
    }

    // --- overrides ---

    #[test]
    fn flw1_1_15_undock_overrides_pending_cargo_operations() {
        let mut engine = loaded_engine();
        tick_with(&mut engine, vec![units(0, 0, 1)]).unwrap();
        unload(&mut engine, "v1", 1).unwrap();
        swap(&mut engine, "v1", 0, 2).unwrap();
        let world = undock(&mut engine, "v1").unwrap();
        let committed = committed(&engine).clone();
        assert_eq!(state_of(&world, "v1"), idle(0));
        assert_eq!(cargo_of(&world, "v1"), cargo_of(&committed, "v1"));
        assert_eq!(world.cargo, committed.cargo);
    }

    #[test]
    fn flw1_3_03_undock_refunds_the_operations_it_overrides() {
        let mut engine = Engine::default();
        engine
            .handle(flw1_init(&[("a", 0), ("b", 0)], vec![units(0, 1, 4)]))
            .unwrap();
        dock(&mut engine, "a").unwrap();
        dock(&mut engine, "b").unwrap();
        tick(&mut engine);
        load(&mut engine, "a", 1).unwrap();
        load(&mut engine, "a", 1).unwrap();
        assert!(load(&mut engine, "b", 1).is_err(), "budget of 2 spent");
        undock(&mut engine, "a").unwrap();
        load(&mut engine, "b", 1).unwrap();
        load(&mut engine, "b", 1).unwrap();
    }

    #[test]
    fn flw1_1_16_cargo_operation_overrides_a_pending_undock() {
        let mut engine = docked_engine();
        undock(&mut engine, "v1").unwrap();
        let world = load(&mut engine, "v1", 2).unwrap();
        assert_eq!(state_of(&world, "v1"), docked(0));
        assert_eq!(cargo_of(&world, "v1"), [(2, 1)]);
        let world = tick(&mut engine);
        assert_eq!(state_of(&world, "v1"), docked(0));
    }

    #[test]
    fn flw1_1_16_rejected_cargo_operation_keeps_the_pending_undock() {
        let mut engine = docked_engine();
        undock(&mut engine, "v1").unwrap();
        assert!(load(&mut engine, "v1", 0).is_err());
        assert_eq!(state_of(&get_state(&mut engine), "v1"), idle(0));
    }

    #[test]
    fn flw1_3_06_load_then_unload_of_the_same_cargo_cancels_out() {
        // Also FLW1-3-03: the pair costs one operation, so one more fits in throughput 2.
        let mut engine = docked_engine();
        let before = get_state(&mut engine);
        load(&mut engine, "v1", 2).unwrap();
        let world = unload(&mut engine, "v1", 2).unwrap();
        assert_eq!(world, before);
        load(&mut engine, "v1", 1).unwrap();
        assert!(matches!(
            load(&mut engine, "v1", 2),
            Err(EngineError::ThroughputExceeded { operations: 3, .. })
        ));
    }

    #[test]
    fn flw1_3_06_unload_then_load_of_the_same_cargo_cancels_out() {
        let mut engine = loaded_engine();
        let before = get_state(&mut engine);
        unload(&mut engine, "v1", 1).unwrap();
        let world = load(&mut engine, "v1", 1).unwrap();
        assert_eq!(world, before);
    }

    #[test]
    fn flw1_1_17_get_state_includes_planned_cargo_and_docks() {
        let mut engine = docked_engine();
        let response = load(&mut engine, "v1", 1).unwrap();
        let state = get_state(&mut engine);
        assert_eq!(state, response);
        assert_eq!(cargo_of(&state, "v1"), [(1, 1)]);
        assert_eq!(docks(&state), [(0, 2, 1), (2, 0, 1)]);
    }

    #[test]
    fn flw1_1_04_every_response_reports_dock_and_vessel_cargo() {
        let mut engine = docked_engine();
        for world in [
            load(&mut engine, "v1", 1).unwrap(),
            tick(&mut engine),
            undock(&mut engine, "v1").unwrap(),
            get_state(&mut engine),
        ] {
            assert_eq!(cargo_of(&world, "v1"), [(1, 1)]);
            assert_eq!(docks(&world), [(0, 2, 1), (2, 0, 1)]);
        }
    }

    #[test]
    fn flw1_1_00_each_location_has_its_own_dock_inventory() {
        let mut engine = docked_engine();
        let world = load(&mut engine, "v2", 0).unwrap();
        assert_eq!(docks(&world), [(0, 2, 1), (0, 1, 1)], "only dock 2 changed");
    }

    // --- FLW2: historical state queries ---

    fn get_state_at(engine: &mut Engine, tick: u64) -> Result<World, EngineError> {
        engine.handle(Command::GetStateAt { tick })
    }

    /// Ticks 0 to 4 with a trip, a dock, a load, a manifest and an undock. Returns the engine and
    /// the world reported by init and by each tick.
    fn flw2_run() -> (Engine, Vec<World>) {
        let mut engine = Engine::default();
        let init = flw1_init(&[("v1", 0), ("v2", 2)], vec![units(0, 2, 1)]);
        let mut worlds = vec![engine.handle(init).unwrap()];
        set_destination(&mut engine, "v2", 1).unwrap();
        dock(&mut engine, "v1").unwrap();
        worlds.push(tick(&mut engine));
        load(&mut engine, "v1", 2).unwrap();
        worlds.push(tick(&mut engine));
        worlds.push(tick_with(&mut engine, vec![units(1, 0, 2)]).unwrap());
        undock(&mut engine, "v1").unwrap();
        worlds.push(tick(&mut engine));
        (engine, worlds)
    }

    #[test]
    fn flw2_2_19_valid_ticks_run_from_0_to_the_current_tick() {
        let (mut engine, _) = flw2_run();
        for tick in 0..=4 {
            assert_eq!(get_state_at(&mut engine, tick).unwrap().tick, tick);
        }
    }

    #[test]
    fn flw2_2_20_a_future_tick_is_an_error() {
        let (mut engine, _) = flw2_run();
        let before = get_state(&mut engine);
        for tick in [5, u64::MAX] {
            assert_eq!(
                get_state_at(&mut engine, tick),
                Err(EngineError::FutureTick { tick, current: 4 })
            );
        }
        assert_eq!(get_state(&mut engine), before);
        let error = EngineError::FutureTick {
            tick: 99,
            current: 36,
        };
        assert_eq!(
            error.to_string(),
            "tick 99 is in the future (current tick: 36)"
        );
    }

    #[test]
    fn flw2_1_00_init_records_tick_0() {
        let mut engine = Engine::default();
        let init = flw1_init(&[("v1", 0)], vec![units(0, 1, 1)]);
        let world = engine.handle(init).unwrap();
        assert_eq!(
            get_state_at(&mut engine, 0),
            Ok(world.clone()),
            "before any tick"
        );
        set_destination(&mut engine, "v1", 1).unwrap();
        tick(&mut engine);
        tick(&mut engine);
        assert_eq!(get_state_at(&mut engine, 0), Ok(world), "after two ticks");
    }

    #[test]
    fn flw2_1_01_each_tick_records_the_world_after_all_transitions() {
        let (mut engine, worlds) = flw2_run();
        let at_3 = get_state_at(&mut engine, 3).unwrap();
        assert_eq!(at_3, worlds[3]);
        assert_eq!(state_of(&at_3, "v2"), idle(1), "the arrival on tick 3");
        assert_eq!(docks(&at_3), [(1, 0, 2)], "tick 3's manifest");
        assert_eq!(
            cargo_of(&at_3, "v1"),
            [(2, 1)],
            "the load committed on tick 2"
        );
    }

    #[test]
    fn flw2_1_02_get_state_at_returns_each_committed_state() {
        let (mut engine, worlds) = flw2_run();
        for (tick, world) in (0..).zip(&worlds) {
            assert_eq!(get_state_at(&mut engine, tick).as_ref(), Ok(world));
        }
    }

    #[test]
    fn flw2_1_03_states_between_ticks_are_not_queryable() {
        let (mut engine, worlds) = flw2_run();
        let planned = set_destination(&mut engine, "v2", 2).unwrap();
        assert_ne!(planned, worlds[4]);
        assert_eq!(
            get_state_at(&mut engine, 4),
            Ok(worlds[4].clone()),
            "the committed world, not the planned one"
        );
    }

    #[test]
    fn flw2_1_01_a_rejected_tick_records_nothing() {
        let (mut engine, mut worlds) = flw2_run();
        let out_of_range = EngineError::OutOfRange {
            field: "cargo_manifest location",
            value: 9,
            locations: 3,
        };
        assert_eq!(
            tick_with(&mut engine, vec![units(9, 0, 1)]),
            Err(out_of_range)
        );
        worlds.push(tick(&mut engine));
        worlds.push(tick(&mut engine));
        for (tick, world) in (0..).zip(&worlds) {
            assert_eq!(get_state_at(&mut engine, tick).as_ref(), Ok(world));
        }
    }

    #[test]
    fn flw2_0_00_past_committed_states_are_queryable() {
        // Also FLW2-1-00 to FLW2-1-03 together: every tick stays queryable as the run goes on.
        let (mut engine, worlds) = flw2_run();
        load(&mut engine, "v1", 0).unwrap_err();
        set_destination(&mut engine, "v1", 1).unwrap();
        for _ in 0..3 {
            tick(&mut engine);
        }
        for (tick, world) in (0..).zip(&worlds) {
            assert_eq!(get_state_at(&mut engine, tick).as_ref(), Ok(world));
        }
        assert_eq!(get_state_at(&mut engine, 7), Ok(committed(&engine).clone()));
    }

    #[test]
    fn flw2_0_01_queries_leave_the_state_unchanged() {
        // Also BASE-2-00: get_state_at before init is an error, like every other command.
        let mut engine = Engine::default();
        assert_eq!(
            get_state_at(&mut engine, 0),
            Err(EngineError::NotInitialized)
        );
        let (mut engine, _) = flw2_run();
        let (mut control, _) = flw2_run();
        for sim in [&mut engine, &mut control] {
            set_destination(sim, "v2", 2).unwrap();
        }
        get_state_at(&mut engine, 2).unwrap();
        get_state_at(&mut engine, 4).unwrap();
        get_state_at(&mut engine, 99).unwrap_err();
        assert_eq!(get_state(&mut engine), get_state(&mut control));
        assert_eq!(tick(&mut engine), tick(&mut control));
    }
}
