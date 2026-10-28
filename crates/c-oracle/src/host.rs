//! The `ag_*` functions translated code calls, implemented in Rust for the tests.
//!
//! `AgState::runtime` points at the [`Host`] of the call in progress (set by [`call`]), so
//! one set of exported symbols serves every kind of run.

use std::{ffi::c_void, ptr};

use anygenesis_abi::{AgFn, AgState, signatures};
use anygenesis_shared::{Address, Bus};

use crate::bus::{Device, SystemBus};

/// A return value no return-address check can match: translated code returns it level by
/// level up to the caller of [`call`], which asks the host why.
pub const POISON: u32 = 0x8000_0000;

/// What a run's translated code talks to.
pub trait Host {
    /// The ROM and the device behind the inline fast path.
    fn memory(&mut self) -> (&[u8], &mut dyn Device);
    /// `ag_lookup`: the dispatcher was reached for `address`.
    fn lookup(&mut self, address: Address) -> AgFn;
    /// `ag_stop`: returns where execution continues.
    fn stop(&mut self, resume: Address) -> u32;
    /// `ag_check`: returns where execution continues.
    fn check(&mut self, state: &mut AgState, pc: Address) -> u32;
    /// `ag_step_check`, at a stepped instruction boundary: as [`Host::check`].
    fn step_check(&mut self, state: &mut AgState, pc: Address) -> u32 {
        self.check(state, pc)
    }
    /// `ag_step_lookup`: no host here has a stepped table, so as [`Host::lookup`].
    fn step_lookup(&mut self, address: Address) -> AgFn {
        self.lookup(address)
    }
    /// `ag_deliver`: no host here raises interrupts, so execution goes on at `pc`.
    fn deliver(&mut self, _state: &mut AgState, pc: Address) -> u32 {
        pc.get()
    }
    fn reset_devices(&mut self);
    /// Called before each bus extern with the state's cycle count at that moment.
    fn clock(&mut self, _cycles: i32) {}
}

/// The function `ag_lookup` hands out when a run must end: it returns [`POISON`].
pub extern "C" fn poisoned(_state: *mut AgState, _pc: u32) -> u32 {
    POISON
}

/// Returns a zeroed state on the heap (it holds 64 KiB of RAM).
pub fn new_state() -> Box<AgState> {
    // SAFETY: every field of AgState is an integer, an integer array or a raw pointer, for
    // which all-zero bytes are a valid value.
    unsafe { Box::<AgState>::new_zeroed().assume_init() }
}

/// Calls the translated `function` at `pc` with `host` attached, and returns its result.
pub fn call(state: &mut AgState, host: &mut dyn Host, function: AgFn, pc: Address) -> u32 {
    let mut host = host;
    state.runtime = (&raw mut host).cast::<c_void>();
    // SAFETY: `function` is a translated function, which only touches `state` and calls the
    // externs below. They reach `host` through `state.runtime`, which stays valid for the
    // call: `host` is a local of this frame and is not used directly until it returns.
    let next = unsafe { function(state, pc.get()) };
    state.runtime = ptr::null_mut();
    next
}

/// Gives `body` the state and the attached host of an extern call.
///
/// # Safety
/// `state` must be the pointer translated code was called with by [`call`].
unsafe fn with_host<T>(
    state: *mut AgState,
    body: impl FnOnce(&mut AgState, &mut dyn Host) -> T,
) -> T {
    // SAFETY: the caller passes the state of a running `call`, whose `runtime` points at the
    // `&mut dyn Host` that `call` attached; generated code holds no reference to either
    // while it waits for an extern to return.
    let state = unsafe { &mut *state };
    let host = unsafe { &mut **state.runtime.cast::<&mut dyn Host>() };
    body(state, host)
}

/// Gives `body` the bus of an extern call.
///
/// # Safety
/// As for [`with_host`].
unsafe fn with_bus<T>(state: *mut AgState, body: impl FnOnce(&mut SystemBus<'_>) -> T) -> T {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_host(state, |state, host| {
            host.clock(state.cycles);
            let (rom, device) = host.memory();
            let mut bus = SystemBus {
                ram: &mut state.ram,
                rom,
                device,
                watch: None,
            };
            body(&mut bus)
        })
    }
}

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
const _: signatures::ag_step_enter = ag_step_enter;
const _: signatures::ag_step_check = ag_step_check;
const _: signatures::ag_step_begin = ag_step_begin;
const _: signatures::ag_step_end = ag_step_end;
const _: signatures::ag_step_late = ag_step_late;
const _: signatures::ag_step_lookup = ag_step_lookup;
const _: signatures::ag_deliver = ag_deliver;
const _: signatures::ag_reset_devices = ag_reset_devices;

/// # Safety
/// Called by translated code only, with the state of a running [`call`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_read8(state: *mut AgState, address: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_bus(state, |bus| bus.read_byte(Address::new(address)).into()) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_read16(state: *mut AgState, address: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_bus(state, |bus| bus.read_word(Address::new(address)).into()) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_read32(state: *mut AgState, address: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_bus(state, |bus| bus.read_long(Address::new(address))) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_write8(state: *mut AgState, address: u32, value: u32) {
    // SAFETY: forwarded from the caller. The C passes the byte in the low bits.
    unsafe {
        with_bus(state, |bus| {
            bus.write_byte(Address::new(address), value as u8)
        })
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_write16(state: *mut AgState, address: u32, value: u32) {
    // SAFETY: forwarded from the caller. The C passes the word in the low bits.
    unsafe {
        with_bus(state, |bus| {
            bus.write_word(Address::new(address), value as u16)
        })
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_write32(state: *mut AgState, address: u32, value: u32) {
    // SAFETY: forwarded from the caller.
    unsafe { with_bus(state, |bus| bus.write_long(Address::new(address), value)) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_lookup(state: *mut AgState, address: u32) -> AgFn {
    // SAFETY: forwarded from the caller.
    unsafe { with_host(state, |_, host| host.lookup(Address::new(address))) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_stop(state: *mut AgState, resume: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_host(state, |_, host| host.stop(Address::new(resume))) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_host(state, |state, host| host.check(state, Address::new(pc))) }
}

/// The slow path of a check point, only reached while the state's count is above 0, which
/// the hosts here never leave it at.
///
/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_enter(_state: *mut AgState, _ahead: u32, _latch: u32) {
    panic!("ag_step_enter with a positive budget");
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_host(state, |state, host| {
            host.step_check(state, Address::new(pc))
        })
    }
}

/// A stepped instruction's start: the hosts here keep no time, so nothing to note.
///
/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_begin(_state: *mut AgState) {}

/// A stepped instruction's end: no host here stalls or raises interrupts, so execution goes
/// on at `next`.
///
/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_end(
    _state: *mut AgState,
    next: u32,
    _latch: u32,
    _kind: u32,
) -> u32 {
    next
}

/// As [`ag_step_end`].
///
/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_late(
    _state: *mut AgState,
    next: u32,
    _latch: u32,
    _charged: u32,
) -> u32 {
    next
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_lookup(state: *mut AgState, address: u32) -> AgFn {
    // SAFETY: forwarded from the caller.
    unsafe { with_host(state, |_, host| host.step_lookup(Address::new(address))) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_deliver(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_host(state, |state, host| host.deliver(state, Address::new(pc))) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_reset_devices(state: *mut AgState) {
    // SAFETY: forwarded from the caller.
    unsafe { with_host(state, |_, host| host.reset_devices()) }
}
