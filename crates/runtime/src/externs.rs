//! The `ag_*` functions translated code calls, exported under their C names.
//!
//! `AgState::runtime` points at the [`Context`] of the run in progress, set by
//! [`Runtime::run`](crate::Runtime::run). `ag_check`, `ag_step_check`, `ag_step_end`,
//! `ag_step_late`, `ag_deliver` and `ag_stop` may run interrupt handlers, so they take the
//! raw state rather than a borrow of it.

use anygenesis_abi::{AgFn, AgState, signatures};
use anygenesis_shared::{Address, Bus};

use crate::{
    bus::SystemBus,
    machine::{self, Context},
};

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

/// Gives `body` the state and context of an extern call.
///
/// # Safety
/// `state` must be the state of a running [`Runtime::run`](crate::Runtime::run), which
/// points at its context; translated code holds no reference to either while it waits for
/// an extern to return.
unsafe fn with_context<T>(
    state: *mut AgState,
    body: impl FnOnce(&mut AgState, &mut Context) -> T,
) -> T {
    // SAFETY: forwarded from the caller.
    unsafe {
        let state = &mut *state;
        let context = &mut *state.runtime.cast::<Context>();
        body(state, context)
    }
}

/// Gives `body` the bus of an extern call, then lets the depth guard look at the stack.
///
/// # Safety
/// As for [`with_context`].
unsafe fn with_bus<T>(
    state: *mut AgState,
    address: u32,
    body: impl FnOnce(&mut SystemBus<'_>) -> T,
) -> T {
    // SAFETY: forwarded from the caller; the runtime attached the ROM the state points at.
    unsafe {
        with_context(state, |state, context| {
            // A stepped instruction's accesses all see its start, as the interpreter's do;
            // its end takes their stalls and rescheduling.
            if context.stepping() {
                return body(&mut context.bus(state));
            }
            context.before_access(state);
            let value = body(&mut context.bus(state));
            context.after_access(state, address);
            value
        })
    }
}

/// # Safety
/// Called by translated code only, with the state of a running run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_read8(state: *mut AgState, address: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_bus(state, address, |bus| {
            bus.read_byte(Address::new(address)).into()
        })
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_read16(state: *mut AgState, address: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_bus(state, address, |bus| {
            bus.read_word(Address::new(address)).into()
        })
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_read32(state: *mut AgState, address: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { with_bus(state, address, |bus| bus.read_long(Address::new(address))) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_write8(state: *mut AgState, address: u32, value: u32) {
    // SAFETY: forwarded from the caller. The C passes the byte in the low bits.
    unsafe {
        with_bus(state, address, |bus| {
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
        with_bus(state, address, |bus| {
            bus.write_word(Address::new(address), value as u16)
        })
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_write32(state: *mut AgState, address: u32, value: u32) {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_bus(state, address, |bus| {
            bus.write_long(Address::new(address), value)
        })
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_lookup(state: *mut AgState, address: u32) -> AgFn {
    // SAFETY: forwarded from the caller.
    unsafe { with_context(state, |_, context| context.lookup(address)) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_stop(state: *mut AgState, resume: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { machine::stop(state, resume) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { machine::check_native(state, pc) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_enter(state: *mut AgState, ahead: u32, latch: u32) {
    // SAFETY: forwarded from the caller.
    unsafe {
        with_context(state, |state, context| {
            context.step_enter(state, ahead, latch);
        });
    }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_check(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { machine::step_check(state, pc) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_begin(state: *mut AgState) {
    // SAFETY: forwarded from the caller.
    unsafe { with_context(state, |state, context| context.step_begin(state)) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_end(state: *mut AgState, next: u32, latch: u32, kind: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { machine::step_end(state, next, latch, kind, None) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_late(
    state: *mut AgState,
    next: u32,
    latch: u32,
    charged: u32,
) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { machine::step_end(state, next, latch, machine::STEP_GO, Some(charged)) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_step_lookup(state: *mut AgState, address: u32) -> AgFn {
    // SAFETY: forwarded from the caller.
    unsafe { with_context(state, |_, context| context.step_lookup(address)) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_deliver(state: *mut AgState, pc: u32) -> u32 {
    // SAFETY: forwarded from the caller.
    unsafe { machine::deliver(state, pc) }
}

/// # Safety
/// As for [`ag_read8`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ag_reset_devices(state: *mut AgState) {
    // SAFETY: forwarded from the caller.
    unsafe { with_context(state, |_, context| context.devices.reset()) }
}
