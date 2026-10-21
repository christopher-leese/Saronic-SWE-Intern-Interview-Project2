//! Black-box harness: runs the built `fleet-sim` binary over stdin/stdout.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Runs the binary on `input` and returns its stdout lines. Fails if it exits unsuccessfully or
/// writes to stderr.
pub fn run(input: &str) -> Vec<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fleet-sim"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary starts");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input.as_bytes())
        .expect("stdin accepts input");
    let output = child.wait_with_output().expect("binary finishes");
    assert!(output.status.success(), "exit status {}", output.status);
    assert!(output.stderr.is_empty(), "unexpected stderr output");
    String::from_utf8(output.stdout)
        .expect("stdout is UTF-8")
        .lines()
        .map(String::from)
        .collect()
}

/// Runs commands given one per element and returns each response parsed as JSON.
pub fn run_commands(commands: &[&str]) -> Vec<Value> {
    run(&(commands.join("\n") + "\n"))
        .iter()
        .map(|line| serde_json::from_str(line).expect("every output line is JSON"))
        .collect()
}

/// Replays a spec transcript: command lines, `→ ` expected responses, `#` comments. Every command
/// runs; each `→` line is compared, as parsed JSON, with the response to the command before it.
pub fn assert_transcript(transcript: &str) {
    let mut commands = Vec::new();
    let mut expected = Vec::new();
    for line in transcript.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(response) = line.strip_prefix('→') {
            let index = commands
                .len()
                .checked_sub(1)
                .expect("→ line follows a command");
            expected.push((index, response.trim().to_string()));
        } else {
            commands.push(line);
        }
    }
    let actual = run(&(commands.join("\n") + "\n"));
    assert_eq!(actual.len(), commands.len(), "one response per command");
    for (index, want) in expected {
        let want: Value = serde_json::from_str(&want).expect("expected line is JSON");
        let got: Value = serde_json::from_str(&actual[index]).expect("response is JSON");
        assert_eq!(
            got,
            want,
            "response to command {}: {}",
            index + 1,
            commands[index]
        );
    }
}
