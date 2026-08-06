use std::io::{Read, Write};

use bytes::{Bytes, BytesMut};
use flate2::Compression;
use flate2::bufread::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::{PacketWriter, ProtocolError, decode_var_i32, encode_var_i32};

/// Decodes one outer frame after compression has been negotiated.
pub fn decode_compressed_packet(
    frame: &[u8],
    threshold: usize,
    max_uncompressed: usize,
) -> Result<Bytes, ProtocolError> {
    let mut input = frame;
    let declared = decode_var_i32(&mut input)?;
    let declared = usize::try_from(declared).map_err(|_| ProtocolError::NegativeLength {
        kind: "uncompressed packet",
        value: declared,
    })?;

    if declared == 0 {
        if input.len() >= threshold {
            return Err(ProtocolError::UncompressedAboveThreshold {
                actual: input.len(),
                threshold,
            });
        }
        if input.len() > max_uncompressed {
            return Err(ProtocolError::LengthLimit {
                kind: "uncompressed packet",
                actual: input.len(),
                limit: max_uncompressed,
            });
        }
        return Ok(Bytes::copy_from_slice(input));
    }
    if declared < threshold {
        return Err(ProtocolError::CompressionBelowThreshold {
            declared,
            threshold,
        });
    }
    if declared > max_uncompressed {
        return Err(ProtocolError::LengthLimit {
            kind: "decompressed packet",
            actual: declared,
            limit: max_uncompressed,
        });
    }

    let mut decoder = ZlibDecoder::new(input);
    let mut output = Vec::with_capacity(declared.min(64 * 1024));
    decoder
        .by_ref()
        .take(max_uncompressed as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|error| ProtocolError::Decompression(error.to_string()))?;
    if output.len() != declared {
        return Err(ProtocolError::DecompressedLengthMismatch {
            actual: output.len(),
            declared,
        });
    }
    let consumed = decoder.total_in() as usize;
    if consumed != input.len() {
        return Err(ProtocolError::TrailingCompressedData(
            input.len() - consumed,
        ));
    }
    Ok(Bytes::from(output))
}

/// Encodes one packet using negotiated compression framing.
pub fn encode_compressed_packet(
    packet_id: i32,
    payload: &[u8],
    threshold: usize,
    max_uncompressed: usize,
) -> Result<Bytes, ProtocolError> {
    let mut body = PacketWriter::new();
    body.write_var_i32(packet_id);
    body.write_bytes(payload);
    let body = body.into_bytes();
    if body.len() > max_uncompressed {
        return Err(ProtocolError::LengthLimit {
            kind: "uncompressed packet",
            actual: body.len(),
            limit: max_uncompressed,
        });
    }

    let mut compressed_body = BytesMut::new();
    if body.len() >= threshold {
        let declared = i32::try_from(body.len()).map_err(|_| ProtocolError::LengthLimit {
            kind: "uncompressed packet",
            actual: body.len(),
            limit: i32::MAX as usize,
        })?;
        encode_var_i32(declared, &mut compressed_body);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(&body)
            .map_err(|error| ProtocolError::Decompression(error.to_string()))?;
        let compressed = encoder
            .finish()
            .map_err(|error| ProtocolError::Decompression(error.to_string()))?;
        compressed_body.extend_from_slice(&compressed);
    } else {
        encode_var_i32(0, &mut compressed_body);
        compressed_body.extend_from_slice(&body);
    }

    let length = i32::try_from(compressed_body.len()).map_err(|_| ProtocolError::LengthLimit {
        kind: "compressed packet frame",
        actual: compressed_body.len(),
        limit: i32::MAX as usize,
    })?;
    let mut framed = BytesMut::new();
    encode_var_i32(length, &mut framed);
    framed.extend_from_slice(&compressed_body);
    Ok(framed.freeze())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{decode_compressed_packet, encode_compressed_packet};
    use crate::{FrameDecoder, ProtocolError};

    fn outer_body(packet: &[u8]) -> Bytes {
        let mut decoder = FrameDecoder::new(4096);
        if decoder.push(packet).is_err() {
            return Bytes::new();
        }
        decoder.try_next().ok().flatten().unwrap_or_default()
    }

    #[test]
    fn below_threshold_packet_uses_zero_data_length() {
        let packet = encode_compressed_packet(1, &[2, 3], 256, 4096).unwrap_or_default();
        let frame = outer_body(&packet);
        assert_eq!(frame.first(), Some(&0));
        assert_eq!(
            decode_compressed_packet(&frame, 256, 4096).as_deref(),
            Ok([1, 2, 3].as_slice())
        );
    }

    #[test]
    fn threshold_packet_round_trips_through_zlib() {
        let payload = vec![0x5a; 512];
        let packet = encode_compressed_packet(7, &payload, 64, 4096).unwrap_or_default();
        let frame = outer_body(&packet);
        let decoded = decode_compressed_packet(&frame, 64, 4096).unwrap_or_default();
        assert_eq!(decoded.first(), Some(&7));
        assert_eq!(&decoded[1..], payload);
    }

    #[test]
    fn rejects_decompression_bombs_before_allocating_declared_size() {
        let frame = [0x81, 0x20, 0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert!(matches!(
            decode_compressed_packet(&frame, 64, 1024),
            Err(ProtocolError::LengthLimit {
                kind: "decompressed packet",
                ..
            })
        ));
    }

    #[test]
    fn rejects_uncompressed_packets_at_threshold() {
        let frame = [0x00, 0x01, 0x02, 0x03];
        assert!(matches!(
            decode_compressed_packet(&frame, 3, 1024),
            Err(ProtocolError::UncompressedAboveThreshold { .. })
        ));
    }
}
