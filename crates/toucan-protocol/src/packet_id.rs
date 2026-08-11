pub mod login {
    pub mod serverbound {
        pub const HELLO: i32 = 0x00;
        pub const KEY: i32 = 0x01;
        pub const CUSTOM_QUERY_ANSWER: i32 = 0x02;
        pub const ACKNOWLEDGED: i32 = 0x03;
        pub const COOKIE_RESPONSE: i32 = 0x04;
    }

    pub mod clientbound {
        pub const DISCONNECT: i32 = 0x00;
        pub const HELLO: i32 = 0x01;
        pub const FINISHED: i32 = 0x02;
        pub const COMPRESSION: i32 = 0x03;
        pub const CUSTOM_QUERY: i32 = 0x04;
        pub const COOKIE_REQUEST: i32 = 0x05;
    }
}

pub mod configuration {
    pub mod serverbound {
        pub const CLIENT_INFORMATION: i32 = 0x00;
        pub const COOKIE_RESPONSE: i32 = 0x01;
        pub const CUSTOM_PAYLOAD: i32 = 0x02;
        pub const FINISH: i32 = 0x03;
        pub const KEEP_ALIVE: i32 = 0x04;
        pub const PONG: i32 = 0x05;
        pub const RESOURCE_PACK: i32 = 0x06;
        pub const SELECT_KNOWN_PACKS: i32 = 0x07;
    }

    pub mod clientbound {
        pub const CUSTOM_PAYLOAD: i32 = 0x01;
        pub const DISCONNECT: i32 = 0x02;
        pub const FINISH: i32 = 0x03;
        pub const REGISTRY_DATA: i32 = 0x07;
        pub const ENABLED_FEATURES: i32 = 0x0c;
        pub const UPDATE_TAGS: i32 = 0x0d;
        pub const SELECT_KNOWN_PACKS: i32 = 0x0e;
    }
}

pub mod play {
    pub mod serverbound {
        pub const ACCEPT_TELEPORTATION: i32 = 0x00;
        pub const CHUNK_BATCH_RECEIVED: i32 = 0x0b;
        pub const CLIENT_TICK_END: i32 = 0x0d;
        pub const CLIENT_INFORMATION: i32 = 0x0e;
        pub const CUSTOM_PAYLOAD: i32 = 0x16;
        pub const KEEP_ALIVE: i32 = 0x1c;
        pub const MOVE_PLAYER_POS: i32 = 0x1e;
        pub const MOVE_PLAYER_POS_ROT: i32 = 0x1f;
        pub const MOVE_PLAYER_ROT: i32 = 0x20;
        pub const MOVE_PLAYER_STATUS_ONLY: i32 = 0x21;
        pub const PLAYER_ACTION: i32 = 0x29;
        pub const PLAYER_LOADED: i32 = 0x2c;
        pub const SET_CARRIED_ITEM: i32 = 0x35;
        pub const SET_CREATIVE_MODE_SLOT: i32 = 0x38;
        pub const USE_ITEM_ON: i32 = 0x42;
    }

    pub mod clientbound {
        pub const BLOCK_CHANGED_ACK: i32 = 0x04;
        pub const BLOCK_UPDATE: i32 = 0x08;
        pub const CHUNK_BATCH_START: i32 = 0x0c;
        pub const DISCONNECT: i32 = 0x20;
        pub const GAME_EVENT: i32 = 0x26;
        pub const FORGET_LEVEL_CHUNK: i32 = 0x25;
        pub const KEEP_ALIVE: i32 = 0x2c;
        pub const LEVEL_CHUNK_WITH_LIGHT: i32 = 0x2d;
        pub const LOGIN: i32 = 0x31;
        pub const PLAYER_ABILITIES: i32 = 0x40;
        pub const PLAYER_POSITION: i32 = 0x48;
        pub const PLAYER_INFO_UPDATE: i32 = 0x46;
        pub const SET_ENTITY_DATA: i32 = 0x63;
        pub const SET_CHUNK_CACHE_CENTER: i32 = 0x5e;
        pub const SET_CHUNK_CACHE_RADIUS: i32 = 0x5f;
        pub const SET_DEFAULT_SPAWN_POSITION: i32 = 0x61;
        pub const CHUNK_BATCH_FINISHED: i32 = 0x0b;
        pub const CONTAINER_SET_SLOT: i32 = 0x14;
    }
}
