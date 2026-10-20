//! Stream framing: each message is a protobuf varint length prefix followed
//! by the body. Datagrams carry exactly one frame body without a prefix.

use bytes::{Buf, Bytes, BytesMut};

use crate::{MAX_FRAME_LEN, ProtoError};

/// Longest valid varint (10 bytes for u64).
const MAX_VARINT_LEN: usize = 10;

/// Prefixes `body` with its varint length.
pub fn encode(body: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(body.len() + MAX_VARINT_LEN);
    prost::encoding::encode_varint(body.len() as u64, &mut buf);
    buf.extend_from_slice(body);
    buf.freeze()
}

/// Reassembles frames from arbitrary stream chunks.
#[derive(Debug, Default)]
pub struct FrameBuffer {
    buf: BytesMut,
}

impl FrameBuffer {
    pub fn new() -> FrameBuffer {
        FrameBuffer::default()
    }

    /// Appends received bytes.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Bytes buffered but not yet returned as frames.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// The next complete frame body, `Ok(None)` if more bytes are needed.
    /// A length above [`MAX_FRAME_LEN`] (or a malformed prefix) is rejected
    /// as soon as the prefix is read, before the body is buffered.
    pub fn next_frame(&mut self) -> Result<Option<Bytes>, ProtoError> {
        let mut len: u64 = 0;
        let mut prefix = 0;
        loop {
            let Some(&byte) = self.buf.get(prefix) else {
                return Ok(None);
            };
            // The tenth byte has room for one bit and must end the varint.
            if prefix == MAX_VARINT_LEN - 1 && byte > 1 {
                return Err(ProtoError::FrameTooLarge { len: usize::MAX });
            }
            len |= u64::from(byte & 0x7F) << (7 * prefix);
            prefix += 1;
            if len > MAX_FRAME_LEN as u64 {
                return Err(ProtoError::FrameTooLarge {
                    len: usize::try_from(len).unwrap_or(usize::MAX),
                });
            }
            if byte & 0x80 == 0 {
                break;
            }
        }
        let len = len as usize;
        if self.buf.len() - prefix < len {
            return Ok(None);
        }
        self.buf.advance(prefix);
        Ok(Some(self.buf.split_to(len).freeze()))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn single_frame_split_across_chunks() {
        let frame = encode(b"hello world");
        let mut fb = FrameBuffer::new();
        for b in frame.iter() {
            assert_eq!(fb.next_frame().unwrap(), None);
            fb.push(&[*b]);
        }
        assert_eq!(
            fb.next_frame().unwrap().as_deref(),
            Some(&b"hello world"[..])
        );
        assert_eq!(fb.next_frame().unwrap(), None);
        assert_eq!(fb.buffered(), 0);
    }

    #[test]
    fn several_frames_in_one_chunk() {
        let mut all = Vec::new();
        for body in [&b"a"[..], &b""[..], &[7u8; 300][..]] {
            all.extend_from_slice(&encode(body));
        }
        let mut fb = FrameBuffer::new();
        fb.push(&all);
        assert_eq!(fb.next_frame().unwrap().as_deref(), Some(&b"a"[..]));
        assert_eq!(fb.next_frame().unwrap().as_deref(), Some(&b""[..]));
        assert_eq!(fb.next_frame().unwrap().map(|b| b.len()), Some(300));
        assert_eq!(fb.next_frame().unwrap(), None);
    }

    #[test]
    fn oversize_length_is_rejected_before_the_body_arrives() {
        let mut prefix = BytesMut::new();
        prost::encoding::encode_varint(MAX_FRAME_LEN as u64 + 1, &mut prefix);
        let mut fb = FrameBuffer::new();
        fb.push(&prefix);
        assert!(
            matches!(fb.next_frame(), Err(ProtoError::FrameTooLarge { len }) if len == MAX_FRAME_LEN + 1)
        );
        // The maximum itself is fine.
        let mut ok = FrameBuffer::new();
        ok.push(&encode(&vec![0u8; MAX_FRAME_LEN]));
        assert_eq!(
            ok.next_frame().unwrap().map(|b| b.len()),
            Some(MAX_FRAME_LEN)
        );
    }

    #[test]
    fn huge_and_overlong_varints_are_rejected() {
        let mut fb = FrameBuffer::new();
        fb.push(&[0xFF; 11]);
        assert!(matches!(
            fb.next_frame(),
            Err(ProtoError::FrameTooLarge { .. })
        ));
        // Non-canonical zero length padded to 10 bytes is still a valid frame.
        let mut fb = FrameBuffer::new();
        fb.push(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
        assert_eq!(fb.next_frame().unwrap().as_deref(), Some(&b""[..]));
        let mut fb = FrameBuffer::new();
        fb.push(&[0x80; 10]);
        fb.push(&[0x00]);
        assert!(matches!(
            fb.next_frame(),
            Err(ProtoError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn overflowing_or_unterminated_tenth_byte_is_rejected_immediately() {
        for last in [0x02, 0x7e, 0x80] {
            let mut fb = FrameBuffer::new();
            fb.push(&[0x80; 9]);
            assert_eq!(fb.next_frame().unwrap(), None);
            fb.push(&[last]);
            assert!(matches!(
                fb.next_frame(),
                Err(ProtoError::FrameTooLarge { .. })
            ));
            assert_eq!(fb.buffered(), 10);
        }
    }

    proptest! {
        #[test]
        fn prop_arbitrary_bytes_never_panic(chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..64), 0..16)) {
            let mut fb = FrameBuffer::new();
            for c in &chunks {
                fb.push(c);
                while let Ok(Some(_)) = fb.next_frame() {}
            }
        }

        #[test]
        fn prop_frames_round_trip(bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..200), 0..8), split in 1usize..50) {
            let mut stream = Vec::new();
            for b in &bodies {
                stream.extend_from_slice(&encode(b));
            }
            let mut fb = FrameBuffer::new();
            let mut out = Vec::new();
            for chunk in stream.chunks(split) {
                fb.push(chunk);
                while let Some(f) = fb.next_frame().unwrap() {
                    out.push(f.to_vec());
                }
            }
            prop_assert_eq!(out, bodies);
        }
    }
}
