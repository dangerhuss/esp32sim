//! X4 Pro UC8279/GT911 board boundary. Pin/geometry/command facts follow Crusty's
//! x4_display.rs and boards.rs; BUSY durations below are nominal, NOT silicon calibration.
use esp_soc::board::{BoardEdge, BoardModel, SpiPins};
use std::sync::{Arc, Mutex};

const HZ: u64 = 240_000_000;
const WIRE: usize = 60_000; // 800 columns, 120 unbonded + 480 visible rows.

pub struct X4Pro {
    old: Vec<u8>,
    new: Vec<u8>,
    shown: Vec<u8>,
    command: u8,
    position: usize,
    old_valid: bool,
    new_valid: bool,
    enabled: u64,
    output: u64,
    clock: u64,
    deadline: Option<u64>,
    refresh_pending: bool,
    powered: bool,
    partial: bool,
    window_data: [u8; 9],
    window: Option<(usize, usize, usize, usize)>,
    frames: u64,
    faults: u64,
    gpio_events: u64,
    edges: Vec<BoardEdge>,
    touch: Arc<Mutex<crate::i2c::TouchState>>,
}
impl Default for X4Pro {
    fn default() -> Self {
        Self::new()
    }
}
impl X4Pro {
    pub fn new() -> Self {
        Self {
            old: vec![255; WIRE],
            new: vec![255; WIRE],
            shown: vec![255; 48_000],
            command: 0,
            position: 0,
            old_valid: false,
            new_valid: false,
            enabled: 0,
            output: 0,
            clock: 0,
            deadline: None,
            refresh_pending: false,
            powered: false,
            partial: false,
            window_data: [0; 9],
            window: None,
            frames: 0,
            faults: 0,
            gpio_events: 0,
            edges: Vec::new(),
            touch: Default::default(),
        }
    }
    fn busy(&mut self, us: u64, refresh: bool) {
        self.deadline = Some(self.clock.saturating_add(us * HZ / 1_000_000));
        self.refresh_pending = refresh;
        self.edges.push(BoardEdge {
            cycle: self.clock,
            pin: 6,
            level: false,
        });
    }
    fn fault(&mut self) {
        self.faults += 1;
    }
    fn command(&mut self, byte: u8) {
        if self.deadline.is_some() {
            self.fault();
            return;
        }
        if matches!(self.command, 0x10 | 0x13) && self.position != WIRE {
            self.fault();
        }
        self.command = byte;
        self.position = 0;
        match byte {
            0x10 => self.old_valid = false,
            0x13 => self.new_valid = false,
            0x04 => {
                self.powered = true;
                self.busy(2_000, false);
            }
            0x02 => {
                self.powered = false;
                self.busy(2_000, false);
            }
            0x12 => {
                if !self.powered
                    || !self.new_valid
                    || !self.old_valid
                    || self.faults != 0
                    || (self.partial && self.window.is_none())
                {
                    self.fault();
                    return;
                }
                self.busy(if self.partial { 150_000 } else { 600_000 }, true);
            }
            0x91 => self.partial = true,
            0x92 => self.partial = false,
            0x90 => self.window = None,
            _ => {}
        }
    }
    fn data(&mut self, bytes: &[u8]) {
        if self.deadline.is_some() {
            self.fault();
            return;
        }
        if self.command == 0x90 {
            // UC8279 PTL: inclusive, byte-aligned X and physical gate Y.
            // FreeInk UC8279X4Driver::startBwRefresh, pinned 28edea3a.
            // A window clips driven pixels; nominal BUSY is intentionally not
            // scaled by area without physical calibration.
            if bytes.len() > 9_usize.saturating_sub(self.position) {
                self.fault();
                return;
            }
            self.window_data[self.position..self.position + bytes.len()].copy_from_slice(bytes);
            self.position += bytes.len();
            if self.position == 9 {
                let d = self.window_data;
                let x0 = usize::from(u16::from_be_bytes([d[0], d[1]]));
                let x1 = usize::from(u16::from_be_bytes([d[2], d[3]]));
                let y0 = usize::from(u16::from_be_bytes([d[4], d[5]]));
                let y1 = usize::from(u16::from_be_bytes([d[6], d[7]]));
                if x0 > x1
                    || x1 >= 800
                    || y0 > y1
                    || y1 >= 600
                    || x0 % 8 != 0
                    || x1 % 8 != 7
                    || d[8] > 1
                {
                    self.fault();
                } else {
                    self.window = Some((x0, x1, y0, y1));
                }
            }
        } else if matches!(self.command, 0x10 | 0x13) {
            if bytes.len() > WIRE.saturating_sub(self.position) {
                self.fault();
                return;
            }
            let target = if self.command == 0x10 {
                &mut self.old
            } else {
                &mut self.new
            };
            target[self.position..self.position + bytes.len()].copy_from_slice(bytes);
            self.position += bytes.len();
            if self.position == WIRE {
                if self.command == 0x10 {
                    self.old_valid = true;
                } else {
                    self.new_valid = true;
                }
            }
        }
    }
}
impl BoardModel for X4Pro {
    fn name(&self) -> &'static str {
        "x4pro"
    }
    fn uses_spi_pins(&self) -> bool {
        true
    }
    fn gpio_output_at(&mut self, cycle: u64, changes: &[(u8, bool)], enabled: u64, output: u64) {
        self.advance_to(cycle);
        self.gpio_events += changes.len() as u64;
        // Hardware reset invalidates both controller planes. Never invent an old-plane baseline.
        if enabled & (1 << 14) != 0
            && output & (1 << 14) == 0
            && (self.enabled & (1 << 14) == 0 || self.output & (1 << 14) != 0)
        {
            self.command = 0;
            self.position = 0;
            self.old_valid = false;
            self.new_valid = false;
            self.powered = false;
            self.partial = false;
            self.window = None;
            self.deadline = None;
            self.refresh_pending = false;
            self.edges.push(BoardEdge {
                cycle,
                pin: 6,
                level: true,
            });
        }
        self.enabled = enabled;
        self.output = output;
    }
    fn spi_transfer_pins(&mut self, host: u8, pins: SpiPins, tx: &[u8], rx: usize) -> Vec<u8> {
        let live = self.enabled & self.output & (1 << 1) != 0
            && self.enabled & self.output & (1 << 14) != 0;
        if host != 2
            || !live
            || pins.sclk & (1 << 12) == 0
            || pins.mosi & (1 << 11) == 0
            || pins.cs & (1 << 13) == 0
        {
            return vec![255; rx];
        }
        if self.output & (1 << 18) != 0 {
            self.data(tx);
        } else {
            for &byte in tx {
                self.command(byte);
            }
        }
        vec![255; rx]
    }
    fn i2c_devices(&mut self) -> Vec<(u8, u8, Box<dyn esp_periph::i2c::I2cDevice>)> {
        vec![(
            0,
            0x5d,
            Box::new(crate::i2c::Gt911::new(self.touch.clone(), 800, 480)),
        )]
    }
    fn touch(&mut self, x: u16, y: u16, down: bool) {
        self.touch.lock().unwrap().update(x, y, down);
    }
    fn input_levels(&self) -> Vec<(u8, bool)> {
        vec![
            (0, true),
            (7, true),
            (3, true),
            (6, self.deadline.is_none()),
            (21, false),
        ]
    }
    fn next_deadline(&self) -> Option<u64> {
        self.deadline
    }
    fn advance_to(&mut self, cycle: u64) {
        self.clock = cycle;
        if self.deadline.is_some_and(|deadline| cycle >= deadline) {
            if self.refresh_pending {
                if self.partial {
                    let (x0, x1, y0, y1) = self.window.expect("refresh validated PTL");
                    for y in y0.max(120)..=y1.min(599) {
                        let first = (y - 120) * 100 + x0 / 8;
                        let end = (y - 120) * 100 + x1 / 8 + 1;
                        self.shown[first..end]
                            .copy_from_slice(&self.new[12_000 + first..12_000 + end]);
                    }
                } else {
                    self.shown.copy_from_slice(&self.new[12_000..]);
                }
                self.frames += 1;
            }
            self.refresh_pending = false;
            let at = self.deadline.take().unwrap();
            self.edges.push(BoardEdge {
                cycle: at,
                pin: 6,
                level: true,
            });
        }
    }
    fn take_edges(&mut self) -> Vec<BoardEdge> {
        std::mem::take(&mut self.edges)
    }
    fn display(&self) -> Option<(u32, u32, Vec<u16>, u64)> {
        Some((
            800,
            480,
            self.shown
                .iter()
                .flat_map(|byte| {
                    (0..8).map(move |bit| if byte & (128 >> bit) != 0 { 65535 } else { 0 })
                })
                .collect(),
            self.frames,
        ))
    }
    fn display_frames(&self) -> u64 {
        self.frames
    }
    fn display_version(&self) -> u64 {
        self.frames
    }
    fn named_pin(&self, name: &str) -> Option<u8> {
        match name {
            "left" => Some(0),
            "right" => Some(7),
            "power" => Some(3),
            _ => None,
        }
    }
    fn report(&self) -> String {
        format!(
            "[x4pro] frames={} protocol_faults={} nominal_BUSY=true",
            self.frames, self.faults
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configured() -> X4Pro {
        let mut b = X4Pro::new();
        let mask = (1 << 1) | (1 << 13) | (1 << 14) | (1 << 18);
        b.gpio_output_at(0, &[], mask, mask);
        b
    }
    #[test]
    fn incomplete_plane_cannot_refresh() {
        let mut b = configured();
        b.command(0x13);
        b.data(&[0]);
        b.command(0x12);
        assert_eq!(b.frames, 0);
        assert!(b.faults > 0);
    }
    #[test]
    fn pixels_publish_only_after_busy_completion_and_crop_unbonded_rows() {
        let mut b = configured();
        b.command(0x10);
        b.data(&vec![255; WIRE]);
        b.command(0x13);
        let mut bytes = vec![255; WIRE];
        bytes[12_000] = 0x7f;
        b.data(&bytes);
        b.command(4);
        b.advance_to(HZ / 500);
        b.command(0x12);
        b.advance_to(HZ / 2);
        assert_eq!(b.frames, 0);
        b.advance_to(HZ);
        assert_eq!(b.frames, 1);
        assert_eq!(b.shown[0], 0x7f);
        assert_eq!(b.faults, 0);
    }
    #[test]
    fn wrong_routes_do_not_reach_panel() {
        let mut b = configured();
        b.spi_transfer_pins(2, SpiPins::default(), &[0x13], 0);
        assert_eq!(b.command, 0);
    }
    #[test]
    fn partial_window_changes_only_selected_glass_after_busy() {
        let mut b = configured();
        b.command(0x10);
        b.data(&vec![255; WIRE]);
        b.command(0x13);
        b.data(&vec![0; WIRE]);
        b.command(4);
        b.advance_to(HZ / 500);
        b.command(0x91);
        b.command(0x90);
        // Split SPI writes must still produce one nine-byte window.
        b.data(&[0, 8, 0, 15]);
        b.data(&[0, 120, 0, 121, 1]);
        b.command(0x12);
        assert_eq!(b.shown[1], 255);
        b.advance_to(b.deadline.unwrap());
        assert_eq!(b.frames, 1);
        assert_eq!(b.faults, 0);
        for (i, byte) in b.shown.iter().enumerate() {
            assert_eq!(*byte, if i == 1 || i == 101 { 0 } else { 255 });
        }
    }
    #[test]
    fn missing_invalid_or_reset_window_cannot_claim_a_partial_frame() {
        for data in [
            &[][..],
            &[0, 1, 0, 15, 0, 120, 0, 121, 1][..],
            &[0, 0, 3, 31, 0, 120, 2, 88, 1][..],
        ] {
            let mut b = configured();
            b.command(0x10);
            b.data(&vec![255; WIRE]);
            b.command(0x13);
            b.data(&vec![0; WIRE]);
            b.command(4);
            b.advance_to(HZ / 500);
            b.command(0x91);
            if !data.is_empty() {
                b.command(0x90);
                b.data(data);
            }
            b.command(0x12);
            assert_eq!(b.frames, 0);
            assert!(b.faults > 0);
        }
        let mut b = configured();
        b.command(0x90);
        b.data(&[0, 0, 0, 7, 0, 120, 0, 121, 1]);
        assert!(b.window.is_some());
        b.gpio_output_at(1, &[], 1 << 14, 0);
        assert!(b.window.is_none() && !b.partial);
    }
}
