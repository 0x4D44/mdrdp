//! Portable input-stream record boundaries and partial-body deadline.

use crate::input_proto;
use std::io::{self, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) enum ReadRecord {
    Complete(usize),
    Eof,
    UnknownKind(u8),
}

pub(crate) fn read_record(
    stream: &mut TcpStream,
    buf: &mut [u8; input_proto::MAX_RECORD_LEN],
    body_timeout: Duration,
) -> io::Result<ReadRecord> {
    match stream.read_exact(&mut buf[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Ok(ReadRecord::Eof);
        }
        Err(error) => return Err(error),
    }
    let kind = buf[0];
    let Some(len) = input_proto::kind_len(kind) else {
        return Ok(ReadRecord::UnknownKind(kind));
    };
    if len > 1 {
        let deadline = Instant::now() + body_timeout;
        let body = (|| {
            let mut offset = 1;
            while offset < len {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "input record body deadline elapsed",
                    ));
                }
                // A zero socket timeout means "block forever" on Windows. Round
                // the final fraction up; the next lap still checks the real deadline.
                stream.set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
                match stream.read(&mut buf[offset..len]) {
                    Ok(0) => return Ok(ReadRecord::Eof),
                    Ok(read) => offset += read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(ReadRecord::Complete(len))
        })();
        let clear = stream.set_read_timeout(None);
        match body {
            Ok(record) => {
                clear?;
                return Ok(record);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(ReadRecord::Complete(len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_proto::{self, InputRecord, KeyKind};
    use std::io::Write;
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread;
    use std::time::{Duration, Instant};

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn a_partial_record_body_times_out() {
        let (mut client, mut server) = pair();
        client.write_all(&[KeyKind::Down as u8]).unwrap();
        let mut buf = [0; input_proto::MAX_RECORD_LEN];
        let started = Instant::now();
        let error = read_record(&mut server, &mut buf, Duration::from_millis(30)).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_byte_dribble_cannot_extend_the_body_deadline() {
        let (mut client, mut server) = pair();
        client.write_all(&[KeyKind::Down as u8]).unwrap();
        let writer = thread::spawn(move || {
            for byte in [0, 0x41, 0, 1, 0, 0, 0] {
                thread::sleep(Duration::from_millis(20));
                if client.write_all(&[byte]).is_err() {
                    break;
                }
            }
        });
        let mut buf = [0; input_proto::MAX_RECORD_LEN];
        let started = Instant::now();
        let error = read_record(&mut server, &mut buf, Duration::from_millis(30)).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(started.elapsed() < Duration::from_millis(100));
        writer.join().unwrap();
    }

    #[test]
    fn idle_between_complete_records_has_no_deadline() {
        let (mut client, mut server) = pair();
        let first = input_proto::encode(InputRecord {
            kind: KeyKind::Down,
            vk: 0x41,
            seq: 1,
        });
        client.write_all(&first).unwrap();
        let mut buf = [0; input_proto::MAX_RECORD_LEN];
        assert!(matches!(
            read_record(&mut server, &mut buf, Duration::from_millis(30)).unwrap(),
            ReadRecord::Complete(8)
        ));

        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            let second = input_proto::encode(InputRecord {
                kind: KeyKind::Up,
                vk: 0x41,
                seq: 2,
            });
            client.write_all(&second).unwrap();
        });
        assert!(matches!(
            read_record(&mut server, &mut buf, Duration::from_millis(30)).unwrap(),
            ReadRecord::Complete(8)
        ));
        writer.join().unwrap();
    }

    #[test]
    fn an_unknown_kind_is_returned_to_the_connection_logger() {
        let (mut client, mut server) = pair();
        client.write_all(&[0xFE]).unwrap();
        let mut buf = [0; input_proto::MAX_RECORD_LEN];
        assert!(matches!(
            read_record(&mut server, &mut buf, Duration::from_millis(30)).unwrap(),
            ReadRecord::UnknownKind(0xFE)
        ));
    }
}
