use bollard::container::LogOutput;
use futures::{Stream, StreamExt};
use tracing::warn;

use crate::job::output::OutputProcessor;

/// Feeds a Docker output stream to the processor one line at a time.
pub async fn process_docker_output<S>(mut stream: S, processor: &OutputProcessor)
where
    S: Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin,
{
    let mut assembler = LineAssembler::default();
    while let Some(frame) = stream.next().await {
        let output = match frame {
            Ok(output) => output,
            Err(e) => {
                warn!(error = %e, "reading docker output failed");
                break;
            }
        };
        for line in assembler.push(output) {
            processor.process_line(&line).await;
        }
    }
    for line in assembler.finish() {
        processor.process_line(&line).await;
    }
}

/// Rebuilds lines from Docker output frames. Frames follow the container's writes, not its
/// lines, and stdout and stderr interleave, so each stream keeps its own partial line.
/// Bytes are decoded only once a line is complete, so a split UTF-8 character survives.
#[derive(Default)]
struct LineAssembler {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl LineAssembler {
    fn push(&mut self, output: LogOutput) -> Vec<String> {
        let buffer = match output {
            LogOutput::StdErr { .. } => &mut self.stderr,
            _ => &mut self.stdout,
        };
        buffer.extend_from_slice(&output.into_bytes());

        let Some(last_newline) = buffer.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let partial = buffer.split_off(last_newline + 1);
        let complete = std::mem::replace(buffer, partial);
        complete[..last_newline]
            .split(|&b| b == b'\n')
            .map(decode_line)
            .collect()
    }

    /// Returns the unterminated last line of each stream.
    fn finish(self) -> Vec<String> {
        [self.stdout, self.stderr]
            .into_iter()
            .filter(|buffer| !buffer.is_empty())
            .map(|buffer| decode_line(&buffer))
            .collect()
    }
}

fn decode_line(bytes: &[u8]) -> String {
    let line = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    String::from_utf8_lossy(line).into_owned()
}

#[cfg(test)]
#[path = "output_test.rs"]
mod output_test;
