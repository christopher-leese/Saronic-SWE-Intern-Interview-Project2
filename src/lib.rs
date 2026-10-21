//! Discrete-time fleet simulation engine driven by newline-delimited JSON commands.

pub mod cargo;
pub mod engine;
pub mod error;
pub mod protocol;
pub mod world;

use std::io::{self, BufRead, Write};

use engine::Engine;
use protocol::{parse, Response};

/// Processes commands in the order received, writing exactly one response line per command.
/// The engine acts only on these commands. Blank lines are skipped. Satisfies BASE-0-01,
/// BASE-0-04, BASE-0-05, BASE-0-06, BASE-1-02, and BASE-1-03 (the planned state is updated by
/// `handle` before the response is written). `main` passes stdin and stdout (BASE-1-04, 1-05).
pub fn run(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut engine = Engine::default();
    for line in input.split(b'\n') {
        let line = line?;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = Response::from(parse(&line).and_then(|command| engine.handle(command)));
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_str(input: &str) -> Vec<String> {
        let mut output = Vec::new();
        run(input.as_bytes(), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    const INIT: &str = r#"{"command":"init","parameters":{"travel_times":[[0,4],[4,0]],"dock_throughput":[1,1],"vessels":[{"id":"v1","location":0}]}}"#;
    const TICK: &str = r#"{"command":"tick","parameters":{}}"#;
    const GO: &str =
        r#"{"command":"set_destination","parameters":{"vessel_id":"v1","destination":1}}"#;

    #[test]
    fn base_0_01_no_commands_no_output() {
        assert!(run_str("").is_empty());
        assert!(run_str("\n\n").is_empty());
    }

    #[test]
    fn base_0_04_every_output_line_is_json() {
        for line in run_str(&format!("{INIT}\nnot json\n{TICK}\n")) {
            serde_json::from_str::<serde_json::Value>(&line).unwrap();
        }
    }

    #[test]
    fn base_0_06_processing_order_decides_the_result() {
        // Also BASE-1-02.
        let tick_first = run_str(&format!("{INIT}\n{TICK}\n{GO}\n"));
        let go_first = run_str(&format!("{INIT}\n{GO}\n{TICK}\n"));
        assert!(tick_first[2].contains(r#""arrives_at_tick":5"#));
        assert!(go_first[2].contains(r#""arrives_at_tick":4"#));
    }

    #[test]
    fn base_1_00_successes_are_reported_ok() {
        for line in run_str(&format!("{INIT}\n{GO}\n{TICK}\n")) {
            assert!(line.starts_with(r#"{"status":"ok","payload":{"tick":"#));
        }
    }

    #[test]
    fn base_1_01_errors_are_reported_with_a_message() {
        let lines = run_str(&format!("{TICK}\nnot json\n{INIT}\n{INIT}\n"));
        for index in [0, 1, 3] {
            assert!(lines[index].starts_with(r#"{"status":"error","payload":{"message":""#));
        }
    }

    #[test]
    fn base_1_04_responses_are_written_to_the_given_output() {
        let mut output = Vec::new();
        run(INIT.as_bytes(), &mut output).unwrap();
        assert!(output.ends_with(b"}\n"));
    }

    #[test]
    fn base_3_00_success_line_is_exact() {
        assert_eq!(
            run_str(INIT),
            [
                r#"{"status":"ok","payload":{"tick":0,"vessels":{"v1":{"state":"idle","location":0,"cargo":[]}},"cargo":[]}}"#
            ]
        );
    }

    #[test]
    fn base_3_01_error_line_is_exact() {
        assert_eq!(
            run_str(&format!("{INIT}\n{GO}\n{TICK}\n{GO}\n"))[3],
            r#"{"status":"error","payload":{"message":"vessel v1 is not idle"}}"#
        );
    }

    #[test]
    fn base_0_05_one_output_line_per_command_including_errors() {
        let input = format!("{INIT}\nnot json\n{{\"command\":\"tick\",\"parameters\":{{}}}}\n");
        let lines = run_str(&input);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with(r#"{"status":"error""#));
    }

    #[test]
    fn base_0_05_blank_lines_produce_no_output() {
        let lines = run_str(&format!("\n{INIT}\n   \n\r\n"));
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn base_1_02_commands_take_effect_in_order() {
        let input = format!(
            "{INIT}\n{}\n{}\n",
            r#"{"command":"set_destination","parameters":{"vessel_id":"v1","destination":1}}"#,
            r#"{"command":"get_state","parameters":{}}"#
        );
        let lines = run_str(&input);
        assert!(lines[2].contains(r#""state":"transit""#));
    }

    #[test]
    fn base_1_05_invalid_utf8_line_gets_an_error_line() {
        let mut input = INIT.as_bytes().to_vec();
        input.extend_from_slice(b"\n\xff\xfe\n");
        let mut output = Vec::new();
        run(&input[..], &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().nth(1).unwrap().contains("invalid command"));
    }

    #[test]
    fn base_1_05_last_line_without_newline_is_processed() {
        assert_eq!(run_str(INIT).len(), 1);
    }
}
