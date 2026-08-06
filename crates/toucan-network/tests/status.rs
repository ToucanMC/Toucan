//! Headless status-protocol integration tests.

use std::error::Error;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};
use toucan_config::Config;
use toucan_network::StatusServer;
use toucan_protocol::{FrameDecoder, PacketReader, PacketWriter, encode_packet};

fn test_config() -> Result<Config, Box<dyn Error>> {
    let source = include_str!("../../../config/toucan.toml")
        .replace("address = \"0.0.0.0\"", "address = \"127.0.0.1\"")
        .replace("port = 25565", "port = 0")
        .replace(
            "motd = \"A Toucan Server\"",
            "motd = \"Toucan test server\"",
        );
    Ok(Config::parse(&source)?)
}

async fn start_server() -> Result<
    (
        std::net::SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), toucan_network::ServerError>>,
    ),
    Box<dyn Error>,
> {
    let server = StatusServer::bind(Arc::new(test_config()?)).await?;
    let address = server.local_addr()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(server.serve_until(async move {
        let _ = shutdown_rx.await;
    }));
    Ok((address, shutdown_tx, task))
}

fn handshake_packet(address: &str) -> Result<Bytes, Box<dyn Error>> {
    let mut payload = PacketWriter::new();
    payload.write_var_i32(775);
    payload.write_string(address)?;
    payload.write_u16(25_565);
    payload.write_var_i32(1);
    Ok(encode_packet(0, &payload.into_bytes())?)
}

async fn read_frame(stream: &mut TcpStream) -> Result<Bytes, Box<dyn Error>> {
    let mut decoder = FrameDecoder::new(2_097_152);
    loop {
        if let Some(frame) = decoder.try_next()? {
            return Ok(frame);
        }
        let mut buffer = [0_u8; 1024];
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err("connection closed before a complete frame".into());
        }
        decoder.push(&buffer[..read])?;
    }
}

#[tokio::test]
async fn status_handshake_request_and_ping_round_trip() -> Result<(), Box<dyn Error>> {
    let (address, shutdown, server_task) = start_server().await?;
    let mut stream = TcpStream::connect(address).await?;
    stream.write_all(&handshake_packet("localhost")?).await?;
    stream.write_all(&encode_packet(0, &[])?).await?;

    let response = read_frame(&mut stream).await?;
    let mut reader = PacketReader::new(&response);
    assert_eq!(reader.read_var_i32()?, 0);
    let json: Value = serde_json::from_str(&reader.read_string(32_767, 131_068)?)?;
    reader.finish()?;
    assert_eq!(json["version"]["name"], "26.1.2");
    assert_eq!(json["version"]["protocol"], 775);
    assert_eq!(json["description"]["text"], "Toucan test server");
    assert_eq!(json["players"]["online"], 0);
    assert_eq!(json["players"]["max"], 20);

    let ping_payload = 0x0102_0304_0506_0708_i64;
    let mut ping = PacketWriter::new();
    ping.write_i64(ping_payload);
    stream
        .write_all(&encode_packet(1, &ping.into_bytes())?)
        .await?;
    let pong = read_frame(&mut stream).await?;
    let mut reader = PacketReader::new(&pong);
    assert_eq!(reader.read_var_i32()?, 1);
    assert_eq!(reader.read_i64()?, ping_payload);
    reader.finish()?;

    let _ = shutdown.send(());
    server_task.await??;
    Ok(())
}

#[tokio::test]
async fn malformed_connection_is_isolated_from_listener() -> Result<(), Box<dyn Error>> {
    let (address, shutdown, server_task) = start_server().await?;
    let mut malformed = TcpStream::connect(address).await?;
    malformed.write_all(&[0xff, 0xff, 0xff, 0xff, 0x10]).await?;
    let mut byte = [0_u8; 1];
    let closed = timeout(Duration::from_secs(1), malformed.read(&mut byte)).await??;
    assert_eq!(closed, 0);

    let mut healthy = TcpStream::connect(address).await?;
    healthy.write_all(&handshake_packet("localhost")?).await?;
    healthy.write_all(&encode_packet(0, &[])?).await?;
    let response = read_frame(&mut healthy).await?;
    let mut reader = PacketReader::new(&response);
    assert_eq!(reader.read_var_i32()?, 0);

    let _ = shutdown.send(());
    server_task.await??;
    Ok(())
}

#[tokio::test]
async fn wrong_status_packet_is_disconnected() -> Result<(), Box<dyn Error>> {
    let (address, shutdown, server_task) = start_server().await?;
    let mut stream = TcpStream::connect(address).await?;
    stream.write_all(&handshake_packet("localhost")?).await?;
    stream.write_all(&encode_packet(2, &[])?).await?;

    let mut byte = [0_u8; 1];
    let closed = timeout(Duration::from_secs(1), stream.read(&mut byte)).await??;
    assert_eq!(closed, 0);

    let _ = shutdown.send(());
    server_task.await??;
    Ok(())
}
