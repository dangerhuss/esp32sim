//! SDMMC IDMAC bus master. The first qualified path uses internal DMA SRAM;
//! PSRAM DMA is not silently accepted through the CPU's cache mapping.
use super::{SocBus, DRAM_LOW, DRAM_HIGH};
use esp_periph::sdmmc_host::SdDmaMemory;
use xtensa_lx7::bus::Bus;

impl SocBus {
    pub(super) fn sdmmc_dma_step(&mut self) {
        let mut host = std::mem::take(&mut self.periph.sdmmc);
        host.dma_step(self);
        self.periph.sdmmc = host;
        self.irq_dirty = true;
    }
}

impl SdDmaMemory for SocBus {
    fn word(&mut self, address: u32) -> Option<u32> {
        if !self.valid_data(address, 4) || address & 3 != 0 { return None; }
        self.read32_unpriced(address).ok()
    }
    fn write_word(&mut self, address: u32, value: u32) -> bool {
        self.valid_data(address, 4) && address & 3 == 0 && self.write32_unpriced(address, value).is_ok()
    }
    fn valid_data(&self, address: u32, count: usize) -> bool {
        address >= DRAM_LOW && address.checked_add(count as u32).is_some_and(|end| end <= DRAM_HIGH)
    }
    fn read_data(&mut self, address: u32, count: usize) -> Option<Vec<u8>> {
        if !self.valid_data(address, count) { return None; }
        (0..count).map(|i| self.read8_unpriced(address + i as u32).ok()).collect()
    }
    fn write_data(&mut self, address: u32, data: &[u8]) -> bool {
        self.valid_data(address, data.len()) && data.iter().enumerate().all(|(i, b)|
            self.write8_unpriced(address + i as u32, *b).is_ok())
    }
}
