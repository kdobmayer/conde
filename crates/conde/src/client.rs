//! Blocking unix-socket client used by the CLI.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, bail};

use crate::protocol::{Request, Response};

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    pub fn connect(path: &Path) -> anyhow::Result<Client> {
        let stream = UnixStream::connect(path).with_context(|| {
            format!("cannot reach the conde daemon at {} (start it with: conde daemon)", path.display())
        })?;
        Ok(Client { reader: BufReader::new(stream.try_clone()?), writer: stream })
    }

    pub fn call(&mut self, req: &Request) -> anyhow::Result<Response> {
        let mut line = serde_json::to_vec(req)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        let mut buf = String::new();
        if self.reader.read_line(&mut buf)? == 0 {
            bail!("the conde daemon closed the connection");
        }
        serde_json::from_str(&buf).context("unreadable response frame")
    }
}
