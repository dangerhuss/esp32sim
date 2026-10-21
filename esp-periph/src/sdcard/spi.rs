//! SD memory-card SPI transport (SD Physical Layer Simplified v5.10, section 7).
//! Reuses the native card's finite media/CSD/CID; no host filesystem or SDK shortcuts.
//! Functional byte transfers, not card latency, wear or electrical qualification.
use std::collections::VecDeque;
use super::SdCard;

pub struct SpiSdCard {
    card: SdCard,
    selected: bool,
    startup_clocks: usize,
    spi_mode: bool,
    idle: bool,
    app: bool,
    crc: bool,
    command: Vec<u8>,
    output: VecDeque<u8>,
    read_sector: Option<u32>,
    write_sector: Option<u32>,
    multi_write: bool,
    writing: bool,
    block: Vec<u8>,
}
impl SpiSdCard {
    pub fn new(image: Vec<u8>) -> Self {
        Self { card: SdCard::new(image), selected: false, startup_clocks: 0,
            spi_mode: false, idle: true, app: false, crc: false, command: Vec::with_capacity(6),
            output: VecDeque::new(), read_sector: None, write_sector: None,
            multi_write: false, writing: false, block: Vec::with_capacity(514) }
    }
    pub fn image(&self) -> &[u8] { self.card.image() }
    pub fn set_write_protected(&mut self, on: bool) { self.card.set_write_protected(on); }
    pub fn set_power(&mut self, on: bool) {
        if self.card.powered() && !on {
            self.select(false);
            self.startup_clocks = 0; self.spi_mode = false; self.idle = true;
            self.app = false; self.crc = false;
        }
        self.card.set_power(on);
    }
    pub fn select(&mut self, on: bool) {
        if !on {
            self.command.clear(); self.output.clear(); self.read_sector = None;
            self.write_sector = None; self.writing = false; self.block.clear();
        }
        self.selected = on;
    }
    /// Clock bytes full duplex, returning only the master's requested MISO bytes.
    pub fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        let clocks = tx.len().max(rx_len);
        if !self.card.powered() { return vec![0xff; rx_len]; }
        if !self.selected {
            self.startup_clocks = self.startup_clocks.saturating_add(clocks * 8).min(80);
            return vec![0xff; rx_len];
        }
        let mut rx = Vec::with_capacity(rx_len);
        for i in 0..clocks {
            let input = tx.get(i).copied().unwrap_or(0xff);
            // A command (CMD12) can interrupt a multi-block read. Keep its response
            // behind the currently clocked byte; newly queued bytes start next clock.
            if self.output.is_empty() && self.command.is_empty() && input & 0xc0 != 0x40 {
                if let Some(sector) = self.read_sector {
                    if sector < self.card.capacity_blocks() {
                        self.queue_sector(sector); self.read_sector = sector.checked_add(1);
                    } else { self.output.push_back(0x08); self.read_sector = None; }
                }
            }
            let out = self.output.pop_front().unwrap_or(0xff);
            if i < rx_len { rx.push(out); }
            self.consume(input);
        }
        rx
    }
    fn consume(&mut self, byte: u8) {
        if self.writing {
            self.block.push(byte);
            if self.block.len() == 514 {
                let good_crc = !self.crc || crc16(&self.block[..512]) == u16::from_be_bytes([self.block[512], self.block[513]]);
                let sector = self.write_sector.unwrap();
                let start = sector as usize * 512;
                let accepted = good_crc && !self.card.write_protected() && start + 512 <= self.card.image.len();
                if accepted {
                    self.card.image[start..start + 512].copy_from_slice(&self.block[..512]);
                    self.card.dirty_bytes = self.card.dirty_bytes.saturating_add(512);
                }
                self.output.extend([if accepted { 0x05 } else if !good_crc { 0x0b } else { 0x0d }, 0, 0xff]);
                self.block.clear(); self.writing = false;
                self.write_sector = if self.multi_write { sector.checked_add(1) } else { None };
            }
            return;
        }
        if self.write_sector.is_some() {
            if self.multi_write && byte == 0xfd { self.write_sector = None; self.output.extend([0, 0xff]); }
            else if byte == if self.multi_write { 0xfc } else { 0xfe } { self.writing = true; self.block.clear(); }
            return;
        }
        if self.command.is_empty() && byte & 0xc0 != 0x40 { return; }
        self.command.push(byte);
        if self.command.len() != 6 { return; }
        let index = self.command[0] & 0x3f;
        let arg = u32::from_be_bytes(self.command[1..5].try_into().unwrap());
        let valid_crc = (crc7(&self.command[..5]) << 1) | 1;
        let bad_crc = self.command[5] & 1 == 0 || ((self.crc || index == 0 || index == 8) && self.command[5] != valid_crc);
        self.command.clear();
        if !self.spi_mode && (index != 0 || self.startup_clocks < 74) { return; }
        if bad_crc { self.output.extend([0xff, self.r1() | 8]); return; }
        let app = self.app; self.app = false;
        self.execute(index, arg, app);
        self.output.push_front(0xff); // Ncr: at least one byte between command and R1.
    }
    fn r1(&self) -> u8 { u8::from(self.idle) }
    fn sector(&self, arg: u32) -> Option<u32> {
        let sector = if self.card.high_capacity() { arg } else { if !arg.is_multiple_of(512) { return None; } arg / 512 };
        (sector < self.card.capacity_blocks()).then_some(sector)
    }
    fn packet(&mut self, bytes: &[u8]) {
        self.output.extend([0xff, 0xfe]); self.output.extend(bytes);
        self.output.extend(crc16(bytes).to_be_bytes());
    }
    fn queue_sector(&mut self, sector: u32) {
        let start = sector as usize * 512;
        if let Some(bytes) = self.card.image.get(start..start + 512) {
            let block: [u8; 512] = bytes.try_into().unwrap(); self.packet(&block);
        } else { self.output.push_back(0x08); self.read_sector = None; }
    }
    fn execute(&mut self, cmd: u8, arg: u32, app: bool) {
        let status = self.r1();
        match (app, cmd) {
            (_, 0) => {
                self.output.clear(); self.read_sector = None; self.write_sector = None;
                self.spi_mode = true; self.idle = true; self.crc = false; self.output.push_back(1);
            }
            (false, 8) => { self.output.push_back(status); self.output.extend((arg & 0xfff).to_be_bytes()); }
            (false, 55) => { self.app = true; self.output.push_back(status); }
            (true, 41) => { self.idle = false; self.output.push_back(0); }
            (false, 58) => {
                self.output.push_back(status);
                let ocr = 0x00ff_8000u32 | if self.idle { 0 } else { 1 << 31 } | if self.card.high_capacity() { 1 << 30 } else { 0 };
                self.output.extend(ocr.to_be_bytes());
            }
            (false, 59) => { self.crc = arg & 1 != 0; self.output.push_back(status); }
            _ if self.idle => self.output.push_back(status | 4),
            (false, 9 | 10) => {
                let words = if cmd == 9 { self.card.csd } else { self.card.cid };
                let mut bytes = [0u8; 16];
                for (i, word) in words.iter().rev().enumerate() { bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes()); }
                self.output.push_back(0); self.packet(&bytes);
            }
            (false, 16) => self.output.push_back(if arg == 512 { 0 } else { 0x40 }),
            (false, 17 | 18 | 24 | 25) => {
                let Some(sector) = self.sector(arg) else { self.output.push_back(0x20); return; };
                self.output.push_back(0);
                if cmd == 17 || cmd == 18 {
                    self.queue_sector(sector);
                    if cmd == 18 { self.read_sector = sector.checked_add(1); }
                } else { self.write_sector = Some(sector); self.multi_write = cmd == 25; }
            }
            (false, 12) => { self.read_sector = None; self.output.clear(); self.output.extend([0xff, 0, 0, 0xff]); }
            (false, 13) => self.output.extend([0, 0]),
            (true, 23) => self.output.push_back(0),
            (true, 51) => { self.output.push_back(0); self.packet(&[0, 1, 0, 0, 0, 0, 0, 0]); }
            (true, 13) => { self.output.push_back(0); self.packet(&[0; 64]); }
            _ => self.output.push_back(status | 4),
        }
    }
}
fn crc7(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in bytes { for bit in (0..8).rev() { let carry = ((crc >> 6) ^ (byte >> bit)) & 1; crc = (crc << 1) & 0x7f; if carry != 0 { crc ^= 9; } } }
    crc
}
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes { crc ^= u16::from(byte) << 8; for _ in 0..8 { crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 }; } }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(card: &mut SpiSdCard, cmd: u8, arg: u32) -> Vec<u8> {
        let mut bytes = vec![0x40 | cmd]; bytes.extend(arg.to_be_bytes()); bytes.push((crc7(&bytes) << 1) | 1);
        card.transfer(&bytes, 0); card.transfer(&[], 9)[1..].to_vec()
    }
    fn ready() -> SpiSdCard {
        let mut c = SpiSdCard::new(vec![0xa5; 4096]);
        c.transfer(&[0xff; 10], 0); c.select(true);
        assert_eq!(command(&mut c, 0, 0)[0], 1);
        assert_eq!(&command(&mut c, 8, 0x1aa)[..5], &[1, 0, 0, 1, 0xaa]);
        assert_eq!(command(&mut c, 55, 0)[0], 1); assert_eq!(command(&mut c, 41, 0)[0], 0);
        assert_eq!(command(&mut c, 59, 1)[0], 0); c
    }
    #[test]
    fn fragmented_write_crc_readback_and_deselect_custody() {
        let mut c = ready(); assert_eq!(command(&mut c, 24, 512)[0], 0);
        c.transfer(&[0xfe], 0); c.transfer(&[0x36; 200], 0); c.transfer(&[0x36; 312], 0);
        let mut tail = crc16(&[0x36; 512]).to_be_bytes().to_vec(); tail.push(0xff);
        assert_eq!(c.transfer(&tail, 3)[2], 5);
        c.transfer(&[], 2); c.select(false); c.select(true);
        let r = command(&mut c, 17, 512); assert_eq!(&r[..3], &[0, 0xff, 0xfe]);
        let mut received = r[3..].to_vec(); received.extend(c.transfer(&[], 509));
        assert_eq!(&received[..512], &[0x36; 512]); assert_eq!(&received[512..514], &crc16(&[0x36; 512]).to_be_bytes());
        // Invalid CRC and a truncated packet never change media.
        c.select(false); c.select(true); command(&mut c, 24, 1024); c.transfer(&[0xfe], 0);
        c.transfer(&[0x77; 512], 0); assert_eq!(c.transfer(&[0, 0, 0xff], 3)[2], 0x0b);
        c.select(false); c.select(true); command(&mut c, 24, 1536); c.transfer(&[0xfe, 1, 2], 0); c.select(false);
        assert_eq!(&c.image()[1024..2048], &[0xa5; 1024]);
    }
    #[test]
    fn bounds_power_selection_and_command_crc() {
        let mut c = ready(); assert_eq!(command(&mut c, 17, 4096)[0], 0x20);
        assert_eq!(command(&mut c, 17, 1)[0], 0x20);
        c.transfer(&[0x40, 0, 0, 0, 0, 1], 0); assert_eq!(c.transfer(&[], 2), [0xff, 8]);
        c.select(false); assert_eq!(c.transfer(&[0x40, 0, 0, 0, 0, 0x95], 8), [0xff; 8]);
        c.set_power(false); c.select(true); assert_eq!(c.transfer(&[0xff; 8], 8), [0xff; 8]);
    }
    #[test]
    fn multiblock_write_stop_and_read_stop() {
        let mut c = ready(); command(&mut c, 25, 0);
        for byte in [0x12, 0x34] {
            c.transfer(&[0xfc], 0); c.transfer(&[byte; 512], 0);
            let mut tail = crc16(&[byte; 512]).to_be_bytes().to_vec(); tail.extend([0xff; 3]);
            assert_eq!(c.transfer(&tail, 5)[2], 5);
        }
        c.transfer(&[0xfd], 0); c.transfer(&[], 3); c.select(false); c.select(true);
        let r = command(&mut c, 18, 0); assert_eq!(&r[..3], &[0, 0xff, 0xfe]);
        c.transfer(&[], 509); // rest of first block and CRC
        let second = c.transfer(&[], 516); assert_eq!(&second[..2], &[0xff, 0xfe]); assert_eq!(&second[2..514], &[0x34; 512]);
        let r = command(&mut c, 12, 0); assert_eq!(&r[..3], &[0xff, 0, 0]);
        assert_eq!(c.transfer(&[], 600), vec![0xff; 600]);
    }
}
