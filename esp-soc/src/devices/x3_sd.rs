//! X3 shared SPI2: SCLK8/MOSI10/MISO7, SD CS12, UC8253 CS21, rail13.
//! Panel timing is nominal; neither card latency nor optical behavior is silicon calibrated.
use crate::board::{BoardModel, SpiPins, BoardEdge};
use esp_periph::sdcard::spi::SpiSdCard;
use super::x3_panel::X3Panel;

pub struct X3Sd { card: SpiSdCard, powered: bool, panel_selected: bool, panel:X3Panel }
impl X3Sd {
    pub fn new(image: Vec<u8>) -> Self {
        let mut card = SpiSdCard::new(image); card.set_power(false);
        Self { card, powered: false, panel_selected: true, panel:X3Panel::new() }
    }
}
impl BoardModel for X3Sd {
    fn name(&self) -> &'static str { "x3-sd" }
    fn uses_gpio_edges(&self) -> bool { true }
    fn uses_spi_pins(&self) -> bool { true }
    fn gpio_output_at(&mut self, cycle: u64, _: &[(u8, bool)], enabled: u64, output: u64) {
        self.panel.gpio(cycle,enabled,output);
        self.powered = enabled & output & (1 << 13) != 0;
        self.panel_selected = enabled & output & (1 << 21) == 0;
        self.card.set_power(self.powered);
        self.card.select(enabled & (1 << 12) != 0 && output & (1 << 12) == 0);
    }
    fn spi_transfer_pins(&mut self, host: u8, pins: SpiPins, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if host != 2 || !self.powered || pins.sclk & (1 << 8) == 0
            || pins.mosi & (1 << 10) == 0 || (rx_len != 0 && pins.miso != Some(7)) { return vec![0xff; rx_len]; }
        if pins.cs&(1<<21)!=0 {
            if pins.cs&(1<<12)!=0 {self.panel.faults+=1;return vec![255;rx_len];}
            self.card.select(false);self.panel.transfer(tx);return vec![255;rx_len];
        }
        if self.panel_selected {return vec![255;rx_len];}
        self.card.select(pins.cs & (1 << 12) != 0);
        self.card.transfer(tx, rx_len)
    }
    fn sd_image_bytes(&self) -> Option<&[u8]> { Some(self.card.image()) }
    fn input_levels(&self)->Vec<(u8,bool)>{vec![(3,true),(6,self.panel.idle())]}
    fn next_deadline(&self)->Option<u64>{self.panel.deadline()}
    fn advance_to(&mut self,cycle:u64){self.panel.advance(cycle);}
    fn take_edges(&mut self)->Vec<BoardEdge>{self.panel.edges()}
    fn display(&self)->Option<(u32,u32,Vec<u16>,u64)>{Some(self.panel.display())}
    fn display_frames(&self)->u64{self.panel.frames}
    fn display_version(&self)->u64{self.panel.frames}
    fn named_pin(&self,name:&str)->Option<u8>{(name=="power").then_some(3)}
    fn report(&self)->String{format!("[x3] frames={} protocol_faults={} nominal_BUSY=true",self.panel.frames,self.panel.faults)}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selecting_both_devices_is_refused_and_sd_keeps_panel_baseline() {
        let mut board=X3Sd::new(vec![0;4096]);
        let mask=(1<<12)|(1<<13)|(1<<21)|(1<<5)|(1<<4);
        board.gpio_output_at(0,&[],mask,mask);
        let mut pins=SpiPins{sclk:1<<8,mosi:1<<10,miso:Some(7),cs:(1<<12)|(1<<21)};
        assert_eq!(board.spi_transfer_pins(2,pins,&[0x12],1),[255]);
        assert_eq!(board.panel.faults,1);assert_eq!(board.panel.frames,0);
        pins.cs=0;board.spi_transfer_pins(2,pins,&[255;10],0);
        pins.cs=1<<12;board.spi_transfer_pins(2,pins,&[0x40,0,0,0,0,0x95],0);
        assert_eq!(board.spi_transfer_pins(2,pins,&[],2),[255,1]);
        assert_eq!(board.panel.frames,0);
    }
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
