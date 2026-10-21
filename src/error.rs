//! The engine's single error type. Every error message is defined once, in `Display`.

use std::fmt;

/// Every reason a command can be rejected. Satisfies BASE-1-01 (with `Response::Error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The line is not a valid command: bad JSON, unknown command, or wrong parameters.
    InvalidCommand(String),
    NotInitialized,
    AlreadyInitialized,
    TravelTimesNotSquare,
    NonZeroDiagonal {
        location: usize,
    },
    NonPositiveTravelTime {
        from: usize,
        to: usize,
    },
    DuplicateVessel(String),
    OutOfRange {
        field: &'static str,
        value: usize,
        locations: usize,
    },
    UnknownVessel(String),
    VesselNotIdle(String),
    AlreadyAtDestination {
        vessel: String,
        location: usize,
    },
    TickOverflow,
    FutureTick {
        tick: u64,
        current: u64,
    },
    ArrivalOverflow(String),
    NoLocations,
    DockThroughputLength {
        locations: usize,
    },
    NonPositiveThroughput {
        location: usize,
    },
    NonPositiveQuantity,
    CargoOverflow,
    NotDocked(String),
    AlreadyDocked(String),
    ThroughputExceeded {
        operations: usize,
        throughput: u64,
        location: usize,
    },
    NoDockCargo {
        location: usize,
        destination: usize,
    },
    NoVesselCargo {
        vessel: String,
        destination: usize,
    },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommand(reason) => write!(f, "invalid command: {reason}"),
            Self::NotInitialized => {
                write!(
                    f,
                    "simulation is not initialized: init must be the first command"
                )
            }
            Self::AlreadyInitialized => write!(f, "simulation is already initialized"),
            Self::TravelTimesNotSquare => write!(f, "travel_times must be an NxN matrix"),
            Self::NonZeroDiagonal { location } => {
                write!(f, "travel_times[{location}][{location}] must be 0")
            }
            Self::NonPositiveTravelTime { from, to } => {
                write!(f, "travel_times[{from}][{to}] must be a positive integer")
            }
            Self::DuplicateVessel(id) => write!(f, "vessel {id} is defined more than once"),
            Self::OutOfRange {
                field,
                value,
                locations,
            } => write!(
                f,
                "{field} {value} is out of range (location count: {locations})"
            ),
            Self::UnknownVessel(id) => write!(f, "vessel {id} does not exist"),
            Self::VesselNotIdle(id) => write!(f, "vessel {id} is not idle"),
            Self::AlreadyAtDestination { vessel, location } => {
                write!(f, "vessel {vessel} is already at location {location}")
            }
            Self::TickOverflow => write!(f, "tick limit reached"),
            Self::FutureTick { tick, current } => {
                write!(f, "tick {tick} is in the future (current tick: {current})")
            }
            Self::ArrivalOverflow(id) => write!(f, "vessel {id} would arrive after the tick limit"),
            Self::NoLocations => write!(
                f,
                "travel_times and dock_throughput must have at least one location"
            ),
            Self::DockThroughputLength { locations } => write!(
                f,
                "dock_throughput must have one entry per location (location count: {locations})"
            ),
            Self::NonPositiveThroughput { location } => {
                write!(f, "dock_throughput[{location}] must be a positive integer")
            }
            Self::NonPositiveQuantity => {
                write!(f, "cargo_manifest quantity must be a positive integer")
            }
            Self::CargoOverflow => write!(f, "cargo quantity limit reached"),
            Self::NotDocked(id) => write!(f, "vessel {id} is not docked"),
            Self::AlreadyDocked(id) => write!(f, "vessel {id} is already docked"),
            Self::ThroughputExceeded {
                operations,
                throughput,
                location,
            } => write!(
                f,
                "transfer of {operations} operations exceeds dock throughput of {throughput} at location {location}"
            ),
            Self::NoDockCargo {
                location,
                destination,
            } => write!(
                f,
                "no cargo for destination {destination} is available to load at location {location}"
            ),
            Self::NoVesselCargo {
                vessel,
                destination,
            } => write!(
                f,
                "vessel {vessel} has no cargo for destination {destination} available to unload"
            ),
        }
    }
}

impl std::error::Error for EngineError {}
