//! Headless Login and Configuration protocol integration test.

use std::error::Error;
use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};
use toucan_auth::authenticate_offline;
use toucan_config::Config;
use toucan_network::{ServerMetrics, ToucanServer};
use toucan_protocol::packet_id::{configuration, login, play};
use toucan_protocol::{
    FrameDecoder, PacketReader, PacketWriter, decode_compressed_packet, encode_compressed_packet,
    encode_packet,
};
use uuid::Uuid;

const MAX_PACKET_SIZE: usize = 2_097_152;
const COMPRESSION_THRESHOLD: usize = 64;

fn test_config() -> Result<Config, Box<dyn Error>> {
    let source = include_str!("../../../config/toucan.toml")
        .replace("address = \"0.0.0.0\"", "address = \"127.0.0.1\"")
        .replace("port = 25565", "port = 0")
        .replace("compression_threshold = 256", "compression_threshold = 64");
    Ok(Config::parse(&source)?)
}

async fn start_server() -> Result<
    (
        std::net::SocketAddr,
        Arc<ServerMetrics>,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), toucan_network::ServerError>>,
    ),
    Box<dyn Error>,
> {
    let server = ToucanServer::bind(Arc::new(test_config()?)).await?;
    let address = server.local_addr()?;
    let metrics = server.metrics();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(server.serve_until(async move {
        let _ = shutdown_rx.await;
    }));
    Ok((address, metrics, shutdown_tx, task))
}

struct TestClient {
    stream: TcpStream,
    decoder: FrameDecoder,
}

impl TestClient {
    async fn connect(address: std::net::SocketAddr) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            stream: TcpStream::connect(address).await?,
            decoder: FrameDecoder::new(MAX_PACKET_SIZE + 1024),
        })
    }

    async fn write_uncompressed(
        &mut self,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        self.stream
            .write_all(&encode_packet(packet_id, payload)?)
            .await?;
        Ok(())
    }

    async fn write_compressed(
        &mut self,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        self.stream
            .write_all(&encode_compressed_packet(
                packet_id,
                payload,
                COMPRESSION_THRESHOLD,
                MAX_PACKET_SIZE,
            )?)
            .await?;
        Ok(())
    }

    async fn read_outer_frame(&mut self) -> Result<Bytes, Box<dyn Error>> {
        loop {
            if let Some(frame) = self.decoder.try_next()? {
                return Ok(frame);
            }
            let mut buffer = [0_u8; 8192];
            let read = self.stream.read(&mut buffer).await?;
            if read == 0 {
                return Err("connection closed before a complete frame".into());
            }
            self.decoder.push(&buffer[..read])?;
        }
    }

    async fn read_compressed(&mut self) -> Result<Bytes, Box<dyn Error>> {
        let frame = self.read_outer_frame().await?;
        Ok(decode_compressed_packet(
            &frame,
            COMPRESSION_THRESHOLD,
            MAX_PACKET_SIZE,
        )?)
    }
}

fn handshake_payload(address: &str) -> Result<Bytes, Box<dyn Error>> {
    let mut payload = PacketWriter::new();
    payload.write_var_i32(775);
    payload.write_string(address)?;
    payload.write_u16(25_565);
    payload.write_var_i32(2);
    Ok(payload.into_bytes())
}

fn login_start_payload(username: &str, profile_id: Uuid) -> Result<Bytes, Box<dyn Error>> {
    let mut payload = PacketWriter::new();
    payload.write_string(username)?;
    payload.write_uuid(profile_id);
    Ok(payload.into_bytes())
}

fn client_information_payload() -> Result<Bytes, Box<dyn Error>> {
    let mut payload = PacketWriter::new();
    payload.write_string("en_us")?;
    payload.write_u8(12);
    payload.write_var_i32(0);
    payload.write_bool(true);
    payload.write_u8(0x7f);
    payload.write_var_i32(1);
    payload.write_bool(false);
    payload.write_bool(true);
    payload.write_var_i32(0);
    Ok(payload.into_bytes())
}

fn brand_payload() -> Result<Bytes, Box<dyn Error>> {
    let mut payload = PacketWriter::new();
    payload.write_string("minecraft:brand")?;
    payload.write_string("toucan-headless-test")?;
    Ok(payload.into_bytes())
}

fn validate_flat_chunk(reader: &mut PacketReader<'_>) -> Result<(), Box<dyn Error>> {
    let _chunk_x = reader.read_i32()?;
    let _chunk_z = reader.read_i32()?;
    assert_eq!(reader.read_count("heightmaps", 16)?, 2);
    for expected_type in [1, 4] {
        assert_eq!(reader.read_var_i32()?, expected_type);
        assert_eq!(reader.read_count("heightmap longs", 64)?, 37);
        for _ in 0..37 {
            let _ = reader.read_i64()?;
        }
    }
    let section_bytes = reader.read_byte_array(MAX_PACKET_SIZE)?;
    {
        let mut sections = PacketReader::new(section_bytes);
        let mut ground_sections = 0;
        for _ in 0..24 {
            let non_air = sections.read_i16()?;
            assert_eq!(sections.read_i16()?, 0);
            let bits = sections.read_u8()?;
            if bits == 0 {
                assert_eq!(non_air, 0);
                assert_eq!(sections.read_var_i32()?, 0);
            } else {
                ground_sections += 1;
                assert_eq!(non_air, 256);
                assert_eq!(bits, 4);
                assert_eq!(sections.read_count("block palette", 256)?, 2);
                assert_eq!(sections.read_var_i32()?, 0);
                assert_eq!(sections.read_var_i32()?, 1);
                for _ in 0..256 {
                    let _ = sections.read_i64()?;
                }
            }
            assert_eq!(sections.read_u8()?, 0);
            assert_eq!(sections.read_var_i32()?, 0);
        }
        assert_eq!(ground_sections, 1);
        sections.finish()?;
    }
    assert_eq!(reader.read_count("block entities", 1024)?, 0);
    for expected_longs in [1, 0, 0, 1] {
        assert_eq!(reader.read_count("light mask longs", 4)?, expected_longs);
        for _ in 0..expected_longs {
            let _ = reader.read_i64()?;
        }
    }
    assert_eq!(reader.read_count("sky light arrays", 26)?, 26);
    for _ in 0..26 {
        assert_eq!(reader.read_byte_array(2048)?.len(), 2048);
    }
    assert_eq!(reader.read_count("block light arrays", 26)?, 0);
    reader.finish()?;
    Ok(())
}

#[tokio::test]
async fn offline_login_joins_generated_flat_world() -> Result<(), Box<dyn Error>> {
    let (address, metrics, shutdown, server_task) = start_server().await?;
    let mut client = TestClient::connect(address).await?;
    client
        .write_uncompressed(0, &handshake_payload("localhost")?)
        .await?;
    client
        .write_uncompressed(
            login::serverbound::HELLO,
            &login_start_payload("ToucanTest", Uuid::nil())?,
        )
        .await?;

    let compression = client.read_outer_frame().await?;
    let mut reader = PacketReader::new(&compression);
    assert_eq!(reader.read_var_i32()?, login::clientbound::COMPRESSION);
    assert_eq!(reader.read_var_i32()?, COMPRESSION_THRESHOLD as i32);
    reader.finish()?;

    let login_success = client.read_compressed().await?;
    let mut reader = PacketReader::new(&login_success);
    assert_eq!(reader.read_var_i32()?, login::clientbound::FINISHED);
    assert_eq!(
        reader.read_uuid()?,
        authenticate_offline("ToucanTest")?.uuid
    );
    assert_eq!(reader.read_string(16, 48)?, "ToucanTest");
    assert_eq!(reader.read_count("profile properties", 16)?, 0);
    reader.finish()?;
    client
        .write_compressed(login::serverbound::ACKNOWLEDGED, &[])
        .await?;

    let features = client.read_compressed().await?;
    let mut reader = PacketReader::new(&features);
    assert_eq!(
        reader.read_var_i32()?,
        configuration::clientbound::ENABLED_FEATURES
    );
    assert_eq!(reader.read_count("features", 16)?, 1);
    assert_eq!(reader.read_identifier(64)?, "minecraft:vanilla");
    reader.finish()?;

    let known_packs = client.read_compressed().await?;
    let mut reader = PacketReader::new(&known_packs);
    assert_eq!(
        reader.read_var_i32()?,
        configuration::clientbound::SELECT_KNOWN_PACKS
    );
    assert_eq!(reader.read_count("known packs", 64)?, 0);
    reader.finish()?;

    client
        .write_compressed(
            configuration::serverbound::CLIENT_INFORMATION,
            &client_information_payload()?,
        )
        .await?;
    client
        .write_compressed(
            configuration::serverbound::CUSTOM_PAYLOAD,
            &brand_payload()?,
        )
        .await?;
    let mut empty_packs = PacketWriter::new();
    empty_packs.write_var_i32(0);
    client
        .write_compressed(
            configuration::serverbound::SELECT_KNOWN_PACKS,
            &empty_packs.into_bytes(),
        )
        .await?;

    for index in 0..29 {
        let packet = client.read_compressed().await?;
        let mut reader = PacketReader::new(&packet);
        let packet_id = reader.read_var_i32()?;
        if index < 28 {
            assert_eq!(packet_id, configuration::clientbound::REGISTRY_DATA);
        } else {
            assert_eq!(packet_id, configuration::clientbound::UPDATE_TAGS);
        }
    }
    let finish = client.read_compressed().await?;
    let mut reader = PacketReader::new(&finish);
    assert_eq!(reader.read_var_i32()?, configuration::clientbound::FINISH);
    reader.finish()?;
    client
        .write_compressed(configuration::serverbound::FINISH, &[])
        .await?;

    let mut play_packets = 0;
    let mut chunks = 0;
    loop {
        let packet = client.read_compressed().await?;
        let mut reader = PacketReader::new(&packet);
        let packet_id = reader.read_var_i32()?;
        if play_packets == 0 {
            assert_eq!(packet_id, play::clientbound::LOGIN);
        }
        let acknowledge_teleport = if packet_id == play::clientbound::PLAYER_POSITION {
            assert_eq!(reader.read_var_i32()?, 1);
            true
        } else {
            false
        };
        if packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
            validate_flat_chunk(&mut reader)?;
            chunks += 1;
        }
        if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
            assert_eq!(reader.read_var_i32()?, 25);
            reader.finish()?;
            break;
        }
        if acknowledge_teleport {
            let mut acknowledgement = PacketWriter::new();
            acknowledgement.write_var_i32(1);
            client
                .write_compressed(
                    play::serverbound::ACCEPT_TELEPORTATION,
                    &acknowledgement.into_bytes(),
                )
                .await?;
        }
        play_packets += 1;
    }
    assert_eq!(chunks, 25);
    client
        .write_compressed(play::serverbound::PLAYER_LOADED, &[])
        .await?;

    timeout(Duration::from_secs(2), async {
        while metrics.snapshot().packets_received < 9 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    client
        .write_compressed(
            play::serverbound::CLIENT_INFORMATION,
            &client_information_payload()?,
        )
        .await?;
    client
        .write_compressed(play::serverbound::CUSTOM_PAYLOAD, &brand_payload()?)
        .await?;
    client.write_compressed(0x44, &[]).await?;
    client
        .write_compressed(play::serverbound::CLIENT_TICK_END, &[])
        .await?;

    timeout(Duration::from_secs(2), async {
        while metrics.snapshot().packets_received < 13 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(metrics.snapshot().online_players, 1);

    let _ = shutdown.send(());
    server_task.await??;
    assert_eq!(metrics.snapshot().online_players, 0);
    Ok(())
}
