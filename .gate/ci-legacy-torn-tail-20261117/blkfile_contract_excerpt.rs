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
