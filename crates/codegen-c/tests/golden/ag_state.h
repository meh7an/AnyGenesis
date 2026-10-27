/* Generated from the anygenesis-abi crate. Do not edit. */
#ifndef AG_STATE_H
#define AG_STATE_H

#include <stddef.h>
#include <stdint.h>

typedef struct AgState AgState;
typedef struct AgEntry AgEntry;

/* A translated function: called with the 68000 address to start at, it returns the
 * address execution continues at. */
typedef uint32_t AgFn(AgState *state, uint32_t pc);

/* The machine state translated code runs on. */
struct AgState {
    /* The ROM image, read-only. */
    const uint8_t *rom;
    /* The runtime's own context, opaque to generated code. */
    void *runtime;
    /* D0-D7. */
    uint32_t d[8];
    /* A0-A7; A7 is the active stack pointer. */
    uint32_t a[8];
    /* USP in supervisor mode, SSP in user mode. */
    uint32_t inactive_sp;
    /* The next instruction, for the runtime; generated code passes addresses instead. */
    uint32_t pc;
    /* Cycles left until a check point calls the hook (at 0 or below). */
    int32_t cycles;
    /* Bytes in `rom`. */
    uint32_t rom_size;
    /* T, S and the interrupt mask in their status register bits; the flag bits stay 0. */
    uint16_t sr;
    /* The X flag, 0 or 1. */
    uint8_t x;
    /* The N flag, 0 or 1. */
    uint8_t n;
    /* The Z flag, 0 or 1. */
    uint8_t z;
    /* The V flag, 0 or 1. */
    uint8_t v;
    /* The C flag, 0 or 1. */
    uint8_t c;
    /* Work RAM in 68000 byte order. */
    uint8_t ram[65536];
};
_Static_assert(offsetof(AgState, rom) == 0, "AgState.rom moved");
_Static_assert(offsetof(AgState, runtime) == 8, "AgState.runtime moved");
_Static_assert(offsetof(AgState, d) == 16, "AgState.d moved");
_Static_assert(offsetof(AgState, a) == 48, "AgState.a moved");
_Static_assert(offsetof(AgState, inactive_sp) == 80, "AgState.inactive_sp moved");
_Static_assert(offsetof(AgState, pc) == 84, "AgState.pc moved");
_Static_assert(offsetof(AgState, cycles) == 88, "AgState.cycles moved");
_Static_assert(offsetof(AgState, rom_size) == 92, "AgState.rom_size moved");
_Static_assert(offsetof(AgState, sr) == 96, "AgState.sr moved");
_Static_assert(offsetof(AgState, x) == 98, "AgState.x moved");
_Static_assert(offsetof(AgState, n) == 99, "AgState.n moved");
_Static_assert(offsetof(AgState, z) == 100, "AgState.z moved");
_Static_assert(offsetof(AgState, v) == 101, "AgState.v moved");
_Static_assert(offsetof(AgState, c) == 102, "AgState.c moved");
_Static_assert(offsetof(AgState, ram) == 103, "AgState.ram moved");
_Static_assert(sizeof(AgState) == 65640, "AgState changed size");

/* One row of the address table: the function that runs `address`. */
struct AgEntry {
    /* A function entry or a resume point inside the function. */
    uint32_t address;
    /* The function to call with `address`. */
    AgFn *function;
};
_Static_assert(offsetof(AgEntry, address) == 0, "AgEntry.address moved");
_Static_assert(offsetof(AgEntry, function) == 8, "AgEntry.function moved");
_Static_assert(sizeof(AgEntry) == 16, "AgEntry changed size");

/* Reads a byte outside the inline fast path; `address` has 24 bits. */
uint32_t ag_read8(AgState *state, uint32_t address);

/* Reads a word outside the inline fast path. */
uint32_t ag_read16(AgState *state, uint32_t address);

/* Reads a long outside the inline fast path, as two words, high word first. */
uint32_t ag_read32(AgState *state, uint32_t address);

/* Writes the low byte of `value` outside the inline fast path. */
void ag_write8(AgState *state, uint32_t address, uint32_t value);

/* Writes the low word of `value` outside the inline fast path. */
void ag_write16(AgState *state, uint32_t address, uint32_t value);

/* Writes a long outside the inline fast path, as two words, high word first. */
void ag_write32(AgState *state, uint32_t address, uint32_t value);

/* Returns the function that runs `address`: a translated function, or an interpreter
 * entry for addresses no function covers. Never null. */
AgFn *ag_lookup(AgState *state, uint32_t address);

/* STOP: waits for an interrupt, runs it and returns where execution continues
 * (`resume` unless the handler changed the stacked PC). */
uint32_t ag_stop(AgState *state, uint32_t resume);

/* The check-point hook, called when `cycles` ran out at `pc`: delivers due interrupts
 * and returns where execution continues (`pc` unless a handler changed the stacked PC). */
uint32_t ag_check(AgState *state, uint32_t pc);

/* The slow path of a check point whose block an event falls inside: interprets from the
 * block start `pc` to the next table row and returns it with `AG_HYBRID_ROW` set, or
 * returns any other address execution goes to instead. */
uint32_t ag_hybrid(AgState *state, uint32_t pc);

/* After a write to SR, at the instruction boundary `pc`: delivers the interrupts the new
 * mask lets through and returns where execution continues (`pc` unless a handler
 * changed the stacked PC). Never ends the run. */
uint32_t ag_deliver(AgState *state, uint32_t pc);

/* RESET: asserts the reset line of the external devices. */
void ag_reset_devices(AgState *state);

#endif
