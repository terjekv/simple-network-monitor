//! Bounded subprocess execution shared by the system probe backends.
use std::{
    io,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

async fn bounded_read(reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > OUTPUT_LIMIT {
        return Err(io::Error::other("probe output exceeded 64 KiB"));
    }
    Ok(bytes)
}

/// Dropping or timing out this future kills the child. Both pipes are drained
/// concurrently and have independent size bounds.
pub async fn run(command: &mut Command, input: &[u8], timeout: Duration) -> io::Result<Output> {
    command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing probe stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing probe stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing probe stderr"))?;
    tokio::time::timeout(timeout, async {
        let write = async {
            stdin.write_all(input).await?;
            drop(stdin);
            Ok::<_, io::Error>(())
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            write,
            bounded_read(stdout),
            bounded_read(stderr),
            child.wait()
        )?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "probe timed out"))?
}

pub fn error_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(4096)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_oversized_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 65537 /dev/zero"]);
        assert!(
            run(&mut command, b"", Duration::from_secs(2))
                .await
                .unwrap_err()
                .to_string()
                .contains("exceeded")
        );
    }
    #[tokio::test]
    async fn times_out_stalled_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 10"]);
        assert_eq!(
            run(&mut command, b"", Duration::from_millis(30))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
