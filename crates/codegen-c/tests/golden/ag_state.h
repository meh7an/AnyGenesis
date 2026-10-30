/* Generated from the anygenesis-abi crate. Do not edit. */
#ifndef AG_STATE_H
#define AG_STATE_H

#include <stddef.h>
#include <stdint.h>

typedef struct AgState AgState;
typedef struct AgEntry AgEntry;
typedef struct AgExtra AgExtra;
typedef struct AgGame AgGame;

/* A translated function: called with the 68000 address to start at, it returns the
 * address execution continues at. */
typedef uint32_t AgFn(AgState *state, uint32_t pc);
/* A program's interrupt entry: takes the interrupt at `level` before the instruction
 * at `pc` and returns its handler's address. */
typedef uint32_t AgInterrupt(AgState *state, uint32_t level, uint32_t pc);

/* The layout version of AgGame. */
#define AG_GAME_ABI 1u
/* The kinds of AgExtra, as the abi crate describes them. */
#define AG_EXTRA_RAM_PATCH 1u
#define AG_EXTRA_FLAG 2u

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

/* Something embedded beside the ROM image for the player, of a kind `EXTRA_*`; a player
 * skips kinds it does not know. */
struct AgExtra {
    /* What the extra holds. */
    uint32_t kind;
    /* Bytes at `data`. */
    uint32_t size;
    /* Its name, NUL-terminated. */
    const char *name;
    /* Its bytes. */
    const uint8_t *data;
};
_Static_assert(offsetof(AgExtra, kind) == 0, "AgExtra.kind moved");
_Static_assert(offsetof(AgExtra, size) == 4, "AgExtra.size moved");
_Static_assert(offsetof(AgExtra, name) == 8, "AgExtra.name moved");
_Static_assert(offsetof(AgExtra, data) == 16, "AgExtra.data moved");
_Static_assert(sizeof(AgExtra) == 24, "AgExtra changed size");

/* Everything a game program tells the player about itself, written by the build as
 * the one symbol `ag_game`. */
struct AgGame {
    /* `AG_GAME_ABI` of the header the build wrote it with. */
    uint32_t abi;
    /* Bytes in `image`. */
    uint32_t image_size;
    /* The whole ROM image the program was translated from. */
    const uint8_t *image;
    /* The SHA-1 of `image` as the build computed it. */
    uint8_t sha1[20];
    /* The title from the ROM's header, NUL-terminated UTF-8. */
    const char *title;
    /* Every function entry and resume point, ascending by address. */
    const AgEntry *table;
    /* Rows in `table`. */
    uint32_t table_size;
    /* Entries in `steps`. */
    uint32_t steps_size;
    /* The stepped function of every even ROM address, index address / 2, null where
     * the superset kept none. */
    AgFn *const *steps;
    /* The checked entry into the stepped functions, `ag_step_entry`. */
    AgFn *step_entry;
    /* The program's interrupt entry, `ag_interrupt`. */
    AgInterrupt *interrupt;
    /* Entries of the functions reset and the exception vectors start, ascending. */
    const uint32_t *vector_handlers;
    /* Entries in `vector_handlers`. */
    uint32_t vector_handler_count;
    /* Entries in `function_entries`. */
    uint32_t function_entry_count;
    /* Entries of every analysed function, ascending. */
    const uint32_t *function_entries;
    /* What is embedded beside the image. */
    const AgExtra *extras;
    /* Entries in `extras`. */
    uint32_t extra_count;
};
_Static_assert(offsetof(AgGame, abi) == 0, "AgGame.abi moved");
_Static_assert(offsetof(AgGame, image_size) == 4, "AgGame.image_size moved");
_Static_assert(offsetof(AgGame, image) == 8, "AgGame.image moved");
_Static_assert(offsetof(AgGame, sha1) == 16, "AgGame.sha1 moved");
_Static_assert(offsetof(AgGame, title) == 40, "AgGame.title moved");
_Static_assert(offsetof(AgGame, table) == 48, "AgGame.table moved");
_Static_assert(offsetof(AgGame, table_size) == 56, "AgGame.table_size moved");
_Static_assert(offsetof(AgGame, steps_size) == 60, "AgGame.steps_size moved");
_Static_assert(offsetof(AgGame, steps) == 64, "AgGame.steps moved");
_Static_assert(offsetof(AgGame, step_entry) == 72, "AgGame.step_entry moved");
_Static_assert(offsetof(AgGame, interrupt) == 80, "AgGame.interrupt moved");
_Static_assert(offsetof(AgGame, vector_handlers) == 88, "AgGame.vector_handlers moved");
_Static_assert(offsetof(AgGame, vector_handler_count) == 96, "AgGame.vector_handler_count moved");
_Static_assert(offsetof(AgGame, function_entry_count) == 100, "AgGame.function_entry_count moved");
_Static_assert(offsetof(AgGame, function_entries) == 104, "AgGame.function_entries moved");
_Static_assert(offsetof(AgGame, extras) == 112, "AgGame.extras moved");
_Static_assert(offsetof(AgGame, extra_count) == 120, "AgGame.extra_count moved");
_Static_assert(sizeof(AgGame) == 128, "AgGame changed size");

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

/* Returns the function that runs `address`: a translated function, else its stepped
 * function, else one that ends the run (or, in runtimes with the interpreter, an
 * interpreter entry). Never null. */
AgFn *ag_lookup(AgState *state, uint32_t address);

/* STOP: waits for an interrupt, runs it and returns where execution continues
 * (`resume` unless the handler changed the stacked PC). */
uint32_t ag_stop(AgState *state, uint32_t resume);

/* The check-point hook, called when `cycles` ran out at `pc`: delivers due interrupts
 * and returns where execution continues (`pc` unless a handler changed the stacked PC). */
uint32_t ag_check(AgState *state, uint32_t pc);

/* The slow path of a check point, before stepped code runs the rest of its block: gives
 * back the `ahead` cycles charged for instructions not yet run and, unless `latch` is
 * `AG_NO_LATCH`, applies the latch rule to the instruction before with its `latch`
 * point. */
void ag_step_enter(AgState *state, uint32_t ahead, uint32_t latch);

/* The scheduler at the stepped instruction boundary `pc` where the budget ran out, as
 * the interpreter calls it: returns where execution continues. */
uint32_t ag_step_check(AgState *state, uint32_t pc);

/* Notes the start of a stepped instruction that may reach a device, write SR, take an
 * exception or reset; its accesses see that start until `ag_step_end`. */
void ag_step_begin(AgState *state);

/* Ends a stepped instruction begun with `ag_step_begin` as the interpreter ends one:
 * stalls, the latch rule with its `latch` point, rescheduling, then by `kind` delivery
 * at `next`, a check soon or nothing. Returns where execution continues. */
uint32_t ag_step_end(AgState *state, uint32_t next, uint32_t latch, uint32_t kind);

/* Ends a stepped instruction without `ag_step_begin` once the budget ran out; it
 * charged `charged` cycles. As `ag_step_end` with `AG_STEP_GO`. */
uint32_t ag_step_late(AgState *state, uint32_t next, uint32_t latch, uint32_t charged);

/* Returns the stepped function for `address`, else as `ag_lookup`. Never null. */
AgFn *ag_step_lookup(AgState *state, uint32_t address);

/* After a write to SR, at the instruction boundary `pc`: delivers the interrupts the new
 * mask lets through and returns where execution continues (`pc` unless a handler
 * changed the stacked PC). Never ends the run. */
uint32_t ag_deliver(AgState *state, uint32_t pc);

/* RESET: asserts the reset line of the external devices. */
void ag_reset_devices(AgState *state);

#endif
