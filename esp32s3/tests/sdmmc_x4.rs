//! The X4 firmware's SDMMC transaction contract against the registered S3 block: the SYSTEM RCC
//! clock/reset dance, the full card initialization (CMD0/8/55/ACMD41/2/3/9/7/16), block I/O
//! through the CPU FIFO port — all through the real `Peripherals` dispatch at 0x60028000,
//! interrupt source included. Register layout cited in `esp-periph/src/sdmmc_host.rs` (IDF
//! v5.5.1 `sdmmc_struct.h`/`sdmmc_ll.h`; driver contract from Crusty `x4_storage.rs`).
use esp32s3::periph::{Peripherals, PERIPH_BASE, SRC_SDMMC};
const SDMMC: u32 = 0x6002_8000;
// SYSTEM is at 0x600c_0000: PERIPH_BASE + 0xc0000 (soc/system_reg.h DR_REG_SYSTEM_BASE, IDF v5.5.1).
const SYSTEM: u32 = PERIPH_BASE + 0xc0000;
const R1_ERRORS: u32 = 0xfdff_e008; // the X4 driver's R1 mask
const RINTSTS_ERRORS: u32 = 0xbfc2; // the X4 driver's rintsts mask

fn rd(p: &mut Peripherals, off: u32) -> u32 { p.read32(SDMMC + off) }
fn wr(p: &mut Peripherals, off: u32, v: u32) { p.write32(SDMMC + off, v) }
fn rsys(p: &mut Peripherals, off: u32) -> u32 { p.read32(SYSTEM + off) }
fn wsys(p: &mut Peripherals, off: u32, v: u32) { p.write32(SYSTEM + off, v) }
fn set_gate(p: &mut Peripherals, clk: bool, rst: bool) {
    // Perip clock/reset control, SYSTEM bit 7 (IDF v5.5.1 system_reg.h).
    let c = rsys(p, 0x1c) & !(1 << 7) | u32::from(clk) << 7;
    wsys(p, 0x1c, c);
    let r = rsys(p, 0x24) & !(1 << 7) | u32::from(rst) << 7;
    wsys(p, 0x24, r);
}

fn fixture() -> Vec<u8> { (0..8193usize * 512).map(|i| (i % 251) as u8).collect() }

/// The X4 driver's card-initialization sequence through the absolute register addresses.
/// Returns (parsed CSD capacity in blocks, high-capacity OCR bit, RCA).
fn x4_initialize(p: &mut Peripherals) -> (u32, bool, u32) {
    wr(p, 0, 7);                                                   // CTRL: controller+fifo+dma reset
    assert_eq!(rd(p, 0) & 7, 0, "CTRL reset bits self-clear");
    wr(p, 0, 0);
    wr(p, 0x24, 0); wr(p, 0x44, u32::MAX); wr(p, 0x18, 0); wr(p, 0x14, u32::MAX);
    wr(p, 0x4c, 0x007f_0080);
    let clock = |fast: bool, p: &mut Peripherals| {
        wr(p, 0x10, 0);
        wr(p, 0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
        assert_eq!(rd(p, 0x44) & RINTSTS_ERRORS, 0);
        wr(p, 0x800, 1 | (3 << 9) | (7 << 13) | (7 << 17) | (1 << 23));
        wr(p, 0x08, if fast { 0 } else { 25 << 8 });
        wr(p, 0x0c, 1 << 2);
        wr(p, 0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
        wr(p, 0x10, 1 << 1);
        wr(p, 0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
    };
    clock(false, p);
    let command = |index: u8, arg: u32, flags: u32, p: &mut Peripherals| -> [u32; 4] {
        assert_eq!(rd(p, 0x2c) & (1 << 31), 0);
        wr(p, 0x44, u32::MAX);
        wr(p, 0x28, arg);
        wr(p, 0x2c, u32::from(index) | flags | (1 << 16) | (1 << 29) | (1 << 31));
        let st = rd(p, 0x44);
        assert_ne!(st & ((1 << 2) | RINTSTS_ERRORS), 0, "command {index} completed");
        assert_eq!(st & (1 << 8), 0, "command {index} did not time out");
        assert_eq!(st & RINTSTS_ERRORS, 0, "command {index} raised no rintsts error");
        [rd(p, 0x30), rd(p, 0x34), rd(p, 0x38), rd(p, 0x3c)]
    };
    command(0, 0, 1 << 15, p);                                     // CMD0 with init sequence
    let r = command(8, 0x1aa, (1 << 6) | (1 << 8), p);
    assert_eq!(r[0] & 0xfff, 0x1aa, "CMD8 echo");
    let mut ocr = 0;
    for _ in 0..200 {                                              // CMD55 + ACMD41 poll loop
        let r = command(55, 0, (1 << 6) | (1 << 8), p);
        assert_eq!(r[0] & R1_ERRORS, 0);
        let r = command(41, 0x00ff_8000 | (1 << 30), 1 << 6, p);
        if r[0] & (1 << 31) != 0 { ocr = r[0]; break; }
    }
    assert_ne!(ocr & (1 << 31), 0, "card ready");
    command(2, 0, (1 << 6) | (1 << 7) | (1 << 8), p);
    let r3 = command(3, 0, (1 << 6) | (1 << 8), p);
    let rca = r3[0] & 0xffff_0000;
    assert_ne!(rca, 0, "RCA assigned");
    let csd = command(9, rca, (1 << 6) | (1 << 7) | (1 << 8), p);
    assert_eq!(command(7, rca, (1 << 6) | (1 << 8), p)[0] & R1_ERRORS, 0);
    let high_capacity = ocr & (1 << 30) != 0;
    if !high_capacity {
        assert_eq!(command(16, 512, (1 << 6) | (1 << 8), p)[0] & R1_ERRORS, 0);
    }
    clock(true, p);
    // firmware `platform/sdmmc.rs::capacity` parse of the latched CSD
    let blocks = match csd[3] >> 30 {
        0 => {
            let size = ((csd[2] & 0x3ff) << 2) | (csd[1] >> 30);
            let block_len = (csd[2] >> 16) & 15;
            let mult = (csd[1] >> 15) & 7;
            let bytes = ((u64::from(size) + 1) << (mult + 2)) << block_len;
            bytes / 512
        }
        1 => {
            let size = ((csd[2] & 0x3f) << 16) | (csd[1] >> 16);
            ((size + 1) * 1024) as u64
        }
        s => panic!("CSD structure must be 0 or 1, got {s}"),
    } as u32;
    (blocks, high_capacity, rca)
}

/// One CMD17 read through the CPU FIFO port, exactly as `Native::block()` drives it.
fn x4_block_read(p: &mut Peripherals, arg: u32) -> [u8; 512] {
    assert_eq!(rd(p, 0x48) & (1 << 9), 0, "card data idle");
    wr(p, 0, 2);
    assert_eq!(rd(p, 0) & 2, 0);
    wr(p, 0x1c, 512); wr(p, 0x20, 512); wr(p, 0x44, u32::MAX); wr(p, 0x28, arg);
    wr(p, 0x2c, 17 | (1 << 6) | (1 << 8) | (1 << 9) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
    let mut block = [0u8; 512];
    let mut words = 0usize;
    for _ in 0..100_000 {
        let st = rd(p, 0x44);
        assert_eq!(st & RINTSTS_ERRORS, 0, "no rintsts error");
        if words < 128 && rd(p, 0x48) & (1 << 2) == 0 {
            block[words * 4..words * 4 + 4].copy_from_slice(&rd(p, 0x200).to_le_bytes());
            words += 1;
        }
        if rd(p, 0x44) & (1 << 3) != 0 && words == 128 { break; }
    }
    assert_eq!(words, 128, "DTO after the whole block");
    assert_eq!(rd(p, 0x30) & R1_ERRORS, 0, "R1 error-free");
    block
}

/// One CMD24 write through the CPU FIFO port, then a read-back check.
#[test]
fn x4_block_write_persists_and_reads_back() {
    let mut p = Peripherals::new([1, 2, 3, 4, 5, 6]);
    p.sdmmc.attach_card(fixture());
    let (_, hc, rca) = x4_initialize(&mut p);
    assert!(!hc, "the 8193-block fixture is CSD v1, byte-addressed");
    assert_ne!(rca & 0xffff_0000, 0);
    let write_block = |p: &mut Peripherals, arg: u32, data: &[u8; 512]| {
        assert_eq!(rd(p, 0x48) & (1 << 9), 0);
        wr(p, 0, 2); assert_eq!(rd(p, 0) & 2, 0);
        wr(p, 0x1c, 512); wr(p, 0x20, 512); wr(p, 0x44, u32::MAX); wr(p, 0x28, arg);
        wr(p, 0x2c, 24 | (1 << 6) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
        let mut words = 0usize;
        for _ in 0..100_000 {
            assert_eq!(rd(p, 0x44) & RINTSTS_ERRORS, 0);
            if words < 128 && rd(p, 0x48) & (1 << 3) == 0 {
                wr(p, 0x200, u32::from_le_bytes(data[words * 4..words * 4 + 4].try_into().unwrap()));
                words += 1;
            }
            if rd(p, 0x44) & (1 << 3) != 0 && words == 128 { break; }
        }
        assert_eq!(words, 128);
        assert_eq!(rd(p, 0x30) & R1_ERRORS, 0);
    };
    let mut block = [0u8; 512];
    for (i, b) in block.iter_mut().enumerate() { *b = ((i * 7) & 0xff) as u8; }
    write_block(&mut p, 42 * 512, &block);                         // SDSC byte address
    assert_eq!(rd(&mut p, 0x5c), 512, "TCBCNT counting the write");
    assert_eq!(p.sdmmc.dirty_bytes(), 512, "the card recorded the write");
    let written = p.sdmmc.image().unwrap()[42 * 512..42 * 512 + 4].to_vec();
    assert_eq!(written, [0x00u8, 0x07, 0x0e, 0x15]);
    let read_back = x4_block_read(&mut p, 42 * 512);
    assert_eq!(read_back, block, "read-after-write through the same card");
}

#[test]
fn sdmmc_dispatches_at_0x60028000_and_parses_the_fixture_csd() {
    let mut p = Peripherals::new([1, 2, 3, 4, 5, 6]);
    p.sdmmc.attach_card(fixture());
    assert_eq!(rd(&mut p, 0x70), 0x0344_4ce1, "HCON identity");
    assert_eq!(rd(&mut p, 0x50), 1, "slot 1 present, slot 0 empty");
    let (blocks, _, _) = x4_initialize(&mut p);
    assert_eq!(blocks, 8196, "CSD covers the 8193-block image, rounded to a representable capacity");
    let block = x4_block_read(&mut p, 0);
    assert_eq!(&block[..8], &[0, 1, 2, 3, 4, 5, 6, 7], "fixture bytes come through the FIFO port");
}

#[test]
fn sdmmc_interrupt_source_is_30_and_rcc_pulses_reset_the_block() {
    let mut p = Peripherals::new([1, 2, 3, 4, 5, 6]);
    p.sdmmc.attach_card(fixture());
    // The X4 driver's RCC dance: PERIP_CLK_EN1 bit 7 on, PERIP_RST_EN1 bit 7 pulse, release.
    set_gate(&mut p, true, false);
    let rst_on = rsys(&mut p, 0x24) | (1 << 7);
    wsys(&mut p, 0x24, rst_on);                                     // RST pulse: assert…
    set_gate(&mut p, true, false);                                  // …and release
    // Block configurable again after the pulse; an update command raises the source once the
    // driver's default intmask and CTRL.int_enable are in place.
    wr(&mut p, 0x24, u32::MAX);
    let ctrl = rd(&mut p, 0) | (1 << 4);                           // CTRL.int_enable
    wr(&mut p, 0, ctrl);
    wr(&mut p, 0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
    let st = p.source_status();
    assert_ne!(st[SRC_SDMMC / 32] & (1 << (SRC_SDMMC % 32)), 0, "source 30 asserted");
    assert_ne!(rd(&mut p, 0x40) & (1 << 2), 0, "mintsts mirrors the masked status");
    // Clock gate off: the block reads zero and drops writes…
    set_gate(&mut p, false, false);
    assert_eq!(rd(&mut p, 0x70), 0);
    // …and a fresh reset pulse returns the block to power-on defaults.
    set_gate(&mut p, true, true);
    set_gate(&mut p, true, false);
    assert_eq!(rd(&mut p, 0x70), 0x0344_4ce1);
    assert_eq!(rd(&mut p, 0), 0, "CTRL reset to defaults");
}

#[test]
fn sdmmc_without_a_card_fails_initialization_like_the_driver_expects() {
    let mut p = Peripherals::new([1, 2, 3, 4, 5, 6]);
    // No card attached: the driver's clock-up + CMD8 attempt ends in RTO, no fabricated state.
    wr(&mut p, 0, 7); assert_eq!(rd(&mut p, 0) & 7, 0); wr(&mut p, 0, 0);
    wr(&mut p, 0x10, 0);
    wr(&mut p, 0x2c, (1 << 21) | (1 << 13) | (1 << 16) | (1 << 29) | (1 << 31));
    wr(&mut p, 0x44, u32::MAX); wr(&mut p, 0x28, 0x1aa);
    wr(&mut p, 0x2c, 8 | (1 << 6) | (1 << 8) | (1 << 16) | (1 << 29) | (1 << 31));
    let st = rd(&mut p, 0x44);
    assert_ne!(st & (1 << 8), 0, "RTO with no card attached");
    assert_eq!(st & (1 << 2), 0, "no fabricated command completion");
    assert_eq!(rd(&mut p, 0x30), 0, "no response latched");
    assert_eq!(rd(&mut p, 0x50), 3, "both slots read absent");
}
