//! Reads newline-delimited JSON commands from stdin and writes one JSON response per line to stdout.
//! Satisfies BASE-1-04 and BASE-1-05.

fn main() -> std::io::Result<()> {
    fleet_sim::run(std::io::stdin().lock(), std::io::stdout().lock())
}
