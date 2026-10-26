//! PBZX (the `Payload` of macOS installer packages) as a randomly accessible, lazily decoded byte stream.
//!
//! A PBZX stream is a 12 byte header (`pbzx` + a big-endian `u64`, the nominal chunk size) followed by chunks of
//! `u64 uncompressed size | u64 stored size | data`. The data is one XZ stream, or the plain bytes when both sizes
//! are equal. The decoded chunks concatenate into a CPIO archive.
//!
//! The chunk headers give every chunk's uncompressed size, so the whole table (and with it the length of the decoded
//! stream) is known after hopping over the chunks without decoding any of them. [`PbzxReader`] then decodes single
//! chunks on demand and keeps the recently used ones in a small cache. Browsing the CPIO index of a multi-gigabyte
//! payload therefore only decodes the chunks that hold CPIO headers, and extracting a file decodes just its chunks.
//! (The `pbzx` crate decodes the whole payload into memory, which does not scale to installer-sized payloads.)

use std::io::{self, Read, Seek, SeekFrom};

use lzma_rust2::XzReader;

pub const MAGIC: &[u8; 4] = b"pbzx";
const HEADER: u64 = 12;
const CHUNK_HEADER: u64 = 16;
/// Largest chunk (stored or decoded) accepted; real payloads use 16 MiB.
const MAX_CHUNK: u64 = 1 << 30;
/// Decoded chunks kept in memory.
const CACHE_BUDGET: usize = 96 << 20;

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[derive(Debug, Clone, Copy)]
struct Chunk {
    /// Offset of the stored data in the underlying stream.
    data_off: u64,
    stored: u64,
    raw: u64,
    /// Offset of the chunk in the decoded stream.
    start: u64,
}

/// Decoded view of a PBZX stream (`Read + Seek`).
pub struct PbzxReader<R> {
    inner: R,
    chunks: Vec<Chunk>,
    len: u64,
    pos: u64,
    /// Decoded chunks, least recently used first.
    cache: Vec<(usize, Vec<u8>)>,
    cached: usize,
}

/// Does the stream start with the PBZX magic?
pub fn is_pbzx(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

impl<R: Read + Seek> PbzxReader<R> {
    /// Read the chunk table. Fails on a bad magic or a chunk that does not fit into the stream.
    pub fn open(mut inner: R) -> io::Result<Self> {
        let total = inner.seek(SeekFrom::End(0))?;
        inner.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; HEADER as usize];
        inner.read_exact(&mut head).map_err(|_| bad("PBZX payload is shorter than its header"))?;
        if !is_pbzx(&head) {
            return Err(bad("not a PBZX stream"));
        }

        let mut chunks = Vec::new();
        let (mut off, mut len) = (HEADER, 0u64);
        // Apple's payloads simply end at the last chunk; an all-zero header is an optional terminator.
        while off + CHUNK_HEADER <= total {
            inner.seek(SeekFrom::Start(off))?;
            let mut h = [0u8; CHUNK_HEADER as usize];
            inner.read_exact(&mut h)?;
            let raw = u64::from_be_bytes(h[..8].try_into().expect("8 bytes"));
            let stored = u64::from_be_bytes(h[8..].try_into().expect("8 bytes"));
            off += CHUNK_HEADER;
            if raw == 0 && stored == 0 {
                break;
            }
            if raw == 0 || stored == 0 || raw > MAX_CHUNK || stored > MAX_CHUNK {
                return Err(bad(format!("invalid PBZX chunk at offset {}", off - CHUNK_HEADER)));
            }
            if off + stored > total {
                return Err(bad(format!("PBZX chunk at offset {} is truncated", off - CHUNK_HEADER)));
            }
            chunks.push(Chunk { data_off: off, stored, raw, start: len });
            len += raw;
            off += stored;
        }
        if chunks.is_empty() {
            return Err(bad("PBZX payload holds no chunk"));
        }
        Ok(PbzxReader { inner, chunks, len, pos: 0, cache: Vec::new(), cached: 0 })
    }

    /// Length of the decoded stream.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn decode(&mut self, idx: usize) -> io::Result<Vec<u8>> {
        let c = self.chunks[idx];
        self.inner.seek(SeekFrom::Start(c.data_off))?;
        let mut stored = vec![0u8; c.stored as usize];
        self.inner.read_exact(&mut stored)?;
        if c.stored == c.raw {
            return Ok(stored);
        }
        let mut out = Vec::with_capacity(c.raw.min(64 << 20) as usize);
        XzReader::new(&stored[..], false)
            .take(c.raw + 1)
            .read_to_end(&mut out)
            .map_err(|e| bad(format!("cannot decode PBZX chunk {idx}: {e}")))?;
        if out.len() as u64 != c.raw {
            return Err(bad(format!("PBZX chunk {idx} decodes to {} bytes, expected {}", out.len(), c.raw)));
        }
        Ok(out)
    }

    /// Cache slot holding the decoded chunk `idx` (decoded now if it is not cached).
    fn slot(&mut self, idx: usize) -> io::Result<usize> {
        if let Some(i) = self.cache.iter().position(|(k, _)| *k == idx) {
            let hit = self.cache.remove(i);
            self.cache.push(hit);
        } else {
            let data = self.decode(idx)?;
            self.cached += data.len();
            self.cache.push((idx, data));
            while self.cached > CACHE_BUDGET && self.cache.len() > 1 {
                let (_, old) = self.cache.remove(0);
                self.cached -= old.len();
            }
        }
        Ok(self.cache.len() - 1)
    }
}

impl<R: Read + Seek> Read for PbzxReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let idx = self.chunks.partition_point(|c| c.start <= self.pos) - 1;
        let inner_off = (self.pos - self.chunks[idx].start) as usize;
        let slot = self.slot(idx)?;
        let data = &self.cache[slot].1;
        let n = buf.len().min(data.len() - inner_off);
        buf[..n].copy_from_slice(&data[inner_off..inner_off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for PbzxReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{pbzx_stored, pbzx_xz};
    use std::io::Cursor;

    fn data() -> Vec<u8> {
        (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()
    }

    #[test]
    fn random_access_over_xz_and_stored_chunks() {
        let d = data();
        for bytes in [pbzx_xz(&d, 10_000), pbzx_stored(&d, 7_777)] {
            let mut r = PbzxReader::open(Cursor::new(bytes)).unwrap();
            assert_eq!(r.len(), d.len() as u64);
            for off in [0usize, 9_999, 10_000, 123_456, 299_990, 5, 200_000] {
                r.seek(SeekFrom::Start(off as u64)).unwrap();
                let mut buf = vec![0u8; 20_000.min(d.len() - off)];
                r.read_exact(&mut buf).unwrap();
                assert_eq!(buf, d[off..off + buf.len()]);
            }
            r.seek(SeekFrom::Start(0)).unwrap();
            let mut all = Vec::new();
            r.read_to_end(&mut all).unwrap();
            assert_eq!(all, d);
            assert_eq!(r.seek(SeekFrom::End(-4)).unwrap(), d.len() as u64 - 4);
        }
    }

    #[test]
    fn damaged_streams_are_rejected() {
        let good = pbzx_stored(&data(), 50_000);
        assert!(PbzxReader::open(Cursor::new(b"nope".to_vec())).is_err());
        assert!(PbzxReader::open(Cursor::new(good[..good.len() - 10].to_vec())).is_err()); // cut-off chunk
        let mut huge = good[..12].to_vec();
        huge.extend_from_slice(&(u64::MAX).to_be_bytes());
        huge.extend_from_slice(&(u64::MAX).to_be_bytes());
        assert!(PbzxReader::open(Cursor::new(huge)).is_err()); // absurd chunk size is not allocated
        assert!(PbzxReader::open(Cursor::new(good[..12].to_vec())).is_err()); // no chunk
        // a chunk that does not decode
        let mut bad = good[..12].to_vec();
        bad.extend_from_slice(&100u64.to_be_bytes());
        bad.extend_from_slice(&50u64.to_be_bytes());
        bad.extend_from_slice(&[0x55; 50]);
        let mut r = PbzxReader::open(Cursor::new(bad)).unwrap();
        assert!(r.read(&mut [0u8; 10]).is_err());
    }
}
