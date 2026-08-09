//! Bounded asynchronous networking and connection-state ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at, timeout, timeout_at};
use toucan_auth::{AuthError, PlayerIdentity, authenticate_offline};
use toucan_config::{Config, ConfigError, GameMode, WorldGenerator};
use toucan_player::{PlayerData, PlayerDataError, PlayerStore};
use toucan_protocol::packet_id::{configuration, login, play};
use toucan_protocol::{
    ConnectionState, FrameDecoder, MINECRAFT_VERSION, PROTOCOL_VERSION, PacketReader, PacketWriter,
    ProtocolError, decode_client_information, decode_compressed_packet, decode_custom_payload,
    decode_handshake, decode_known_packs, decode_login_start, encode_block_changed_ack,
    encode_block_update, encode_chunk, encode_chunk_batch_finished, encode_common_disconnect,
    encode_compressed_packet, encode_empty_known_packs, encode_enabled_features,
    encode_forget_level_chunk, encode_game_event, encode_initial_player_position,
    encode_keep_alive, encode_login_disconnect, encode_login_success, encode_packet,
    encode_play_login, encode_player_abilities, encode_spawn_position, encode_view_center,
    encode_view_distance,
};
use toucan_registry::{RegistryError, configuration_packets};
use toucan_world::{
    BlockPosition as WorldBlockPosition, BlockStateId, ChunkPosition, GeneratorKind, World,
    WorldError,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

const SERVER_TICK_PERIOD: Duration = Duration::from_millis(50);
const CONTROL_QUEUE_CAPACITY: usize = 16;
type ActivePlayers = Arc<Mutex<HashMap<Uuid, PlayerData>>>;

/// Result of one completed operator or automatic persistence pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveReport {
    /// Number of dirty chunks committed.
    pub chunks: usize,
    /// Number of online or retry-pending player snapshots committed.
    pub players: usize,
}

/// Cloneable handle for bounded operator commands.
#[derive(Clone, Debug)]
pub struct ServerControl {
    commands: mpsc::Sender<ServerCommand>,
}

impl ServerControl {
    /// Requests a complete world and online-player save and waits for its result.
    pub async fn save(&self) -> Result<SaveReport, ServerControlError> {
        let (completion, result) = oneshot::channel();
        self.commands
            .send(ServerCommand::Save { completion })
            .await
            .map_err(|_| ServerControlError::Unavailable)?;
        result
            .await
            .map_err(|_| ServerControlError::Unavailable)?
            .map_err(ServerControlError::Save)
    }
}

#[derive(Debug)]
enum ServerCommand {
    Save {
        completion: oneshot::Sender<Result<SaveReport, String>>,
    },
}

/// Failure while submitting or executing an operator command.
#[derive(Debug, Error)]
pub enum ServerControlError {
    /// The server loop has already stopped.
    #[error("server control channel is unavailable")]
    Unavailable,
    /// The persistence pass failed.
    #[error("server save failed: {0}")]
    Save(String),
}

/// Listener or coordinated server failure.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Configuration was invalid when resolving the bind address.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The embedded vanilla registry fixture was invalid.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The configured world folder could not be opened or created.
    #[error(transparent)]
    World(#[from] WorldError),
    /// A player's persisted state could not be loaded or saved.
    #[error(transparent)]
    Player(#[from] PlayerDataError),
    /// A blocking world worker terminated unexpectedly.
    #[error("world worker terminated unexpectedly: {0}")]
    WorldWorker(#[from] tokio::task::JoinError),
    /// The online-player snapshot registry was poisoned by a panic.
    #[error("online-player snapshot registry is poisoned")]
    PlayerRegistryPoisoned,
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
    #[error(transparent)]
    World(#[from] WorldError),
    #[error(transparent)]
    Player(#[from] PlayerDataError),
    #[error("world worker terminated unexpectedly: {0}")]
    WorldWorker(#[from] tokio::task::JoinError),
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
    #[error("player fell behind authoritative world updates by {0} events")]
    WorldEventsLagged(u64),
    #[error("online-player snapshot registry is poisoned")]
    PlayerRegistryPoisoned,
    #[error("player UUID {0} is already connected")]
    DuplicatePlayer(Uuid),
    #[error("server has reached its configured player limit")]
    ServerFull,
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
    ticks_completed: AtomicU64,
    tick_overruns: AtomicU64,
    last_tick_duration_micros: AtomicU64,
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
            ticks_completed: self.ticks_completed.load(Ordering::Relaxed),
            tick_overruns: self.tick_overruns.load(Ordering::Relaxed),
            last_tick_duration_micros: self.last_tick_duration_micros.load(Ordering::Relaxed),
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
    /// Completed 20 Hz server lifecycle ticks.
    pub ticks_completed: u64,
    /// Ticks that began at least one complete tick period late.
    pub tick_overruns: u64,
    /// Work duration of the most recently completed tick.
    pub last_tick_duration_micros: u64,
}

/// A bound Toucan server ready to enter its accept loop.
pub struct ToucanServer {
    listener: TcpListener,
    config: Arc<Config>,
    world: Arc<World>,
    player_store: PlayerStore,
    active_players: ActivePlayers,
    player_save_gate: Arc<AsyncMutex<()>>,
    metrics: Arc<ServerMetrics>,
    world_events: broadcast::Sender<WorldEvent>,
    control: ServerControl,
    commands: mpsc::Receiver<ServerCommand>,
}

#[derive(Clone, Copy, Debug)]
struct WorldEvent {
    source_connection_id: u64,
    position: WorldBlockPosition,
    state: BlockStateId,
}

impl ToucanServer {
    /// Validates embedded data and binds the configured listener.
    pub async fn bind(config: Arc<Config>) -> Result<Self, ServerError> {
        configuration_packets()?;
        let world_path = config.server.world.clone();
        let player_store = PlayerStore::new(world_path.join("playerdata"));
        let max_loaded_chunks = config.performance.max_loaded_chunks;
        let world_seed = config.server.world_seed;
        let generator = match config.server.world_generator {
            WorldGenerator::Terrain => GeneratorKind::Terrain,
            WorldGenerator::Flat => GeneratorKind::Flat,
        };
        let world = Arc::new(
            tokio::task::spawn_blocking(move || {
                World::open_or_create(world_path, max_loaded_chunks, generator, world_seed)
            })
            .await??,
        );
        let spawn = world.metadata().spawn;
        info!(
            world = %world.path().display(),
            generator = world.generator_identifier(),
            seed = world.metadata().seed,
            spawn_x = spawn.x,
            spawn_y = spawn.y,
            spawn_z = spawn.z,
            loaded_chunks = 0,
            "world opened"
        );
        let listener = TcpListener::bind(config.bind_address()?).await?;
        let (world_events, _) =
            broadcast::channel(config.performance.max_packets_per_tick.clamp(64, 65_536));
        let (commands, command_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        Ok(Self {
            listener,
            config,
            world,
            player_store,
            active_players: Arc::new(Mutex::new(HashMap::new())),
            player_save_gate: Arc::new(AsyncMutex::new(())),
            metrics: Arc::new(ServerMetrics::default()),
            world_events,
            control: ServerControl { commands },
            commands: command_rx,
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

    /// Returns a bounded handle for operator save requests.
    #[must_use]
    pub fn control_handle(&self) -> ServerControl {
        self.control.clone()
    }

    /// Serves connections until `shutdown` resolves, then drains connection tasks.
    pub async fn serve_until<F>(mut self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send,
    {
        let max_connections = self.config.network.max_connections;
        let permits = Arc::new(Semaphore::new(max_connections));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let mut next_connection_id = 1_u64;
        let mut autosave = interval(Duration::from_secs(
            self.config.performance.autosave_interval_seconds,
        ));
        autosave.set_missed_tick_behavior(MissedTickBehavior::Delay);
        autosave.tick().await;
        let mut server_tick = interval_at(Instant::now() + SERVER_TICK_PERIOD, SERVER_TICK_PERIOD);
        server_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
                    let world = Arc::clone(&self.world);
                    let player_store = self.player_store.clone();
                    let active_players = Arc::clone(&self.active_players);
                    let player_save_gate = Arc::clone(&self.player_save_gate);
                    let metrics = Arc::clone(&self.metrics);
                    let world_events = self.world_events.clone();
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
                            world,
                            player_store,
                            active_players,
                            player_save_gate,
                            &metrics,
                            connection_shutdown,
                            world_events,
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
                _ = autosave.tick() => {
                    if let Err(error) = save_all(
                        Arc::clone(&self.world),
                        self.player_store.clone(),
                        Arc::clone(&self.active_players),
                        Arc::clone(&self.player_save_gate),
                        "autosave",
                    ).await {
                        warn!(%error, "autosave failed; dirty state retained");
                    }
                }
                Some(command) = self.commands.recv() => {
                    match command {
                        ServerCommand::Save { completion } => {
                            let result = save_all(
                                Arc::clone(&self.world),
                                self.player_store.clone(),
                                Arc::clone(&self.active_players),
                                Arc::clone(&self.player_save_gate),
                                "operator",
                            ).await.map_err(|error| error.to_string());
                            let _ = completion.send(result);
                        }
                    }
                }
                scheduled = server_tick.tick() => {
                    let started = Instant::now();
                    if started.saturating_duration_since(scheduled) >= SERVER_TICK_PERIOD {
                        self.metrics.tick_overruns.fetch_add(1, Ordering::Relaxed);
                    }
                    self.metrics.ticks_completed.fetch_add(1, Ordering::Relaxed);
                    let elapsed = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                    self.metrics.last_tick_duration_micros.store(elapsed, Ordering::Relaxed);
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
        save_all(
            Arc::clone(&self.world),
            self.player_store.clone(),
            Arc::clone(&self.active_players),
            Arc::clone(&self.player_save_gate),
            "shutdown",
        )
        .await?;
        info!("graceful shutdown complete");
        Ok(())
    }
}

async fn save_all(
    world: Arc<World>,
    player_store: PlayerStore,
    active_players: ActivePlayers,
    player_save_gate: Arc<AsyncMutex<()>>,
    reason: &'static str,
) -> Result<SaveReport, ServerError> {
    let started = Instant::now();
    let chunks = save_world(world).await?;
    let players = save_online_players(player_store, active_players, player_save_gate).await?;
    info!(
        reason,
        saved_chunks = chunks,
        saved_players = players,
        elapsed_ms = started.elapsed().as_millis(),
        "server save completed"
    );
    Ok(SaveReport { chunks, players })
}

async fn save_world(world: Arc<World>) -> Result<usize, ServerError> {
    let dirty = world.dirty_chunk_count()?;
    if dirty == 0 {
        return Ok(0);
    }
    let saved = tokio::task::spawn_blocking(move || world.save_dirty()).await??;
    Ok(saved)
}

async fn save_online_players(
    player_store: PlayerStore,
    active_players: ActivePlayers,
    player_save_gate: Arc<AsyncMutex<()>>,
) -> Result<usize, ServerError> {
    let _save_guard = player_save_gate.lock().await;
    let snapshots = active_players
        .lock()
        .map_err(|_| ServerError::PlayerRegistryPoisoned)?
        .values()
        .cloned()
        .collect::<Vec<_>>();
    if snapshots.is_empty() {
        return Ok(0);
    }
    tokio::task::spawn_blocking(move || {
        for player in &snapshots {
            player_store.save(player)?;
        }
        Ok::<_, PlayerDataError>(snapshots.len())
    })
    .await?
    .map_err(ServerError::Player)
}

/// Compatibility alias retained for Phase 0/1 callers.
pub type StatusServer = ToucanServer;

#[allow(clippy::too_many_arguments)]
async fn serve_connection(
    mut stream: TcpStream,
    connection_id: u64,
    peer_address: SocketAddr,
    config: &Config,
    world: Arc<World>,
    player_store: PlayerStore,
    active_players: ActivePlayers,
    player_save_gate: Arc<AsyncMutex<()>>,
    metrics: &ServerMetrics,
    mut shutdown: watch::Receiver<bool>,
    world_events: broadcast::Sender<WorldEvent>,
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
                world,
                player_store,
                active_players,
                player_save_gate,
                metrics,
                &mut shutdown,
                packet_timeout,
                world_events,
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

#[allow(clippy::too_many_arguments)]
async fn serve_login(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection_id: u64,
    config: &Config,
    world: Arc<World>,
    player_store: PlayerStore,
    active_players: ActivePlayers,
    player_save_gate: Arc<AsyncMutex<()>>,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    packet_timeout: Duration,
    world_events: broadcast::Sender<WorldEvent>,
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

    let player_uuid = identity.uuid;
    let load_store = player_store.clone();
    let stored_player = tokio::task::spawn_blocking(move || load_store.load(player_uuid)).await??;
    let spawn = world.metadata().spawn;
    let mut player_data = match stored_player {
        Some(player) => player,
        None => PlayerData::new(
            player_uuid,
            [
                f64::from(spawn.x) + 0.5,
                f64::from(spawn.y),
                f64::from(spawn.z) + 0.5,
            ],
            [0.0, 0.0],
            config.server.default_gamemode.protocol_id(),
            0,
        )?,
    };
    {
        let mut players = active_players
            .lock()
            .map_err(|_| ConnectionError::PlayerRegistryPoisoned)?;
        if players.contains_key(&player_uuid) {
            return Err(ConnectionError::DuplicatePlayer(player_uuid));
        }
        if players.len() >= config.server.max_players as usize {
            return Err(ConnectionError::ServerFull);
        }
        players.insert(player_uuid, player_data.clone());
    }

    metrics.online_players.fetch_add(1, Ordering::Relaxed);
    info!(
        connection_id,
        username = %identity.username,
        uuid = %identity.uuid,
        state = "play",
        "player completed configuration"
    );
    let result = serve_play(
        stream,
        decoder,
        connection_id,
        config,
        world,
        world_events,
        &mut player_data,
        &active_players,
        player_uuid,
        metrics,
        shutdown,
        compression,
    )
    .await;
    let _save_guard = player_save_gate.lock().await;
    let save_result = tokio::task::spawn_blocking(move || player_store.save(&player_data))
        .await
        .map_err(ConnectionError::WorldWorker)
        .and_then(|result| result.map_err(ConnectionError::Player));
    let persistence_result = save_result.and_then(|()| {
        active_players
            .lock()
            .map_err(|_| ConnectionError::PlayerRegistryPoisoned)?
            .remove(&player_uuid);
        Ok(())
    });
    metrics.online_players.fetch_sub(1, Ordering::Relaxed);
    match (result, persistence_result) {
        (Err(connection), Err(save)) => {
            warn!(connection_id, %save, "player save failed while connection was closing");
            Err(connection)
        }
        (Err(connection), Ok(())) => Err(connection),
        (Ok(()), Err(save)) => Err(save),
        (Ok(()), Ok(())) => Ok(()),
    }
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

#[allow(clippy::too_many_arguments)]
async fn serve_play(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection_id: u64,
    config: &Config,
    world: Arc<World>,
    world_events: broadcast::Sender<WorldEvent>,
    player_data: &mut PlayerData,
    active_players: &ActivePlayers,
    player_uuid: Uuid,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    compression: Option<usize>,
) -> Result<(), ConnectionError> {
    let mut world_events_rx = world_events.subscribe();
    let mut player = send_initial_play(
        stream,
        config,
        &world,
        player_data,
        metrics,
        shutdown,
        compression,
    )
    .await?;
    send_next_chunk_batch(
        stream,
        config,
        &world,
        &mut player,
        metrics,
        shutdown,
        compression,
    )
    .await?;

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
                let outcome = handle_play_packet(
                    &frame,
                    &mut pending_keep_alive,
                    &mut player,
                    &world,
                )?;
                player_data.update_session(
                    player.position,
                    player.rotation,
                    player.game_mode.protocol_id(),
                    player.selected_hotbar as u8,
                )?;
                active_players
                    .lock()
                    .map_err(|_| ConnectionError::PlayerRegistryPoisoned)?
                    .insert(player_uuid, player_data.clone());
                for (packet_id, payload) in outcome.packets {
                    write_packet(
                        stream,
                        packet_id,
                        &payload,
                        metrics,
                        shutdown,
                        compression,
                        config.network.max_packet_size,
                    ).await?;
                }
                if let Some((position, state)) = outcome.world_change {
                    let _ = world_events.send(WorldEvent {
                        source_connection_id: connection_id,
                        position,
                        state,
                    });
                }
                if outcome.center_changed {
                    refresh_chunk_view(&mut player);
                }
                if outcome.batch_received {
                    player.batch_in_flight = false;
                }
                if !player.batch_in_flight {
                    send_next_chunk_batch(
                        stream,
                        config,
                        &world,
                        &mut player,
                        metrics,
                        shutdown,
                        compression,
                    ).await?;
                }
            }
            event = world_events_rx.recv() => {
                match event {
                    Ok(event) => {
                        if let Some((packet_id, payload)) = world_event_packet(
                            event,
                            connection_id,
                            &player,
                        ) {
                            write_packet(
                                stream,
                                packet_id,
                                &payload,
                                metrics,
                                shutdown,
                                compression,
                                config.network.max_packet_size,
                            ).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        return Err(ConnectionError::WorldEventsLagged(skipped));
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
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
    world: &Arc<World>,
    player_data: &PlayerData,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    compression: Option<usize>,
) -> Result<PlaySession, ConnectionError> {
    let radius = config.server.view_distance;
    let spawn = world.metadata().spawn;
    let position = player_data.position();
    let rotation = player_data.rotation();
    let center = ChunkPosition::from_block(position[0].floor() as i32, position[2].floor() as i32);
    let game_mode = player_data.game_mode();
    let packets = [
        (
            play::clientbound::LOGIN,
            encode_play_login(
                1,
                config.server.max_players,
                radius,
                config.server.simulation_distance.min(radius),
                game_mode,
            )?,
        ),
        (
            play::clientbound::PLAYER_ABILITIES,
            encode_player_abilities(game_mode),
        ),
        (
            play::clientbound::SET_CHUNK_CACHE_RADIUS,
            encode_view_distance(radius),
        ),
        (
            play::clientbound::SET_CHUNK_CACHE_CENTER,
            encode_view_center(center.x, center.z),
        ),
        (
            play::clientbound::SET_DEFAULT_SPAWN_POSITION,
            encode_spawn_position(toucan_protocol::BlockPosition {
                x: spawn.x,
                y: spawn.y,
                z: spawn.z,
            })?,
        ),
        (
            play::clientbound::PLAYER_POSITION,
            encode_initial_player_position(
                1,
                position[0],
                position[1],
                position[2],
                rotation[0],
                rotation[1],
            ),
        ),
        (play::clientbound::GAME_EVENT, encode_game_event(13, 0.0)),
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

    let mut player = PlaySession {
        position,
        rotation,
        center,
        view_distance: radius,
        subscribed: HashSet::new(),
        pending_chunks: VecDeque::new(),
        batch_in_flight: false,
        chunks_per_batch: 4,
        game_mode: GameMode::from_protocol_id(game_mode)
            .expect("PlayerData validates game-mode ordinals"),
        selected_hotbar: usize::from(player_data.selected_hotbar()),
        hotbar: [None; 9],
    };
    refresh_chunk_view(&mut player);
    Ok(player)
}

#[derive(Debug)]
struct PlaySession {
    position: [f64; 3],
    rotation: [f32; 2],
    center: ChunkPosition,
    view_distance: u8,
    subscribed: HashSet<ChunkPosition>,
    pending_chunks: VecDeque<ChunkPosition>,
    batch_in_flight: bool,
    chunks_per_batch: usize,
    game_mode: GameMode,
    selected_hotbar: usize,
    hotbar: [Option<BlockStateId>; 9],
}

#[derive(Debug, Default)]
struct PlayPacketOutcome {
    packets: Vec<(i32, Bytes)>,
    center_changed: bool,
    batch_received: bool,
    world_change: Option<(WorldBlockPosition, BlockStateId)>,
}

fn world_event_packet(
    event: WorldEvent,
    connection_id: u64,
    player: &PlaySession,
) -> Option<(i32, Bytes)> {
    let chunk = ChunkPosition::from_block(event.position.x, event.position.z);
    if event.source_connection_id == connection_id || !player.subscribed.contains(&chunk) {
        return None;
    }
    Some((
        play::clientbound::BLOCK_UPDATE,
        encode_block_update(
            toucan_protocol::BlockPosition {
                x: event.position.x,
                y: event.position.y,
                z: event.position.z,
            },
            protocol_775_block_state(event.state),
        ),
    ))
}

fn desired_chunks(center: ChunkPosition, radius: u8) -> Vec<ChunkPosition> {
    let radius = i32::from(radius);
    let mut positions = Vec::with_capacity(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
    for z in center.z - radius..=center.z + radius {
        for x in center.x - radius..=center.x + radius {
            positions.push(ChunkPosition { x, z });
        }
    }
    positions.sort_unstable_by_key(|position| {
        let dx = position.x - center.x;
        let dz = position.z - center.z;
        (dx * dx + dz * dz, dz, dx)
    });
    positions
}

fn refresh_chunk_view(player: &mut PlaySession) {
    let desired = desired_chunks(player.center, player.view_distance);
    let desired_set: HashSet<_> = desired.iter().copied().collect();
    player
        .pending_chunks
        .retain(|position| desired_set.contains(position));
    for position in desired {
        if !player.subscribed.contains(&position) && !player.pending_chunks.contains(&position) {
            player.pending_chunks.push_back(position);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_next_chunk_batch(
    stream: &mut TcpStream,
    config: &Config,
    world: &Arc<World>,
    player: &mut PlaySession,
    metrics: &ServerMetrics,
    shutdown: &mut watch::Receiver<bool>,
    compression: Option<usize>,
) -> Result<(), ConnectionError> {
    if player.pending_chunks.is_empty() {
        return Ok(());
    }
    write_packet(
        stream,
        play::clientbound::CHUNK_BATCH_START,
        &[],
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;

    let mut sent = 0_i32;
    while sent < player.chunks_per_batch as i32 {
        let Some(position) = player.pending_chunks.pop_front() else {
            break;
        };
        let world = Arc::clone(world);
        let payload = tokio::task::spawn_blocking(move || {
            let chunk = world.chunk(position)?;
            Ok::<_, WorldError>(encode_chunk(position.x, position.z, |x, y, z| {
                chunk.block(x, y, z).map_or(0, protocol_775_block_state)
            }))
        })
        .await??;
        write_packet(
            stream,
            play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            &payload,
            metrics,
            shutdown,
            compression,
            config.network.max_packet_size,
        )
        .await?;
        player.subscribed.insert(position);
        sent += 1;
    }
    write_packet(
        stream,
        play::clientbound::CHUNK_BATCH_FINISHED,
        &encode_chunk_batch_finished(sent),
        metrics,
        shutdown,
        compression,
        config.network.max_packet_size,
    )
    .await?;
    player.batch_in_flight = true;
    Ok(())
}

fn protocol_775_block_state(state: BlockStateId) -> i32 {
    match state {
        BlockStateId::AIR => 0,
        BlockStateId::STONE => 1,
        BlockStateId::GRANITE => 2,
        BlockStateId::DIORITE => 4,
        BlockStateId::ANDESITE => 6,
        BlockStateId::GRASS_BLOCK => 9,
        BlockStateId::DIRT => 10,
        BlockStateId::COBBLESTONE => 14,
        BlockStateId::OAK_PLANKS => 15,
        BlockStateId::SPRUCE_PLANKS => 16,
        BlockStateId::BIRCH_PLANKS => 17,
        BlockStateId::JUNGLE_PLANKS => 18,
        BlockStateId::ACACIA_PLANKS => 19,
        BlockStateId::CHERRY_PLANKS => 20,
        BlockStateId::DARK_OAK_PLANKS => 21,
        BlockStateId::PALE_OAK_PLANKS => 25,
        BlockStateId::SAND => 118,
        BlockStateId::GRAVEL => 124,
        BlockStateId::GLASS => 562,
        BlockStateId::WHITE_WOOL => 2293,
        BlockStateId::BLUE_WOOL => 2304,
        BlockStateId::RED_WOOL => 2307,
        BlockStateId::GOLD_BLOCK => 2338,
        BlockStateId::IRON_BLOCK => 2339,
        BlockStateId::BRICKS => 2340,
        BlockStateId::OBSIDIAN => 3369,
        BlockStateId::DIAMOND_BLOCK => 5309,
        BlockStateId::NETHERRACK => 6997,
        BlockStateId::STONE_BRICKS => 7754,
        BlockStateId::END_STONE => 9477,
        BlockStateId::EMERALD_BLOCK => 9727,
        _ => 0,
    }
}

fn handle_play_packet(
    frame: &[u8],
    pending_keep_alive: &mut Option<i64>,
    player: &mut PlaySession,
    world: &World,
) -> Result<PlayPacketOutcome, ConnectionError> {
    let mut outcome = PlayPacketOutcome::default();
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
            player.chunks_per_batch = chunks_per_tick.clamp(1.0, 64.0).round() as usize;
            outcome.batch_received = true;
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
            player.position = validate_position(&mut reader)?;
            read_movement_flags(&mut reader)?;
            outcome.center_changed = update_chunk_center(player, &mut outcome.packets);
        }
        play::serverbound::MOVE_PLAYER_POS_ROT => {
            player.position = validate_position(&mut reader)?;
            player.rotation = validate_rotation(&mut reader)?;
            read_movement_flags(&mut reader)?;
            outcome.center_changed = update_chunk_center(player, &mut outcome.packets);
        }
        play::serverbound::MOVE_PLAYER_ROT => {
            player.rotation = validate_rotation(&mut reader)?;
            read_movement_flags(&mut reader)?;
        }
        play::serverbound::MOVE_PLAYER_STATUS_ONLY => read_movement_flags(&mut reader)?,
        play::serverbound::SET_CARRIED_ITEM => {
            let slot = reader.read_i16()?;
            reader.finish()?;
            if !(0..=8).contains(&slot) {
                return Err(ConnectionError::InvalidMovement);
            }
            player.selected_hotbar = slot as usize;
        }
        play::serverbound::SET_CREATIVE_MODE_SLOT => {
            handle_creative_slot(&mut reader, player)?;
        }
        play::serverbound::PLAYER_ACTION => {
            outcome.world_change =
                handle_player_action(&mut reader, player, world, &mut outcome.packets)?;
        }
        play::serverbound::USE_ITEM_ON => {
            outcome.world_change =
                handle_use_item_on(&mut reader, player, world, &mut outcome.packets)?;
        }
        _ => {
            let bytes = reader.remaining();
            debug!(packet_id, bytes, "unsupported Play packet ignored");
        }
    }
    Ok(outcome)
}

fn validate_position(reader: &mut PacketReader<'_>) -> Result<[f64; 3], ConnectionError> {
    let position = [reader.read_f64()?, reader.read_f64()?, reader.read_f64()?];
    for value in position {
        if !value.is_finite() || value.abs() > 30_000_000.0 {
            return Err(ConnectionError::InvalidMovement);
        }
    }
    Ok(position)
}

fn update_chunk_center(player: &mut PlaySession, packets: &mut Vec<(i32, Bytes)>) -> bool {
    let center = ChunkPosition::from_block(
        player.position[0].floor() as i32,
        player.position[2].floor() as i32,
    );
    if center == player.center {
        return false;
    }
    player.center = center;
    packets.push((
        play::clientbound::SET_CHUNK_CACHE_CENTER,
        encode_view_center(center.x, center.z),
    ));

    let radius = i32::from(player.view_distance);
    let mut removed = Vec::new();
    player.subscribed.retain(|position| {
        let keep =
            (position.x - center.x).abs() <= radius && (position.z - center.z).abs() <= radius;
        if !keep {
            removed.push(*position);
        }
        keep
    });
    for position in removed {
        packets.push((
            play::clientbound::FORGET_LEVEL_CHUNK,
            encode_forget_level_chunk(position.x, position.z),
        ));
    }
    true
}

fn handle_creative_slot(
    reader: &mut PacketReader<'_>,
    player: &mut PlaySession,
) -> Result<(), ConnectionError> {
    let slot = reader.read_i16()?;
    let count = reader.read_var_i32()?;
    if !(0..=99).contains(&count) {
        return Err(ConnectionError::InvalidMovement);
    }
    let block = if count == 0 {
        None
    } else {
        let item_id = reader.read_var_i32()?;
        let added_components = reader.read_var_i32()?;
        let removed_components = reader.read_var_i32()?;
        if added_components != 0 || removed_components != 0 {
            debug!(
                added_components,
                removed_components, "creative item components ignored"
            );
            return Ok(());
        }
        creative_item_block_state(item_id)
    };
    reader.finish()?;
    if player.game_mode == GameMode::Creative && (36..=44).contains(&slot) {
        player.hotbar[(slot - 36) as usize] = block;
    }
    Ok(())
}

fn handle_player_action(
    reader: &mut PacketReader<'_>,
    player: &PlaySession,
    world: &World,
    packets: &mut Vec<(i32, Bytes)>,
) -> Result<Option<(WorldBlockPosition, BlockStateId)>, ConnectionError> {
    let action = reader.read_var_i32()?;
    let position = reader.read_block_position()?;
    let face = reader.read_u8()?;
    let sequence = reader.read_var_i32()?;
    reader.finish()?;
    if !(0..=6).contains(&action) || face > 5 || sequence < 0 {
        return Err(ConnectionError::InvalidMovement);
    }

    let world_position = WorldBlockPosition {
        x: position.x,
        y: position.y,
        z: position.z,
    };
    let world_change = if action == 0
        && player.game_mode.can_modify_blocks()
        && block_in_reach(player.position, world_position)
    {
        world.set_block(world_position, BlockStateId::AIR)?;
        packets.push((
            play::clientbound::BLOCK_UPDATE,
            encode_block_update(position, protocol_775_block_state(BlockStateId::AIR)),
        ));
        Some((world_position, BlockStateId::AIR))
    } else if action == 0 {
        let state = world.block(world_position)?;
        packets.push((
            play::clientbound::BLOCK_UPDATE,
            encode_block_update(position, protocol_775_block_state(state)),
        ));
        None
    } else {
        None
    };
    packets.push((
        play::clientbound::BLOCK_CHANGED_ACK,
        encode_block_changed_ack(sequence),
    ));
    Ok(world_change)
}

fn handle_use_item_on(
    reader: &mut PacketReader<'_>,
    player: &PlaySession,
    world: &World,
    packets: &mut Vec<(i32, Bytes)>,
) -> Result<Option<(WorldBlockPosition, BlockStateId)>, ConnectionError> {
    let hand = reader.read_var_i32()?;
    let clicked = reader.read_block_position()?;
    let face = reader.read_var_i32()?;
    for cursor in [reader.read_f32()?, reader.read_f32()?, reader.read_f32()?] {
        if !cursor.is_finite() || !(0.0..=1.0).contains(&cursor) {
            return Err(ConnectionError::InvalidMovement);
        }
    }
    let _inside_block = reader.read_bool()?;
    let _world_border_hit = reader.read_bool()?;
    let sequence = reader.read_var_i32()?;
    reader.finish()?;
    if !(0..=1).contains(&hand) || !(0..=5).contains(&face) || sequence < 0 {
        return Err(ConnectionError::InvalidMovement);
    }
    let (dx, dy, dz) = match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        5 => (1, 0, 0),
        _ => unreachable!(),
    };
    let target = toucan_protocol::BlockPosition {
        x: clicked.x.saturating_add(dx),
        y: clicked.y.saturating_add(dy),
        z: clicked.z.saturating_add(dz),
    };
    let world_target = WorldBlockPosition {
        x: target.x,
        y: target.y,
        z: target.z,
    };
    let held = player.hotbar[player.selected_hotbar];
    let can_place = hand == 0
        && player.game_mode.can_modify_blocks()
        && block_in_reach(player.position, world_target)
        && held.is_some()
        && world.block(world_target)? == BlockStateId::AIR;
    let world_change = if can_place {
        let state = held.expect("checked above");
        world.set_block(world_target, state)?;
        packets.push((
            play::clientbound::BLOCK_UPDATE,
            encode_block_update(target, protocol_775_block_state(state)),
        ));
        Some((world_target, state))
    } else {
        let state = world.block(world_target)?;
        packets.push((
            play::clientbound::BLOCK_UPDATE,
            encode_block_update(target, protocol_775_block_state(state)),
        ));
        None
    };
    packets.push((
        play::clientbound::BLOCK_CHANGED_ACK,
        encode_block_changed_ack(sequence),
    ));
    Ok(world_change)
}

fn block_in_reach(player: [f64; 3], block: WorldBlockPosition) -> bool {
    let dx = player[0] - (f64::from(block.x) + 0.5);
    let dy = (player[1] + 1.62) - (f64::from(block.y) + 0.5);
    let dz = player[2] - (f64::from(block.z) + 0.5);
    dx * dx + dy * dy + dz * dz <= 64.0
}

fn creative_item_block_state(item_id: i32) -> Option<BlockStateId> {
    Some(match item_id {
        1 => BlockStateId::STONE,
        2 => BlockStateId::GRANITE,
        4 => BlockStateId::DIORITE,
        6 => BlockStateId::ANDESITE,
        27 => BlockStateId::GRASS_BLOCK,
        28 => BlockStateId::DIRT,
        35 => BlockStateId::COBBLESTONE,
        36 => BlockStateId::OAK_PLANKS,
        37 => BlockStateId::SPRUCE_PLANKS,
        38 => BlockStateId::BIRCH_PLANKS,
        39 => BlockStateId::JUNGLE_PLANKS,
        40 => BlockStateId::ACACIA_PLANKS,
        41 => BlockStateId::CHERRY_PLANKS,
        42 => BlockStateId::DARK_OAK_PLANKS,
        43 => BlockStateId::PALE_OAK_PLANKS,
        59 => BlockStateId::SAND,
        63 => BlockStateId::GRAVEL,
        90 => BlockStateId::IRON_BLOCK,
        92 => BlockStateId::GOLD_BLOCK,
        93 => BlockStateId::DIAMOND_BLOCK,
        195 => BlockStateId::GLASS,
        213 => BlockStateId::WHITE_WOOL,
        224 => BlockStateId::BLUE_WOOL,
        227 => BlockStateId::RED_WOOL,
        305 => BlockStateId::BRICKS,
        322 => BlockStateId::OBSIDIAN,
        360 => BlockStateId::NETHERRACK,
        376 => BlockStateId::STONE_BRICKS,
        436 => BlockStateId::END_STONE,
        441 => BlockStateId::EMERALD_BLOCK,
        _ => return None,
    })
}

fn validate_rotation(reader: &mut PacketReader<'_>) -> Result<[f32; 2], ConnectionError> {
    let rotation = [reader.read_f32()?, reader.read_f32()?];
    if !rotation.into_iter().all(f32::is_finite) {
        return Err(ConnectionError::InvalidMovement);
    }
    Ok(rotation)
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

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::{
        PlaySession, WorldEvent, creative_item_block_state, desired_chunks, handle_play_packet,
        protocol_775_block_state, world_event_packet,
    };
    use crate::GameMode;
    use toucan_protocol::packet_id::play;
    use toucan_protocol::{BlockPosition, PacketWriter};
    use toucan_world::{
        BlockPosition as WorldBlockPosition, BlockStateId, ChunkPosition, GeneratorKind, World,
    };

    fn player(game_mode: GameMode) -> PlaySession {
        PlaySession {
            position: [0.5, 64.0, 0.5],
            rotation: [0.0, 0.0],
            center: ChunkPosition { x: 0, z: 0 },
            view_distance: 2,
            subscribed: Default::default(),
            pending_chunks: Default::default(),
            batch_in_flight: false,
            chunks_per_batch: 4,
            game_mode,
            selected_hotbar: 0,
            hotbar: [None; 9],
        }
    }

    fn start_break_packet(position: BlockPosition, sequence: i32) -> bytes::Bytes {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(play::serverbound::PLAYER_ACTION);
        writer.write_var_i32(0);
        writer.write_block_position(position);
        writer.write_u8(1);
        writer.write_var_i32(sequence);
        writer.into_bytes()
    }

    fn use_item_on_packet(position: BlockPosition, sequence: i32) -> bytes::Bytes {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(play::serverbound::USE_ITEM_ON);
        writer.write_var_i32(0);
        writer.write_block_position(position);
        writer.write_var_i32(1);
        writer.write_f32(0.5);
        writer.write_f32(1.0);
        writer.write_f32(0.5);
        writer.write_bool(false);
        writer.write_bool(false);
        writer.write_var_i32(sequence);
        writer.into_bytes()
    }

    #[test]
    fn chunk_view_is_complete_and_center_first() {
        let chunks = desired_chunks(ChunkPosition { x: 8, z: -3 }, 2);
        assert_eq!(chunks.len(), 25);
        assert_eq!(chunks[0], ChunkPosition { x: 8, z: -3 });
    }

    #[test]
    fn common_creative_items_map_to_target_block_states() {
        let oak = creative_item_block_state(36);
        assert_eq!(oak, Some(BlockStateId::OAK_PLANKS));
        assert_eq!(oak.map(protocol_775_block_state), Some(15));
        assert_eq!(creative_item_block_state(999_999), None);
    }

    #[test]
    fn survival_can_break_but_adventure_cannot() -> Result<(), Box<dyn Error>> {
        let path =
            std::env::temp_dir().join(format!("toucan-network-block-test-{}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let position = BlockPosition { x: 0, y: 63, z: 0 };
        let world_position = WorldBlockPosition { x: 0, y: 63, z: 0 };

        let mut survival = player(GameMode::Survival);
        let mut pending_keep_alive = None;
        let outcome = handle_play_packet(
            &start_break_packet(position, 7),
            &mut pending_keep_alive,
            &mut survival,
            &world,
        )?;
        assert_eq!(world.block(world_position)?, BlockStateId::AIR);
        assert_eq!(outcome.packets.len(), 2);
        assert_eq!(
            outcome.world_change,
            Some((world_position, BlockStateId::AIR))
        );

        world.set_block(world_position, BlockStateId::STONE)?;
        let mut adventure = player(GameMode::Adventure);
        let outcome = handle_play_packet(
            &start_break_packet(position, 8),
            &mut pending_keep_alive,
            &mut adventure,
            &world,
        )?;
        assert_eq!(world.block(world_position)?, BlockStateId::STONE);
        assert_eq!(outcome.packets.len(), 2);
        assert_eq!(outcome.world_change, None);

        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn target_version_right_click_packet_places_selected_block() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-network-use-item-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let clicked = BlockPosition { x: 0, y: 63, z: 0 };
        let target = WorldBlockPosition { x: 0, y: 64, z: 0 };
        let mut creative = player(GameMode::Creative);
        creative.hotbar[0] = Some(BlockStateId::OAK_PLANKS);
        let mut pending_keep_alive = None;

        let outcome = handle_play_packet(
            &use_item_on_packet(clicked, 11),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;

        assert_eq!(world.block(target)?, BlockStateId::OAK_PLANKS);
        assert_eq!(outcome.packets.len(), 2);
        assert_eq!(
            outcome.world_change,
            Some((target, BlockStateId::OAK_PLANKS))
        );
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn authoritative_world_change_reaches_other_subscribers_only() {
        let mut recipient = player(GameMode::Survival);
        recipient.subscribed.insert(ChunkPosition { x: 0, z: 0 });
        let event = WorldEvent {
            source_connection_id: 7,
            position: WorldBlockPosition { x: 1, y: 64, z: 2 },
            state: BlockStateId::STONE,
        };

        assert!(world_event_packet(event, 8, &recipient).is_some());
        assert!(world_event_packet(event, 7, &recipient).is_none());
        recipient.subscribed.clear();
        assert!(world_event_packet(event, 8, &recipient).is_none());
    }
}
