//! blk0001.dat frame reader. Frame layout (legacy Bitcoin-compatible, this
//! chain writes LITTLE-endian length — confirmed empirically against the
//! live chain file and validated by genesis hash match):
//!   [u32 LE net magic][u32 LE block length][block bytes]

use crate::legacy::{Block, Reader};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

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

pub struct BlkFileReader {
    inner: BufReader<File>,
    next_magic: Option<u32>,
}

impl BlkFileReader {
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<BlkFileReader> {
        Ok(BlkFileReader {
            inner: BufReader::with_capacity(1 << 20, File::open(path)?),
            next_magic: None,
        })
    }

    /// Read the next frame; None at clean EOF.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, String> {
        let mut hdr = [0u8; 8];
        loop {
            // Read 8-byte frame header; tolerate NUL padding between files.
            match read_exact_or_eof(&mut self.inner, &mut hdr) {
                Ok(false) => return Ok(None),
                Ok(true) => {}
                Err(e) => return Err(e),
            }
            let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            if magic == 0 {
                continue; // padding
            }
            let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
            if len == 0 || len > 8 << 20 {
                return Err(format!("implausible frame length {} at next read", len));
            }
            let mut data = vec![0u8; len];
            std::io::Read::read_exact(&mut self.inner, &mut data)
                .map_err(|e| format!("frame body read: {}", e))?;
            match self.next_magic {
                None => self.next_magic = Some(magic),
                Some(m) if m != magic => {
                    return Err(format!("magic changed: {:08x} -> {:08x}", m, magic))
                }
                _ => {}
            }
            return Ok(Some(Frame { magic, data }));
        }
    }
}

/// Ok(true) = filled, Ok(false) = clean EOF at first byte.
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool, String> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => {
                return if got == 0 {
                    Ok(false)
                } else {
                    Err("EOF mid-frame-header".into())
                }
            }
            Ok(n) => got += n,
            Err(e) => return Err(format!("frame header read: {}", e)),
        }
    }
    Ok(true)
}
