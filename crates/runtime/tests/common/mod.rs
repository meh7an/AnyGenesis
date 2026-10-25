//! Helpers shared by the runtime's integration tests: synthetic functions in the emitter's
//! shape, ROM images built from code, and hardware that raises interrupts on a script.

// Each test binary uses its own subset of the helpers.
#![allow(dead_code)]

use anygenesis_abi::{AgState, ram_index};
use anygenesis_rom::Rom;
use anygenesis_runtime::{Hardware, Memory, ag_check};
use anygenesis_shared::Size;

pub const SSP: u32 = 0x00FF_FE00;

/// Gives `body` the state for one short access, so no borrow lives across a call.
pub fn with<T>(state: *mut AgState, body: impl FnOnce(&mut AgState) -> T) -> T {
    // SAFETY: the runtime passes a live state, and every caller drops the borrow before it
    // calls anything that derives its own.
    unsafe { body(&mut *state) }
}

pub fn push(state: *mut AgState, value: u32) {
    with(state, |state| {
        state.a[7] = state.a[7].wrapping_sub(4);
        for (offset, byte) in value.to_be_bytes().into_iter().enumerate() {
            state.ram[ram_index(state.a[7] + offset as u32)] = byte;
        }
    });
}

pub fn pop(state: *mut AgState) -> u32 {
    with(state, |state| {
        let bytes = std::array::from_fn(|offset| state.ram[ram_index(state.a[7] + offset as u32)]);
        state.a[7] = state.a[7].wrapping_add(4);
        u32::from_be_bytes(bytes)
    })
}

/// `AG_CYCLES` then `AG_CHECK` at a check point: the hook's result when it is not `pc`.
pub fn check(state: *mut AgState, pc: u32, cost: i32) -> Option<u32> {
    let due = with(state, |state| {
        state.cycles -= cost;
        state.cycles <= 0
    });
    if !due {
        return None;
    }
    // SAFETY: the state of a running run.
    let next = unsafe { ag_check(state, pc) };
    (next != pc).then_some(next)
}

/// A ROM image: SSP and reset PC in the vector table, the given vectors, and code words at
/// their addresses.
pub fn image(reset: u32, vectors: &[(u8, u32)], code: &[(u32, &[u16])]) -> Rom {
    let mut bytes = vec![0; 0x1000];
    bytes[0..4].copy_from_slice(&SSP.to_be_bytes());
    bytes[4..8].copy_from_slice(&reset.to_be_bytes());
    for &(vector, handler) in vectors {
        let at = usize::from(vector) * 4;
        bytes[at..at + 4].copy_from_slice(&handler.to_be_bytes());
    }
    for &(address, words) in code {
        for (index, word) in words.iter().enumerate() {
            let at = address as usize + 2 * index;
            bytes[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
    }
    Rom::from_bytes(bytes).expect("the synthetic image is a well-formed .bin")
}

/// Raises interrupt levels at fixed cycle counts; each stays asserted until acknowledged.
#[derive(Debug, Default)]
pub struct Scripted {
    now: u64,
    /// (cycle, level), ascending by cycle.
    events: Vec<(u64, u8)>,
    next: usize,
    /// Asserted levels, one bit per level.
    raised: u8,
}

impl Scripted {
    pub fn new(events: &[(u64, u8)]) -> Box<Self> {
        let mut events = events.to_vec();
        events.sort_unstable();
        Box::new(Self {
            events,
            ..Self::default()
        })
    }
}

impl Hardware for Scripted {
    fn read(&mut self, _port: u8, _size: Size) -> u16 {
        0
    }

    fn write(&mut self, _port: u8, _size: Size, _value: u16, _memory: Memory<'_>) {}

    fn advance(&mut self, cycles: u32) {
        self.now += u64::from(cycles);
        while let Some(&(at, level)) = self.events.get(self.next)
            && at <= self.now
        {
            self.raised |= 1 << level;
            self.next += 1;
        }
    }

    fn until_event(&self) -> u32 {
        self.events
            .get(self.next)
            .map_or(1_000, |&(at, _)| (at - self.now) as u32)
    }

    fn frames(&self) -> u64 {
        0
    }

    fn pending_interrupt(&self) -> Option<u8> {
        (1..=7).rev().find(|level| self.raised & 1 << level != 0)
    }

    fn enabled_interrupts(&self) -> u8 {
        self.events[self.next..]
            .iter()
            .fold(self.raised, |levels, &(_, level)| levels | 1 << level)
    }

    fn acknowledge(&mut self, level: u8) {
        self.raised &= !(1 << level);
    }

    fn reset(&mut self) {}
}
