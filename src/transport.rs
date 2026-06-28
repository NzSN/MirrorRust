use crate::Error;
use std::io::{BufRead, BufReader, Lines, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Transport {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
}

pub fn spawn_mirror(bin_path: &str) -> Result<Transport, Error> {
    let mut child = Command::new(bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let lines = BufReader::new(stdout).lines();
    Ok(Transport { child, stdin: Some(stdin), lines })
}

impl Transport {
    /// Write a single newline-terminated line and flush.
    pub fn send(&mut self, line: &str) -> Result<(), Error> {
        let stdin = self.stdin.as_mut().ok_or(Error::TransportClosed)?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Read one line from the child's stdout. `Ok(None)` on EOF.
    pub fn recv(&mut self) -> Result<Option<String>, Error> {
        match self.lines.next() {
            Some(Ok(line)) => Ok(Some(line)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Close stdin (signal EOF) and wait for the child. Idempotent.
    pub fn close(&mut self) -> Result<i32, Error> {
        self.stdin.take(); // dropping ChildStdin closes the pipe
        let status = self.child.wait()?;
        Ok(status.code().unwrap_or(0))
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}
