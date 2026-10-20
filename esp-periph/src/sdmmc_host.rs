//! SDMMC host controller (the DesignWare-derived block the ESP32-S3 mounts at 0x60028000, block
//! 0x28) plus the attached `SdCard`. Register layout, reset/self-clear semantics and clock fields
//! follow ESP-IDF v5.5.1 `components/soc/esp32s3/register/soc/sdmmc_struct.h`, `sdmmc_reg.h` and
//! `components/hal/esp32s3/include/hal/sdmmc_ll.h`; the transaction contract (CPU FIFO at offset
//! 0x200, no DMA) follows the X4 firmware's `board_adapters/x4_storage.rs`, which cites the same
//! headers. The FIFO data port at +0x200 is proven by that driver; the IDF struct marks the span
//! reserved. Controller timing is functional: commands complete within the register write that
//! starts them (write data phases complete as the FIFO fills), never pretending physical latency.
//!
//! Only slot 1 carries a card (the X4's wiring: GPIO40 DAT, 41 CLK, 42 CMD, slot-1 signals
//! `SDHOST_C*_2`). Slot 0 reads absent. The internal-DMA data path (CTRL.use_internal_dma) is not
//! modelled — the block has no bus master here — and honestly never completes: a driver that
//! enables it sees a data timeout, not fabricated success.
use crate::device::{Device, WriteEffect};
use crate::regram::RegRam;
use crate::sdcard::{SdCard, CardReply};
use std::collections::VecDeque;

/// Raw-interrupt bits (rintsts/intmask/mintsts; sdmmc_reg.h SDMMC_INTMASK_*, IDF v5.5.1). The
/// response-CRC (RCRC), data-CRC (DCRC), response-error (RE) and hardware-locked (HLE) bits exist
/// in the hardware but are never raised by this model: the synthetic card's responses are always
/// CRC-valid and there is no asynchronous CIU state to conflict with.
const INT_CD: u32 = 1 << 0;
const INT_CMD_DONE: u32 = 1 << 2;
const INT_DTO: u32 = 1 << 3;
const INT_RTO: u32 = 1 << 8;
const INT_DRTO: u32 = 1 << 9;
const INT_FRUN: u32 = 1 << 11;

/// FIFO capacity in bytes. The IDF headers do not state the depth; STATUS.fifo_count is 13 bits
/// and the X4 driver programs watermarks of 128/127 words, implying a half-full point of 128 —
/// modelled as 256 words (1 KiB), comfortably above the driver's 512-byte block stream.
const FIFO_CAP_BYTES: usize = 1024;
/// Read-modify-write HCON (0x70) value from the sdmmc_struct.h field defaults: card_type 1,
/// card_num 1, bus_type 1, data_width 1, addr_width 19, dma_width 1, hold 1, num_clk_div 3.
const HCON: u32 = (1 << 0) | (1 << 5) | (1 << 6) | (1 << 7) | (19 << 10) | (1 << 18) | (1 << 22) | (3 << 24);

/// An in-flight data phase. Reads pull from the card stream into the FIFO; writes stream FIFO
/// chunks to the card as they reach `blksiz`.
struct DataXfer { write: bool, left: u32, total: u32, accept: bool }

/// The SDMMC host block. Mount as a `Device` (the S3 registers it at block 0x28) and attach a
/// card image through `attach_card` — the image bytes come from the board/CLI layer, never file
/// I/O here.
pub struct SdMmcHost {
    ram: RegRam,
    ctrl: u32, pwren: u32, clkdiv: u32, clksrc: u32, clkena: u32, tmout: u32, ctype: u32,
    blksiz: u32, bytcnt: u32, intmask: u32, cmdarg: u32, cmd: u32, resp: [u32; 4],
    rintsts: u32, fifoth: u32, tcbcnt: u32, tbbcnt: u32, uhs: u32, rst_n: u32, clock_reg: u32,
    resp_index: u32,
    fifo: VecDeque<u8>,
    data: Option<DataXfer>,
    pub card: Option<SdCard>,
    /// SYSTEM gate state, kept current by the chip's `pre_access`/SYSTEM-write hooks
    clock_enabled: bool, held_in_reset: bool,
    pub log: bool,
}
impl SdMmcHost {
    pub fn new() -> SdMmcHost {
        SdMmcHost {
            ram: RegRam::new(),
            ctrl: 0, pwren: 0, clkdiv: 0, clksrc: 0, clkena: 0, tmout: 0, ctype: 0,
            blksiz: 0, bytcnt: 0, intmask: 0, cmdarg: 0, cmd: 0, resp: [0; 4],
            rintsts: 0, fifoth: 0, tcbcnt: 0, tbbcnt: 0,
            // SDMMC_RST_N (0x78): "bit N resets card N, active low"; the power-on value is not in
            // the IDF headers — both slots assumed out of card-reset (the IDF LL never touches it).
            uhs: 0, rst_n: 3, clock_reg: 0, resp_index: 0,
            fifo: VecDeque::new(), data: None, card: None,
            clock_enabled: true, held_in_reset: false, log: false,
        }
    }

    // ---------------------------------------------------------------- card management (host API)
    /// Attach a card backed by `image` into slot 1 (the X4's slot), freshly powered and in idle
    /// state. Replaces any card; never touches the file system — the caller owns loading/saving.
    pub fn attach_card(&mut self, image: Vec<u8>) {
        if self.card.is_some() { self.rintsts |= INT_CD; }         // card-removed detect pulse
        self.card = Some(SdCard::new(image));
        self.data = None;
        self.fifo.clear();
        self.rintsts |= INT_CD;                                    // insertion raises CD
    }
    /// Remove the card; commands time out until another is attached.
    pub fn detach_card(&mut self) {
        self.card = None;
        self.data = None;
        self.fifo.clear();
        self.rintsts |= INT_CD;
    }
    pub fn card(&self) -> Option<&SdCard> { self.card.as_ref() }
    pub fn card_mut(&mut self) -> Option<&mut SdCard> { self.card.as_mut() }
    /// The attached image (padded to the card's CSD capacity), for host-side save-back.
    pub fn image(&self) -> Option<&[u8]> { self.card.as_ref().map(SdCard::image) }
    /// Bytes written since attach.
    pub fn dirty_bytes(&self) -> u32 { self.card.as_ref().map_or(0, SdCard::dirty_bytes) }

    /// SYSTEM gate: SDIO_HOST_CLK_EN and SDIO_HOST_RST (PERIP_CLK_EN1/RST_EN1 bit 7, IDF v5.5.1
    /// system_reg.h). A reset pulse returns the block to power-on defaults; the attached card is
    /// unaffected (it is outside the host's power domain — the X4 gates it through GPIO5).
    pub fn apply_system_gate(&mut self, clk_en: bool, rst: bool) {
        if rst {
            if !self.held_in_reset {
                *self = SdMmcHost { card: self.card.take(), log: self.log, ..SdMmcHost::new() };
                self.held_in_reset = true;
            }
        } else {
            self.held_in_reset = false;
        }
        self.clock_enabled = clk_en;
    }
    pub fn clock_enabled(&self) -> bool { self.clock_enabled }
    pub fn held_in_reset(&self) -> bool { self.held_in_reset }

    /// Card clock frequency the configured dividers produce for `slot` (functional model:
    /// source / (div_factor_l + 1) / (2·card_div, or bypass at 0) — sdmmc_ll.h clock scheme).
    pub fn card_clock_hz(&self, slot: u32) -> u64 {
        let src = if self.clock_reg & (1 << 23) != 0 { 160_000_000 } else { 40_000_000 };
        let host = src / (u64::from((self.clock_reg >> 13) & 0xf) + 1);
        let sel = match slot { 0 => self.clksrc & 3, _ => (self.clksrc >> 2) & 3 };
        let d = match sel {
            0 => self.clkdiv & 0xff, 1 => (self.clkdiv >> 8) & 0xff,
            2 => (self.clkdiv >> 16) & 0xff, _ => (self.clkdiv >> 24) & 0xff,
        };
        host / if d == 0 { 1 } else { 2 * u64::from(d) }
    }

    // ---------------------------------------------------------------- register model
    fn raw(&self, off: u32) -> u32 {
        match off {
            0x00 => self.ctrl, 0x04 => self.pwren, 0x08 => self.clkdiv, 0x0c => self.clksrc,
            0x10 => self.clkena, 0x14 => self.tmout, 0x18 => self.ctype, 0x1c => self.blksiz,
            0x20 => self.bytcnt, 0x24 => self.intmask, 0x28 => self.cmdarg, 0x2c => self.cmd,
            0x30..=0x3c => self.resp[((off - 0x30) / 4) as usize],
            0x40 => self.rintsts & self.intmask,                   // MINTSTS
            0x44 => self.rintsts,                                  // RINTSTS
            0x48 => self.status(),
            0x4c => self.fifoth, 0x50 => self.cdetect(), 0x54 => self.wrtprt(),
            0x5c => self.tcbcnt, 0x60 => self.tbbcnt,
            0x70 => HCON, 0x74 => self.uhs, 0x78 => self.rst_n,
            0x800 => self.clock_reg,
            _ => self.ram.read(off),
        }
    }

    fn write_reg(&mut self, off: u32, v: u32) {
        match off {
            0x00 => {                                              // CTRL: reset bits self-clear
                if v & 1 != 0 { self.controller_reset(); }
                if v & (1 << 1) != 0 { self.fifo_reset(); }
                if v & (1 << 2) != 0 { self.ram.write(0x80, 1); }  // DMA reset: IDMAC is RegRam here
                self.ctrl = v & 0xffff_fff8;
            }
            0x04 => self.pwren = v,
            0x08 => self.clkdiv = v,
            0x0c => self.clksrc = v,
            0x10 => self.clkena = v,
            0x14 => self.tmout = v,
            0x18 => self.ctype = v,
            0x1c => self.blksiz = v,
            0x20 => self.bytcnt = v,
            0x24 => self.intmask = v,
            0x28 => self.cmdarg = v,
            0x2c => { if v & (1 << 31) != 0 { self.execute(v); } else { self.cmd = v; } }
            0x44 => self.rintsts &= !v,                            // W1C
            0x4c => self.fifoth = v,
            0x74 => self.uhs = v,
            0x78 => self.rst_n = v,
            0x800 => self.clock_reg = v,
            // FIFO data port: the word window the X4 driver streams blocks through.
            o @ 0x200..=0x3ff => { let _ = o; self.fifo_push_word(v); }
            0x30..=0x3c | 0x40 | 0x48 | 0x50 | 0x54 | 0x5c | 0x60 | 0x70 => {}
            _ => self.ram.write(off, v),
        }
    }

    fn status(&self) -> u32 {
        let words = self.fifo.len().div_ceil(4) as u32;
        let tx_wm = self.fifoth & 0xfff;
        let rx_wm = (self.fifoth >> 16) & 0xfff;
        let present = self.card.is_some();
        (if words >= rx_wm.max(1) { 1 } else { 0 })                // fifo_rx_watermark
        | (if words <= tx_wm { 1 << 1 } else { 0 })                // fifo_tx_watermark
        | (if self.fifo.is_empty() { 1 << 2 } else { 0 })          // fifo_empty
        | (if self.fifo.len() >= FIFO_CAP_BYTES { 1 << 3 } else { 0 }) // fifo_full
        | (u32::from(present) << 8)                                // data3_status mirrors presence
        | (if self.data.as_ref().is_some_and(|d| d.write) { 1 << 9 } else { 0 }) // data_busy
        | (if self.data.is_some() { 1 << 10 } else { 0 })          // data_fsm_busy
        | (self.resp_index & 0x3f) << 16                           // response_index
        | (words & 0x1fff) << 16                                   // fifo_count
    }

    /// CDETECT: bit N reads 0 if card N is present; only slot 1 can carry the model's card.
    fn cdetect(&self) -> u32 { if self.card.is_some() { 1 } else { 3 } }
    /// WRTTPRT: bit N reads 1 if card N is write-protected (slot 1 only).
    fn wrtprt(&self) -> u32 { if self.card().is_some_and(SdCard::write_protected) { 1 << 1 } else { 0 } }

    fn controller_reset(&mut self) {
        self.fifo_reset();
        self.rintsts = 0;
        self.resp = [0; 4];
        self.resp_index = 0;
        self.cmd = 0;
    }
    fn fifo_reset(&mut self) {
        self.fifo.clear();
        self.data = None;
        self.tcbcnt = 0;
        self.tbbcnt = 0;
    }

    /// A start_command write: run the CIU transaction synchronously.
    fn execute(&mut self, cmd: u32) {
        let index = (cmd & 0x3f) as u8;
        let slot = (cmd >> 16) & 0x1f;
        let expect_response = cmd & (1 << 6) != 0;
        let update = cmd & (1 << 21) != 0;
        let data = cmd & (1 << 9) != 0;
        let write = cmd & (1 << 10) != 0;
        self.cmd = cmd & !(1 << 31);                               // taken: start_command clears
        if update {
            // Clock-update command: an internal CIU register apply, nothing reaches the card.
            self.rintsts |= INT_CMD_DONE;
            return;
        }
        if slot != 1 {                                             // only slot 1 is wired
            self.rintsts |= if expect_response { INT_RTO } else { INT_CMD_DONE };
            return;
        }
        if expect_response && self.clkena & (1 << 1) == 0 {
            self.rintsts |= INT_RTO;                               // card clock gated: no response
            return;
        }
        let Some(card) = self.card.as_mut() else {
            self.rintsts |= if expect_response { INT_RTO } else { INT_CMD_DONE };
            return;
        };
        let bytcnt = self.bytcnt;
        let reply: CardReply = card.command(index, self.cmdarg);
        match reply.response {
            Some(words) => { self.resp = words; self.resp_index = u32::from(index); self.rintsts |= INT_CMD_DONE; }
            None => self.rintsts |= if expect_response { INT_RTO } else { INT_CMD_DONE },
        }
        if !data { return; }
        if self.ctrl & (1 << 25) != 0 {                            // CTRL.use_internal_dma
            if self.log { eprintln!("[sdmmc] internal-DMA data path is not modelled; data phase will time out"); }
            return;                                                // no data ever moves: driver times out
        }
        if write {
            if self.blksiz == 0 {
                if self.log { eprintln!("[sdmmc] data command with BLKSIZ=0 cannot complete"); }
                return;                                            // nothing to stream: driver times out
            }
            self.data = Some(DataXfer { write: true, left: bytcnt, total: bytcnt, accept: reply.accept_write });
            self.pump_write();
        } else {
            match reply.read_bytes {
                None => self.rintsts |= INT_DRTO,                  // card refused: data timeout
                Some(total) => {
                    // BYTCNT governs the transfer length (auto-stop ends the card's stream).
                    let total = total.min(bytcnt);
                    self.data = Some(DataXfer { write: false, left: total, total, accept: false });
                    self.refill_read();
                }
            }
        }
    }

    /// Move completed read data from the card stream into the FIFO (functional: instantly).
    fn refill_read(&mut self) {
        loop {
            let Some(x) = &self.data else { return };
            if x.write || x.left == 0 { return; }
            let (left, space) = (x.left, FIFO_CAP_BYTES - self.fifo.len());
            if space < 4 { return; }
            let want = left.min(space as u32) as usize;
            let bytes = match self.card.as_mut() {
                Some(card) => card.read_chunk(want),
                None => Vec::new(),
            };
            if bytes.is_empty() { return; }                        // stream dry: transfer never completes
            let got = bytes.len() as u32;
            self.fifo.extend(bytes);
            let x = self.data.as_mut().expect("checked above");
            x.left -= got;
            if x.left == 0 { self.finish_data(); }
        }
    }

    /// Stream FIFO bytes to the card in BLKSIZ chunks as they accumulate.
    fn pump_write(&mut self) {
        loop {
            let Some(x) = &self.data else { return };
            if !x.write { return; }
            let blksiz = self.blksiz as usize;
            let left = x.left;
            let give = match self.card.as_mut() {
                Some(card) if x.accept => blksiz.min(left as usize).min(card.stream_left() as usize),
                _ => blksiz.min(left as usize),
            };
            if give == 0 {
                if left == 0 { self.finish_data(); }
                return;
            }
            if self.fifo.len() < give { return; }
            let bytes: Vec<u8> = self.fifo.drain(..give).collect();
            if x.accept {
                if let Some(card) = self.card.as_mut() { card.write_chunk(&bytes); }
            }
            let x = self.data.as_mut().expect("checked above");
            x.left -= give as u32;
            if x.left == 0 { self.finish_data(); }
        }
    }

    fn finish_data(&mut self) {
        let total = self.data.as_ref().map_or(0, |x| x.total);
        self.rintsts |= INT_DTO;
        self.tcbcnt = total;
        self.tbbcnt = total;
        self.data = None;
    }

    fn fifo_push_word(&mut self, v: u32) {
        if self.fifo.len() + 4 > FIFO_CAP_BYTES {
            self.rintsts |= INT_FRUN;                              // FIFO overrun: word dropped
            return;
        }
        self.fifo.extend(v.to_le_bytes());
        self.pump_write();
    }
    fn fifo_pop_word(&mut self) -> u32 {
        self.refill_read();                                        // a freed FIFO pulls the next stream data
        if self.fifo.len() < 4 {
            self.rintsts |= INT_FRUN;                              // underrun: read past the transfer
            return 0;
        }
        let b = [self.fifo.pop_front().unwrap(), self.fifo.pop_front().unwrap(),
                 self.fifo.pop_front().unwrap(), self.fifo.pop_front().unwrap()];
        u32::from_le_bytes(b)
    }

    pub fn irq(&self) -> bool { self.ctrl & (1 << 4) != 0 && self.rintsts & self.intmask != 0 }
}

impl Default for SdMmcHost { fn default() -> Self { Self::new() } }

impl Device for SdMmcHost {
    fn read(&mut self, off: u32) -> u32 {
        if self.held_in_reset || !self.clock_enabled { return 0; }
        if (0x200..=0x3ff).contains(&off) { return self.fifo_pop_word(); }
        self.raw(off)
    }
    fn write(&mut self, off: u32, v: u32) -> WriteEffect {
        if self.held_in_reset || !self.clock_enabled { return WriteEffect::NONE; }
        self.write_reg(off, v);
        WriteEffect::NONE
    }
    fn irq_sources(&self) -> u64 { u64::from(self.irq()) }
    fn debug(&mut self, on: bool) { self.log = on; }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Device;

    const R1_ERRORS: u32 = 0xfdff_e008;                            // the X4 driver's mask

    /// Drives `SdMmcHost` through the exact register protocol of the X4 firmware's
    /// `board_adapters/x4_storage.rs` (CPU FIFO, slot 1, no DMA).
    struct X4 { host: SdMmcHost }
    impl X4 {
        fn new(image: Option<Vec<u8>>) -> X4 {
            let mut host = SdMmcHost::new();
            if let Some(image) = image { host.attach_card(image); }
            X4 { host }
        }
        fn read(&mut self, off: u32) -> u32 { Device::read(&mut self.host, off) }
        fn write(&mut self, off: u32, v: u32) { Device::write(&mut self.host, off, v); }

        /// `Native::reset()` after the GPIO/RCC dance.
        fn reset(&mut self) {
            self.write(0, 7);
            assert_eq!(self.read(0) & 7, 0, "CTRL reset bits self-clear");
            self.write(0, 0);
            self.write(0x24, 0);
            self.write(0x44, u32::MAX);
            self.write(0x18, 0);
            self.write(0x14, u32::MAX);
            self.write(0x4c, 0x007f_0080);
        }
        /// `Native::update_clock()`.
        fn update_clock(&mut self) -> Result<(), &'static str> {
            self.write(0x28, 0);
            self.write(0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
            assert_eq!(self.read(0x2c) & (1 << 31), 0, "update command is taken");
            self.result()
        }
        /// `Native::clock()`.
        fn clock(&mut self, fast: bool) {
            self.write(0x10, 0);
            self.update_clock().unwrap();
            self.write(0x800, 1 | (3 << 9) | (7 << 13) | (7 << 17) | (1 << 23));
            self.write(0x08, if fast { 0 } else { 25 << 8 });
            self.write(0x0c, 1 << 2);
            self.update_clock().unwrap();
            self.write(0x10, 1 << 1);
            self.update_clock().unwrap();
        }
        /// `Native::result()`: RTO first, then the rintsts error mask (0xbfc2).
        fn result(&mut self) -> Result<(), &'static str> {
            let st = self.read(0x44);
            if st & INT_RTO != 0 { return Err("timeout"); }
            if st & 0xbfc2 != 0 { return Err("controller"); }
            Ok(())
        }
        /// `Native::command()`.
        fn command(&mut self, index: u8, arg: u32, flags: u32) -> Result<[u32; 4], &'static str> {
            assert_eq!(self.read(0x2c) & (1 << 31), 0, "previous command taken");
            self.write(0x44, u32::MAX);
            self.write(0x28, arg);
            self.write(0x2c, u32::from(index) | flags | (1 << 16) | (1 << 29) | (1 << 31));
            let st = self.read(0x44);
            if st & (INT_CMD_DONE | 0xbfc2) == 0 { return Err("no completion"); }
            self.result()?;
            Ok([self.read(0x30), self.read(0x34), self.read(0x38), self.read(0x3c)])
        }
        /// `Native::block()` read path (CMD17, CPU FIFO).
        fn block_read(&mut self, arg: u32) -> Result<[u8; 512], &'static str> {
            self.start_block(arg);
            self.issue(17);
            let mut bytes = [0u8; 512];
            let mut words = 0usize;
            for _ in 0..100_000 {
                self.result()?;
                if words < 128 && self.read(0x48) & (1 << 2) == 0 {
                    let at = words * 4;
                    bytes[at..at + 4].copy_from_slice(&self.read(0x200).to_le_bytes());
                    words += 1;
                }
                if self.read(0x44) & INT_DTO != 0 && words == 128 { break; }
            }
            if words != 128 { return Err("timeout"); }
            let resp = self.read(0x30);
            if resp & R1_ERRORS != 0 { return Err("card"); }
            Ok(bytes)
        }
        /// `Native::block()` write path (CMD24, CPU FIFO).
        fn block_write(&mut self, arg: u32, bytes: &[u8; 512]) -> Result<(), &'static str> {
            self.start_block(arg);
            self.issue(24 | (1 << 10));                                // rw bit set
            let mut words = 0usize;
            for _ in 0..100_000 {
                self.result()?;
                if words < 128 && self.read(0x48) & (1 << 3) == 0 {
                    let at = words * 4;
                    self.write(0x200, u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
                    words += 1;
                }
                if self.read(0x44) & INT_DTO != 0 && words == 128 { break; }
            }
            if words != 128 { return Err("timeout"); }
            let resp = self.read(0x30);
            if resp & R1_ERRORS != 0 { return Err("card"); }
            Ok(())
        }
        /// `Native::block()` preamble: card idle check, CTRL fifo-reset, BLKSIZ/BYTCNT, status clear.
        fn start_block(&mut self, arg: u32) {
            assert_eq!(self.read(0x48) & (1 << 9), 0, "card data idle");
            self.write(0, 2);                                          // CTRL fifo_reset
            assert_eq!(self.read(0) & 2, 0);
            self.write(0x1c, 512);
            self.write(0x20, 512);
            self.write(0x44, u32::MAX);
            self.write(0x28, arg);
        }
        /// The CMD write that starts the data phase (RESPONSE | CRC | data_expected | wait_complete).
        fn issue(&mut self, cmd: u32) {
            self.write(0x2c, cmd | (1 << 6) | (1 << 8) | (1 << 9) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
        }
        /// The X4 `Card::initialize()` sequence through the registers; returns the capacity in
        /// 512-byte blocks the firmware's `platform/sdmmc.rs::capacity` would parse from the CSD.
        fn initialize(&mut self) -> u32 {
            self.reset();
            self.clock(false);
            self.command(0, 0, 1 << 15).unwrap();                      // CMD0, INIT sequence
            let r = self.command(8, 0x1aa, (1 << 6) | (1 << 8)).unwrap();
            assert_eq!(r[0] & 0xfff, 0x1aa, "CMD8 echoes voltage+pattern");
            let mut ocr = 0;
            for _ in 0..200 {
                let r = self.command(55, 0, (1 << 6) | (1 << 8)).unwrap();
                assert_eq!(r[0] & R1_ERRORS, 0);
                let r = self.command(41, 0x00ff_8000 | (1 << 30), 1 << 6).unwrap();
                if r[0] & (1 << 31) != 0 { ocr = r[0]; break; }
            }
            assert_ne!(ocr & (1 << 31), 0, "card became ready");
            self.command(2, 0, (1 << 6) | (1 << 7) | (1 << 8)).unwrap();
            let r = self.command(3, 0, (1 << 6) | (1 << 8)).unwrap();
            let rca = r[0] & 0xffff_0000;
            assert_ne!(rca, 0, "RCA assigned");
            assert_eq!(r[0] & 0xe000, 0);
            let csd = self.command(9, rca, (1 << 6) | (1 << 7) | (1 << 8)).unwrap();
            assert_eq!(self.command(7, rca, (1 << 6) | (1 << 8)).unwrap()[0] & R1_ERRORS, 0);
            if ocr & (1 << 30) == 0 {                                  // not high capacity: byte addressing
                assert_eq!(self.command(16, 512, (1 << 6) | (1 << 8)).unwrap()[0] & R1_ERRORS, 0);
            }
            self.clock(true);
            match csd[3] >> 30 {                                       // firmware `capacity()` parse
                0 => {
                    let size = ((csd[2] & 0x3ff) << 2) | (csd[1] >> 30);
                    let block_len = (csd[2] >> 16) & 15;
                    let mult = (csd[1] >> 15) & 7;
                    let bytes = ((u64::from(size) + 1) << (mult + 2)) << block_len;
                    (bytes / 512) as u32
                }
                1 => {
                    let size = ((csd[2] & 0x3f) << 16) | (csd[1] >> 16);
                    (size + 1) * 1024
                }
                _ => panic!("CSD structure must be 0 or 1"),
            }
        }
        fn patterned(len: usize) -> Vec<u8> { (0..len).map(|i| (i % 251) as u8).collect() }
    }

    #[test]
    fn clock_update_completes_without_a_card() {
        let mut x4 = X4::new(None);
        x4.reset();
        x4.clock(false);
        assert_eq!(x4.read(0x44) & R1_ERRORS, 0, "update commands never touch the card");
        // An addressed command with the card absent times out: RTO, never a fake completion.
        assert_eq!(x4.command(8, 0x1aa, (1 << 6) | (1 << 8)), Err("timeout"));
        assert_eq!(x4.read(0x30), 0, "no response latched");
        assert_eq!(x4.host.irq_sources(), 0, "intmask is cleared by reset");
    }

    #[test]
    fn full_x4_initialization_parses_fixture_capacity() {
        let mut x4 = X4::new(Some(X4::patterned(8193 * 512)));         // the real fixture's size
        let blocks = x4.initialize();
        assert_eq!(blocks, 8196, "CSD must cover the 8193-block image (rounded to representable)");
        assert!(!x4.host.card.as_ref().unwrap().high_capacity(), "≤2 GiB images are CSD v1, byte-addressed");
    }

    #[test]
    fn fifo_block_read_returns_image_bytes_and_counts() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        let mut block = x4.block_read(0).unwrap();
        assert_eq!(&block[..8], &[0, 1, 2, 3, 4, 5, 6, 7]);
        block.reverse();                                               // mutate to prove it is a fresh copy
        let again = x4.block_read(0).unwrap();
        assert_eq!(&again[..8], &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(x4.read(0x5c), 512, "TCBCNT counts the transfer");
        assert_eq!(x4.read(0x60), 512, "TBBCNT counts the transfer");
        assert_eq!(x4.read(0x48) & (1 << 10), 0, "data FSM idle after completion");
    }

    #[test]
    fn fifo_block_write_persists_and_reads_back() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        let mut block = [0u8; 512];
        for (i, b) in block.iter_mut().enumerate() { *b = ((i * 7) & 0xff) as u8; }
        x4.block_write(5 * 512, &block).unwrap();                      // SDSC: byte address
        assert_eq!(x4.host.dirty_bytes(), 512);
        let read_back = x4.block_read(5 * 512).unwrap();
        assert_eq!(read_back, block, "read-after-write through the same card");
        assert_eq!(&x4.host.image().unwrap()[5 * 512..5 * 512 + 4], &[0, 7, 14, 21]);
    }

    #[test]
    fn out_of_range_accesses_fail_like_the_driver_expects() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        // Read past the end: R1 error, no data → controller data timeout, no fabricated block.
        assert_eq!(x4.block_read(128 * 512), Err("controller"));
        let st = x4.read(0x44);
        assert_ne!(st & (1 << 9), 0, "DRTO raised");
        // Write past the end: transfer completes, response carries OUT_OF_RANGE, image unchanged.
        let block = [0xa5; 512];
        assert_eq!(x4.block_write(130 * 512, &block), Err("card"));
        let tail = x4.block_read(127 * 512).unwrap();
        assert!(tail.iter().enumerate().all(|(i, b)| *b == ((127 * 512 + i) % 251) as u8),
                "the rejected write left the card image untouched");
        // Unaligned byte-address on an SDSC card: ADDRESS_ERROR in the R1, and the refused data
        // phase makes the controller raise DRTO (a Controller error for the driver's result()).
        assert_eq!(x4.command(17, 513, (1 << 6) | (1 << 8) | (1 << 9) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31)),
            Err("controller"));
        assert_ne!(x4.read(0x30) & (1 << 30), 0, "ADDRESS_ERROR in the latched R1");
    }

    #[test]
    fn multi_block_read_streams_consecutive_sectors_and_stops() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        x4.write(0x1c, 512);                                           // BLKSIZ
        x4.write(0x20, 2048);                                          // BYTCNT: 4 blocks
        x4.write(0x44, u32::MAX);
        x4.write(0x28, 10 * 512);
        x4.write(0x2c, 18 | (1 << 6) | (1 << 8) | (1 << 9) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
        let mut data = Vec::new();
        for _ in 0..2048 / 4 {
            assert_eq!(x4.read(0x48) & (1 << 2), 0, "FIFO refills as the driver drains it");
            data.extend(x4.read(0x200).to_le_bytes());
        }
        assert_eq!(data, X4::patterned(128 * 512)[10 * 512..14 * 512].to_vec());
        assert_ne!(x4.read(0x44) & INT_DTO, 0, "DTO when BYTCNT bytes delivered");
        let r = x4.command(12, 0, (1 << 6) | (1 << 8)).unwrap();       // STOP_TRANSMISSION
        assert_eq!(r[0] & R1_ERRORS, 0);
    }

    #[test]
    fn fifo_overrun_and_underrun_raise_frun() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        x4.reset();
        for _ in 0..256 { x4.write(0x200, 0x1234_5678); }              // fill the FIFO exactly
        assert_ne!(x4.read(0x48) & (1 << 3), 0, "fifo_full");
        x4.write(0x200, 0);                                            // one word too many
        assert_ne!(x4.read(0x44) & (1 << 11), 0, "FRUN on overrun");
        x4.write(0x44, u32::MAX);
        let mut last = 0u32;
        for _ in 0..256 { last = x4.read(0x200); }                     // drain the 256 valid words
        assert_eq!(last, 0x1234_5678);
        assert_eq!(x4.read(0x200), 0, "underrun past an empty FIFO reads 0");
        assert_ne!(x4.read(0x44) & (1 << 11), 0, "FRUN on underrun past an empty FIFO");
    }

    #[test]
    fn rintsts_is_write_one_to_clear_and_intmask_gates_mintsts_and_irq() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        x4.reset();
        x4.write(0x800, 1 | (3 << 9) | (7 << 13) | (7 << 17) | (1 << 23));
        x4.write(0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31)); // update cmd
        let raw = x4.read(0x44);
        assert_ne!(raw & INT_CMD_DONE, 0);
        x4.write(0x44, 0);                                             // writing 0 clears nothing (W1C)
        assert_eq!(x4.read(0x44), raw);
        x4.write(0x44, INT_CMD_DONE);
        assert_eq!(x4.read(0x44) & INT_CMD_DONE, 0);
        // MINTSTS = rintsts & intmask; the IRQ line additionally needs CTRL.int_enable.
        x4.write(0x24, u32::MAX);
        let ctrl = x4.read(0) | (1 << 4);
        x4.write(0, ctrl);
        x4.write(0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31)); // another update cmd
        assert_ne!(x4.read(0x40) & INT_CMD_DONE, 0, "mintsts mirrors the masked raw status");
        assert_eq!(x4.host.irq_sources(), 1);
        x4.write(0x24, 0);
        assert_eq!(x4.host.irq_sources(), 0);
        x4.write(0x24, u32::MAX);
        let ctrl = x4.read(0) & !(1 << 4);                             // global interrupt enable off
        x4.write(0, ctrl);
        assert_eq!(x4.host.irq_sources(), 0);
    }

    #[test]
    fn card_clock_frequency_matches_the_x4_divider_writes() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        x4.reset();
        x4.clock(false);
        assert_eq!(x4.host.card_clock_hz(1), 400_000, "PLL160M/8/50: identification clock");
        x4.clock(true);
        assert_eq!(x4.host.card_clock_hz(1), 20_000_000, "PLL160M/8, divider bypassed");
    }

    #[test]
    fn system_gate_holds_the_block_in_reset_and_off() {
        let mut x4 = X4::new(None);
        x4.host.attach_card(X4::patterned(128 * 512));
        x4.reset();
        // Clock off: the block reads zero and ignores writes.
        x4.host.apply_system_gate(false, false);
        assert_eq!(x4.read(0x70), 0);
        x4.write(0x24, u32::MAX);
        // A reset pulse (PERIP_RST_EN1 bit 7) returns the block to power-on defaults…
        x4.host.apply_system_gate(true, true);
        x4.host.apply_system_gate(true, false);
        assert_eq!(x4.read(0x70), HCON);
        assert_eq!(x4.read(0x24), 0, "intmask back to reset default");
        assert_eq!(x4.read(0x50), 1, "slot-1 card still present across a host reset");
        // …and the card itself keeps its state (it sits outside the host's power domain).
        assert_eq!(x4.host.card.as_ref().unwrap().capacity_blocks(), 128);
    }

    #[test]
    fn dma_data_path_never_completes() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        x4.start_block(0);
        let ctrl = x4.read(0) | (1 << 25);                             // CTRL.use_internal_dma
        x4.write(0, ctrl);
        x4.issue(17);
        let mut words = 0usize;
        for _ in 0..100_000 {
            x4.result().ok();
            if words < 128 && x4.read(0x48) & (1 << 2) == 0 {
                let _ = x4.read(0x200);
                words += 1;
            }
            if x4.read(0x44) & INT_DTO != 0 && words == 128 { break; }
        }
        assert_ne!(words, 128, "no bus master here: the data phase honestly never delivers");
        assert_eq!(x4.read(0x30), 4 << 9, "the command phase completed normally; only the data phase hangs");
    }

    #[test]
    fn write_protected_card_rejects_writes() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        x4.host.card_mut().unwrap().set_write_protected(true);
        assert_eq!(x4.read(0x54), 1 << 1, "WRTTPRT mirrors the modeled switch");
        let block = [0x5a; 512];
        assert_eq!(x4.block_write(0, &block), Err("card"));
        let r = x4.read(0x30);
        assert_ne!(r & (1 << 26), 0, "WP_VIOLATION in the latched R1");
        assert_eq!(x4.host.dirty_bytes(), 0);
    }

    #[test]
    fn power_cycle_forgets_initialization() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        assert_eq!(x4.initialize(), 128);
        x4.host.card_mut().unwrap().set_power(false);                  // GPIO5 high on the X4
        assert_eq!(x4.command(13, 1 << 16, (1 << 6) | (1 << 8)), Err("timeout"), "unpowered card is silent");
        x4.host.card_mut().unwrap().set_power(true);
        // A read before re-initializing: the idle card answers with an illegal-command R1 and no
        // data, so the controller raises its data timeout — the driver quarantines the fault.
        x4.start_block(0);
        x4.issue(17);
        assert_eq!(x4.result(), Err("controller"), "DRTO on the refused data phase");
        assert_ne!(x4.read(0x30) & (1 << 22), 0, "ILLEGAL_COMMAND in the latched R1");
    }

    #[test]
    fn acmd_without_prefix_is_illegal_and_cmd1_is_mmc_only() {
        let mut x4 = X4::new(Some(X4::patterned(128 * 512)));
        x4.reset();
        x4.clock(false);
        let r = x4.command(41, 0x00ff_8000, 1 << 6).unwrap();          // ACMD41 without CMD55
        assert_ne!(r[0] & (1 << 22), 0, "ILLEGAL_COMMAND");
        let r = x4.command(1, 0, (1 << 6) | (1 << 8)).unwrap();        // CMD1 is MMC-only
        assert_ne!(r[0] & (1 << 22), 0, "ILLEGAL_COMMAND");
        assert_eq!(x4.read(0x48) & (0xf << 9), 0, "card stayed in idle");
    }
}
