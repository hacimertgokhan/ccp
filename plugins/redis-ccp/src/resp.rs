//! A tiny RESP2 client: enough for read-only commands, with pipelining.
//! No dependencies, no allocation beyond the replies themselves.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_BULK: usize = 128 * 1024 * 1024;
const MAX_ARRAY: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Resp {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Resp>>),
}

impl Resp {
    pub fn text(&self) -> Option<String> {
        match self {
            Resp::Simple(s) => Some(s.clone()),
            Resp::Bulk(Some(b)) => Some(String::from_utf8_lossy(b).into_owned()),
            Resp::Int(i) => Some(i.to_string()),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Resp::Int(i) => Some(*i),
            Resp::Bulk(Some(b)) => std::str::from_utf8(b).ok()?.parse().ok(),
            _ => None,
        }
    }
    pub fn items(&self) -> &[Resp] {
        match self {
            Resp::Array(Some(v)) => v,
            _ => &[],
        }
    }
}

pub fn encode(args: &[&[u8]], out: &mut Vec<u8>) {
    out.extend_from_slice(format!("*{}\r\n", args.len()).as_bytes());
    for a in args {
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a);
        out.extend_from_slice(b"\r\n");
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn read_resp<R: BufRead>(r: &mut R) -> io::Result<Resp> {
    let mut line = Vec::new();
    if r.read_until(b'\n', &mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    if line.len() < 3 || !line.ends_with(b"\r\n") {
        return Err(bad("malformed reply"));
    }
    let body = &line[1..line.len() - 2];
    let text = || String::from_utf8_lossy(body).into_owned();
    let num = || text().parse::<i64>().map_err(|_| bad("bad length"));
    match line[0] {
        b'+' => Ok(Resp::Simple(text())),
        b'-' => Ok(Resp::Error(text())),
        b':' => Ok(Resp::Int(num()?)),
        b'$' => {
            let n = num()?;
            if n < 0 {
                return Ok(Resp::Bulk(None));
            }
            let n = n as usize;
            if n > MAX_BULK {
                return Err(bad("bulk string too large"));
            }
            let mut buf = vec![0u8; n + 2];
            r.read_exact(&mut buf)?;
            buf.truncate(n);
            Ok(Resp::Bulk(Some(buf)))
        }
        b'*' => {
            let n = num()?;
            if n < 0 {
                return Ok(Resp::Array(None));
            }
            let n = n as usize;
            if n > MAX_ARRAY {
                return Err(bad("array too large"));
            }
            let mut v = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                v.push(read_resp(r)?);
            }
            Ok(Resp::Array(Some(v)))
        }
        _ => Err(bad("unknown reply type")),
    }
}

pub struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Conn {
    pub fn connect(host: &str, port: u16) -> io::Result<Conn> {
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| bad("cannot resolve host"))?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        s.set_read_timeout(Some(Duration::from_secs(15)))?;
        s.set_write_timeout(Some(Duration::from_secs(15)))?;
        s.set_nodelay(true)?;
        Ok(Conn {
            reader: BufReader::with_capacity(64 * 1024, s.try_clone()?),
            writer: s,
        })
    }

    /// Send every command in one write, then read every reply (one round trip).
    pub fn pipeline(&mut self, cmds: &[Vec<&[u8]>]) -> io::Result<Vec<Resp>> {
        let mut buf = Vec::with_capacity(cmds.len() * 32);
        for c in cmds {
            encode(c, &mut buf);
        }
        self.writer.write_all(&buf)?;
        self.writer.flush()?;
        cmds.iter().map(|_| read_resp(&mut self.reader)).collect()
    }

    pub fn cmd(&mut self, args: &[&[u8]]) -> Result<Resp, String> {
        let mut out = self.pipeline(&[args.to_vec()]).map_err(|e| e.to_string())?;
        match out.remove(0) {
            Resp::Error(e) => Err(e),
            r => Ok(r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encodes_commands() {
        let mut out = Vec::new();
        encode(&[b"GET", b"k"], &mut out);
        assert_eq!(out, b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n");
    }

    #[test]
    fn parses_every_reply_type() {
        let data =
            b"+OK\r\n-ERR boom\r\n:42\r\n$5\r\nhe\r\nl\r\n$-1\r\n*2\r\n$1\r\na\r\n:1\r\n*-1\r\n";
        let mut c = Cursor::new(&data[..]);
        assert_eq!(read_resp(&mut c).unwrap(), Resp::Simple("OK".into()));
        assert_eq!(read_resp(&mut c).unwrap(), Resp::Error("ERR boom".into()));
        assert_eq!(read_resp(&mut c).unwrap(), Resp::Int(42));
        assert_eq!(
            read_resp(&mut c).unwrap(),
            Resp::Bulk(Some(b"he\r\nl".to_vec()))
        );
        assert_eq!(read_resp(&mut c).unwrap(), Resp::Bulk(None));
        assert_eq!(
            read_resp(&mut c).unwrap(),
            Resp::Array(Some(vec![Resp::Bulk(Some(b"a".to_vec())), Resp::Int(1)]))
        );
        assert_eq!(read_resp(&mut c).unwrap(), Resp::Array(None));
        assert!(read_resp(&mut c).is_err());
    }

    #[test]
    fn rejects_oversized_and_malformed() {
        assert!(read_resp(&mut Cursor::new(&b"$999999999999\r\n"[..])).is_err());
        assert!(read_resp(&mut Cursor::new(&b"?x\r\n"[..])).is_err());
    }
}
