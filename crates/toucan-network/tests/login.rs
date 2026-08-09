//! Headless Login and Configuration protocol integration test.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};
use toucan_auth::authenticate_offline;
use toucan_config::Config;
use toucan_network::{ServerControl, ServerMetrics, ToucanServer};
use toucan_player::PlayerStore;
use toucan_protocol::packet_id::{configuration, login, play};
use toucan_protocol::{
    BlockPosition, FrameDecoder, PacketReader, PacketWriter, decode_compressed_packet,
    encode_compressed_packet, encode_packet,
};
use toucan_world::{BlockPosition as WorldBlockPosition, BlockStateId, GeneratorKind, World};
use uuid::Uuid;

const MAX_PACKET_SIZE: usize = 2_097_152;
const COMPRESSION_THRESHOLD: usize = 64;

fn test_config(world: &Path) -> Result<Config, Box<dyn Error>> {
    let source = include_str!("../../../config/toucan.toml")
        .replace("address = \"0.0.0.0\"", "address = \"127.0.0.1\"")
        .replace("port = 25565", "port = 0")
        .replace("view_distance = 8", "view_distance = 2")
        .replace("compression_threshold = 256", "compression_threshold = 64")
        .replace(
            "world_generator = \"terrain\"",
            "world_generator = \"flat\"",
        )
        .replace(
            "world = \"world\"",
            &format!("world = \"{}\"", world.display()),
        );
    Ok(Config::parse(&source)?)
}

async fn start_server() -> Result<
    (
        std::net::SocketAddr,
        Arc<ServerMetrics>,
        ServerControl,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), toucan_network::ServerError>>,
        PathBuf,
    ),
    Box<dyn Error>,
> {
    let world = std::env::temp_dir().join(format!("toucan-login-world-{}", std::process::id()));
    if world.exists() {
        std::fs::remove_dir_all(&world)?;
    }
    let server = ToucanServer::bind(Arc::new(test_config(&world)?)).await?;
    let address = server.local_addr()?;
    let metrics = server.metrics();
    let control = server.control_handle();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(server.serve_until(async move {
        let _ = shutdown_rx.await;
    }));
    Ok((address, metrics, control, shutdown_tx, task, world))
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
        for section in 0..24 {
            let non_air = sections.read_i16()?;
            assert_eq!(sections.read_i16()?, 0);
            let bits = sections.read_u8()?;
            assert_eq!(bits, 0);
            if section < 8 {
                assert_eq!(non_air, 4096);
                assert_eq!(sections.read_var_i32()?, 1);
            } else {
                assert_eq!(non_air, 0);
                assert_eq!(sections.read_var_i32()?, 0);
            }
            assert_eq!(sections.read_u8()?, 0);
            assert_eq!(sections.read_var_i32()?, 0);
        }
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
async fn offline_login_streams_generated_flat_world() -> Result<(), Box<dyn Error>> {
    let (address, metrics, control, shutdown, server_task, world) = start_server().await?;
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
            let batch_size = reader.read_var_i32()?;
            assert!((1..=4).contains(&batch_size));
            reader.finish()?;
            assert_eq!(batch_size, if chunks == 25 { 1 } else { 4 });
            let mut acknowledgement = PacketWriter::new();
            acknowledgement.write_f32(4.0);
            client
                .write_compressed(
                    play::serverbound::CHUNK_BATCH_RECEIVED,
                    &acknowledgement.into_bytes(),
                )
                .await?;
            if chunks == 25 {
                break;
            }
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

    let mut movement = PacketWriter::new();
    movement.write_f64(48.5);
    movement.write_f64(64.0);
    movement.write_f64(0.5);
    movement.write_f32(90.0);
    movement.write_f32(12.5);
    movement.write_u8(1);
    client
        .write_compressed(
            play::serverbound::MOVE_PLAYER_POS_ROT,
            &movement.into_bytes(),
        )
        .await?;

    let mut moved_chunks = 0;
    let mut unloaded_chunks = 0;
    let mut saw_center = false;
    let mut finished_moved_chunks = false;
    while !finished_moved_chunks {
        let packet = client.read_compressed().await?;
        let mut reader = PacketReader::new(&packet);
        let packet_id = reader.read_var_i32()?;
        match packet_id {
            play::clientbound::SET_CHUNK_CACHE_CENTER => {
                assert_eq!(reader.read_var_i32()?, 3);
                assert_eq!(reader.read_var_i32()?, 0);
                reader.finish()?;
                saw_center = true;
            }
            play::clientbound::FORGET_LEVEL_CHUNK => {
                let _ = reader.read_i64()?;
                reader.finish()?;
                unloaded_chunks += 1;
            }
            play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
                validate_flat_chunk(&mut reader)?;
                moved_chunks += 1;
            }
            play::clientbound::CHUNK_BATCH_FINISHED => {
                let batch_size = reader.read_var_i32()?;
                assert!((1..=4).contains(&batch_size));
                reader.finish()?;
                let mut acknowledgement = PacketWriter::new();
                acknowledgement.write_f32(4.0);
                client
                    .write_compressed(
                        play::serverbound::CHUNK_BATCH_RECEIVED,
                        &acknowledgement.into_bytes(),
                    )
                    .await?;
                finished_moved_chunks = moved_chunks == 15;
            }
            play::clientbound::CHUNK_BATCH_START => reader.finish()?,
            _ => return Err(format!("unexpected Play packet 0x{packet_id:02x}").into()),
        }
    }
    assert!(saw_center);
    assert_eq!(unloaded_chunks, 15);
    client
        .write_compressed(play::serverbound::PLAYER_LOADED, &[])
        .await?;

    let broken_position = BlockPosition { x: 48, y: 63, z: 0 };
    let mut break_block = PacketWriter::new();
    break_block.write_var_i32(0);
    break_block.write_block_position(broken_position);
    break_block.write_u8(1);
    break_block.write_var_i32(23);
    client
        .write_compressed(play::serverbound::PLAYER_ACTION, &break_block.into_bytes())
        .await?;

    let update = client.read_compressed().await?;
    let mut reader = PacketReader::new(&update);
    assert_eq!(reader.read_var_i32()?, play::clientbound::BLOCK_UPDATE);
    assert_eq!(reader.read_block_position()?, broken_position);
    assert_eq!(reader.read_var_i32()?, 0);
    reader.finish()?;

    let acknowledgement = client.read_compressed().await?;
    let mut reader = PacketReader::new(&acknowledgement);
    assert_eq!(reader.read_var_i32()?, play::clientbound::BLOCK_CHANGED_ACK);
    assert_eq!(reader.read_var_i32()?, 23);
    reader.finish()?;

    timeout(Duration::from_secs(2), async {
        while metrics.snapshot().packets_received < 9 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let packets_before_miscellaneous = metrics.snapshot().packets_received;
    client
        .write_compressed(
            play::serverbound::CLIENT_INFORMATION,
            &client_information_payload()?,
        )
        .await?;
    client
        .write_compressed(play::serverbound::CUSTOM_PAYLOAD, &brand_payload()?)
        .await?;
    let mut selected_slot = PacketWriter::new();
    selected_slot.write_i16(7);
    client
        .write_compressed(
            play::serverbound::SET_CARRIED_ITEM,
            &selected_slot.into_bytes(),
        )
        .await?;
    client.write_compressed(0x44, &[]).await?;
    client
        .write_compressed(play::serverbound::CLIENT_TICK_END, &[])
        .await?;

    timeout(Duration::from_secs(2), async {
        while metrics.snapshot().packets_received < packets_before_miscellaneous + 5 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(metrics.snapshot().online_players, 1);
    let save = control.save().await?;
    assert_eq!(save.chunks, 1);
    assert_eq!(save.players, 1);

    let _ = shutdown.send(());
    server_task.await??;
    assert_eq!(metrics.snapshot().online_players, 0);

    let player_store = PlayerStore::new(world.join("playerdata"));
    let player_data = player_store
        .load(authenticate_offline("ToucanTest")?.uuid)?
        .ok_or("player data was not saved during shutdown")?;
    assert_eq!(player_data.position(), [48.5, 64.0, 0.5]);
    assert_eq!(player_data.rotation(), [90.0, 12.5]);
    assert_eq!(player_data.selected_hotbar(), 7);

    let reopened = World::open_or_create(&world, 4096, GeneratorKind::Flat, 0)?;
    assert_eq!(
        reopened.block(WorldBlockPosition {
            x: broken_position.x,
            y: broken_position.y,
            z: broken_position.z,
        })?,
        BlockStateId::AIR
    );
    std::fs::remove_dir_all(world)?;
    Ok(())
}
