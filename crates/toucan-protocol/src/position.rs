#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockPosition {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPosition {
    const HORIZONTAL_MASK: i64 = 0x3ff_ffff;
    const VERTICAL_MASK: i64 = 0xfff;

    #[must_use]
    pub const fn pack(self) -> i64 {
        ((self.x as i64 & Self::HORIZONTAL_MASK) << 38)
            | ((self.z as i64 & Self::HORIZONTAL_MASK) << 12)
            | (self.y as i64 & Self::VERTICAL_MASK)
    }

    #[must_use]
    pub const fn unpack(value: i64) -> Self {
        Self {
            x: (value >> 38) as i32,
            y: (value << 52 >> 52) as i32,
            z: (value << 26 >> 38) as i32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BlockPosition;

    #[test]
    fn packed_positions_round_trip_at_boundaries() {
        for position in [
            BlockPosition { x: 0, y: 0, z: 0 },
            BlockPosition {
                x: 12,
                y: -64,
                z: -45,
            },
            BlockPosition {
                x: 33_554_431,
                y: 2_047,
                z: -33_554_432,
            },
            BlockPosition {
                x: -33_554_432,
                y: -2_048,
                z: 33_554_431,
            },
        ] {
            assert_eq!(BlockPosition::unpack(position.pack()), position);
        }
    }
}
