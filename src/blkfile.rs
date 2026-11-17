//! blk0001.dat frame reader. Frame layout (legacy Bitcoin-compatible, this
//! chain writes LITTLE-endian length — confirmed empirically against the
//! live chain file and validated by genesis hash match):
//!   [u32 LE net magic][u32 LE block length][block bytes]

use crate::legacy::{Block, Reader};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

#[derive(Debug)]
pub struct Frame {
    pub magic: u32,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn block(&self) -> Result<Block, String> {
        let mut r = Reader::new(&self.data);
        let b = Block::read(&mut r)?;
        r.expect_done()?;
        Ok(b)
    }
}

/// Why a frame could not be delivered.
///
/// `TornTail` is NOT corruption: append_block fsyncs only every 256 frames, so
/// a power cut / hard kill leaves the file ending inside the last frame it was
/// writing. The whole archive up to `last_complete` is intact and every body
/// before it is valid — the caller must keep serving them. `Corrupt` covers
/// everything a half-written tail cannot explain (bad magic mid-file, an
/// implausible length, an I/O error) and stays fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameRead {
    /// Short header or body at EOF. `last_complete` = offset just past the
    /// last whole frame (0 when the very first frame is torn); `offset` = the
    /// file offset where the incomplete frame starts (== `last_complete`).
    TornTail { last_complete: u64, offset: u64 },
    /// Mid-file corruption or an I/O failure — never silently tolerated.
    Corrupt(String),
}

impl std::fmt::Display for FrameRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameRead::TornTail {
                last_complete,
                offset,
            } => write!(
                f,
                "torn tail: incomplete frame at offset {} (last complete frame ends at {})",
                offset, last_complete
            ),
            FrameRead::Corrupt(m) => write!(f, "{}", m),
        }
    }
}

pub struct BlkFileReader {
    inner: BufReader<File>,
    next_magic: Option<u32>,
    /// File offset where the next frame starts.
    offset: u64,
    /// Offset just past the last WHOLE frame delivered (the torn-tail cut
    /// point a caller holding the state lock can truncate to).
    last_complete: u64,
    /// Set once a torn tail was seen, so the "one log line" is really one.
    torn_logged: bool,
}

impl BlkFileReader {
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<BlkFileReader> {
        Ok(BlkFileReader {
            inner: BufReader::with_capacity(1 << 20, File::open(path)?),
            next_magic: None,
            offset: 0,
            last_complete: 0,
            torn_logged: false,
        })
    }

    /// Offset just past the last complete frame read so far.
    pub fn last_complete_offset(&self) -> u64 {
        self.last_complete
    }

    /// Read the next frame; None at clean EOF. A torn final frame is reported
    /// as a clean EOF after one log line (the tolerant shape every existing
    /// caller already handles); mid-file corruption is still an Err. Callers
    /// that want to distinguish the two use `next_frame_checked`.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, String> {
        match self.next_frame_checked() {
            Ok(f) => Ok(f),
            Err(FrameRead::TornTail {
                last_complete,
                offset,
            }) => {
                self.log_torn_tail(last_complete, offset);
                Ok(None)
            }
            Err(FrameRead::Corrupt(m)) => Err(m),
        }
    }

    fn log_torn_tail(&mut self, last_complete: u64, offset: u64) {
        if self.torn_logged {
            return;
        }
        self.torn_logged = true;
        eprintln!(
            "tri7-blkfile: torn tail — incomplete frame at offset {} (last complete frame ends at {}); \
             serving the archive up to that offset",
            offset, last_complete
        );
    }

    /// Read the next frame with the failure mode preserved: a short read on the
    /// FINAL frame comes back as `FrameRead::TornTail` (not an error the caller
    /// must abandon the archive for), everything else as `FrameRead::Corrupt`.
    pub fn next_frame_checked(&mut self) -> Result<Option<Frame>, FrameRead> {
        let mut hdr = [0u8; 8];
        loop {
            // Read 8-byte frame header; tolerate NUL padding between files.
            let frame_off = self.offset;
            let got = match read_up_to(&mut self.inner, &mut hdr) {
                Ok(n) => n,
                Err(e) => return Err(FrameRead::Corrupt(e)),
            };
            if got == 0 {
                return Ok(None); // clean EOF on a frame boundary
            }
            if got < hdr.len() {
                if hdr[..got].iter().all(|&b| b == 0) {
                    return Ok(None); // preallocated NUL padding, not a frame
                }
                // A short header that is NOT padding: the tail was cut
                // mid-header by the last un-fsynced append.
                return Err(FrameRead::TornTail {
                    last_complete: self.last_complete,
                    offset: frame_off,
                });
            }
            let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            if magic == 0 {
                self.offset += 8;
                continue; // padding
            }
            let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
            if len == 0 || len > 8 << 20 {
                return Err(FrameRead::Corrupt(format!(
                    "implausible frame length {} at offset {}",
                    len, frame_off
                )));
            }
            let mut data = vec![0u8; len];
            let mut filled = 0usize;
            while filled < len {
                match self.inner.read(&mut data[filled..]) {
                    Ok(0) => {
                        // Body cut by EOF: the frame header was written but not
                        // the whole body. Same torn-tail shape as above.
                        return Err(FrameRead::TornTail {
                            last_complete: self.last_complete,
                            offset: frame_off,
                        });
                    }
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(FrameRead::Corrupt(format!("frame body read: {}", e))),
                }
            }
            match self.next_magic {
                None => self.next_magic = Some(magic),
                Some(m) if m != magic => {
                    return Err(FrameRead::Corrupt(format!(
                        "magic changed: {:08x} -> {:08x}",
                        m, magic
                    )))
                }
                _ => {}
            }
            self.offset += (8 + len) as u64;
            self.last_complete = self.offset;
            return Ok(Some(Frame { magic, data }));
        }
    }
}

/// Read until `buf` is full or the reader hits EOF; returns the bytes read
/// (0 = clean EOF at the first byte). Only a real I/O error is an Err.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, String> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("frame header read: {}", e)),
        }
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(magic: u32, body: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&magic.to_le_bytes());
        f.extend_from_slice(&(body.len() as u32).to_le_bytes());
        f.extend_from_slice(body);
        f
    }

    fn tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tri7-blkfile-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blk.dat");
        std::fs::write(&p, bytes).unwrap();
        p
    }

    const MAGIC: u32 = 0xD9B4_BEF9;

    #[test]
    fn clean_archive_reads_all_frames_then_eof() {
        let mut bytes = frame(MAGIC, &[1u8; 40]);
        bytes.extend_from_slice(&frame(MAGIC, &[2u8; 80]));
        let p = tmp("clean", &bytes);
        let mut r = BlkFileReader::open(&p).unwrap();
        assert_eq!(r.next_frame_checked().unwrap().unwrap().data.len(), 40);
        assert_eq!(r.next_frame_checked().unwrap().unwrap().data.len(), 80);
        assert!(r.next_frame_checked().unwrap().is_none());
        assert_eq!(r.last_complete_offset(), bytes.len() as u64);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A power cut mid-append: header written, body short. The frames before it
    /// stay readable and the tear is a distinct signal, not an error.
    #[test]
    fn short_body_at_eof_is_a_torn_tail_not_an_error() {
        let mut bytes = frame(MAGIC, &[7u8; 40]);
        let complete = bytes.len() as u64;
        bytes.extend_from_slice(&frame(MAGIC, &[8u8; 400])[..100]); // torn
        let p = tmp("tornbody", &bytes);
        let mut r = BlkFileReader::open(&p).unwrap();
        assert_eq!(r.next_frame().unwrap().unwrap().data.len(), 40);
        // tolerant view: clean EOF
        assert!(r.next_frame().unwrap().is_none());
        assert_eq!(r.last_complete_offset(), complete);
        // strict view: the distinct torn-tail signal with the cut point
        let mut r2 = BlkFileReader::open(&p).unwrap();
        assert!(r2.next_frame_checked().unwrap().is_some());
        match r2.next_frame_checked() {
            Err(FrameRead::TornTail {
                last_complete,
                offset,
            }) => {
                assert_eq!(last_complete, complete);
                assert_eq!(offset, complete);
            }
            other => panic!("expected a torn tail, got {:?}", other),
        }
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Header cut mid-way (1..7 bytes) is the same torn-tail shape.
    #[test]
    fn short_header_at_eof_is_a_torn_tail() {
        let mut bytes = frame(MAGIC, &[3u8; 40]);
        let complete = bytes.len() as u64;
        bytes.extend_from_slice(&MAGIC.to_le_bytes()[..3]);
        let p = tmp("tornhdr", &bytes);
        let mut r = BlkFileReader::open(&p).unwrap();
        assert!(r.next_frame().unwrap().is_some());
        assert!(r.next_frame().unwrap().is_none());
        assert_eq!(r.last_complete_offset(), complete);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Mid-file corruption must stay fatal — a bad length is not a lost tail.
    #[test]
    fn implausible_length_mid_file_is_still_an_error() {
        let mut bytes = frame(MAGIC, &[9u8; 40]);
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        bytes.extend_from_slice(&frame(MAGIC, &[4u8; 40]));
        let p = tmp("badlen", &bytes);
        let mut r = BlkFileReader::open(&p).unwrap();
        assert!(r.next_frame().unwrap().is_some());
        let e = r.next_frame().unwrap_err();
        assert!(e.contains("implausible frame length"), "err: {}", e);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// A zero-length body is corruption, not an EOF.
    #[test]
    fn zero_length_frame_is_an_error() {
        let p = tmp("zerolen", &frame(MAGIC, &[0u8; 0]));
        let mut r = BlkFileReader::open(&p).unwrap();
        assert!(r.next_frame().is_err());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}
