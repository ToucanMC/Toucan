mod placement;

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
use toucan_auth::{AuthError, PlayerIdentity, authenticate_offline, lookup_profile_properties};
use toucan_config::{Config, ConfigError, GameMode, WorldGenerator};
use toucan_player::{
    InventoryError, ItemStack, PlayerData, PlayerDataError, PlayerInventory, PlayerStore,
};
use toucan_protocol::packet_id::{configuration, login, play};
use toucan_protocol::{
    ConnectionState, FrameDecoder, MINECRAFT_VERSION, PROTOCOL_VERSION, PacketReader, PacketWriter,
    ProtocolError, ProtocolItemStack, decode_client_information, decode_compressed_packet,
    decode_custom_payload, decode_handshake, decode_known_packs, decode_login_start,
    encode_block_changed_ack, encode_block_update, encode_chunk, encode_chunk_batch_finished,
    encode_common_disconnect, encode_compressed_packet, encode_container_set_content,
    encode_container_set_slot, encode_empty_known_packs, encode_enabled_features,
    encode_forget_level_chunk, encode_game_event, encode_initial_player_position,
    encode_keep_alive, encode_login_disconnect, encode_login_success, encode_packet,
    encode_play_login, encode_player_abilities, encode_player_info_add, encode_player_skin_parts,
    encode_set_cursor_item, encode_spawn_position, encode_view_center, encode_view_distance,
};
use toucan_registry::{ItemId, RegistryError, configuration_packets, vanilla_registries};
use toucan_world::{
    BlockPosition as WorldBlockPosition, BlockStateId, ChunkPosition, GeneratorKind, World,
    WorldError,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::placement::{
    BlockChange, PlacementContext, PlacementError, companion_for_break, plan_interaction,
    plan_placement, refresh_connectable_shapes, refresh_stair_shapes,
};

const SERVER_TICK_PERIOD: Duration = Duration::from_millis(50);
const CONTROL_QUEUE_CAPACITY: usize = 16;
type ActivePlayers = Arc<Mutex<HashMap<Uuid, PlayerData>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveReport {
    pub chunks: usize,
    pub players: usize,
}

#[derive(Clone, Debug)]
pub struct ServerControl {
    commands: mpsc::Sender<ServerCommand>,
}

impl ServerControl {
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

#[derive(Debug, Error)]
pub enum ServerControlError {
    #[error("server control channel is unavailable")]
    Unavailable,
    #[error("server save failed: {0}")]
    Save(String),
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    World(#[from] WorldError),
    #[error(transparent)]
    Player(#[from] PlayerDataError),
    #[error("world worker terminated unexpectedly: {0}")]
    WorldWorker(#[from] tokio::task::JoinError),
    #[error("online-player snapshot registry is poisoned")]
    PlayerRegistryPoisoned,
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
    #[error(transparent)]
    Inventory(#[from] InventoryError),
    #[error(transparent)]
    Placement(#[from] PlacementError),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetricsSnapshot {
    pub active_connections: usize,
    pub online_players: usize,
    pub accepted_connections: u64,
    pub packets_received: u64,
    pub packets_sent: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub rejected_connections: u64,
    pub ticks_completed: u64,
    pub tick_overruns: u64,
    pub last_tick_duration_micros: u64,
}

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

    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.listener.local_addr()
    }

    #[must_use]
    pub fn metrics(&self) -> Arc<ServerMetrics> {
        Arc::clone(&self.metrics)
    }

    #[must_use]
    pub fn control_handle(&self) -> ServerControl {
        self.control.clone()
    }

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
    let mut identity = match authenticate_offline(&login_start.username) {
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
    if config.server.fetch_profile_textures {
        match lookup_profile_properties(&identity.username).await {
            Ok(Some(properties)) => {
                debug!(
                    connection_id,
                    username = %identity.username,
                    properties = properties.len(),
                    "signed Mojang profile properties resolved"
                );
                identity.properties = properties;
            }
            Ok(None) => debug!(
                connection_id,
                username = %identity.username,
                "no Mojang profile found; using default skin"
            ),
            Err(error) => warn!(
                connection_id,
                username = %identity.username,
                %error,
                "Mojang profile lookup failed; using default skin"
            ),
        }
    }
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
    let model_customization = configuration_result?;

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
        &identity,
        model_customization,
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
) -> Result<u8, ConnectionError> {
    let mut model_customization = 0x7f;
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
                model_customization = information.model_customization;
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
                model_customization = decode_client_information(&mut reader)?.model_customization;
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

    Ok(model_customization)
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
    identity: &PlayerIdentity,
    model_customization: u8,
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
        identity,
        model_customization,
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
                if let Some(inventory) = inventory_for_persistence(&player)? {
                    player_data.set_inventory(inventory);
                }
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
                for (position, state) in outcome.world_changes {
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

#[allow(clippy::too_many_arguments)]
async fn send_initial_play(
    stream: &mut TcpStream,
    config: &Config,
    world: &Arc<World>,
    player_data: &PlayerData,
    identity: &PlayerIdentity,
    model_customization: u8,
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
    let inventory = player_data.inventory().clone();
    let properties = identity
        .properties
        .iter()
        .map(|property| {
            (
                property.name.as_str(),
                property.value.as_str(),
                property.signature.as_deref(),
            )
        })
        .collect::<Vec<_>>();
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
            play::clientbound::PLAYER_INFO_UPDATE,
            encode_player_info_add(identity.uuid, &identity.username, &properties)?,
        ),
        (
            play::clientbound::SET_ENTITY_DATA,
            encode_player_skin_parts(1, model_customization),
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
        (
            play::clientbound::CONTAINER_SET_CONTENT,
            encode_inventory_content(0, &inventory, None),
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
        inventory,
        carried: None,
        inventory_state_id: 0,
        model_customization,
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
    inventory: PlayerInventory,
    carried: Option<ItemStack>,
    inventory_state_id: i32,
    model_customization: u8,
}

#[derive(Debug, Default)]
struct PlayPacketOutcome {
    packets: Vec<(i32, Bytes)>,
    center_changed: bool,
    batch_received: bool,
    world_changes: Vec<(WorldBlockPosition, BlockStateId)>,
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
            i32::from(event.state.raw()),
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

    let plains = vanilla_registries()?
        .biome_by_name("minecraft:plains")?
        .id();
    let mut sent = 0_i32;
    while sent < player.chunks_per_batch as i32 {
        let Some(position) = player.pending_chunks.pop_front() else {
            break;
        };
        let world = Arc::clone(world);
        let payload = tokio::task::spawn_blocking(move || {
            let chunk = world.chunk(position)?;
            Ok::<_, WorldError>(encode_chunk(
                position.x,
                position.z,
                i32::from(plains.raw()),
                |x, y, z| {
                    chunk
                        .block(x, y, z)
                        .map_or(0, |state| i32::from(state.raw()))
                },
            ))
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
            let information = decode_client_information(&mut reader)?;
            if information.model_customization != player.model_customization {
                player.model_customization = information.model_customization;
                outcome.packets.push((
                    play::clientbound::SET_ENTITY_DATA,
                    encode_player_skin_parts(1, player.model_customization),
                ));
            }
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
        play::serverbound::CONTAINER_CLICK => {
            handle_container_click(&mut reader, player, &mut outcome.packets)?;
        }
        play::serverbound::SET_CREATIVE_MODE_SLOT => {
            handle_creative_slot(&mut reader, player, &mut outcome.packets)?;
        }
        play::serverbound::PLAYER_ACTION => {
            outcome.world_changes.extend(handle_player_action(
                &mut reader,
                player,
                world,
                &mut outcome.packets,
            )?);
        }
        play::serverbound::USE_ITEM_ON => {
            outcome.world_changes.extend(handle_use_item_on(
                &mut reader,
                player,
                world,
                &mut outcome.packets,
            )?);
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
    packets: &mut Vec<(i32, Bytes)>,
) -> Result<(), ConnectionError> {
    let slot = reader.read_i16()?;
    let count = reader.read_var_i32()?;
    if !(0..=99).contains(&count) {
        return Err(ConnectionError::InvalidMovement);
    }
    let stack = if count == 0 {
        None
    } else {
        let item_id = reader.read_var_i32()?;
        let added_components = reader.read_var_i32()?;
        if added_components != 0 {
            debug!(
                added_components,
                "creative stack with unsupported added components rejected"
            );
            push_inventory_content(player, false, packets);
            return Ok(());
        }
        let removed_components = reader.read_var_i32()?;
        if removed_components != 0 {
            debug!(
                removed_components,
                "creative stack with unsupported removed components rejected"
            );
            push_inventory_content(player, false, packets);
            return Ok(());
        }
        let registries = vanilla_registries()?;
        let item = registries.item_by_protocol_id(item_id)?;
        Some(ItemStack::new(item.id(), count as u8)?)
    };
    reader.finish()?;
    if let (GameMode::Creative, Some(inventory_slot)) =
        (player.game_mode, menu_slot_to_inventory(slot))
    {
        player.inventory.set_slot(inventory_slot, stack)?;
        push_inventory_content(player, true, packets);
    } else {
        push_inventory_content(player, false, packets);
    }
    Ok(())
}

fn handle_container_click(
    reader: &mut PacketReader<'_>,
    player: &mut PlaySession,
    packets: &mut Vec<(i32, Bytes)>,
) -> Result<(), ConnectionError> {
    let container_id = reader.read_var_i32()?;
    let state_id = reader.read_var_i32()?;
    let slot = reader.read_i16()?;
    let button = reader.read_i8()?;
    let input = reader.read_var_i32()?;
    let changed_slots = reader.read_count("changed container slots", 128)?;
    for _ in 0..changed_slots {
        let _changed_slot = reader.read_i16()?;
        read_hashed_stack(reader)?;
    }
    read_hashed_stack(reader)?;
    reader.finish()?;

    let Some(inventory_slot) = menu_slot_to_inventory(slot) else {
        push_inventory_content(player, false, packets);
        return Ok(());
    };
    if container_id != 0 || state_id != player.inventory_state_id || input != 0 {
        push_inventory_content(player, false, packets);
        return Ok(());
    }
    let slot_stack = player.inventory.slot(inventory_slot)?;
    match button {
        0 => left_click(
            &mut player.carried,
            &mut player.inventory,
            inventory_slot,
            slot_stack,
        )?,
        1 => right_click(
            &mut player.carried,
            &mut player.inventory,
            inventory_slot,
            slot_stack,
        )?,
        _ => {
            push_inventory_content(player, false, packets);
            return Ok(());
        }
    }
    push_click_updates(player, slot, packets);
    Ok(())
}

fn read_hashed_stack(reader: &mut PacketReader<'_>) -> Result<(), ConnectionError> {
    if !reader.read_bool()? {
        return Ok(());
    }
    let _item = reader.read_var_i32()?;
    let count = reader.read_var_i32()?;
    if !(1..=99).contains(&count) {
        return Err(ConnectionError::InvalidMovement);
    }
    let added = reader.read_count("hashed stack added components", 256)?;
    for _ in 0..added {
        let _component = reader.read_var_i32()?;
        let _hash = reader.read_i32()?;
    }
    let removed = reader.read_count("hashed stack removed components", 256)?;
    for _ in 0..removed {
        let _component = reader.read_var_i32()?;
    }
    Ok(())
}

fn left_click(
    carried: &mut Option<ItemStack>,
    inventory: &mut PlayerInventory,
    slot: usize,
    slot_stack: Option<ItemStack>,
) -> Result<(), ConnectionError> {
    match (*carried, slot_stack) {
        (None, stack) => {
            *carried = stack;
            inventory.set_slot(slot, None)?;
        }
        (Some(stack), None) => {
            inventory.set_slot(slot, Some(stack))?;
            *carried = None;
        }
        (Some(mut cursor), Some(mut target)) if cursor.item() == target.item() => {
            let maximum = vanilla_registries()?.item(cursor.item())?.max_stack_size();
            let moved = maximum.saturating_sub(target.count()).min(cursor.count());
            target.set_count(target.count() + moved)?;
            inventory.set_slot(slot, Some(target))?;
            if moved == cursor.count() {
                *carried = None;
            } else {
                cursor.set_count(cursor.count() - moved)?;
                *carried = Some(cursor);
            }
        }
        (Some(cursor), Some(target)) => {
            inventory.set_slot(slot, Some(cursor))?;
            *carried = Some(target);
        }
    }
    Ok(())
}

fn right_click(
    carried: &mut Option<ItemStack>,
    inventory: &mut PlayerInventory,
    slot: usize,
    slot_stack: Option<ItemStack>,
) -> Result<(), ConnectionError> {
    match (*carried, slot_stack) {
        (None, Some(mut stack)) => {
            let taken = stack.count().div_ceil(2);
            *carried = Some(ItemStack::new(stack.item(), taken)?);
            if taken == stack.count() {
                inventory.set_slot(slot, None)?;
            } else {
                stack.set_count(stack.count() - taken)?;
                inventory.set_slot(slot, Some(stack))?;
            }
        }
        (Some(mut cursor), None) => {
            inventory.set_slot(slot, Some(ItemStack::new(cursor.item(), 1)?))?;
            if cursor.count() == 1 {
                *carried = None;
            } else {
                cursor.set_count(cursor.count() - 1)?;
                *carried = Some(cursor);
            }
        }
        (Some(mut cursor), Some(mut target)) if cursor.item() == target.item() => {
            let maximum = vanilla_registries()?.item(cursor.item())?.max_stack_size();
            if target.count() < maximum {
                target.set_count(target.count() + 1)?;
                inventory.set_slot(slot, Some(target))?;
                if cursor.count() == 1 {
                    *carried = None;
                } else {
                    cursor.set_count(cursor.count() - 1)?;
                    *carried = Some(cursor);
                }
            }
        }
        (Some(cursor), Some(target)) => {
            inventory.set_slot(slot, Some(cursor))?;
            *carried = Some(target);
        }
        (None, None) => {}
    }
    Ok(())
}

fn handle_player_action(
    reader: &mut PacketReader<'_>,
    player: &mut PlaySession,
    world: &World,
    packets: &mut Vec<(i32, Bytes)>,
) -> Result<Vec<(WorldBlockPosition, BlockStateId)>, ConnectionError> {
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
    let completes_break = match player.game_mode {
        GameMode::Creative => action == 0,
        GameMode::Survival => action == 2,
        GameMode::Adventure | GameMode::Spectator => false,
    };
    let current_state = world.block(world_position)?;
    let registries = vanilla_registries()?;
    let current_block = registries.block(registries.state(current_state)?.block())?;
    let mut world_changes = Vec::new();
    if completes_break
        && block_in_reach(player.position, world_position)
        && current_block.name().as_str() != "minecraft:air"
    {
        let companion = companion_for_break(world, world_position, current_state)?;
        let mut refresh_positions = vec![world_position];
        apply_block_change(
            world,
            BlockChange {
                position: world_position,
                state: BlockStateId::AIR,
            },
            packets,
            &mut world_changes,
        )?;
        if let Some(companion) = companion {
            refresh_positions.push(companion);
            apply_block_change(
                world,
                BlockChange {
                    position: companion,
                    state: BlockStateId::AIR,
                },
                packets,
                &mut world_changes,
            )?;
        }
        if current_block.name().as_str().ends_with("_stairs") {
            for change in refresh_stair_shapes(world, world_position)? {
                push_block_change(change, packets, &mut world_changes);
            }
        }
        for position in refresh_positions {
            for change in refresh_connectable_shapes(world, position)? {
                push_block_change(change, packets, &mut world_changes);
            }
        }
        if player.game_mode == GameMode::Survival {
            collect_broken_block(player, current_state, packets);
        }
    } else if action <= 2 {
        packets.push((
            play::clientbound::BLOCK_UPDATE,
            encode_block_update(position, i32::from(current_state.raw())),
        ));
    }
    packets.push((
        play::clientbound::BLOCK_CHANGED_ACK,
        encode_block_changed_ack(sequence),
    ));
    Ok(world_changes)
}

fn handle_use_item_on(
    reader: &mut PacketReader<'_>,
    player: &mut PlaySession,
    world: &World,
    packets: &mut Vec<(i32, Bytes)>,
) -> Result<Vec<(WorldBlockPosition, BlockStateId)>, ConnectionError> {
    let hand = reader.read_var_i32()?;
    let clicked = reader.read_block_position()?;
    let face = reader.read_var_i32()?;
    let cursor = [reader.read_f32()?, reader.read_f32()?, reader.read_f32()?];
    for coordinate in cursor {
        if !coordinate.is_finite() || !(0.0..=1.0).contains(&coordinate) {
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
    let world_clicked = WorldBlockPosition {
        x: clicked.x,
        y: clicked.y,
        z: clicked.z,
    };
    if player.game_mode != GameMode::Spectator
        && block_in_reach(player.position, world_clicked)
        && let Some(changes) = plan_interaction(world, world_clicked)?
    {
        let mut world_changes = Vec::new();
        for change in changes {
            apply_block_change(world, change, packets, &mut world_changes)?;
        }
        packets.push((
            play::clientbound::BLOCK_CHANGED_ACK,
            encode_block_changed_ack(sequence),
        ));
        return Ok(world_changes);
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
    let held = player.inventory.slot(player.selected_hotbar)?;
    let placement_plan = held
        .and_then(|stack| {
            let registries = vanilla_registries().ok()?;
            let block = registries.item(stack.item()).ok()?.block()?;
            Some(registries.block(block).ok()?.default_state())
        })
        .map(|state| {
            plan_placement(
                state,
                world_target,
                PlacementContext {
                    clicked_face: face,
                    cursor,
                    player_yaw: player.rotation[0],
                    player_pitch: player.rotation[1],
                },
            )
        })
        .transpose()?
        .flatten();
    let target_state = world.block(world_target)?;
    let mut can_place = hand == 0
        && player.game_mode.can_modify_blocks()
        && block_in_reach(player.position, world_target)
        && placement_plan.is_some();
    if let Some(plan) = &placement_plan {
        let registries = vanilla_registries()?;
        for change in &plan.occupied {
            let current = world.block(change.position)?;
            let block = registries.block(registries.state(current)?.block())?;
            can_place &= block.replaceable()
                && block_in_reach(player.position, change.position)
                && !player_intersects_block(player.position, change.position);
        }
    }

    let mut world_changes = Vec::new();
    if can_place {
        let plan = placement_plan.expect("checked above");
        let occupied_positions = plan
            .occupied
            .iter()
            .map(|change| change.position)
            .collect::<Vec<_>>();
        for change in plan.occupied {
            apply_block_change(world, change, packets, &mut world_changes)?;
        }
        if plan.refresh_stairs {
            for change in refresh_stair_shapes(world, world_target)? {
                push_block_change(change, packets, &mut world_changes);
            }
        }
        for position in occupied_positions {
            for change in refresh_connectable_shapes(world, position)? {
                push_block_change(change, packets, &mut world_changes);
            }
        }
        if player.game_mode == GameMode::Survival {
            consume_selected_block(player, packets);
        }
    } else {
        packets.push((
            play::clientbound::BLOCK_UPDATE,
            encode_block_update(target, i32::from(target_state.raw())),
        ));
    }
    packets.push((
        play::clientbound::BLOCK_CHANGED_ACK,
        encode_block_changed_ack(sequence),
    ));
    Ok(world_changes)
}

fn apply_block_change(
    world: &World,
    change: BlockChange,
    packets: &mut Vec<(i32, Bytes)>,
    world_changes: &mut Vec<(WorldBlockPosition, BlockStateId)>,
) -> Result<(), WorldError> {
    if world.set_block(change.position, change.state)? != change.state {
        push_block_change(change, packets, world_changes);
    }
    Ok(())
}

fn push_block_change(
    change: BlockChange,
    packets: &mut Vec<(i32, Bytes)>,
    world_changes: &mut Vec<(WorldBlockPosition, BlockStateId)>,
) {
    packets.push((
        play::clientbound::BLOCK_UPDATE,
        encode_block_update(
            toucan_protocol::BlockPosition {
                x: change.position.x,
                y: change.position.y,
                z: change.position.z,
            },
            i32::from(change.state.raw()),
        ),
    ));
    world_changes.push((change.position, change.state));
}

fn collect_broken_block(
    player: &mut PlaySession,
    block: BlockStateId,
    packets: &mut Vec<(i32, Bytes)>,
) {
    let Some(item_id) = block_item_id(block) else {
        return;
    };
    let Ok(registries) = vanilla_registries() else {
        return;
    };
    let Ok(item) = registries.item(item_id) else {
        return;
    };
    if player.inventory.add(item.id(), 1).ok() == Some(0) {
        push_inventory_content(player, true, packets);
    }
}

fn consume_selected_block(player: &mut PlaySession, packets: &mut Vec<(i32, Bytes)>) {
    if player.inventory.consume_one(player.selected_hotbar).is_ok() {
        push_inventory_content(player, true, packets);
    }
}

fn menu_slot_to_inventory(slot: i16) -> Option<usize> {
    match slot {
        9..=35 => Some(slot as usize),
        36..=44 => Some((slot - 36) as usize),
        _ => None,
    }
}

fn protocol_stack(stack: Option<ItemStack>) -> Option<ProtocolItemStack> {
    stack.map(|stack| ProtocolItemStack {
        item_id: i32::from(stack.item().raw()),
        count: stack.count(),
    })
}

fn encode_inventory_content(
    state_id: i32,
    inventory: &PlayerInventory,
    carried: Option<ItemStack>,
) -> Bytes {
    let mut slots = vec![None; 46];
    for menu_slot in 9_i16..=44 {
        let inventory_slot = menu_slot_to_inventory(menu_slot).expect("storage menu slot");
        slots[menu_slot as usize] = protocol_stack(inventory.slots()[inventory_slot]);
    }
    encode_container_set_content(state_id, &slots, protocol_stack(carried))
}

fn push_inventory_content(
    player: &mut PlaySession,
    changed: bool,
    packets: &mut Vec<(i32, Bytes)>,
) {
    if changed {
        player.inventory_state_id = player.inventory_state_id.wrapping_add(1);
    }
    packets.push((
        play::clientbound::CONTAINER_SET_CONTENT,
        encode_inventory_content(player.inventory_state_id, &player.inventory, player.carried),
    ));
}

fn push_click_updates(player: &mut PlaySession, menu_slot: i16, packets: &mut Vec<(i32, Bytes)>) {
    player.inventory_state_id = player.inventory_state_id.wrapping_add(1);
    let inventory_slot = menu_slot_to_inventory(menu_slot).expect("validated inventory menu slot");
    let stack = player.inventory.slots()[inventory_slot];
    packets.push((
        play::clientbound::CONTAINER_SET_SLOT,
        encode_container_set_slot(
            player.inventory_state_id,
            menu_slot,
            stack.map_or(0, ItemStack::count),
            stack.map(|stack| i32::from(stack.item().raw())),
        ),
    ));
    packets.push((
        play::clientbound::SET_CURSOR_ITEM,
        encode_set_cursor_item(protocol_stack(player.carried)),
    ));
}

fn inventory_for_persistence(
    player: &PlaySession,
) -> Result<Option<PlayerInventory>, ConnectionError> {
    let mut inventory = player.inventory.clone();
    let Some(carried) = player.carried else {
        return Ok(Some(inventory));
    };
    let remaining = inventory.add(carried.item(), carried.count())?;
    if remaining == 0 {
        Ok(Some(inventory))
    } else {
        debug!(
            remaining,
            "cursor stack could not fit in persistence snapshot; retaining previous safe snapshot"
        );
        Ok(None)
    }
}

fn block_in_reach(player: [f64; 3], block: WorldBlockPosition) -> bool {
    let dx = player[0] - (f64::from(block.x) + 0.5);
    let dy = (player[1] + 1.62) - (f64::from(block.y) + 0.5);
    let dz = player[2] - (f64::from(block.z) + 0.5);
    dx * dx + dy * dy + dz * dz <= 64.0
}

fn player_intersects_block(player: [f64; 3], block: WorldBlockPosition) -> bool {
    let block_x = f64::from(block.x);
    let block_y = f64::from(block.y);
    let block_z = f64::from(block.z);
    player[0] + 0.3 > block_x
        && player[0] - 0.3 < block_x + 1.0
        && player[1] + 1.8 > block_y
        && player[1] < block_y + 1.0
        && player[2] + 0.3 > block_z
        && player[2] - 0.3 < block_z + 1.0
}

fn block_item_id(state: BlockStateId) -> Option<ItemId> {
    let registries = vanilla_registries().ok()?;
    let block = registries.state(state).ok()?.block();
    registries.block(block).ok()?.item()
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
        PlacementContext, PlaySession, WorldEvent, desired_chunks, handle_play_packet,
        inventory_for_persistence, plan_placement, player_intersects_block, world_event_packet,
    };
    use crate::GameMode;
    use toucan_player::{ItemStack, PlayerInventory};
    use toucan_protocol::packet_id::play;
    use toucan_protocol::{BlockPosition, PacketWriter};
    use toucan_registry::vanilla_registries;
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
            inventory: PlayerInventory::default(),
            carried: None,
            inventory_state_id: 0,
            model_customization: 0x7f,
        }
    }

    fn break_packet(position: BlockPosition, action: i32, sequence: i32) -> bytes::Bytes {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(play::serverbound::PLAYER_ACTION);
        writer.write_var_i32(action);
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

    fn creative_slot_packet(slot: i16, item_id: i32) -> bytes::Bytes {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(play::serverbound::SET_CREATIVE_MODE_SLOT);
        writer.write_i16(slot);
        writer.write_var_i32(1);
        writer.write_var_i32(item_id);
        writer.write_var_i32(0);
        writer.write_var_i32(0);
        writer.into_bytes()
    }

    fn container_click_packet(state_id: i32, slot: i16, button: u8) -> bytes::Bytes {
        let mut writer = PacketWriter::new();
        writer.write_var_i32(play::serverbound::CONTAINER_CLICK);
        writer.write_var_i32(0);
        writer.write_var_i32(state_id);
        writer.write_i16(slot);
        writer.write_u8(button);
        writer.write_var_i32(0);
        writer.write_var_i32(0);
        writer.write_bool(false);
        writer.into_bytes()
    }

    fn item_block_state(item_id: i32) -> Option<BlockStateId> {
        let registries = vanilla_registries().ok()?;
        let block = registries.item_by_protocol_id(item_id).ok()?.block()?;
        Some(registries.block(block).ok()?.default_state())
    }

    fn placement_context(clicked_face: i32, player_yaw: f32, cursor_y: f32) -> PlacementContext {
        PlacementContext {
            clicked_face,
            cursor: [0.5, cursor_y, 0.5],
            player_yaw,
            player_pitch: 0.0,
        }
    }

    #[test]
    fn chunk_view_is_complete_and_center_first() {
        let chunks = desired_chunks(ChunkPosition { x: 8, z: -3 }, 2);
        assert_eq!(chunks.len(), 25);
        assert_eq!(chunks[0], ChunkPosition { x: 8, z: -3 });
    }

    #[test]
    fn common_creative_items_map_to_target_block_states() -> Result<(), Box<dyn Error>> {
        let oak = item_block_state(36);
        assert_eq!(oak, Some(BlockStateId::OAK_PLANKS));
        assert_eq!(oak.map(BlockStateId::raw), Some(15));
        assert_eq!(item_block_state(615).map(BlockStateId::raw), Some(15030));
        assert_eq!(item_block_state(630).map(BlockStateId::raw), Some(15045));
        assert_eq!(item_block_state(214).map(BlockStateId::raw), Some(2294));
        assert_eq!(item_block_state(999_999), None);
        assert_eq!(item_block_state(78).map(BlockStateId::raw), Some(5307));
        for (face, expected) in [
            (0, BlockStateId::OAK_LOG),
            (1, BlockStateId::OAK_LOG),
            (2, BlockStateId::OAK_LOG_Z),
            (3, BlockStateId::OAK_LOG_Z),
            (4, BlockStateId::OAK_LOG_X),
            (5, BlockStateId::OAK_LOG_X),
        ] {
            let plan = plan_placement(
                BlockStateId::OAK_LOG,
                WorldBlockPosition { x: 0, y: 64, z: 0 },
                placement_context(face, 0.0, 0.5),
            )?
            .expect("log placement plan");
            assert_eq!(plan.occupied[0].state, expected);
        }
        Ok(())
    }

    #[test]
    fn stair_placement_uses_player_direction_and_clicked_half() -> Result<(), Box<dyn Error>> {
        let registries = vanilla_registries()?;
        let stairs = registries
            .block_by_name("minecraft:oak_stairs")?
            .default_state();

        for (yaw, expected_facing) in [
            (0.0, "south"),
            (90.0, "west"),
            (180.0, "north"),
            (270.0, "east"),
        ] {
            let placed = plan_placement(
                stairs,
                WorldBlockPosition { x: 0, y: 64, z: 0 },
                placement_context(1, yaw, 1.0),
            )?
            .expect("stair placement plan")
            .occupied[0]
                .state;
            let properties = registries.state_properties(placed)?;
            assert!(properties.contains(&("facing", expected_facing.to_owned())));
            assert!(properties.contains(&("half", "bottom".to_owned())));
        }

        let upside_down = plan_placement(
            stairs,
            WorldBlockPosition { x: 0, y: 64, z: 0 },
            placement_context(2, 90.0, 0.75),
        )?
        .expect("stair placement plan")
        .occupied[0]
            .state;
        assert!(
            registries
                .state_properties(upside_down)?
                .contains(&("half", "top".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn survival_breaks_on_finish_but_adventure_cannot() -> Result<(), Box<dyn Error>> {
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
        let started = handle_play_packet(
            &break_packet(position, 0, 7),
            &mut pending_keep_alive,
            &mut survival,
            &world,
        )?;
        assert_eq!(world.block(world_position)?, BlockStateId::STONE);
        assert!(started.world_changes.is_empty());

        let outcome = handle_play_packet(
            &break_packet(position, 2, 8),
            &mut pending_keep_alive,
            &mut survival,
            &world,
        )?;
        assert_eq!(world.block(world_position)?, BlockStateId::AIR);
        assert_eq!(outcome.packets.len(), 3);
        assert_eq!(
            outcome.world_changes,
            vec![(world_position, BlockStateId::AIR)]
        );
        assert_eq!(survival.inventory.slot(0)?.map(ItemStack::count), Some(1));

        let placement_target = WorldBlockPosition { x: 1, y: 64, z: 0 };
        let placement = handle_play_packet(
            &use_item_on_packet(BlockPosition { x: 1, y: 63, z: 0 }, 9),
            &mut pending_keep_alive,
            &mut survival,
            &world,
        )?;
        assert_eq!(world.block(placement_target)?, BlockStateId::STONE);
        assert_eq!(
            placement.world_changes,
            vec![(placement_target, BlockStateId::STONE)]
        );
        assert_eq!(survival.inventory.slot(0)?, None);

        world.set_block(world_position, BlockStateId::STONE)?;
        let mut adventure = player(GameMode::Adventure);
        let outcome = handle_play_packet(
            &break_packet(position, 2, 10),
            &mut pending_keep_alive,
            &mut adventure,
            &world,
        )?;
        assert_eq!(world.block(world_position)?, BlockStateId::STONE);
        assert_eq!(outcome.packets.len(), 2);
        assert!(outcome.world_changes.is_empty());

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
        let mut pending_keep_alive = None;

        let inventory_outcome = handle_play_packet(
            &creative_slot_packet(36, 36),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert_eq!(inventory_outcome.packets.len(), 1);
        assert_eq!(
            creative.inventory.slot(0)?.map(ItemStack::item),
            Some(vanilla_registries()?.item_by_protocol_id(36)?.id())
        );

        let rejected = handle_play_packet(
            &use_item_on_packet(clicked, 10),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert!(rejected.world_changes.is_empty());
        assert_eq!(world.block(target)?, BlockStateId::AIR);
        assert!(player_intersects_block(creative.position, target));
        creative.position = [2.5, 64.0, 0.5];

        let outcome = handle_play_packet(
            &use_item_on_packet(clicked, 11),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;

        assert_eq!(world.block(target)?, BlockStateId::OAK_PLANKS);
        assert_eq!(outcome.packets.len(), 2);
        assert_eq!(
            outcome.world_changes,
            vec![(target, BlockStateId::OAK_PLANKS)]
        );
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn right_click_places_stairs_facing_west() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-network-stair-placement-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let clicked = BlockPosition { x: 0, y: 63, z: 0 };
        let target = WorldBlockPosition { x: 0, y: 64, z: 0 };
        let registries = vanilla_registries()?;
        let stairs = registries.block_by_name("minecraft:oak_stairs")?;
        let stairs_item = stairs.item().ok_or("oak stairs has no block item")?;
        let mut creative = player(GameMode::Creative);
        creative.position = [2.5, 64.0, 0.5];
        creative.rotation[0] = 90.0;
        let mut pending_keep_alive = None;

        handle_play_packet(
            &creative_slot_packet(36, i32::from(stairs_item.raw())),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        let outcome = handle_play_packet(
            &use_item_on_packet(clicked, 12),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;

        let placed = world.block(target)?;
        assert_eq!(registries.state(placed)?.block(), stairs.id());
        assert!(
            registries
                .state_properties(placed)?
                .contains(&("facing", "west".to_owned()))
        );
        assert!(outcome.world_changes.contains(&(target, placed)));
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn door_placement_and_break_are_atomic_pairs() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-network-door-placement-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let clicked = BlockPosition { x: 0, y: 63, z: 0 };
        let lower = WorldBlockPosition { x: 0, y: 64, z: 0 };
        let upper = WorldBlockPosition { x: 0, y: 65, z: 0 };
        let registries = vanilla_registries()?;
        let door = registries.block_by_name("minecraft:oak_door")?;
        let door_item = door.item().ok_or("oak door has no block item")?;
        let mut creative = player(GameMode::Creative);
        creative.position = [2.5, 64.0, 0.5];
        creative.rotation[0] = 90.0;
        let mut pending_keep_alive = None;

        handle_play_packet(
            &creative_slot_packet(36, i32::from(door_item.raw())),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        let placed = handle_play_packet(
            &use_item_on_packet(clicked, 13),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert_eq!(placed.world_changes.len(), 2);
        assert_eq!(
            registries.state(world.block(lower)?)?.block(),
            registries.state(world.block(upper)?)?.block()
        );
        assert!(
            registries
                .state_properties(world.block(lower)?)?
                .contains(&("half", "lower".to_owned()))
        );
        assert!(
            registries
                .state_properties(world.block(upper)?)?
                .contains(&("half", "upper".to_owned()))
        );

        let opened = handle_play_packet(
            &use_item_on_packet(
                BlockPosition {
                    x: lower.x,
                    y: lower.y,
                    z: lower.z,
                },
                14,
            ),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert_eq!(opened.world_changes.len(), 2);
        assert!(
            registries
                .state_properties(world.block(lower)?)?
                .contains(&("open", "true".to_owned()))
        );
        assert!(
            registries
                .state_properties(world.block(upper)?)?
                .contains(&("open", "true".to_owned()))
        );
        let closed = handle_play_packet(
            &use_item_on_packet(
                BlockPosition {
                    x: upper.x,
                    y: upper.y,
                    z: upper.z,
                },
                15,
            ),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert_eq!(closed.world_changes.len(), 2);
        assert!(
            registries
                .state_properties(world.block(lower)?)?
                .contains(&("open", "false".to_owned()))
        );

        let broken = handle_play_packet(
            &break_packet(
                BlockPosition {
                    x: upper.x,
                    y: upper.y,
                    z: upper.z,
                },
                0,
                16,
            ),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert_eq!(broken.world_changes.len(), 2);
        assert_eq!(world.block(lower)?, BlockStateId::AIR);
        assert_eq!(world.block(upper)?, BlockStateId::AIR);

        world.set_block(upper, BlockStateId::STONE)?;
        let rejected = handle_play_packet(
            &use_item_on_packet(clicked, 17),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert!(rejected.world_changes.is_empty());
        assert_eq!(world.block(lower)?, BlockStateId::AIR);
        assert_eq!(world.block(upper)?, BlockStateId::STONE);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn fence_placement_and_break_refresh_connections() -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-network-fence-connection-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let first_support = BlockPosition { x: 0, y: 63, z: 0 };
        let second_support = BlockPosition { x: 1, y: 63, z: 0 };
        let first = WorldBlockPosition { x: 0, y: 64, z: 0 };
        let second = WorldBlockPosition { x: 1, y: 64, z: 0 };
        let registries = vanilla_registries()?;
        let fence = registries.block_by_name("minecraft:oak_fence")?;
        let fence_item = fence.item().ok_or("oak fence has no block item")?;
        let mut creative = player(GameMode::Creative);
        creative.position = [3.5, 64.0, 0.5];
        let mut pending_keep_alive = None;

        handle_play_packet(
            &creative_slot_packet(36, i32::from(fence_item.raw())),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        handle_play_packet(
            &use_item_on_packet(first_support, 18),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        let connected = handle_play_packet(
            &use_item_on_packet(second_support, 19),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert!(
            connected
                .world_changes
                .iter()
                .any(|change| change.0 == first)
        );
        assert!(
            registries
                .state_properties(world.block(first)?)?
                .contains(&("east", "true".to_owned()))
        );
        assert!(
            registries
                .state_properties(world.block(second)?)?
                .contains(&("west", "true".to_owned()))
        );

        let disconnected = handle_play_packet(
            &break_packet(
                BlockPosition {
                    x: second.x,
                    y: second.y,
                    z: second.z,
                },
                0,
                20,
            ),
            &mut pending_keep_alive,
            &mut creative,
            &world,
        )?;
        assert!(
            disconnected
                .world_changes
                .iter()
                .any(|change| change.0 == first)
        );
        assert!(
            registries
                .state_properties(world.block(first)?)?
                .contains(&("east", "false".to_owned()))
        );
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn player_inventory_clicks_are_authoritative_and_reject_stale_state()
    -> Result<(), Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "toucan-network-inventory-test-{}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        let world = World::open_or_create(&path, 25, GeneratorKind::Flat, 42)?;
        let stone = vanilla_registries()?.item_by_name("minecraft:stone")?.id();
        let mut player = player(GameMode::Survival);
        player
            .inventory
            .set_slot(0, Some(ItemStack::new(stone, 5)?))?;
        let mut pending_keep_alive = None;

        let picked_up = handle_play_packet(
            &container_click_packet(0, 36, 0),
            &mut pending_keep_alive,
            &mut player,
            &world,
        )?;
        assert_eq!(picked_up.packets.len(), 2);
        assert_eq!(
            picked_up.packets[0].0,
            play::clientbound::CONTAINER_SET_SLOT
        );
        assert_eq!(picked_up.packets[1].0, play::clientbound::SET_CURSOR_ITEM);
        assert_eq!(player.inventory_state_id, 1);
        assert_eq!(player.carried.map(ItemStack::count), Some(5));
        assert_eq!(player.inventory.slot(0)?, None);
        let safe_snapshot = inventory_for_persistence(&player)?.ok_or("missing safe snapshot")?;
        assert_eq!(safe_snapshot.slot(0)?.map(ItemStack::count), Some(5));

        let placed = handle_play_packet(
            &container_click_packet(1, 37, 0),
            &mut pending_keep_alive,
            &mut player,
            &world,
        )?;
        assert_eq!(placed.packets.len(), 2);
        assert_eq!(placed.packets[0].0, play::clientbound::CONTAINER_SET_SLOT);
        assert_eq!(placed.packets[1].0, play::clientbound::SET_CURSOR_ITEM);
        assert_eq!(player.inventory_state_id, 2);
        assert_eq!(player.carried, None);
        assert_eq!(player.inventory.slot(0)?, None);
        assert_eq!(player.inventory.slot(1)?.map(ItemStack::count), Some(5));

        let stale = handle_play_packet(
            &container_click_packet(1, 36, 0),
            &mut pending_keep_alive,
            &mut player,
            &world,
        )?;
        assert_eq!(stale.packets.len(), 1);
        assert_eq!(player.inventory_state_id, 2);
        assert_eq!(player.carried, None);
        assert_eq!(player.inventory.slot(1)?.map(ItemStack::count), Some(5));
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
