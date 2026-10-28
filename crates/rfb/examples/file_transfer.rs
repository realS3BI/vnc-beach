//! Round-trip smoke test against a running VNC Beach server.
//! Usage: cargo run -p rfb --example file_transfer -- HOST:PORT PASSWORD REMOTE_PATH
//! Creates REMOTE_PATH (never overwrites); remove the test file afterwards.

use rfb::files::{Request, Response, CHUNK_SIZE};
use rfb::messages::{ClientMessage, ServerMessage};
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

fn receive(reader: &mut BufReader<TcpStream>) -> rfb::Result<Response> {
    loop {
        match ServerMessage::read(reader)? {
            ServerMessage::FileTransfer(response) => return Ok(response),
            ServerMessage::Bell
            | ServerMessage::ServerCutText(_)
            | ServerMessage::SetColourMapEntries
            | ServerMessage::Camera(_) => {}
            ServerMessage::FramebufferUpdate { .. } => {
                return Err(rfb::Error::Protocol("unexpected framebuffer update".into()))
            }
        }
    }
}

fn request(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: Request,
) -> rfb::Result<Response> {
    request.write(writer)?;
    writer.flush()?;
    receive(reader)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: file_transfer HOST:PORT PASSWORD REMOTE_PATH".into());
    }
    let mut writer = TcpStream::connect(&args[1])?;
    writer.set_nodelay(true)?;
    writer.set_read_timeout(Some(Duration::from_secs(10)))?;
    rfb::handshake::client_handshake(&mut writer, Some(&args[2]))?;
    ClientMessage::SetEncodings(vec![rfb::encoding::BEACH_FILE_TRANSFER]).write(&mut writer)?;
    writer.flush()?;
    let mut reader = BufReader::new(writer.try_clone()?);
    let hello = receive(&mut reader)?;
    assert!(
        matches!(hello, Response::Hello { .. }),
        "missing negotiation: {hello:?}"
    );
    let data: Vec<u8> = (0..CHUNK_SIZE * 17 + 193).map(|n| (n % 251) as u8).collect();
    assert_eq!(
        request(
            &mut writer,
            &mut reader,
            Request::WriteStart {
                path: args[3].clone(),
                size: data.len() as u64
            }
        )?,
        Response::Ok
    );
    for chunk in data.chunks(CHUNK_SIZE) {
        assert_eq!(
            request(&mut writer, &mut reader, Request::WriteChunk(chunk.to_vec()))?,
            Response::Ok
        );
    }
    assert_eq!(
        request(&mut writer, &mut reader, Request::WriteFinish)?,
        Response::Ok
    );
    assert_eq!(
        request(&mut writer, &mut reader, Request::ReadStart(args[3].clone()))?,
        Response::Started {
            size: data.len() as u64
        }
    );
    let mut actual = Vec::new();
    while actual.len() < data.len() {
        let Response::Data(chunk) = request(&mut writer, &mut reader, Request::ReadChunk)? else {
            return Err("missing download data".into());
        };
        if chunk.is_empty() {
            return Err("truncated download".into());
        }
        actual.extend(chunk);
    }
    assert_eq!(actual, data, "download differs from upload");
    assert_eq!(request(&mut writer, &mut reader, Request::Cancel)?, Response::Ok);
    assert_eq!(
        request(
            &mut writer,
            &mut reader,
            Request::WriteStart {
                path: args[3].clone(),
                size: 0
            }
        )?,
        Response::Failed(rfb::files::Failure::Exists)
    );
    println!(
        "Verified {} bytes in both directions; existing file protected: {}",
        data.len(),
        args[3]
    );
    Ok(())
}
