//! A stand-in for the runtime: every `ag_*` extern translated code calls, so the program
//! links. None of them may be reached yet; the runtime with the bus, dispatcher and
//! scheduler replaces this module.

use anygenesis_abi::{AgFn, AgState, signatures};
use anygenesis_rom::Rom;

/// The status register after reset: supervisor mode, every interrupt masked.
const RESET_SR: u16 = 0x2700;

// Each export has the signature the abi crate declares for it.
const _: signatures::ag_read8 = ag_read8;
const _: signatures::ag_read16 = ag_read16;
const _: signatures::ag_read32 = ag_read32;
const _: signatures::ag_write8 = ag_write8;
const _: signatures::ag_write16 = ag_write16;
const _: signatures::ag_write32 = ag_write32;
const _: signatures::ag_lookup = ag_lookup;
const _: signatures::ag_stop = ag_stop;
const _: signatures::ag_check = ag_check;
const _: signatures::ag_reset_devices = ag_reset_devices;

/// Returns the state reset leaves: the ROM attached, SSP and PC from the vector table,
/// supervisor mode with interrupts masked, registers and RAM zero.
pub fn reset_state(rom: &Rom) -> Box<AgState> {
    // SAFETY: every field of AgState is an integer, an integer array or a raw pointer, for
    // which all-zero bytes are a valid value.
    let mut state = unsafe { Box::<AgState>::new_zeroed().assume_init() };
    let bytes = rom.bytes();
    state.rom = bytes.as_ptr();
    // The loader refuses images over 4 MiB, so the size fits.
    state.rom_size = bytes.len() as u32;
    state.a[7] = rom.vectors().initial_ssp();
    state.pc = rom.vectors().reset_pc() & 0xFF_FFFF;
    state.sr = RESET_SR;
    state
}

/// Ends the process: translated code reached the runtime, which does not exist yet.
fn unreachable_extern(name: &str, address: u32) -> ! {
    eprintln!("stub runtime: {name} called at ${address:06X}; the runtime is not built yet");
    std::process::abort()
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_read8(_state: *mut AgState, address: u32) -> u32 {
    unreachable_extern("ag_read8", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_read16(_state: *mut AgState, address: u32) -> u32 {
    unreachable_extern("ag_read16", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_read32(_state: *mut AgState, address: u32) -> u32 {
    unreachable_extern("ag_read32", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_write8(_state: *mut AgState, address: u32, _value: u32) {
    unreachable_extern("ag_write8", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_write16(_state: *mut AgState, address: u32, _value: u32) {
    unreachable_extern("ag_write16", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_write32(_state: *mut AgState, address: u32, _value: u32) {
    unreachable_extern("ag_write32", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_lookup(_state: *mut AgState, address: u32) -> AgFn {
    unreachable_extern("ag_lookup", address)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_stop(_state: *mut AgState, resume: u32) -> u32 {
    unreachable_extern("ag_stop", resume)
}

#[unsafe(no_mangle)]
pub extern "C" fn ag_check(_state: *mut AgState, pc: u32) -> u32 {
    unreachable_extern("ag_check", pc)
}

/// # Safety
/// Called by translated code only, with the state it runs on.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_reset_devices(state: *mut AgState) {
    // SAFETY: the caller passes the state translated code runs on, a live AgState.
    let pc = unsafe { (*state).pc };
    unreachable_extern("ag_reset_devices", pc)
}
