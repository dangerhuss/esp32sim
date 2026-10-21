//! X3 card-only fixture: SPI2 SCLK8/MOSI10/MISO7, SD CS12, rail13.
//! The panel (CS21) must be deselected. This is not a display or whole-board model.
use crate::board::{BoardModel, SpiPins};
use esp_periph::sdcard::spi::SpiSdCard;

pub struct X3Sd { card: SpiSdCard, powered: bool, panel_selected: bool }
impl X3Sd {
    pub fn new(image: Vec<u8>) -> Self {
        let mut card = SpiSdCard::new(image); card.set_power(false);
        Self { card, powered: false, panel_selected: true }
    }
}
impl BoardModel for X3Sd {
    fn name(&self) -> &'static str { "x3-sd" }
    fn uses_gpio_edges(&self) -> bool { false }
    fn uses_spi_pins(&self) -> bool { true }
    fn gpio_output_at(&mut self, _: u64, _: &[(u8, bool)], enabled: u64, output: u64) {
        self.powered = enabled & output & (1 << 13) != 0;
        self.panel_selected = enabled & output & (1 << 21) == 0;
        self.card.set_power(self.powered);
        self.card.select(enabled & (1 << 12) != 0 && output & (1 << 12) == 0);
    }
    fn spi_transfer_pins(&mut self, host: u8, pins: SpiPins, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if host != 2 || !self.powered || self.panel_selected || pins.sclk & (1 << 8) == 0
            || pins.mosi & (1 << 10) == 0 || (rx_len != 0 && pins.miso != Some(7)) { return vec![0xff; rx_len]; }
        self.card.select(pins.cs & (1 << 12) != 0);
        self.card.transfer(tx, rx_len)
    }
    fn sd_image_bytes(&self) -> Option<&[u8]> { Some(self.card.image()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_actual_power_routes_and_selected_card_respond() {
        let mut board = X3Sd::new(vec![0; 4096]);
        let mut pins = SpiPins { sclk: 1 << 8, mosi: 1 << 10, miso: Some(7), cs: 0 };
        let mask = (1 << 12) | (1 << 13) | (1 << 21);
        board.gpio_output_at(0, &[], mask, mask);
        board.spi_transfer_pins(2, pins, &[0xff; 10], 0);
        board.gpio_output_at(1, &[], mask, mask & !(1 << 12)); pins.cs = 1 << 12;
        board.spi_transfer_pins(2, pins, &[0x40, 0, 0, 0, 0, 0x95], 0);
        assert_eq!(board.spi_transfer_pins(2, pins, &[], 2), [0xff, 1]);
        pins.miso = Some(6); assert_eq!(board.spi_transfer_pins(2, pins, &[], 1), [0xff]);
        pins.miso = Some(7); board.gpio_output_at(2, &[], mask, 1 << 13);
        assert_eq!(board.spi_transfer_pins(2, pins, &[], 1), [0xff]);
        board.gpio_output_at(3, &[], mask, 0);
        assert_eq!(board.spi_transfer_pins(2, pins, &[], 1), [0xff]);
        assert_eq!(board.sd_image_bytes().unwrap(), &[0; 4096]);
    }
}
