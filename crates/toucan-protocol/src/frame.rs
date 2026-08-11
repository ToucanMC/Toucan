use bytes::{Buf, Bytes, BytesMut};

use crate::{ProtocolError, encode_var_i32};

const MAX_VARINT_BYTES: usize = 5;

#[derive(Debug)]
pub struct FrameDecoder {
    buffer: BytesMut,
    max_frame_length: usize,
}

impl FrameDecoder {
    #[must_use]
    pub fn new(max_frame_length: usize) -> Self {
        Self {
            buffer: BytesMut::new(),
            max_frame_length,
        }
    }

    #[must_use]
    pub fn remaining_capacity(&self) -> usize {
        self.max_frame_length
            .saturating_add(MAX_VARINT_BYTES)
            .saturating_sub(self.buffer.len())
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    #[must_use]
    pub const fn max_frame_length(&self) -> usize {
        self.max_frame_length
    }

    pub fn push(&mut self, input: &[u8]) -> Result<(), ProtocolError> {
        let limit = self.max_frame_length.saturating_add(MAX_VARINT_BYTES);
        let new_length =
            self.buffer
                .len()
                .checked_add(input.len())
                .ok_or(ProtocolError::LengthLimit {
                    kind: "packet buffer",
                    actual: usize::MAX,
                    limit,
                })?;
        if new_length > limit {
            return Err(ProtocolError::LengthLimit {
                kind: "packet buffer",
                actual: new_length,
                limit,
            });
        }
        self.buffer.extend_from_slice(input);
        Ok(())
    }

    pub fn try_next(&mut self) -> Result<Option<Bytes>, ProtocolError> {
        let Some((frame_length, prefix_length)) = inspect_length_prefix(&self.buffer)? else {
            return Ok(None);
        };
        if frame_length > self.max_frame_length {
            return Err(ProtocolError::LengthLimit {
                kind: "packet frame",
                actual: frame_length,
                limit: self.max_frame_length,
            });
        }
        let total_length =
            prefix_length
                .checked_add(frame_length)
                .ok_or(ProtocolError::LengthLimit {
                    kind: "packet frame",
                    actual: usize::MAX,
                    limit: self.max_frame_length,
                })?;
        if self.buffer.len() < total_length {
            return Ok(None);
        }

        let mut frame = self.buffer.split_to(total_length).freeze();
        frame.advance(prefix_length);
        Ok(Some(frame))
    }
}

fn inspect_length_prefix(buffer: &[u8]) -> Result<Option<(usize, usize)>, ProtocolError> {
    let mut value = 0_u32;
    for index in 0..MAX_VARINT_BYTES {
        let Some(&byte) = buffer.get(index) else {
            return Ok(None);
        };
        if index == 4 && byte & 0xf0 != 0 {
            return Err(ProtocolError::MalformedVarInt);
        }
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            let signed = value as i32;
            let length = usize::try_from(signed).map_err(|_| ProtocolError::NegativeLength {
                kind: "packet frame",
                value: signed,
            })?;
            return Ok(Some((length, index + 1)));
        }
    }
    Err(ProtocolError::MalformedVarInt)
}

pub fn encode_packet(packet_id: i32, payload: &[u8]) -> Result<Bytes, ProtocolError> {
    let mut body = BytesMut::with_capacity(MAX_VARINT_BYTES + payload.len());
    encode_var_i32(packet_id, &mut body);
    body.extend_from_slice(payload);
    let body_length = i32::try_from(body.len()).map_err(|_| ProtocolError::LengthLimit {
        kind: "packet frame",
        actual: body.len(),
        limit: i32::MAX as usize,
    })?;

    let mut framed = BytesMut::with_capacity(MAX_VARINT_BYTES + body.len());
    encode_var_i32(body_length, &mut framed);
    framed.extend_from_slice(&body);
    Ok(framed.freeze())
}

#[cfg(test)]
mod tests {
    use super::{FrameDecoder, encode_packet};
    use crate::ProtocolError;

    #[test]
    fn decodes_fixed_status_request_fixture() {
        let mut decoder = FrameDecoder::new(1024);
        assert!(decoder.push(&[0x01, 0x00]).is_ok());
        let frame = decoder.try_next();
        assert_eq!(
            frame.as_ref().ok().and_then(Option::as_deref),
            Some([0x00].as_slice())
        );
    }

    #[test]
    fn retains_partial_frames() {
        let packet = encode_packet(1, &[0, 1, 2, 3]).unwrap_or_default();
        let mut decoder = FrameDecoder::new(1024);
        assert!(decoder.push(&packet[..2]).is_ok());
        assert_eq!(decoder.try_next(), Ok(None));
        assert!(decoder.push(&packet[2..]).is_ok());
        assert_eq!(
            decoder.try_next().ok().flatten().as_deref(),
            Some([1, 0, 1, 2, 3].as_slice())
        );
    }

    #[test]
    fn decodes_multiple_buffered_frames() {
        let first = encode_packet(0, &[]).unwrap_or_default();
        let second = encode_packet(1, &[7]).unwrap_or_default();
        let mut decoder = FrameDecoder::new(1024);
        assert!(
            decoder
                .push(&[first.as_ref(), second.as_ref()].concat())
                .is_ok()
        );
        assert_eq!(
            decoder.try_next().ok().flatten().as_deref(),
            Some([0].as_slice())
        );
        assert_eq!(
            decoder.try_next().ok().flatten().as_deref(),
            Some([1, 7].as_slice())
        );
    }

    #[test]
    fn rejects_oversized_and_malformed_lengths() {
        let mut oversized = FrameDecoder::new(4);
        assert!(oversized.push(&[0x05]).is_ok());
        assert!(matches!(
            oversized.try_next(),
            Err(ProtocolError::LengthLimit { .. })
        ));

        let mut malformed = FrameDecoder::new(1024);
        assert!(malformed.push(&[0xff, 0xff, 0xff, 0xff, 0x10]).is_ok());
        assert_eq!(malformed.try_next(), Err(ProtocolError::MalformedVarInt));

        let mut negative = FrameDecoder::new(1024);
        assert!(negative.push(&[0xff, 0xff, 0xff, 0xff, 0x0f]).is_ok());
        assert!(matches!(
            negative.try_next(),
            Err(ProtocolError::NegativeLength {
                kind: "packet frame",
                value: -1
            })
        ));
    }
}
