//! SD memory-card protocol model: the state machine the SDMMC host talks to (CMD0/2/3/7/8/9/10/12/13/16,
//! ACMD41/6, single- and multi-block reads and writes). The card holds a finite, host-supplied image
//! (a disposable SD fixture file's bytes); it never touches the file system itself — the board/CLI
//! layer owns loading and saving, so this stays free of host I/O for the WASM build.
//!
//! Register/protocol facts follow the SD Physical Layer Simplified Specification (response and CSD
//! field layouts) as the X4 firmware's `embedded_sdmmc` parses them: CSD v1 for capacities up to
//! 2 GiB (C_SIZE [73:62], C_SIZE_MULT [49:47], READ_BL_LEN [83:80]), CSD v2 above (C_SIZE [69:48],
//! 512 KiB units). TAAC/NSAC/current/R2W fields are advisory values real cards vary on; nothing the
//! emulated drivers parse.

/// Card states (SD spec state numbers, R1 bits [12:9]). The model never enters the Data/Rcv/Prg
/// states: data phases complete synchronously inside the command that starts them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State { Idle, Ready, Ident, Stby, Tran }
impl State {
    fn bits(self) -> u32 { match self {
        State::Idle => 0, State::Ready => 1, State::Ident => 2, State::Stby => 3, State::Tran => 4,
    } }
}

/// What the card puts on the wire for one command.
pub struct CardReply {
    /// Response payload words, RESP0..RESP3 (R1/R3/R6/R7 use only word 0; R2 carries CID/CSD words).
    /// `None` = the card sends no response: the host sees a response timeout (RTO).
    pub response: Option<[u32; 4]>,
    /// A read command will stream this many bytes (0 when the command carries no data phase).
    /// The controller pulls them in `read_chunk` calls. `None` = the card sends no data: the
    /// controller raises its data-timeout (DRTO) status instead.
    pub read_bytes: Option<u32>,
    /// A write command's data will be stored (false: the card discards it — error response).
    pub accept_write: bool,
}
impl CardReply {
    fn response(words: [u32; 4]) -> CardReply { CardReply { response: Some(words), read_bytes: None, accept_write: false } }
    fn silence() -> CardReply { CardReply { response: None, read_bytes: None, accept_write: false } }
}

/// R1 status bits the SD spec defines; the X4 driver's `R1_ERRORS` mask (0xfdff_e008) checks all of
/// these except CARD_IS_LOCKED (bit 25), which the mask leaves out.
pub const R1_OUT_OF_RANGE: u32 = 1 << 31;
pub const R1_ADDRESS_ERROR: u32 = 1 << 30;
pub const R1_BLOCK_LEN_ERROR: u32 = 1 << 29;
pub const R1_WP_VIOLATION: u32 = 1 << 26;
pub const R1_ILLEGAL_COMMAND: u32 = 1 << 22;
pub const R1_APP_CMD: u32 = 1 << 7;

/// One SD card backed by an in-memory image. Attach one to an `SdMmcHost` (slot 1, the X4's slot).
pub struct SdCard {
    /// 512-byte sectors; padded at attach to the CSD-declared capacity.
    image: Vec<u8>,
    capacity_blocks: u32,
    high_capacity: bool,
    csd: [u32; 4],
    cid: [u32; 4],
    rca: u16,
    state: State,
    /// a CMD55 was accepted: the next command is an application command (ACMD*)
    app_next: bool,
    /// ACMD41 after the last reset has not yet reported ready: the first poll answers busy=0
    init_pending: bool,
    /// card power (GPIO5 card switch on the X4): an unpowered card answers nothing
    powered: bool,
    /// modeled card write-protect switch (defaults off; exposed for the board layer)
    write_protected: bool,
    /// read/write stream cursor for data phases (byte offset into `image`)
    stream: u32,
    stream_left: u32,
    /// bytes written since attach (the host's save-back hint for a disposable fixture)
    dirty_bytes: u32,
}
impl SdCard {
    /// Attach a card whose image is `bytes` (any length; padded up to the CSD-representable
    /// capacity). The card powers up in idle state; the host must run the initialization
    /// sequence (CMD0, CMD8, ACMD41…) before block I/O, as real cards require.
    pub fn new(bytes: Vec<u8>) -> SdCard {
        let (capacity_blocks, high_capacity) = capacity_for(bytes.len());
        let mut image = bytes;
        image.resize(capacity_blocks as usize * 512, 0);
        SdCard {
            image, capacity_blocks, high_capacity,
            csd: build_csd(capacity_blocks, high_capacity),
            cid: build_cid(),
            rca: 0, state: State::Idle, app_next: false, init_pending: true,
            powered: true, write_protected: false, stream: 0, stream_left: 0, dirty_bytes: 0,
        }
    }

    /// Modeled card power switch (the X4's GPIO5, active-low, gates the card's supply).
    /// Power-off forgets initialization — the card falls back to idle state, like a real
    /// card losing supply; power-on re-arms it for a fresh initialization sequence.
    pub fn set_power(&mut self, on: bool) {
        if !on {
            self.state = State::Idle;
            self.app_next = false;
            self.init_pending = true;
            self.stream_left = 0;
        }
        self.powered = on;
    }
    pub fn powered(&self) -> bool { self.powered }
    pub fn set_write_protected(&mut self, wp: bool) { self.write_protected = wp; }
    pub fn write_protected(&self) -> bool { self.write_protected }
    pub fn capacity_blocks(&self) -> u32 { self.capacity_blocks }
    pub fn high_capacity(&self) -> bool { self.high_capacity }
    /// The image as backed (padded to CSD capacity); the host may persist it.
    pub fn image(&self) -> &[u8] { &self.image }
    /// Bytes written since attach (the host's save-back hint for a disposable fixture).
    pub fn dirty_bytes(&self) -> u32 { self.dirty_bytes }
    fn mark_dirty(&mut self, n: u32) { self.dirty_bytes += n; }

    /// Run one command. `index` is the raw command number (app-command pairing via CMD55 is the
    /// card's own state). Called only while the card is powered.
    pub fn command(&mut self, index: u8, arg: u32) -> CardReply {
        if !self.powered {
            return CardReply::silence();
        }
        let app = core::mem::take(&mut self.app_next);
        let r1 = |state: State, extra: u32| [extra | (state.bits() << 9), 0, 0, 0];
        match (index, app) {
            // CMD0 GO_IDLE_STATE: no response, always accepted.
            (0, _) => { self.state = State::Idle; self.init_pending = true; CardReply::silence() }
            // CMD1 is MMC-only; an SD card flags it illegal.
            (1, _) => CardReply::response(r1(self.state, R1_ILLEGAL_COMMAND)),
            // CMD8 SEND_IF_COND: echo the check pattern and accepted voltage (2.7-3.6 V).
            (8, _) if self.state == State::Idle => CardReply::response([arg & 0xfff, 0, 0, 0]),
            // ACMD41: first poll reports busy (OCR bit 31 clear), the second is ready. HCS/CCS
            // decide block addressing exactly as the driver's `Card::initialize` reads them.
            (41, true) if self.state == State::Idle => {
                if arg & 0x00ff_8000 == 0 {                       // no VDD window overlap: never ready
                    return CardReply::response([0, 0, 0, 0]);
                }
                let ocr = if self.init_pending {
                    self.init_pending = false;
                    0x00ff_8000
                } else {
                    self.state = State::Ready;
                    0x8000_0000 | 0x00ff_8000 | if self.high_capacity && arg & (1 << 30) != 0 { 1 << 30 } else { 0 }
                };
                CardReply::response([ocr, 0, 0, 0])
            }
            (41, _) => CardReply::response(r1(self.state, R1_ILLEGAL_COMMAND)),
            // CMD2 ALL_SEND_CID (R2).
            (2, _) if self.state == State::Ready => { self.state = State::Ident; self.r2(false) }
            // CMD3 SET_RELATIVE_ADDR: the card publishes a fixed synthetic RCA.
            (3, _) if self.state == State::Ident => {
                self.rca = 1;
                self.state = State::Stby;
                CardReply::response([(self.rca as u32) << 16 | (State::Stby.bits() << 9), 0, 0, 0])
            }
            // CMD7 SELECT/DESELECT; only the addressed card answers (wrong RCA: silence → RTO).
            (7, _) => {
                let wants = arg & 0xffff_0000;
                if wants == (self.rca as u32) << 16 && self.rca != 0 {
                    if self.state == State::Stby { self.state = State::Tran; }
                    CardReply::response(r1(self.state, 0))
                } else if wants == 0 && self.state == State::Tran {
                    self.state = State::Stby;                     // deselect: no response
                    CardReply::silence()
                } else {
                    CardReply::silence()
                }
            }
            // CMD9 SEND_CSD / CMD10 SEND_CID: R2 payloads, addressed.
            (9 | 10, _) => {
                if arg & 0xffff_0000 == (self.rca as u32) << 16 && self.rca != 0
                    && matches!(self.state, State::Stby | State::Tran) {
                    self.r2(index == 10)
                } else {
                    CardReply::silence()
                }
            }
            // CMD12 STOP_TRANSMISSION: transfers complete synchronously in this model, so there
            // is never an open stream to stop; the transaction-state card answers with its state.
            (12, _) if self.state == State::Tran => CardReply::response(r1(self.state, 0)),
            // CMD13 SEND_STATUS: addressed R1 with the live status word.
            (13, _) => {
                if arg & 0xffff_0000 == (self.rca as u32) << 16 && self.rca != 0 {
                    CardReply::response(r1(self.state, 0))
                } else {
                    CardReply::silence()
                }
            }
            // CMD16 SET_BLOCKLEN: only 512-byte blocks are meaningful to the emulated drivers.
            (16, _) if self.state == State::Tran => {
                if arg == 512 { CardReply::response(r1(self.state, 0)) }
                else { CardReply::response(r1(self.state, R1_BLOCK_LEN_ERROR)) }
            }
            // CMD17 READ_SINGLE_BLOCK / CMD18 READ_MULTIPLE: R1 then a data stream.
            (17 | 18, _) if self.state == State::Tran => match self.sector_of(arg) {
                Ok(sector) => {
                    let len = if index == 17 { 512 } else { 4096 };
                    self.stream = sector * 512;
                    self.stream_left = len;
                    CardReply { response: Some(r1(self.state, 0)), read_bytes: Some(len), accept_write: false }
                }
                Err(word) => CardReply::response(r1(self.state, word)),
            },
            // CMD24 WRITE_SINGLE_BLOCK / CMD25 WRITE_MULTIPLE: R1 then the card accepts the data.
            (24 | 25, _) if self.state == State::Tran => {
                if self.write_protected {
                    return CardReply::response(r1(self.state, R1_WP_VIOLATION));
                }
                match self.sector_of(arg) {
                    Ok(sector) => {
                        self.stream = sector * 512;
                        self.stream_left = if index == 24 { 512 } else { 4096 };
                        CardReply { response: Some(r1(self.state, 0)), read_bytes: None, accept_write: true }
                    }
                    Err(word) => CardReply::response(r1(self.state, word)),
                }
            }
            // CMD55 APP_CMD: the next command is an application command.
            (55, _) => { self.app_next = true; CardReply::response(r1(self.state, R1_APP_CMD)) },
            // ACMD6 SET_BUS_WIDTH: accepted, bus width has no register-model effect (1/4-bit is
            // electrical; the FIFO port is word-wide either way).
            (6, true) if self.state == State::Tran =>
                CardReply::response(r1(self.state, R1_APP_CMD)),
            _ => CardReply::response(r1(self.state, R1_ILLEGAL_COMMAND)),
        }
    }

    /// Pull the next read-stream bytes (the controller asks for what still fits its FIFO).
    pub fn read_chunk(&mut self, len: usize) -> Vec<u8> {
        let take = (len as u32).min(self.stream_left) as usize;
        let at = self.stream as usize;
        let out = self.image.get(at..at + take).map(<[u8]>::to_vec).unwrap_or_default();
        self.stream += take as u32;
        self.stream_left -= take as u32;
        out
    }
    /// Store accepted write-stream bytes.
    pub fn write_chunk(&mut self, bytes: &[u8]) {
        let take = (bytes.len() as u32).min(self.stream_left) as usize;
        if take == 0 { return; }
        let at = self.stream as usize;
        if let Some(dst) = self.image.get_mut(at..at + take) { dst.copy_from_slice(&bytes[..take]); }
        self.stream += take as u32;
        self.stream_left -= take as u32;
        self.mark_dirty(take as u32);
    }
    pub fn stream_left(&self) -> u32 { self.stream_left }

    /// Sector number for a block-I/O argument, honoring high-capacity block addressing vs
    /// byte addressing, and the card's finite capacity.
    fn sector_of(&self, arg: u32) -> Result<u32, u32> {
        let sector = if self.high_capacity {
            arg
        } else {
            if !arg.is_multiple_of(512) { return Err(R1_ADDRESS_ERROR); }
            arg / 512
        };
        if sector >= self.capacity_blocks { return Err(R1_OUT_OF_RANGE); }
        Ok(sector)
    }

    fn r2(&self, cid: bool) -> CardReply {
        let words = if cid { self.cid } else { self.csd };
        CardReply { response: Some(words), read_bytes: None, accept_write: false }
    }
}

/// Capacity from an image length: the smallest CSD-representable capacity covering every byte.
/// CSD v1 (≤ 2 GiB) expresses `bytes = (C_SIZE+1) << (MULT+2) << READ_BL_LEN` with C_SIZE ≤ 4095;
/// CSD v2 (SDHC) expresses `(C_SIZE+1) * 512 KiB`. Returns (blocks, high_capacity).
pub fn capacity_for(bytes: usize) -> (u32, bool) {
    let blocks = bytes.div_ceil(512) as u64;
    if bytes <= 2 << 30 {
        let mut best: Option<(u64, u64, u32, u32)> = None;         // (cap, c_size+1, mult, bl)
        for bl in [9u32, 10, 11] {
            for mult in 0..=7u32 {
                let gran = 1u64 << (mult + 2 + bl - 9);
                let n = blocks.div_ceil(gran);
                if n > 4096 { continue; }
                let cap = n * gran;
                let better = match best { Some((bc, _, _, _)) => cap < bc, None => true };
                if better { best = Some((cap, n, mult, bl)); }
            }
        }
        let (cap, _, _, _) = best.expect("a ≤2 GiB image always fits v1 CSD");
        (cap as u32, false)
    } else {
        let n = blocks.div_ceil(1024);
        (u32::try_from(n * 1024).unwrap_or(u32::MAX), true)
    }
}

/// Build the CSD register (128 payload bits, words RESP0..3 = CSD[31:0]…CSD[127:96]).
fn build_csd(blocks: u32, high_capacity: bool) -> [u32; 4] {
    let mut b = [0u8; 16];
    if high_capacity {
        // CSD v2: structure 1, C_SIZE in 512 KiB units, READ_BL_LEN fixed at 9.
        b[0] = 1 << 6;
        b[1] = 0x0e; b[2] = 100; b[3] = 0x32;
        b[4] = 0x05; b[5] = 0xd0 | 9;                          // CCC advisory, READ_BL_LEN 512
        let c_size = (blocks as u64 / 1024) - 1;
        b[6] = ((c_size >> 20) as u8) & 3;
        b[7] = (c_size >> 12) as u8;
        b[8] = (c_size >> 4) as u8;
        b[9] = ((c_size & 0xf) << 4) as u8;
    } else {
        // CSD v1: pick the encoding `capacity_for` chose (smallest covering capacity).
        let mut best: Option<(u64, u64, u32, u32)> = None;     // (cap, c_size+1, mult, bl)
        for bl in [9u32, 10, 11] {
            for mult in 0..=7u32 {
                let gran = 1u64 << (mult + 2 + bl - 9);
                let n = u64::from(blocks).div_ceil(gran);
                if n > 4096 { continue; }
                let cap = n * gran;
                let better = match best { Some((bc, _, _, _)) => cap < bc, None => true };
                if better { best = Some((cap, n, mult, bl)); }
            }
        }
        let (_, n, mult, bl) = best.expect("v1 capacity re-derivation must succeed");
        let c_size = n - 1;
        b[0] = 0;                                              // CSD_STRUCTURE 0 (v1)
        b[1] = 0x0e; b[2] = 100; b[3] = 0x32;
        b[4] = 0x05; b[5] = 0xd0 | bl as u8;
        b[6] = ((c_size >> 10) as u8) & 3;                     // C_SIZE[11:10]
        b[7] = (c_size >> 2) as u8;                            // C_SIZE[9:2]
        b[8] = ((c_size & 3) << 6) as u8 | 0x1b;               // C_SIZE[1:0], VDD read currents
        b[9] = 0x6c | (((mult >> 1) as u8) & 3);               // VDD write currents, C_SIZE_MULT[2:1]
        b[10] = (if mult & 1 != 0 { 1 << 7 } else { 0 }) | 0x7f; // C_SIZE_MULT[0], erase sector advisory
    }
    b[11] = 0;                                                 // WP_GRP_SIZE 0, WP_GRP_ENABLE 0
    b[12] = 0xa0;                                              // R2W_FACTOR 5 (advisory)
    b[13] = 0;
    b[14] = 0;                                                 // no copy/protect flags, FAT partitioned
    let crc = crc7(&b[..15]);
    b[15] = (crc << 1) | 1;
    // The controller latches the response MSB-first: RESP3 receives CSD[127:96] (whose top bits
    // are the structure the firmware's `capacity()` reads), RESP0 receives CSD[31:0].
    [u32::from_be_bytes(b[12..16].try_into().unwrap()),
     u32::from_be_bytes(b[8..12].try_into().unwrap()),
     u32::from_be_bytes(b[4..8].try_into().unwrap()),
     u32::from_be_bytes(b[0..4].try_into().unwrap())]
}

/// Synthetic CID: a fixed, clearly-emulated identity (SD spec CID field layout, CRC7 valid).
fn build_cid() -> [u32; 4] {
    let mut b = [0u8; 16];
    b[0] = 0x03;                                               // MID (synthetic)
    b[1] = b'S'; b[2] = b'D';                                  // OID
    b[3] = b'E'; b[4] = b'M'; b[5] = b'U'; b[6] = b'3'; b[7] = b'2'; // PNM
    b[8] = 0x10;                                               // PRV 1.0
    b[9..13].copy_from_slice(&0x2026_0001u32.to_be_bytes());   // PSN (synthetic)
    b[13] = 26;                                                // MDT year 2026
    b[14] = 10 << 4;                                           // MDT month 10
    let crc = crc7(&b[..15]);
    b[15] = (crc << 1) | 1;
    [u32::from_be_bytes(b[12..16].try_into().unwrap()),
     u32::from_be_bytes(b[8..12].try_into().unwrap()),
     u32::from_be_bytes(b[4..8].try_into().unwrap()),
     u32::from_be_bytes(b[0..4].try_into().unwrap())]
}

/// CRC7 (x^7+x^3+1) over the MSB-first bits of `data`, as the SD spec computes register CRCs.
fn crc7(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in data {
        for bit in (0..8).rev() {
            let msb = crc & 0x40 != 0;
            crc = (crc << 1) & 0x7f | ((byte >> bit) & 1);
            if msb { crc ^= 0x12; }
        }
    }
    crc
}
