use bollard::container::LogOutput;

use super::LineAssembler;

fn stdout(text: &[u8]) -> LogOutput {
    LogOutput::StdOut {
        message: text.to_vec().into(),
    }
}

fn stderr(text: &[u8]) -> LogOutput {
    LogOutput::StdErr {
        message: text.to_vec().into(),
    }
}

#[test]
fn assembler_joins_a_line_split_across_frames() {
    let mut assembler = LineAssembler::default();

    let first = assembler.push(stdout(b"::set-output name=x::"));
    let second = assembler.push(stdout(b"value\n"));

    assert!(first.is_empty());
    assert_eq!(second, vec!["::set-output name=x::value"]);
}

#[test]
fn assembler_splits_a_frame_with_several_lines() {
    let mut assembler = LineAssembler::default();

    let lines = assembler.push(stdout(b"one\n\ntwo\nthr"));

    assert_eq!(lines, vec!["one", "", "two"]);
    assert_eq!(assembler.finish(), vec!["thr"]);
}

#[test]
fn assembler_keeps_stdout_and_stderr_partials_apart() {
    let mut assembler = LineAssembler::default();

    assembler.push(stdout(b"out-"));
    let from_stderr = assembler.push(stderr(b"err\n"));
    let from_stdout = assembler.push(stdout(b"line\n"));

    assert_eq!(from_stderr, vec!["err"]);
    assert_eq!(from_stdout, vec!["out-line"]);
}

#[test]
fn assembler_keeps_a_utf8_character_split_across_frames() {
    let mut assembler = LineAssembler::default();
    let bytes = "caf\u{e9}\n".as_bytes();

    assembler.push(stdout(&bytes[..4]));
    let lines = assembler.push(stdout(&bytes[4..]));

    assert_eq!(lines, vec!["caf\u{e9}"]);
}

#[test]
fn assembler_strips_carriage_returns() {
    let mut assembler = LineAssembler::default();

    let lines = assembler.push(stdout(b"windows\r\n"));

    assert_eq!(lines, vec!["windows"]);
}

#[test]
fn assembler_finish_is_empty_after_complete_lines() {
    let mut assembler = LineAssembler::default();

    assembler.push(stdout(b"done\n"));

    assert!(assembler.finish().is_empty());
}
