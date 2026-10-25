//! Safe wrapper over the vendored Musashi core, configured as a bare 68000 with address
//! error emulation, and over its disassembler in 68000 mode.

use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    ptr,
    sync::{Mutex, MutexGuard, Once, PoisonError},
};

use crate::Registers;

/// The 68000 drives 24 address lines.
const ADDRESS_MASK: u32 = 0x00FF_FFFF;

type ReadFn = extern "C" fn(*mut c_void, c_uint, c_uint) -> c_uint;
type WriteFn = extern "C" fn(*mut c_void, c_uint, c_uint, c_uint);

unsafe extern "C" {
    fn oracle_init();
    fn oracle_attach_bus(bus: *mut c_void, read: ReadFn, write: WriteFn);
    fn oracle_load(registers: *const Registers);
    fn oracle_save(registers: *mut Registers);
    fn oracle_step() -> c_int;
    fn oracle_stopped() -> c_int;
    fn oracle_is_valid_68000(word: c_uint) -> c_uint;
    fn oracle_disassemble_68000(
        pc: c_uint,
        bytes: *const u8,
        length: c_uint,
        text: *mut c_char,
    ) -> c_uint;
}

/// Musashi keeps its CPU in C globals, so at most one [`Musashi`] may exist at a time.
static CORE: Mutex<()> = Mutex::new(());
static INIT: Once = Once::new();

/// The disassembler keeps C globals of its own, disjoint from the core's, so it has its own
/// lock and never waits on a running CPU test.
static DISASSEMBLER: Mutex<()> = Mutex::new(());

/// Musashi writes at most two 100-byte strings (text and helper comment) into the buffer.
const TEXT_CAPACITY: usize = 256;

/// One instruction as Musashi's disassembler reads it on a 68000.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disassembly {
    /// Bytes consumed, opcode word included.
    pub length: u32,
    /// Musashi's text, including its `; ...` helper comments.
    pub text: String,
}

/// Returns Musashi's `m68k_is_valid_instruction` answer for a 68000.
///
/// A table lookup on the first word only. It calls ILLEGAL (0x4AFC) invalid, and it is
/// inconsistent on line F and on Bcc with displacement 0xFF (see PROGRESS.md, M1.2).
pub fn is_valid_68000(word: u16) -> bool {
    let _lock = DISASSEMBLER.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: a pure table lookup; the disassembler lock is held.
    unsafe { oracle_is_valid_68000(c_uint::from(word)) != 0 }
}

/// Disassembles the instruction at the start of `bytes`, which sit at address `pc`.
///
/// Bytes past the end of `bytes` read as zero, so a `length` beyond `bytes.len()` means
/// Musashi consumed padding.
pub fn disassemble_68000(pc: u32, bytes: &[u8]) -> Disassembly {
    // Longer slices are only cut short: Musashi never reads more than a few dozen bytes.
    let available = c_uint::try_from(bytes.len()).unwrap_or(c_uint::MAX);
    let mut text = [0 as c_char; TEXT_CAPACITY];
    let _lock = DISASSEMBLER.lock().unwrap_or_else(PoisonError::into_inner);
    // SAFETY: `bytes` is valid for `available` bytes and the shim reads only inside them;
    // `text` holds more than Musashi writes; both outlive the call, and the lock is held.
    let length =
        unsafe { oracle_disassemble_68000(pc, bytes.as_ptr(), available, text.as_mut_ptr()) };
    // SAFETY: Musashi always terminates the string it writes, well inside the buffer.
    let text = unsafe { CStr::from_ptr(text.as_ptr()) };
    Disassembly {
        length,
        text: text.to_string_lossy().into_owned(),
    }
}

/// Direction of a bus cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Write,
}

/// Width of a bus cycle. The 68000 data bus is 16 bits, so long accesses appear as two words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessSize {
    Byte,
    Word,
}

/// One bus cycle performed by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusAccess {
    pub kind: AccessKind,
    pub size: AccessSize,
    pub address: u32,
    pub value: u16,
}

/// Memory a caller lends the core for [`Musashi::step_with`]. Long accesses arrive as two
/// word accesses, high word first; `address` may carry bits above the 24 the 68000 drives.
pub trait Memory {
    fn read(&mut self, address: u32, size: AccessSize) -> u16;
    fn write(&mut self, address: u32, size: AccessSize, value: u16);
}

/// Sparse memory: SingleStepTests cases touch a few dozen bytes of a 16 MiB space, and a
/// dense array would cost a 16 MiB clear per case. Unwritten bytes read as zero.
#[derive(Debug, Default)]
struct Bus {
    memory: HashMap<u32, u8>,
    log: Vec<BusAccess>,
}

impl Bus {
    fn peek(&self, address: u32) -> u8 {
        self.memory
            .get(&(address & ADDRESS_MASK))
            .copied()
            .unwrap_or(0)
    }

    fn poke(&mut self, address: u32, value: u8) {
        self.memory.insert(address & ADDRESS_MASK, value);
    }

    fn read(&mut self, address: u32, size: AccessSize) -> u16 {
        let address = address & ADDRESS_MASK;
        let value = match size {
            AccessSize::Byte => u16::from(self.peek(address)),
            AccessSize::Word => u16::from_be_bytes([self.peek(address), self.peek(address + 1)]),
        };
        self.log.push(BusAccess {
            kind: AccessKind::Read,
            size,
            address,
            value,
        });
        value
    }

    fn write(&mut self, address: u32, size: AccessSize, value: u16) {
        let address = address & ADDRESS_MASK;
        match size {
            AccessSize::Byte => self.poke(address, value.to_be_bytes()[1]),
            AccessSize::Word => {
                let [high, low] = value.to_be_bytes();
                self.poke(address, high);
                self.poke(address + 1, low);
            }
        }
        self.log.push(BusAccess {
            kind: AccessKind::Write,
            size,
            address,
            value,
        });
    }
}

impl Memory for Bus {
    fn read(&mut self, address: u32, size: AccessSize) -> u16 {
        Bus::read(self, address, size)
    }

    fn write(&mut self, address: u32, size: AccessSize, value: u16) {
        Bus::write(self, address, size, value);
    }
}

/// Exclusive handle to the Musashi core and the memory it runs against.
#[derive(Debug)]
pub struct Musashi {
    bus: Bus,
    _core: MutexGuard<'static, ()>,
}

impl Musashi {
    /// Takes exclusive use of the core, blocking while another handle exists.
    ///
    /// Memory starts empty; CPU state is undefined until [`Musashi::set_registers`].
    pub fn lock() -> Self {
        // A panic while the lock was held cannot leave stale state behind: every case
        // starts with `set_registers`, which resets the core.
        let core = CORE.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: builds Musashi's opcode table once; the core lock is held.
        INIT.call_once(|| unsafe { oracle_init() });
        Self {
            bus: Bus::default(),
            _core: core,
        }
    }

    /// Returns the byte at `address`, without a bus cycle.
    pub fn read_byte(&self, address: u32) -> u8 {
        self.bus.peek(address)
    }

    /// Stores `value` at `address`, without a bus cycle.
    pub fn write_byte(&mut self, address: u32, value: u8) {
        self.bus.poke(address, value);
    }

    /// Forgets every byte written so far.
    pub fn clear_memory(&mut self) {
        self.bus.memory.clear();
    }

    /// Resets the core, then loads `registers`. Clears the bus log.
    pub fn set_registers(&mut self, registers: &Registers) {
        // SAFETY: `registers` is a valid `repr(C)` struct for the duration of the call.
        self.with_bus(|| unsafe { oracle_load(registers) });
        self.bus.log.clear();
    }

    /// Reads the current registers back from the core.
    pub fn registers(&self) -> Registers {
        let mut registers = Registers::default();
        // SAFETY: `registers` is a valid, writable `repr(C)` struct; no bus access happens.
        unsafe { oracle_save(&mut registers) };
        registers
    }

    /// Executes one instruction, including any exception it raises, and returns the cycles
    /// Musashi charged for it. Replaces the bus log with this instruction's accesses.
    pub fn step(&mut self) -> i32 {
        self.bus.log.clear();
        // SAFETY: plain call into the core; the bus is attached for its duration.
        self.with_bus(|| unsafe { oracle_step() })
    }

    /// Whether the core is halted by STOP, or by a double fault, waiting for an interrupt.
    pub fn is_stopped(&self) -> bool {
        // SAFETY: reads a flag of the core; no bus access happens.
        unsafe { oracle_stopped() != 0 }
    }

    /// Executes one instruction against `memory` instead of the handle's own memory, and
    /// returns the cycles Musashi charged. The bus log is not kept.
    pub fn step_with(&mut self, memory: &mut dyn Memory) -> i32 {
        // SAFETY: plain call into the core; `memory` is attached for its duration and the
        // core lock is held by `self`.
        with_memory(memory, || unsafe { oracle_step() })
    }

    /// The bus cycles of the last [`Musashi::step`], in order.
    pub fn bus_log(&self) -> &[BusAccess] {
        &self.bus.log
    }

    fn with_bus<T>(&mut self, call: impl FnOnce() -> T) -> T {
        with_memory(&mut self.bus, call)
    }
}

/// Attaches `memory` while `call` runs.
fn with_memory<T>(memory: &mut dyn Memory, call: impl FnOnce() -> T) -> T {
    let mut memory = memory;
    let attached: *mut &mut dyn Memory = &mut memory;
    // SAFETY: Musashi only calls back while `call` runs. During it `memory` is exclusively
    // borrowed and the caller holds the core lock, so the pointer is the only access to it;
    // it is detached again before the borrow ends.
    unsafe { oracle_attach_bus(attached.cast(), bus_read, bus_write) };
    let result = call();
    unsafe { oracle_attach_bus(ptr::null_mut(), bus_read, bus_write) };
    result
}

/// Recovers the memory attached by [`with_memory`].
fn attached<'a>(bus: *mut c_void) -> &'a mut dyn Memory {
    // SAFETY: a non-null pointer was attached by `with_memory` and is valid until it
    // detaches. Musashi never calls back outside a call wrapped in `with_memory`, so null is
    // a bug in this module, and the panic aborts the process at the FFI boundary.
    let memory = unsafe { bus.cast::<&mut dyn Memory>().as_mut() };
    &mut **memory.expect("Musashi called back with no bus attached")
}

extern "C" fn bus_read(bus: *mut c_void, address: c_uint, size: c_uint) -> c_uint {
    let bus = attached(bus);
    match size {
        1 => c_uint::from(bus.read(address, AccessSize::Byte)),
        2 => c_uint::from(bus.read(address, AccessSize::Word)),
        _ => {
            let high = c_uint::from(bus.read(address, AccessSize::Word));
            let low = c_uint::from(bus.read(address.wrapping_add(2), AccessSize::Word));
            (high << 16) | low
        }
    }
}

extern "C" fn bus_write(bus: *mut c_void, address: c_uint, size: c_uint, value: c_uint) {
    let bus = attached(bus);
    // Truncation is intended: Musashi passes byte and word values in a wider integer.
    match size {
        1 => bus.write(address, AccessSize::Byte, (value & 0xFF) as u16),
        2 => bus.write(address, AccessSize::Word, value as u16),
        _ => {
            bus.write(address, AccessSize::Word, (value >> 16) as u16);
            bus.write(address.wrapping_add(2), AccessSize::Word, value as u16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPERVISOR: u32 = 0x2700;

    fn load_program(core: &mut Musashi, address: u32, words: &[u16]) {
        for (offset, word) in (0..).step_by(2).zip(words) {
            let [high, low] = word.to_be_bytes();
            core.write_byte(address + offset, high);
            core.write_byte(address + offset + 1, low);
        }
    }

    fn supervisor_at(pc: u32) -> Registers {
        Registers {
            ssp: 0x8000,
            sr: SUPERVISOR,
            pc,
            ..Registers::default()
        }
    }

    #[test]
    fn test_step_moveq_sets_register_and_logs_fetch() {
        let mut core = Musashi::lock();
        load_program(&mut core, 0x1000, &[0x7005]);
        core.set_registers(&supervisor_at(0x1000));

        core.step();

        let registers = core.registers();
        assert_eq!(registers.d[0], 5);
        assert_eq!(registers.pc, 0x1002);
        assert_eq!(
            core.bus_log(),
            [BusAccess {
                kind: AccessKind::Read,
                size: AccessSize::Word,
                address: 0x1000,
                value: 0x7005,
            }]
        );
    }

    #[test]
    fn test_step_move_long_to_memory_logs_two_word_writes() {
        let mut core = Musashi::lock();
        load_program(&mut core, 0x1000, &[0x2080]);
        let mut registers = supervisor_at(0x1000);
        registers.d[0] = 0x1122_3344;
        registers.a[0] = 0x3000;
        core.set_registers(&registers);

        core.step();

        let writes: Vec<_> = core
            .bus_log()
            .iter()
            .filter(|access| access.kind == AccessKind::Write)
            .map(|access| (access.address, access.value))
            .collect();
        assert_eq!(writes, [(0x3000, 0x1122), (0x3002, 0x3344)]);
        assert_eq!(core.read_byte(0x3000), 0x11);
        assert_eq!(core.read_byte(0x3003), 0x44);
    }

    #[test]
    fn test_step_with_runs_against_the_lent_memory() {
        let mut core = Musashi::lock();
        let mut memory = Bus::default();
        // move.w d0,(a0)
        memory.poke(0x1000, 0x30);
        memory.poke(0x1001, 0x80);
        let mut registers = supervisor_at(0x1000);
        registers.d[0] = 0xBEEF;
        registers.a[0] = 0x3000;
        core.set_registers(&registers);

        core.step_with(&mut memory);

        assert_eq!(core.registers().pc, 0x1002);
        assert_eq!((memory.peek(0x3000), memory.peek(0x3001)), (0xBE, 0xEF));
        assert_eq!(
            core.read_byte(0x3000),
            0,
            "the handle's own memory is untouched"
        );
    }

    #[test]
    fn test_set_registers_user_mode_round_trips_both_stack_pointers() {
        let mut core = Musashi::lock();
        let registers = Registers {
            d: [1, 2, 3, 4, 5, 6, 7, 8],
            a: [9, 10, 11, 12, 13, 14, 15],
            usp: 0x4000,
            ssp: 0x8000,
            sr: 0x0015,
            pc: 0x1000,
        };

        core.set_registers(&registers);

        assert_eq!(core.registers(), registers);
    }

    #[test]
    fn test_step_stop_in_supervisor_mode_stops_core() {
        let mut core = Musashi::lock();
        load_program(&mut core, 0x1000, &[0x4E72, 0x2700]);
        core.set_registers(&supervisor_at(0x1000));

        core.step();

        assert!(core.is_stopped());
        assert_eq!(core.registers().pc, 0x1004);
    }

    #[test]
    fn test_step_odd_word_read_raises_address_error() {
        let mut core = Musashi::lock();
        // move.w (a0),d0 with a0 odd.
        load_program(&mut core, 0x1000, &[0x3010]);
        // Address error vector (3) points at 0x2000.
        load_program(&mut core, 0x000C, &[0x0000, 0x2000]);
        let mut registers = supervisor_at(0x1000);
        registers.a[0] = 0x3001;
        core.set_registers(&registers);

        core.step();

        assert_eq!(core.registers().pc, 0x2000);
    }

    #[test]
    fn test_is_valid_68000_moveq_and_illegal_differ() {
        assert!(is_valid_68000(0x7005));
        assert!(!is_valid_68000(0x4AFC));
        // movec is a 68010 instruction.
        assert!(!is_valid_68000(0x4E7A));
    }

    #[test]
    fn test_disassemble_68000_move_long_immediate_counts_extension_words() {
        let disassembly = disassemble_68000(0x1000, &[0x20, 0x3C, 0x12, 0x34, 0x56, 0x78]);

        assert_eq!(disassembly.length, 6);
        assert!(
            disassembly.text.starts_with("move.l"),
            "{}",
            disassembly.text
        );
        assert!(
            disassembly.text.contains("#$12345678"),
            "{}",
            disassembly.text
        );
    }

    #[test]
    fn test_disassemble_68000_short_buffer_reads_zero_padding() {
        // jmp $xxxxxxxx.l with its address words missing.
        let disassembly = disassemble_68000(0x1000, &[0x4E, 0xF9]);

        assert_eq!(disassembly.length, 6);
        assert!(disassembly.text.contains("$0.l"), "{}", disassembly.text);
    }
}
