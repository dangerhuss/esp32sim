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
            if pins.cs&(1<<12)!=0 {self.panel.fault();return vec![255;rx_len];}
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
    struct PanelFixture { board: X3Sd, cycle: u64 }
    impl PanelFixture {
        fn new() -> Self {Self {board:X3Sd::new(vec![0;4096]),cycle:0}}
        fn pins() -> SpiPins {SpiPins{sclk:1<<8,mosi:1<<10,miso:Some(7),cs:1<<21}}
        fn mask() -> u64 {(1<<4)|(1<<5)|(1<<12)|(1<<13)|(1<<21)}
        fn command(&mut self, byte: u8) {
            self.cycle+=1;self.board.gpio_output_at(self.cycle,&[],Self::mask(),Self::mask()&!((1<<4)|(1<<21)));
            self.board.spi_transfer_pins(2,Self::pins(),&[byte],0);
        }
        fn data(&mut self, bytes: &[u8]) {
            self.cycle+=1;self.board.gpio_output_at(self.cycle,&[],Self::mask(),Self::mask()&!(1<<21));
            self.board.spi_transfer_pins(2,Self::pins(),bytes,0);
        }
        fn complete_busy(&mut self) {self.cycle=self.board.next_deadline().unwrap();self.board.advance_to(self.cycle);}
        fn reinitialize(&mut self, old: bool) {
            if old {self.command(0x10);self.data(&vec![255;52_272]);self.command(0x11);}
            self.command(0x13);self.data(&vec![0;52_272]);self.command(0x11);
            for command in 0x20..=0x24 {self.command(command);let mut lut=[0;42];lut[1]=0x18;self.data(&lut);}
            self.command(4);self.complete_busy();
        }
        fn reset(&mut self) {
            self.cycle+=1;self.board.gpio_output_at(self.cycle,&[],Self::mask(),Self::mask()&!(1<<5));
            self.cycle+=1;self.board.gpio_output_at(self.cycle,&[],Self::mask(),Self::mask());
        }
    }
    #[test]
    fn hardware_reset_recovers_after_fault_without_erasing_cumulative_evidence() {
        let mut p=PanelFixture::new();p.command(0x13);p.data(&[0]);p.command(0x12);
        assert!(p.board.panel.faults>0);p.reinitialize(true);p.command(0x12);
        assert_eq!(p.board.display_frames(),0);assert!(p.board.next_deadline().is_none());
        let faults=p.board.panel.faults;p.reset();assert_eq!(p.board.panel.faults,faults);
        p.reinitialize(true);p.command(0x12);
        assert!(p.board.next_deadline().is_some(),"reset plus complete reinitialization must recover");
        assert_eq!(p.board.display_frames(),0);p.complete_busy();assert_eq!(p.board.display_frames(),1);
        assert_eq!(p.board.panel.faults,faults);assert!(p.board.report().contains(&format!("protocol_faults={faults}")));
    }
    #[test]
    fn hardware_reset_does_not_make_incomplete_reinitialization_valid() {
        let mut p=PanelFixture::new();p.command(0x13);p.data(&[0]);p.command(0x12);
        let faults=p.board.panel.faults;p.reset();p.reinitialize(false);p.command(0x12);
        assert_eq!(p.board.display_frames(),0);assert!(p.board.next_deadline().is_none());assert!(p.board.panel.faults>faults);
    }
    #[test]
    fn shared_select_fault_requires_actual_controller_reset_before_recovery() {
        let mut p=PanelFixture::new();p.command(0x11);
        let mut pins=PanelFixture::pins();pins.cs|=1<<12;
        p.board.spi_transfer_pins(2,pins,&[0x12],0);assert_eq!(p.board.panel.faults,1);
        p.reinitialize(true);p.command(0x12);assert_eq!(p.board.display_frames(),0);assert!(p.board.next_deadline().is_none());
        let faults=p.board.panel.faults;p.reset();p.reinitialize(true);p.command(0x12);p.complete_busy();
        assert_eq!(p.board.display_frames(),1);assert_eq!(p.board.panel.faults,faults);
    }

}
