//! C3 SPI2 GDMA using the common bounded descriptor walkers.
//! ESP-IDF 6.1-dev fff9895c, soc/esp32c3/register/soc/{spi_reg,gdma_reg}.h:
//! DMA_CONF RX/TX bits27/28; SPI2 GDMA peripheral trigger0. No silicon timing claim.
use super::{SocBus, DRAM_LOW, DRAM_IN_SRAM};
use emu_core::{Bus, Fault};
use esp_periph::gdma::{gather_out, SramOut};

impl SocBus {
    pub(super) fn spi2_dma_write(&mut self) {
        if let Some(bits) = self.periph.spi2.dma_tx_pending {
            let Some(ch) = self.periph.gdma.state.out_channel_for(0) else { return; };
            let needed = bits.div_ceil(8) as usize;
            let mut data = Vec::with_capacity(needed);
            let channel = &mut self.periph.gdma.state.out[ch];
            let previous_interrupts = channel.int_raw;
            let result = gather_out(&mut SramOut { base: DRAM_LOW, bytes: &mut self.sram[DRAM_IN_SRAM..], channel }, needed, &mut data);
            channel.running = false;
            match result {
                Ok(next) if data.len() == needed => { channel.desc = next; self.periph.spi2.complete_dma_tx(&data); }
                _ => {
                    // OUT_DSCR_ERR is bit2 in the shared channel; the C3 adapter
                    // maps it into the combined interrupt bank (IDF gdma_reg.h).
                    channel.int_raw = previous_interrupts | (1 << 2);
                    self.periph.spi2.fail_dma_tx();
                    self.irq_dirty = true;
                    return;
                }
            }
        }
        // Do not consume the card response before the driver arms its destination.
        let receive_dma = self.periph.spi2.regs.read(0x30) & (1 << 27) != 0
            && self.periph.spi2.regs.read(0x10) & (1 << 28) != 0;
        let rx_ch = if receive_dma {
            let Some(ch) = self.periph.gdma.state.in_channel_for(0) else { return; };
            Some(ch)
        } else { None };
        let Some(transfer) = self.periph.spi2.take_transfer() else { return; };
        if self.periph.spi2.log { eprintln!("[spi2] routes {:?} gpio enable={:#x} out={:#x}", self.periph.spi2_pins(), self.periph.gpio.enable, self.periph.gpio.out); }
        let rx = if self.board.uses_spi_pins() {
            self.board.spi_transfer_pins(2, self.periph.spi2_pins(), &transfer.tx, transfer.rx_len)
        } else { self.board.spi_transfer(2, &transfer.tx, transfer.rx_len) };
        if let Some(ch) = rx_ch {
            let mut channel = self.periph.gdma.state.inp[ch];
            let mut memory = SramDma { bytes: &mut self.sram[DRAM_IN_SRAM..] };
            let mut changed = false;
            let mut data = rx;
            data.resize(transfer.rx_len, 0xff);
            channel.receive(&mut memory, &data, Some(data.len() as u32), false, |_| false, &mut changed);
            channel.running = false;
            self.periph.gdma.state.inp[ch] = channel;
            self.periph.spi2.finish_transfer(transfer, &[]);
        } else { self.periph.spi2.finish_transfer(transfer, &rx); }
        self.irq_dirty = true;
    }
}

/// All DMA descriptor and payload accesses are restricted to data SRAM, including
/// descriptor writeback. A malformed chain cannot access MMIO or flash through Bus.
struct SramDma<'a> { bytes: &'a mut [u8] }
impl SramDma<'_> {
    fn range(&self, address: u32, width: usize) -> Result<std::ops::Range<usize>, Fault> {
        if !address.is_multiple_of(width as u32) { return Err(Fault::Misaligned); }
        let start = address.checked_sub(DRAM_LOW).ok_or(Fault::Unmapped)? as usize;
        let end = start.checked_add(width).filter(|&n| n <= self.bytes.len()).ok_or(Fault::Unmapped)?;
        Ok(start..end)
    }
}
impl Bus for SramDma<'_> {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> { Ok(self.bytes[self.range(a, 1)?][0]) }
    fn read16(&mut self, a: u32) -> Result<u16, Fault> { Ok(u16::from_le_bytes(self.bytes[self.range(a, 2)?].try_into().unwrap())) }
    fn read32(&mut self, a: u32) -> Result<u32, Fault> { Ok(u32::from_le_bytes(self.bytes[self.range(a, 4)?].try_into().unwrap())) }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> { let r = self.range(a, 1)?; self.bytes[r].copy_from_slice(&[v]); Ok(()) }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), Fault> { let r = self.range(a, 2)?; self.bytes[r].copy_from_slice(&v.to_le_bytes()); Ok(()) }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), Fault> { let r = self.range(a, 4)?; self.bytes[r].copy_from_slice(&v.to_le_bytes()); Ok(()) }
    fn fetch(&mut self, _: u32) -> Result<[u8; 4], Fault> { Err(Fault::Prohibited) }
    fn note_code_page(&mut self, _: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct Echo(Arc<Mutex<usize>>);
    impl esp_soc::BoardModel for Echo {
        fn name(&self) -> &'static str { "echo" }
        fn spi_transfer(&mut self, _: u8, tx: &[u8], n: usize) -> Vec<u8> {
            *self.0.lock().unwrap() += 1;
            tx.iter().copied().chain(std::iter::repeat(0xff)).take(n).collect()
        }
    }
    fn setup() -> (SocBus, Arc<Mutex<usize>>) {
        let calls = Arc::new(Mutex::new(0));
        let mut b = SocBus::new(1 << 20, [0; 6]); b.board = Box::new(Echo(calls.clone()));
        for (a, w) in [(DRAM_LOW, 0xc000_4004), (DRAM_LOW + 4, DRAM_LOW + 0x100), (DRAM_LOW + 8, 0),
            (DRAM_LOW + 12, 0x8000_0004), (DRAM_LOW + 16, DRAM_LOW + 0x200), (DRAM_LOW + 20, 0),
            (DRAM_LOW + 0x100, 0x4433_2211)] { b.write32(a, w).unwrap(); }
        // IDF6.1 C3 gdma_reg.h: OUT_CONF0/1 0xd0/d4, OUTLINK e0,
        // OUT_PERI_SEL100; IN_CONF1 74, INLINK80, IN_PERI_SELa0.
        for (off, v) in [(0xd0, 1 << 2), (0xd4, 1 << 12), (0x100, 0), (0xe0, (1 << 21) | (DRAM_LOW & 0xfffff)),
            (0x74, 1 << 12), (0xa0, 0), (0x80, (1 << 22) | ((DRAM_LOW + 12) & 0xfffff))] {
            b.write32(0x6003_f000 + off, v).unwrap();
        }
        for (off, v) in [(0x10, (1 << 27) | (1 << 28) | 1), (0x1c, 31), (0x30, (1 << 27) | (1 << 28))] {
            b.write32(0x6002_4000 + off, v).unwrap();
        }
        (b, calls)
    }
    #[test]
    fn full_duplex_gdma_returns_owner_length_eof_and_exact_bytes() {
        let (mut b, calls) = setup(); b.write32(0x6002_4000, 1 << 24).unwrap();
        assert_eq!(*calls.lock().unwrap(), 1); assert_eq!(b.read32(DRAM_LOW + 0x200).unwrap(), 0x4433_2211);
        assert_eq!(b.read32(DRAM_LOW).unwrap() >> 31, 0);
        assert_eq!(b.read32(DRAM_LOW + 12).unwrap(), 0x4000_4004);
        assert_eq!(b.periph.gdma.state.inp[0].int_raw, 3);
        assert_eq!(b.periph.gdma.state.out[0].int_raw, 3);
        assert_eq!(b.periph.spi2.read(0) & (1 << 24), 0);
    }
    #[test]
    fn waits_for_both_dma_directions_without_clocking_partial_work() {
        let (mut b, calls) = setup();
        b.periph.gdma.state.out[0].running = false;
        b.periph.gdma.state.inp[0].running = false;
        b.write32(0x6002_4000, 1 << 24).unwrap();
        assert_eq!(*calls.lock().unwrap(), 0);
        b.write32(0x6003_f0e0, (1 << 21) | (DRAM_LOW & 0xfffff)).unwrap();
        assert_eq!(*calls.lock().unwrap(), 0);
        b.write32(0x6003_f080, (1 << 22) | ((DRAM_LOW + 12) & 0xfffff)).unwrap();
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(b.read32(DRAM_LOW + 0x200).unwrap(), 0x4433_2211);
    }
    #[test]
    fn split_receive_returns_only_the_final_eof() {
        let (mut b, calls) = setup();
        for (a, v) in [(DRAM_LOW, 0xc000_8008), (DRAM_LOW + 20, DRAM_LOW + 24),
            (DRAM_LOW + 24, 0x8000_0004), (DRAM_LOW + 28, DRAM_LOW + 0x204),
            (DRAM_LOW + 32, 0), (DRAM_LOW + 0x104, 0x8877_6655)] { b.write32(a, v).unwrap(); }
        b.write32(0x6002_401c, 63).unwrap(); b.write32(0x6002_4000, 1 << 24).unwrap();
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(b.read32(DRAM_LOW + 12).unwrap(), 0x0000_4004);
        assert_eq!(b.read32(DRAM_LOW + 24).unwrap(), 0x4000_4004);
        assert_eq!(b.read32(DRAM_LOW + 0x204).unwrap(), 0x8877_6655);
        assert_eq!(b.periph.gdma.state.inp[0].eof_desc, DRAM_LOW + 24);
    }
    #[test]
    fn invalid_tx_never_clocks_the_card_or_reports_successful_eof() {
        for (control, address) in [(0x4000_4004, DRAM_LOW + 0x100), (0xc000_4004, 0x6000_4000), (0xc000_2002, DRAM_LOW + 0x100)] {
            let (mut b, calls) = setup(); b.write32(DRAM_LOW, control).unwrap(); b.write32(DRAM_LOW + 4, address).unwrap();
            b.write32(0x6002_4000, 1 << 24).unwrap();
            assert_eq!(*calls.lock().unwrap(), 0); assert_ne!(b.periph.gdma.state.out[0].int_raw & 4, 0);
            assert_eq!(b.periph.gdma.state.inp[0].int_raw & 3, 0);
            assert_eq!(b.periph.gdma.state.out[0].int_raw & 3, 0);
        }
    }
    #[test]
    fn invalid_rx_owner_or_mmio_destination_never_gets_successful_eof() {
        for (control, address) in [(4, DRAM_LOW + 0x200), (0x8000_0004, 0x6000_4000)] {
            let (mut b, _) = setup(); b.write32(DRAM_LOW + 12, control).unwrap(); b.write32(DRAM_LOW + 16, address).unwrap();
            b.write32(0x6002_4000, 1 << 24).unwrap();
            assert_ne!(b.periph.gdma.state.inp[0].int_raw & (1 << 3), 0);
            assert_eq!(b.periph.gdma.state.inp[0].int_raw & 3, 0);
            assert_eq!(b.read32(DRAM_LOW + 0x200).unwrap(), 0);
        }
    }
}
