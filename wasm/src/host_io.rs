//! Explicit host effects for unchanged connected SDK firmware. No filesystem,
//! synthesized telemetry or changes to guest scheduling live in these bindings.
use super::{bytes, log, machine::MachineKind, Emu};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

pub(super) type Entropy = Rc<RefCell<VecDeque<u32>>>;
const ENTROPY_WORDS: usize = 4096;
const MAX_CARD_BYTES: usize = 64 << 20;

/// Raw analog-pad input for the chip's actual ADC conversion boundary.
/// Returns one for an unsupported pad or a value outside the 12-bit range.
/// # Safety
/// `e` is an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_adc_raw(e:*mut Emu,pin:u32,raw:u32)->u32 {
    if pin>u8::MAX.into() || raw>4095 {return 1;}
    u32::from(!unsafe{&mut *e}.m.bus().adc_set_raw(pin as u8,raw as u16))
}

/// Attach a bounded, disposable SD image before boot, using the native model.
/// Returns 1 for an unsupported chip, invalid image size or an already booted machine.
///
/// # Safety
/// `e` is an exclusively borrowed live emulator. `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_sd_attach(e: *mut Emu, ptr: *const u8, len: usize) -> u32 {
    let e = unsafe { &mut *e };
    if e.booted || !(512..=MAX_CARD_BYTES).contains(&len) || !len.is_multiple_of(512) { return 1; }
    let image = unsafe { bytes(ptr, len) }.to_vec();
    match &mut e.m {
        MachineKind::C3(m) => {
            m.bus.board = Box::new(esp_soc::devices::x3_sd::X3Sd::new(image));
            m.bus.attach_board_devices();
        }
        MachineKind::S3(m) => m.bus.periph.sdmmc.attach_card(image),
        _ => return 1,
    }
    0
}

/// Length of the attached mutable card, or zero if none is attached.
///
/// # Safety
/// `e` is an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_sd_len(e: *mut Emu) -> usize {
    unsafe { &mut *e }.m.bus().sd_image_bytes().map_or(0, <[u8]>::len)
}

/// Copy the complete card into a separate caller allocation. Refuse truncation.
///
/// # Safety
/// `e` is exclusively borrowed; `ptr` is writable for `len` bytes and does not
/// overlap the emulator or its owned allocations.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_sd_copy(e: *mut Emu, ptr: *mut u8, len: usize) -> u32 {
    let Some(image) = unsafe { &mut *e }.m.bus().sd_image_bytes() else { return 1; };
    if len != image.len() || ptr.is_null() { return 1; }
    unsafe { std::ptr::copy_nonoverlapping(image.as_ptr(), ptr, len); }
    0
}

/// Explicitly select host-fed entropy at the same RNG register as the native
/// frontend. Exhaustion stops execution; it never falls back to deterministic RNG.
///
/// # Safety
/// `e` is an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_rng_host(e: *mut Emu) -> u32 {
    let e = unsafe { &mut *e };
    if e.booted || e.entropy.is_some() { return 1; }
    let words = Rc::new(RefCell::new(VecDeque::with_capacity(ENTROPY_WORDS)));
    let input = Rc::clone(&words);
    if let Err(error) = e.m.bus().set_rng_source(Box::new(move || {
        input.borrow_mut().pop_front().expect("host entropy exhausted")
    })) { log(&error); return 1; }
    e.entropy = Some(words);
    0
}

/// Current unread entropy words. Zero also denotes a frontend not configured.
///
/// # Safety
/// `e` is an exclusively borrowed live emulator.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_rng_available(e: *mut Emu) -> usize {
    unsafe { &mut *e }.entropy.as_ref().map_or(0, |q| q.borrow().len())
}

/// Append little-endian host entropy words to a bounded FIFO; refuse overflow,
/// partial words and unconfigured input without changing the previous contents.
///
/// # Safety
/// `e` is exclusively borrowed. `ptr` is readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn esp32sim_rng_feed(e: *mut Emu, ptr: *const u8, len: usize) -> u32 {
    let e = unsafe { &mut *e };
    let Some(words) = &e.entropy else { return 1; };
    let mut words = words.borrow_mut();
    if !len.is_multiple_of(4) || len > (ENTROPY_WORDS - words.len()) * 4 { return 1; }
    for word in unsafe { bytes(ptr, len) }.as_chunks::<4>().0 {
        words.push_back(u32::from_le_bytes(*word));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{esp32sim_delete, esp32sim_new};
    use esp_periph::device::Device;

    #[test]
    fn raw_adc_pad_input_is_bounded_and_reaches_the_c3_pad() {
        unsafe {
            let e=esp32sim_new(b"esp32c3".as_ptr(),7,16,0);
            assert_eq!(esp32sim_adc_raw(e,1,1000),0);
            assert_eq!(esp32sim_adc_raw(e,1,4096),1);
            assert_eq!(esp32sim_adc_raw(e,256,1),1);
            assert_eq!(esp32sim_adc_raw(e,6,1000),1);
            let MachineKind::C3(m)=&mut (*e).m else {panic!("wrong chip")};
            m.bus.periph.adc.write(0x20,(1<<31)|(1<<29)|(1<<25));
            assert_eq!(m.bus.periph.adc.read(0x2c),1000);
            esp32sim_delete(e);
        }
    }

    #[test]
    fn card_roundtrip_and_refusals_preserve_owned_bytes() {
        unsafe {
            let e = esp32sim_new(b"esp32c3".as_ptr(), 7, 16, 0);
            let image: Vec<u8> = (0..4096).map(|i| i as u8).collect();
            assert_eq!(esp32sim_sd_len(e), 0);
            assert_eq!(esp32sim_sd_attach(e, image.as_ptr(), image.len()), 0);
            assert_eq!(esp32sim_sd_len(e), image.len());
            assert_eq!(esp32sim_sd_attach(e, image.as_ptr(), 513), 1);
            let mut copy = vec![0; image.len()];
            assert_eq!(esp32sim_sd_copy(e, copy.as_mut_ptr(), 512), 1);
            assert_eq!(copy, vec![0; image.len()]);
            assert_eq!(esp32sim_sd_copy(e, copy.as_mut_ptr(), copy.len()), 0);
            assert_eq!(copy, image);
            (*e).booted = true;
            assert_eq!(esp32sim_sd_attach(e, image.as_ptr(), image.len()), 1);
            esp32sim_delete(e);
        }
    }

    #[test]
    fn entropy_fifo_rejects_partial_and_overflow_without_mutation() {
        unsafe {
            let e = esp32sim_new(b"esp32c3".as_ptr(), 7, 16, 0);
            let words = [1, 2, 3, 4, 5, 6, 7, 8];
            assert_eq!(esp32sim_rng_feed(e, words.as_ptr(), words.len()), 1);
            assert_eq!(esp32sim_rng_host(e), 0);
            assert_eq!(esp32sim_rng_host(e), 1);
            assert_eq!(esp32sim_rng_feed(e, words.as_ptr(), words.len()), 0);
            assert_eq!(esp32sim_rng_feed(e, words.as_ptr(), 3), 1);
            // Oversized input must be rejected before the pointer is dereferenced.
            assert_eq!(esp32sim_rng_feed(e, std::ptr::null(), ENTROPY_WORDS * 4), 1);
            assert_eq!(esp32sim_rng_available(e), 2);
            let MachineKind::C3(m) = &mut (*e).m else { panic!("wrong fixture chip") };
            assert_eq!(m.bus.periph.rng.read(0), 0x04030201);
            assert_eq!(m.bus.periph.rng.read(0), 0x08070605);
            assert_eq!(esp32sim_rng_available(e), 0);
            esp32sim_delete(e);
        }
    }
}
