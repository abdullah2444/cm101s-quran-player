//! Called by the patched AirPlay receiver before opening the ALSA device.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let addr: SocketAddr = "127.0.0.1:8790".parse().unwrap();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(8)) else {
        return ExitCode::FAILURE;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(8)));

    if stream
        .write_all(b"GET /airplay?act=claim_audio HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .is_err()
    {
        return ExitCode::FAILURE;
    }

    let mut body = String::new();
    if stream.read_to_string(&mut body).is_err() {
        return ExitCode::FAILURE;
    }

    if body.contains("\"claimed\": true") || body.contains("\"claimed\":true") {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
