//! Bounded asynchronous networking and connection-state ownership.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval, timeout, timeout_at};
use toucan_auth::{AuthError, PlayerIdentity, authenticate_offline};
use toucan_config::{Config, ConfigError, GameMode};
use toucan_protocol::packet_id::{configuration, login, play};
use toucan_protocol::{
    ConnectionState, FrameDecoder, MINECRAFT_VERSION, PROTOCOL_VERSION, PacketReader, PacketWriter,
    ProtocolError, decode_client_information, decode_compressed_packet, decode_custom_payload,
    decode_handshake, decode_known_packs, decode_login_start, encode_chunk_batch_finished,
    encode_common_disconnect, encode_compressed_packet, encode_empty_known_packs,
    encode_enabled_features, encode_flat_chunk, encode_game_event, encode_initial_player_position,
    encode_keep_alive, encode_login_disconnect, encode_login_success, encode_packet,
    encode_play_login, encode_player_abilities, encode_spawn_position, encode_view_center,
    encode_view_distance,
};
use toucan_registry::{RegistryError, configuration_packets};
use tracing::{debug, info, warn};

/// Listener or coordinated server failure.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Configuration was invalid when resolving the bind address.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The embedded vanilla registry fixture was invalid.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Listener I/O failed.
    #[error("server I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
enum ConnectionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("timed out waiting for a packet")]
    PacketTimeout,
    #[error("online-mode authentication is not implemented; set server.online_mode = false")]
    OnlineModeUnavailable,
    #[error("client selected {0} known packs after Toucan requested none")]
    UnexpectedKnownPacks(usize),
    #[error("connection closed before the required packet arrived")]
    UnexpectedEof,
    #[error("client confirmed teleport {received}; expected {expected}")]
    InvalidTeleport { expected: i32, received: i32 },
    #[error("client answered keep-alive {received}; expected {expected}")]
    InvalidKeepAlive { expected: i64, received: i64 },
    #[error("player movement contained non-finite or out-of-bounds values")]
    InvalidMovement,
}

/// Lock-free counters suitable for logging and future metrics exporters.
#[derive(Debug, Default)]
pub struct ServerMetrics {
    active_connections: AtomicUsize,
    online_players: AtomicUsize,
    accepted_connections: AtomicU64,
    packets_received: AtomicU64,
    packets_sent: AtomicU64,
    bytes_received: AtomicU64,
    bytes_sent: AtomicU64,
    rejected_connections: AtomicU64,
}

impl ServerMetrics {
    /// Takes a point-in-time copy of all network counters.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            active_connections: self.active_connections.load(Ordering::Relaxed),
            online_players: self.online_players.load(Ordering::Relaxed),
            accepted_connections: self.accepted_connections.load(Ordering::Relaxed),
            packets_received: self.packets_received.load(Ordering::Relaxed),
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            rejected_connections: self.rejected_connections.load(Ordering::Relaxed),
        }
    }
}

/// Immutable point-in-time network metrics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetricsSnapshot {
    /// Connections currently serviced, including status clients.
    pub active_connections: usize,
    /// Players that completed Login and Configuration.
    pub online_players: usize,
    /// Connections accepted since startup.
    pub accepted_connections: u64,
    /// Complete packets received since startup.
    pub packets_received: u64,
    /// Complete packets sent since startup.
    pub packets_sent: u64,
    /// TCP payload bytes read since startup.
    pub bytes_received: u64,
    /// TCP payload bytes written since startup.
    pub bytes_sent: u64,
    /// Connections rejected by the concurrency bound.
    pub rejected_connections: u64,
}

/// A bound Toucan server ready to enter its accept loop.
pub struct ToucanServer {
    listener: TcpListener,
    config: Arc<Config>,
    metrics: Arc<ServerMetrics>,
}

impl ToucanServer {
    /// Validates embedded data and binds the configured listener.
    pub async fn bind(config: Arc<Config>) -> Result<Self, ServerError> {
        configuration_packets()?;
        let listener = TcpListener::bind(config.bind_address()?).await?;
        Ok(Self {
            listener,
            config,
            metrics: Arc::new(ServerMetrics::default()),
        })
    }

    /// Returns the actual listener address, including an assigned ephemeral port.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.listener.local_addr()
    }

    /// Returns the shared metrics registry.
    #[must_use]
    pub fn metrics(&self) -> Arc<ServerMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Serves connections until `shutdown` resolves, then drains connection tasks.
    pub async fn serve_until<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send,
    {
        let max_connections = self.config.network.max_connections;
        let permits = Arc::new(Semaphore::new(max_connections));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut next_connection_id = 1_u64;
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = self.listener.accept() => {
                    let (stream, peer_address) = accepted?;
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        self.metrics.rejected_connections.fetch_add(1, Ordering::Relaxed);
                        debug!(%peer_address, max_connections, "connection rejected at concurrency limit");
                        continue;
                    };

                    let connection_id = next_connection_id;
                    next_connection_id = next_connection_id.wrapping_add(1);
                    let config = Arc::clone(&self.config);
                    let metrics = Arc::clone(&self.metrics);
                    let connection_shutdown = shutdown_rx.clone();
                    metrics.accepted_connections.fetch_add(1, Ordering::Relaxed);
                    metrics.active_connections.fetch_add(1, Ordering::Relaxed);

                    tasks.spawn(async move {
                        let _permit = permit;
                        let result = serve_connection(
                            stream,
                            connection_id,
                            peer_address,
                            &config,
                            &metrics,
                            connection_shutdown,
                        ).await;
                        metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
                        if let Err(error) = result {
                            debug!(connection_id, %peer_address, %error, "connection closed after protocol error");
                        }
                    });
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(error)) = joined {
                        warn!(%error, "connection task terminated unexpectedly");
                    }
                }
            }
        }

        info!(
            active_connections = tasks.len(),
            "graceful shutdown started"
        );
        let _ = shutdown_tx.send(true);
        let drain_deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.config.network.shutdown_timeout_seconds);
        while !tasks.is_empty() {
            match timeout_at(drain_deadline, tasks.join_next()).await {
                Ok(Some(Err(error))) => warn!(%error, "connection task terminated during shutdown"),
                Ok(Some(Ok(()))) => {}
                Ok(None) => break,
                Err(_) => {
                    warn!(
                        remaining_tasks = tasks.len(),
                        "shutdown deadline reached; aborting connection tasks"
                    );
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    break;
                }
            }
        }
        info!("graceful shutdown complete");
        Ok(())
    }
}

/// Compatibility alias retained for Phase 0/1 callers.
pub type StatusServer = ToucanServer;

async fn serve_connection(
    mut stream: TcpStream,
    connection_id: u64,
    peer_address: SocketAddr,
    config: &Config,
    metrics: &ServerMetrics,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ConnectionError> {
    debug!(connection_id, %peer_address, state = "handshake", "connection accepted");
    let mut decoder = FrameDecoder::new(config.network.max_packet_size.saturating_add(1024));
    let packet_timeout = Duration::from_secs(config.network.packet_timeout_seconds);
    let Some(frame) = read_frame(
        &mut stream,
        &mut decoder,
        metrics,
        &mut shutdown,
        Some(packet_timeout),
        None,
        config.network.max_packet_size,
    )
    .await?
    else {
        return Ok(());
    };

    let mut reader = PacketReader::new(&frame);
    let packet_id = reader.read_var_i32()?;
    if packet_id != 0 {
        return Err(unexpected_packet(ConnectionState::Handshake, packet_id));
    }
    let handshake = decode_handshake(&mut reader)?;
    debug!(
        connection_id,
        %peer_address,
        client_protocol = handshake.protocol_version,
        requested_host = %handshake.server_address,
        requested_port = handshake.server_port,
        next_state = handshake.next_state.as_str(),
        "handshake accepted"
    );

    match handshake.next_state {
        ConnectionState::Status => {
            serve_status(
                &mut stream,
                &mut decoder,
                connection_id,
                config,
                metrics,
                &mut shutdown,
                packet_timeout,
            )
            .await
        }
        ConnectionState::Login => {
            if handshake.protocol_version != PROTOCOL_VERSION {
                let error = ProtocolError::UnsupportedProtocol {
                    received: handshake.protocol_version,
                    supported: PROTOCOL_VERSION,
                };
                let reason = encode_login_disconnect("Toucan requires Minecraft 26.1.2")?;
                write_packet(
                    &mut stream,
                    login::clientbound::DISCONNECT,
                    &reason,
                    metrics,
                    &mut shutdown,
                    None,
                    config.network.max_packet_size,
                )
                .await?;
                return Err(error.into());
            }
            serve_login(
                &mut stream,
                &mut decoder,
                connection_id,
                config,
                metrics,
                &mut shutdown,
                packet_timeout,
            )
            .await
        }
        _ => unreachable!("handshake only routes status or login"),
    }
}

async fn serve_status(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection_id: u64,
    config: &Config,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    packet_timeout: Duration,
) -> Result<(), ConnectionError> {
    let request = required_frame(
        stream,
        decoder,
        metrics,
        shutdown,
        Some(packet_timeout),
        None,
        config.network.max_packet_size,
    )
    .await?;
    let mut reader = PacketReader::new(&request);
    let packet_id = reader.read_var_i32()?;
    if packet_id != 0 {
        return Err(unexpected_packet(ConnectionState::Status, packet_id));
    }
    reader.finish()?;

    let response = StatusResponse {
        version: StatusVersion {
            name: MINECRAFT_VERSION,
            protocol: PROTOCOL_VERSION,
        },
        players: StatusPlayers {
            max: config.server.max_players,
            online: metrics.online_players.load(Ordering::Relaxed),
        },
        description: StatusDescription {
            text: &config.server.motd,
        },
        enforces_secure_chat: false,
    };
    let response_json = serde_json::to_string(&response).map_err(|error| {
        ConnectionError::Io(std::io::Error::other(format!(
            "failed to serialize status response: {error}"
        )))
    })?;
    let mut payload = PacketWriter::new();
    payload.write_string(&response_json)?;
    write_packet(
        stream,
        0,
        &payload.into_bytes(),
        metrics,
        shutdown,
        None,
        config.network.max_packet_size,
    )
    .await?;
    debug!(
        connection_id,
        protocol = PROTOCOL_VERSION,
        "status response sent"
    );

    let Some(ping) = read_frame(
        stream,
        decoder,
        metrics,
        shutdown,
        Some(packet_timeout),
        None,
        config.network.max_packet_size,
    )
    .await?
    else {
        return Ok(());
    };
    let mut reader = PacketReader::new(&ping);
    let packet_id = reader.read_var_i32()?;
    if packet_id != 1 {
        return Err(unexpected_packet(ConnectionState::Status, packet_id));
    }
    let payload = reader.read_i64()?;
    reader.finish()?;
    let mut response = PacketWriter::new();
    response.write_i64(payload);
    write_packet(
        stream,
        1,
        &response.into_bytes(),
        metrics,
        shutdown,
        None,
        config.network.max_packet_size,
    )
    .await?;
    debug!(connection_id, "status pong sent");
    Ok(())
}

async fn serve_login(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection_id: u64,
    config: &Config,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    packet_timeout: Duration,
) -> Result<(), ConnectionError> {
    if config.server.online_mode {
        let reason = encode_login_disconnect(
            "Online-mode authentication is not implemented; set server.online_mode to false",
        )?;
        write_packet(
            stream,
            login::clientbound::DISCONNECT,
            &reason,
            metrics,
            shutdown,
            None,
            config.network.max_packet_size,
        )
        .await?;
        return Err(ConnectionError::OnlineModeUnavailable);
    }

    let frame = required_frame(
        stream,
        decoder,
        metrics,
        shutdown,
        Some(packet_timeout),
        None,
        config.network.max_packet_size,
    )
    .await?;
    let mut reader = PacketReader::new(&frame);
    let packet_id = reader.read_var_i32()?;
    if packet_id != login::serverbound::HELLO {
        return Err(unexpected_packet(ConnectionState::Login, packet_id));
    }
    let login_start = decode_login_start(&mut reader)?;
    let identity = match authenticate_offline(&login_start.username) {
        Ok(identity) => identity,
        Err(error) => {
            let reason = encode_login_disconnect("Invalid Minecraft username")?;
            write_packet(
                stream,
                login::clientbound::DISCONNECT,
                &reason,
                metrics,
                shutdown,
                None,
                config.network.max_packet_size,
            )
            .await?;
            return Err(error.into());
        }
    };
    debug!(
        connection_id,
        username = %identity.username,
        uuid = %identity.uuid,
        client_uuid = %login_start.profile_id,
        "offline identity accepted"
    );

    let compression = config.server.compression_threshold.try_into().ok();
    if compression.is_some() {
        let mut payload = PacketWriter::new();
        payload.write_var_i32(config.server.compression_threshold);
        write_packet(
            stream,
            login::clientbound::COMPRESSION,
            &payload.into_bytes(),
            metrics,
            shutdown,
            None,
            config.network.max_packet_size,
        )
        .await?;
    }

    let login_success = encode_login_success(&identity.username, identity.uuid)?;
    write_packet(
        stream,
        login::clientbound::FINISHED,
        &login_success,
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;

    let acknowledged = required_frame(
        stream,
        decoder,
        metrics,
        shutdown,
        Some(packet_timeout),
        compression,
        config.network.max_packet_size,
    )
    .await?;
    let mut reader = PacketReader::new(&acknowledged);
    let packet_id = reader.read_var_i32()?;
    if packet_id != login::serverbound::ACKNOWLEDGED {
        return Err(unexpected_packet(ConnectionState::Login, packet_id));
    }
    reader.finish()?;

    let configuration_result = serve_configuration(
        stream,
        decoder,
        connection_id,
        config,
        metrics,
        shutdown,
        packet_timeout,
        compression,
        &identity,
    )
    .await;
    if configuration_result.is_err()
        && !matches!(&configuration_result, Err(ConnectionError::UnexpectedEof))
    {
        let reason = encode_common_disconnect("Invalid configuration packet")?;
        let _ = write_packet(
            stream,
            configuration::clientbound::DISCONNECT,
            &reason,
            metrics,
            shutdown,
            compression,
            config.network.max_packet_size,
        )
        .await;
    }
    configuration_result?;

    metrics.online_players.fetch_add(1, Ordering::Relaxed);
    info!(
        connection_id,
        username = %identity.username,
        uuid = %identity.uuid,
        state = "play",
        "player completed configuration"
    );
    let result = serve_play(stream, decoder, config, metrics, shutdown, compression).await;
    metrics.online_players.fetch_sub(1, Ordering::Relaxed);
    result
}

#[allow(clippy::too_many_arguments)]
async fn serve_configuration(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection_id: u64,
    config: &Config,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    packet_timeout: Duration,
    compression: Option<usize>,
    identity: &PlayerIdentity,
) -> Result<(), ConnectionError> {
    write_packet(
        stream,
        configuration::clientbound::ENABLED_FEATURES,
        &encode_enabled_features()?,
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;
    write_packet(
        stream,
        configuration::clientbound::SELECT_KNOWN_PACKS,
        &encode_empty_known_packs(),
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;

    loop {
        let frame = required_frame(
            stream,
            decoder,
            metrics,
            shutdown,
            Some(packet_timeout),
            compression,
            config.network.max_packet_size,
        )
        .await?;
        let mut reader = PacketReader::new(&frame);
        let packet_id = reader.read_var_i32()?;
        match packet_id {
            configuration::serverbound::CLIENT_INFORMATION => {
                let information = decode_client_information(&mut reader)?;
                debug!(
                    connection_id,
                    username = %identity.username,
                    locale = %information.locale,
                    view_distance = information.view_distance,
                    "client information received"
                );
            }
            configuration::serverbound::CUSTOM_PAYLOAD => {
                log_custom_payload(connection_id, identity, &mut reader)?;
            }
            configuration::serverbound::SELECT_KNOWN_PACKS => {
                let packs = decode_known_packs(&mut reader)?;
                if !packs.is_empty() {
                    return Err(ConnectionError::UnexpectedKnownPacks(packs.len()));
                }
                break;
            }
            _ => return Err(unexpected_packet(ConnectionState::Configuration, packet_id)),
        }
    }

    for packet in configuration_packets()? {
        write_packet(
            stream,
            packet.id,
            &packet.payload,
            metrics,
            shutdown,
            compression,
            config.network.max_packet_size,
        )
        .await?;
    }
    write_packet(
        stream,
        configuration::clientbound::FINISH,
        &[],
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;

    loop {
        let frame = required_frame(
            stream,
            decoder,
            metrics,
            shutdown,
            Some(packet_timeout),
            compression,
            config.network.max_packet_size,
        )
        .await?;
        let mut reader = PacketReader::new(&frame);
        let packet_id = reader.read_var_i32()?;
        match packet_id {
            configuration::serverbound::CLIENT_INFORMATION => {
                let _ = decode_client_information(&mut reader)?;
            }
            configuration::serverbound::CUSTOM_PAYLOAD => {
                log_custom_payload(connection_id, identity, &mut reader)?;
            }
            configuration::serverbound::FINISH => {
                reader.finish()?;
                break;
            }
            _ => return Err(unexpected_packet(ConnectionState::Configuration, packet_id)),
        }
    }

    Ok(())
}

fn log_custom_payload(
    connection_id: u64,
    identity: &PlayerIdentity,
    reader: &mut PacketReader<'_>,
) -> Result<(), ProtocolError> {
    let payload = decode_custom_payload(reader)?;
    if payload.channel == "minecraft:brand" {
        let mut brand_reader = PacketReader::new(&payload.data);
        let brand = brand_reader.read_string(32_767, 32_767)?;
        brand_reader.finish()?;
        debug!(connection_id, username = %identity.username, %brand, "client brand received");
    } else {
        debug!(connection_id, channel = %payload.channel, bytes = payload.data.len(), "configuration custom payload ignored");
    }
    Ok(())
}

async fn serve_play(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    config: &Config,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    compression: Option<usize>,
) -> Result<(), ConnectionError> {
    send_initial_play(stream, config, metrics, shutdown, compression).await?;

    let mut keep_alive_timer = interval(Duration::from_secs(10));
    keep_alive_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    keep_alive_timer.tick().await;
    let mut next_keep_alive = 1_i64;
    let mut pending_keep_alive = None;

    loop {
        tokio::select! {
            frame = read_frame(
                stream,
                decoder,
                metrics,
                shutdown,
                None,
                compression,
                config.network.max_packet_size,
            ) => {
                let Some(frame) = frame? else {
                    return Ok(());
                };
                handle_play_packet(&frame, &mut pending_keep_alive)?;
            }
            _ = keep_alive_timer.tick() => {
                if pending_keep_alive.is_some() {
                    let reason = encode_common_disconnect("Timed out waiting for keep-alive")?;
                    write_packet(
                        stream,
                        play::clientbound::DISCONNECT,
                        &reason,
                        metrics,
                        shutdown,
                        compression,
                        config.network.max_packet_size,
                    ).await?;
                    return Err(ConnectionError::PacketTimeout);
                }
                let id = next_keep_alive;
                next_keep_alive = next_keep_alive.wrapping_add(1);
                write_packet(
                    stream,
                    play::clientbound::KEEP_ALIVE,
                    &encode_keep_alive(id),
                    metrics,
                    shutdown,
                    compression,
                    config.network.max_packet_size,
                ).await?;
                pending_keep_alive = Some(id);
            }
        }
    }
}

async fn send_initial_play(
    stream: &mut TcpStream,
    config: &Config,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    compression: Option<usize>,
) -> Result<(), ConnectionError> {
    const RADIUS: u8 = 2;
    let game_mode = match config.server.default_gamemode {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 3,
    };
    let packets = [
        (
            play::clientbound::LOGIN,
            encode_play_login(
                1,
                config.server.max_players,
                RADIUS,
                config.server.simulation_distance.min(RADIUS),
                game_mode,
            )?,
        ),
        (
            play::clientbound::PLAYER_ABILITIES,
            encode_player_abilities(game_mode),
        ),
        (
            play::clientbound::SET_CHUNK_CACHE_RADIUS,
            encode_view_distance(RADIUS),
        ),
        (
            play::clientbound::SET_CHUNK_CACHE_CENTER,
            encode_view_center(0, 0),
        ),
        (
            play::clientbound::SET_DEFAULT_SPAWN_POSITION,
            encode_spawn_position(toucan_protocol::BlockPosition { x: 0, y: 65, z: 0 })?,
        ),
        (
            play::clientbound::PLAYER_POSITION,
            encode_initial_player_position(1, 0.5, 65.0, 0.5),
        ),
        (play::clientbound::GAME_EVENT, encode_game_event(13, 0.0)),
        (play::clientbound::CHUNK_BATCH_START, Bytes::new()),
    ];
    for (packet_id, payload) in packets {
        write_packet(
            stream,
            packet_id,
            &payload,
            metrics,
            shutdown,
            compression,
            config.network.max_packet_size,
        )
        .await?;
    }

    let mut chunks = 0;
    for chunk_z in -i32::from(RADIUS)..=i32::from(RADIUS) {
        for chunk_x in -i32::from(RADIUS)..=i32::from(RADIUS) {
            write_packet(
                stream,
                play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
                &encode_flat_chunk(chunk_x, chunk_z),
                metrics,
                shutdown,
                compression,
                config.network.max_packet_size,
            )
            .await?;
            chunks += 1;
        }
    }
    write_packet(
        stream,
        play::clientbound::CHUNK_BATCH_FINISHED,
        &encode_chunk_batch_finished(chunks),
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;
    Ok(())
}

fn handle_play_packet(
    frame: &[u8],
    pending_keep_alive: &mut Option<i64>,
) -> Result<(), ConnectionError> {
    let mut reader = PacketReader::new(frame);
    let packet_id = reader.read_var_i32()?;
    match packet_id {
        play::serverbound::ACCEPT_TELEPORTATION => {
            let received = reader.read_var_i32()?;
            reader.finish()?;
            if received != 1 {
                return Err(ConnectionError::InvalidTeleport {
                    expected: 1,
                    received,
                });
            }
        }
        play::serverbound::CHUNK_BATCH_RECEIVED => {
            let chunks_per_tick = reader.read_f32()?;
            reader.finish()?;
            if !chunks_per_tick.is_finite() || chunks_per_tick < 0.0 {
                return Err(ConnectionError::InvalidMovement);
            }
        }
        play::serverbound::CLIENT_TICK_END | play::serverbound::PLAYER_LOADED => {
            reader.finish()?;
        }
        play::serverbound::CLIENT_INFORMATION => {
            let _ = decode_client_information(&mut reader)?;
        }
        play::serverbound::CUSTOM_PAYLOAD => {
            let _ = decode_custom_payload(&mut reader)?;
        }
        play::serverbound::KEEP_ALIVE => {
            let received = reader.read_i64()?;
            reader.finish()?;
            let expected = pending_keep_alive
                .take()
                .ok_or(ConnectionError::InvalidKeepAlive {
                    expected: -1,
                    received,
                })?;
            if received != expected {
                return Err(ConnectionError::InvalidKeepAlive { expected, received });
            }
        }
        play::serverbound::MOVE_PLAYER_POS => {
            validate_position(&mut reader)?;
            read_movement_flags(&mut reader)?;
        }
        play::serverbound::MOVE_PLAYER_POS_ROT => {
            validate_position(&mut reader)?;
            validate_rotation(&mut reader)?;
            read_movement_flags(&mut reader)?;
        }
        play::serverbound::MOVE_PLAYER_ROT => {
            validate_rotation(&mut reader)?;
            read_movement_flags(&mut reader)?;
        }
        play::serverbound::MOVE_PLAYER_STATUS_ONLY => read_movement_flags(&mut reader)?,
        _ => {
            let bytes = reader.remaining();
            debug!(packet_id, bytes, "unsupported Play packet ignored");
        }
    }
    Ok(())
}

fn validate_position(reader: &mut PacketReader<'_>) -> Result<(), ConnectionError> {
    for value in [reader.read_f64()?, reader.read_f64()?, reader.read_f64()?] {
        if !value.is_finite() || value.abs() > 30_000_000.0 {
            return Err(ConnectionError::InvalidMovement);
        }
    }
    Ok(())
}

fn validate_rotation(reader: &mut PacketReader<'_>) -> Result<(), ConnectionError> {
    for value in [reader.read_f32()?, reader.read_f32()?] {
        if !value.is_finite() {
            return Err(ConnectionError::InvalidMovement);
        }
    }
    Ok(())
}

fn read_movement_flags(reader: &mut PacketReader<'_>) -> Result<(), ConnectionError> {
    let flags = reader.read_u8()?;
    reader.finish()?;
    if flags & !0x03 != 0 {
        return Err(ConnectionError::InvalidMovement);
    }
    Ok(())
}

fn unexpected_packet(state: ConnectionState, packet_id: i32) -> ConnectionError {
    ProtocolError::UnexpectedPacket {
        state: state.as_str(),
        packet_id,
    }
    .into()
}

#[allow(clippy::too_many_arguments)]
async fn required_frame(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    packet_timeout: Option<Duration>,
    compression: Option<usize>,
    max_uncompressed: usize,
) -> Result<Bytes, ConnectionError> {
    read_frame(
        stream,
        decoder,
        metrics,
        shutdown,
        packet_timeout,
        compression,
        max_uncompressed,
    )
    .await?
    .ok_or(ConnectionError::UnexpectedEof)
}

#[allow(clippy::too_many_arguments)]
async fn read_frame(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    packet_timeout: Option<Duration>,
    compression: Option<usize>,
    max_uncompressed: usize,
) -> Result<Option<Bytes>, ConnectionError> {
    let operation = async {
        loop {
            if let Some(frame) = decoder.try_next()? {
                metrics.packets_received.fetch_add(1, Ordering::Relaxed);
                let frame = match compression {
                    Some(threshold) => {
                        decode_compressed_packet(&frame, threshold, max_uncompressed)?
                    }
                    None => frame,
                };
                return Ok(Some(frame));
            }

            let mut buffer = [0_u8; 8192];
            let read_limit = decoder.remaining_capacity().min(buffer.len());
            if read_limit == 0 {
                return Err(ProtocolError::LengthLimit {
                    kind: "packet buffer",
                    actual: decoder.buffered_len(),
                    limit: decoder.max_frame_length().saturating_add(5),
                }
                .into());
            }
            let read = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(None);
                    }
                    continue;
                }
                read = stream.read(&mut buffer[..read_limit]) => read?,
            };
            if read == 0 {
                if decoder.buffered_len() > 0 {
                    return Err(ProtocolError::UnexpectedEof.into());
                }
                return Ok(None);
            }
            metrics
                .bytes_received
                .fetch_add(read as u64, Ordering::Relaxed);
            decoder.push(&buffer[..read])?;
        }
    };

    match packet_timeout {
        Some(duration) => timeout(duration, operation)
            .await
            .map_err(|_| ConnectionError::PacketTimeout)?,
        None => operation.await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_packet(
    stream: &mut TcpStream,
    packet_id: i32,
    payload: &[u8],
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    compression: Option<usize>,
    max_uncompressed: usize,
) -> Result<(), ConnectionError> {
    let packet = match compression {
        Some(threshold) => {
            encode_compressed_packet(packet_id, payload, threshold, max_uncompressed)?
        }
        None => encode_packet(packet_id, payload)?,
    };
    tokio::select! {
        changed = shutdown.changed() => {
            if changed.is_err() || *shutdown.borrow() {
                return Ok(());
            }
        }
        result = stream.write_all(&packet) => result?,
    }
    metrics.packets_sent.fetch_add(1, Ordering::Relaxed);
    metrics
        .bytes_sent
        .fetch_add(packet.len() as u64, Ordering::Relaxed);
    Ok(())
}

#[derive(Serialize)]
struct StatusResponse<'a> {
    version: StatusVersion<'a>,
    players: StatusPlayers,
    description: StatusDescription<'a>,
    #[serde(rename = "enforcesSecureChat")]
    enforces_secure_chat: bool,
}

#[derive(Serialize)]
struct StatusVersion<'a> {
    name: &'a str,
    protocol: i32,
}

#[derive(Serialize)]
struct StatusPlayers {
    max: u32,
    online: usize,
}

#[derive(Serialize)]
struct StatusDescription<'a> {
    text: &'a str,
}
