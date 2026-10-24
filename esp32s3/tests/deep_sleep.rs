//! S3 register-level sleep contract, ESP-IDF v6.1 rtc_cntl_reg.h / rtc_sleep.c.
//! Not an energy, RF, wake-stub or cycle-accurate silicon qualification.
use emu_core::{Bus, Core};
use esp_soc::{SocBus, Stop};
const RTC: u32 = 0x6000_8000;

fn armed() -> esp32s3::Machine {
    let mut m = esp32s3::machine([1, 2, 3, 4, 5, 6]);
    m.bus.gpio_set_input(3, true);
    m.bus.write32(RTC + 0x90, 1 << 31).unwrap(); // Digital supply off.
    m.bus.write32(RTC + 0xe0, 1 << 3).unwrap(); // EXT1 selects RTC GPIO3.
    m.bus.write32(RTC + 0x3c, (1 << 1) << 15).unwrap();
    m.bus.write32(RTC + 0x18, 1 << 31).unwrap();
    m
}
#[test]
fn sleeping_cores_and_digital_timer_do_not_run_and_ext1_reboots() {
    let mut m = armed();
    let pc = m.cores[0].pc();
    let before = m.insns();
    let rtc_before = m.bus.periph.rtc.slow_ticks;
    m.bus.periph.systimer.conf = 1 << 30;
    m.bus.periph.systimer.unit[0] = 1234;
    m.max_cycles = m.bus.cycles + 240_000;
    assert!(matches!(m.run(u64::MAX), Stop::Halted));
    assert!(m.bus.periph.rtc.sleeping);
    assert_eq!(m.insns(), before);
    assert_eq!(m.cores[0].pc(), pc);
    assert!(m.bus.periph.rtc.slow_ticks > rtc_before);
    assert_eq!(m.bus.periph.systimer.unit[0], 1234);
    // An unselected input and an inactive selected level cannot wake the chip.
    m.bus.gpio_set_input(7, false);
    m.bus.periph.rtc.check_wakeup(m.bus.periph.gpio.input);
    assert!(!m.bus.periph.rtc.sw_reset);
    m.bus.gpio_set_input(3, false);
    m.max_cycles += 240_000;
    assert!(matches!(m.run(u64::MAX), Stop::SwReset));
    assert_eq!(m.bus.periph.rtc.reset_cause, 5);
    assert_eq!(m.bus.periph.rtc.ram.read(0x130), 1 << 1);
    assert_eq!(m.bus.periph.rtc.ram.read(0xe4), 1 << 3);
    // Deep sleep loses volatile memory while retaining RTC custody and flash.
    m.bus.sram[100] = 99; m.bus.psram = vec![42; 4096];
    m.bus.rebuild_page_table();
    m.bus.rtc_slow[100] = 77;
    m.bus.flash[100] = 55;
    m.bus.periph.sdmmc.attach_card(vec![19; 512 * 1024]);
    assert_eq!(m.reboot(), 5);
    assert!(!m.bus.periph.rtc.sleeping);
    assert_eq!(m.bus.sram[100], 0);
    assert_eq!(m.bus.psram[100], 0);
    assert_eq!(m.bus.rtc_slow[100], 77);
    assert_eq!(m.bus.flash[100], 55);
    assert_eq!(m.bus.sd_image_bytes().unwrap()[100], 19);
    assert_eq!(m.bus.periph.rtc.ram.read(0x130), 1 << 1);
}
#[test]
fn rtc_timer_wake_preserves_its_actual_cause() {
    let mut m = armed();
    m.bus.periph.rtc.write(0x3c, (1 << 3) << 15);
    let deadline = m.bus.periph.rtc.slow_ticks + 20;
    m.bus.periph.rtc.write(4, deadline as u32);
    m.bus.periph.rtc.write(8, (deadline >> 32) as u32);
    m.max_cycles = m.bus.cycles + 240_000;
    assert!(matches!(m.run(u64::MAX), Stop::SwReset));
    assert_eq!(m.bus.periph.rtc.ram.read(0x130), 1 << 3);
}
#[test]
fn configured_watchdog_pauses_only_when_its_sleep_bit_requests_it() {
    let mut m = armed();
    let rtc = &mut m.bus.periph.rtc;
    rtc.write(0xb0, 0x50d8_3aa1);
    rtc.write(0x9c, 10);
    rtc.write(0x98, (1 << 31) | (3 << 28) | (1 << 9));
    rtc.wdt_tick(100);
    assert!(!rtc.sw_reset);
    rtc.write(0x98, (1 << 31) | (3 << 28));
    rtc.wdt_tick(10);
    assert!(rtc.sw_reset);
    assert_eq!(rtc.reset_cause, esp_periph::RST_RTCWDT_SYS);
}
