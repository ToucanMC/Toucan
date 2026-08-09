//! Protocol-775 packet identifiers grouped by connection state and direction.

/// Login-state packet IDs.
pub mod login {
    /// Serverbound Login packets.
    pub mod serverbound {
        /// Login Start / Hello.
        pub const HELLO: i32 = 0x00;
        /// Encryption response.
        pub const KEY: i32 = 0x01;
        /// Custom query response.
        pub const CUSTOM_QUERY_ANSWER: i32 = 0x02;
        /// Login acknowledged.
        pub const ACKNOWLEDGED: i32 = 0x03;
        /// Cookie response.
        pub const COOKIE_RESPONSE: i32 = 0x04;
    }

    /// Clientbound Login packets.
    pub mod clientbound {
        /// Login disconnect.
        pub const DISCONNECT: i32 = 0x00;
        /// Encryption request.
        pub const HELLO: i32 = 0x01;
        /// Login success.
        pub const FINISHED: i32 = 0x02;
        /// Compression negotiation.
        pub const COMPRESSION: i32 = 0x03;
        /// Custom query.
        pub const CUSTOM_QUERY: i32 = 0x04;
        /// Cookie request.
        pub const COOKIE_REQUEST: i32 = 0x05;
    }
}

/// Configuration-state packet IDs.
pub mod configuration {
    /// Serverbound Configuration packets.
    pub mod serverbound {
        /// Client settings.
        pub const CLIENT_INFORMATION: i32 = 0x00;
        /// Cookie response.
        pub const COOKIE_RESPONSE: i32 = 0x01;
        /// Plugin payload.
        pub const CUSTOM_PAYLOAD: i32 = 0x02;
        /// Configuration finished acknowledgement.
        pub const FINISH: i32 = 0x03;
        /// Keep-alive response.
        pub const KEEP_ALIVE: i32 = 0x04;
        /// Pong response.
        pub const PONG: i32 = 0x05;
        /// Resource-pack response.
        pub const RESOURCE_PACK: i32 = 0x06;
        /// Known-pack selection.
        pub const SELECT_KNOWN_PACKS: i32 = 0x07;
    }

    /// Clientbound Configuration packets.
    pub mod clientbound {
        /// Plugin payload.
        pub const CUSTOM_PAYLOAD: i32 = 0x01;
        /// Disconnect.
        pub const DISCONNECT: i32 = 0x02;
        /// Finish Configuration.
        pub const FINISH: i32 = 0x03;
        /// Registry synchronization data.
        pub const REGISTRY_DATA: i32 = 0x07;
        /// Enabled feature flags.
        pub const ENABLED_FEATURES: i32 = 0x0c;
        /// Registry tags.
        pub const UPDATE_TAGS: i32 = 0x0d;
        /// Known-pack negotiation request.
        pub const SELECT_KNOWN_PACKS: i32 = 0x0e;
    }
}

/// Play-state packet IDs required at the Login/Configuration boundary.
pub mod play {
    /// Serverbound Play packets.
    pub mod serverbound {
        /// Confirms a server position synchronization.
        pub const ACCEPT_TELEPORTATION: i32 = 0x00;
        /// Reports completion of a chunk batch.
        pub const CHUNK_BATCH_RECEIVED: i32 = 0x0b;
        /// Marks the end of a client simulation tick.
        pub const CLIENT_TICK_END: i32 = 0x0d;
        /// Updated client preferences while in Play.
        pub const CLIENT_INFORMATION: i32 = 0x0e;
        /// Namespaced mod/plugin payload.
        pub const CUSTOM_PAYLOAD: i32 = 0x16;
        /// Keep-alive response.
        pub const KEEP_ALIVE: i32 = 0x1c;
        /// Absolute player position.
        pub const MOVE_PLAYER_POS: i32 = 0x1e;
        /// Absolute player position and rotation.
        pub const MOVE_PLAYER_POS_ROT: i32 = 0x1f;
        /// Player rotation.
        pub const MOVE_PLAYER_ROT: i32 = 0x20;
        /// On-ground and collision flags only.
        pub const MOVE_PLAYER_STATUS_ONLY: i32 = 0x21;
        /// Starts, aborts, or completes a block-breaking action.
        pub const PLAYER_ACTION: i32 = 0x29;
        /// Signals that the client finished loading the player.
        pub const PLAYER_LOADED: i32 = 0x2c;
        /// Selects one of the nine hotbar slots.
        pub const SET_CARRIED_ITEM: i32 = 0x35;
        /// Changes a creative inventory slot.
        pub const SET_CREATIVE_MODE_SLOT: i32 = 0x38;
        /// Uses the held item against a block face.
        pub const USE_ITEM_ON: i32 = 0x42;
    }

    /// Clientbound Play packets.
    pub mod clientbound {
        /// Acknowledges a sequenced block interaction.
        pub const BLOCK_CHANGED_ACK: i32 = 0x04;
        /// Updates one block state.
        pub const BLOCK_UPDATE: i32 = 0x08;
        /// Starts one chunk batch.
        pub const CHUNK_BATCH_START: i32 = 0x0c;
        /// Disconnect with a network-NBT text component.
        pub const DISCONNECT: i32 = 0x20;
        /// Small game-state event.
        pub const GAME_EVENT: i32 = 0x26;
        /// Unloads one chunk from the client cache.
        pub const FORGET_LEVEL_CHUNK: i32 = 0x25;
        /// Keep-alive request.
        pub const KEEP_ALIVE: i32 = 0x2c;
        /// Chunk data and lighting.
        pub const LEVEL_CHUNK_WITH_LIGHT: i32 = 0x2d;
        /// Initial Join Game packet.
        pub const LOGIN: i32 = 0x31;
        /// Player abilities and movement speeds.
        pub const PLAYER_ABILITIES: i32 = 0x40;
        /// Synchronizes the authoritative player position.
        pub const PLAYER_POSITION: i32 = 0x48;
        /// Updates the chunk-cache center.
        pub const SET_CHUNK_CACHE_CENTER: i32 = 0x5e;
        /// Updates the chunk-cache radius.
        pub const SET_CHUNK_CACHE_RADIUS: i32 = 0x5f;
        /// Sets the default respawn position.
        pub const SET_DEFAULT_SPAWN_POSITION: i32 = 0x61;
        /// Finishes one chunk batch and carries its chunk count.
        pub const CHUNK_BATCH_FINISHED: i32 = 0x0b;
    }
}
