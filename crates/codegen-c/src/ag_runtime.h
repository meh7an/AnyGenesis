/* Support code for generated functions, from the anygenesis-codegen-c crate. Do not edit. */
#ifndef AG_RUNTIME_H
#define AG_RUNTIME_H

#include "ag_state.h"

/* A guaranteed tail call: jumps between functions never grow the native stack. */
#define AG_TAIL __attribute__((musttail))

#define AG_ADDRESS_MASK 0xFFFFFFu
#define AG_SR_MASK @SR_MASK@
#define AG_SR_SUPERVISOR @SR_SUPERVISOR@
#define AG_RAM_START 0xE00000u
#define AG_RAM_MASK @RAM_MASK@

/* Cycle accounting and interrupt check points; the policy belongs to the runtime. */
#define AG_CYCLES(s, n) ((s)->cycles -= (n))
#define AG_CHECK(s, pc)                                                                     \
    do {                                                                                    \
        if ((s)->cycles <= 0) {                                                             \
            uint32_t ag_next = ag_check((s), (pc));                                         \
            if (ag_next != (pc))                                                            \
                return ag_next;                                                             \
        }                                                                                   \
    } while (0)
/* Stepped native code: every kept ROM address has a stepped function in `AG_STEPS` that runs
   that one instruction with the boundary protocol of the interpreter (the latch rule,
   delivery after it, then the scheduler at the next boundary where the budget ran out) and
   goes on at the next. A stepped function at a table row hands over to the translated
   function there, except the first one a slow path enters, which `AG_STEP_FIRST` in `pc`
   marks. Stepped functions assume the scheduler ran where the budget ran out; entries from
   anywhere else go through `AG_STEP_ENTRY`, which runs it first. */
#define AG_STEP_FIRST 0x20000000u
/* `latch` for `ag_step_enter` when no instruction's latch rule applies. */
#define AG_NO_LATCH 0xFFFFu
/* `kind` for `ag_step_end`: delivery at the next instruction, a check soon (a call, whose
   target takes it), or nothing (STOP, which waits for it). */
#define AG_STEP_GO 0u
#define AG_STEP_CALL 1u
#define AG_STEP_STOP 2u
/* A check point with `bound` cycles at most from it to its block's end: at a block start,
   or where code it cannot bound (a call, an exception handler, STOP) came back inside the
   block. Once the budget may end before the block does, also the one a check just started,
   the block goes on as stepped code, so an interrupt is taken at its instruction boundary,
   and the stepped code hands back at the next row. */
#define AG_HCHECK(s, pc, bound)                                                             \
    do {                                                                                    \
        if ((s)->cycles <= (bound)) {                                                       \
            if ((s)->cycles <= 0) {                                                         \
                uint32_t ag_next = ag_check((s), (pc));                                     \
                if (ag_next != (pc))                                                        \
                    return ag_next;                                                         \
            }                                                                               \
            if ((s)->cycles > 0 && (s)->cycles <= (bound)) {                                \
                ag_step_enter((s), 0u, AG_NO_LATCH);                                        \
                AG_TAIL return AG_STEPS[(pc) >> 1]((s), (pc) | AG_STEP_FIRST);              \
            }                                                                               \
        }                                                                                   \
    } while (0)
/* After an instruction that may reach a device, or one that wrote SR: a stall, an interrupt
   its access made due or one delivered after the SR write can leave the budget, with the
   `ahead` cycles charged for instructions not yet run, at no more than `rest`, the rest of
   the block's most cycles. The rest then goes on as stepped code from `pc` (the latch rule
   of the instruction with its `latch` point first, unless it is `AG_NO_LATCH`). */
#define AG_ACHECK(s, pc, ahead, rest, latch)                                                \
    do {                                                                                    \
        if ((s)->cycles + (ahead) <= (rest)) {                                              \
            ag_step_enter((s), (ahead), (latch));                                           \
            AG_TAIL return AG_STEP_ENTRY((s), (pc) | AG_STEP_FIRST);                        \
        }                                                                                   \
    } while (0)
/* The scheduler at the stepped instruction boundary `pc` where the budget ran out, as the
   interpreter calls it: where a call or STOP came back inside stepped code. */
#define AG_STEP_CHECK(s, pc)                                                                \
    do {                                                                                    \
        if ((s)->cycles <= 0) {                                                             \
            uint32_t ag_next = ag_step_check((s), (pc));                                    \
            if (ag_next != (pc))                                                            \
                return ag_next;                                                             \
        }                                                                                   \
    } while (0)
/* The end of a fast-path stepped instruction going on at `next`, whose start left `ag_c0`
   cycles: the runtime only once the budget ran out. */
#define AG_STEP_FAST_END(s, next, latch)                                                    \
    do {                                                                                    \
        if ((s)->cycles <= 0) {                                                             \
            uint32_t ag_next =                                                              \
                ag_step_late((s), (next), (latch), (uint32_t)(ag_c0 - (s)->cycles));        \
            if (ag_next != (next))                                                          \
                return ag_next;                                                             \
        }                                                                                   \
    } while (0)
/* The end of a stepped instruction begun with `ag_step_begin`, going on at `next`. */
#define AG_STEP_END(s, next, latch)                                                         \
    do {                                                                                    \
        uint32_t ag_next = ag_step_end((s), (next), (latch), AG_STEP_GO);                   \
        if (ag_next != (next))                                                              \
            return ag_next;                                                                 \
    } while (0)
/* On at `target`, known only when the instruction runs, through the dispatcher. */
#define AG_STEP_JUMP(s, target)                                                             \
    do {                                                                                    \
        AgFn *ag_fn = ag_step_lookup((s), (target));                                        \
        AG_TAIL return ag_fn((s), (target));                                                \
    } while (0)
/* After a write to SR: an interrupt the new mask lets through is taken before `pc`. */
#define AG_DELIVER(s, pc)                                                                   \
    do {                                                                                    \
        uint32_t ag_next = ag_deliver((s), (pc));                                           \
        if (ag_next != (pc))                                                                \
            return ag_next;                                                                 \
    } while (0)

/* Status register: the system byte lives in s->sr, each flag in its own field. */

static inline uint32_t ag_read_sr(const AgState *s)
{
    return (uint32_t)s->sr | (uint32_t)s->x << 4 | (uint32_t)s->n << 3 | (uint32_t)s->z << 2 |
           (uint32_t)s->v << 1 | (uint32_t)s->c;
}

static inline void ag_write_ccr(AgState *s, uint32_t value)
{
    s->x = (uint8_t)(value >> 4 & 1u);
    s->n = (uint8_t)(value >> 3 & 1u);
    s->z = (uint8_t)(value >> 2 & 1u);
    s->v = (uint8_t)(value >> 1 & 1u);
    s->c = (uint8_t)(value & 1u);
}

/* Writes the status register; a change of S swaps A7 with the inactive stack pointer. */
static inline void ag_write_sr(AgState *s, uint32_t value)
{
    uint32_t was = (uint32_t)s->sr & AG_SR_SUPERVISOR;
    value &= AG_SR_MASK;
    s->sr = (uint16_t)(value & 0xFF00u);
    ag_write_ccr(s, value);
    if ((value & AG_SR_SUPERVISOR) != was) {
        uint32_t sp = s->a[7];
        s->a[7] = s->inactive_sp;
        s->inactive_sp = sp;
    }
}

static inline uint32_t ag_supervisor(const AgState *s)
{
    return ((uint32_t)s->sr & AG_SR_SUPERVISOR) != 0;
}

/* Pure primitives, bit for bit the IR's definitions (anygenesis-ir eval.rs). Inputs fit
 * their width; callers mask results that may not. */

static inline uint32_t ag_sext(uint32_t value, unsigned bits)
{
    unsigned shift = 32u - bits;
    return (uint32_t)((int32_t)(value << shift) >> shift);
}

static inline uint32_t ag_divu(uint32_t left, uint32_t right)
{
    return right == 0 ? 0u : left / right;
}

static inline uint32_t ag_remu(uint32_t left, uint32_t right)
{
    return right == 0 ? 0u : left % right;
}

/* In 64 bits the most negative value divided by -1 cannot overflow. */
static inline uint32_t ag_divs(uint32_t left, uint32_t right, unsigned bits)
{
    int64_t l = (int32_t)ag_sext(left, bits);
    int64_t r = (int32_t)ag_sext(right, bits);
    return r == 0 ? 0u : (uint32_t)(l / r);
}

static inline uint32_t ag_rems(uint32_t left, uint32_t right, unsigned bits)
{
    int64_t l = (int32_t)ag_sext(left, bits);
    int64_t r = (int32_t)ag_sext(right, bits);
    return r == 0 ? 0u : (uint32_t)(l % r);
}

static inline uint32_t ag_lts(uint32_t left, uint32_t right, unsigned bits)
{
    return (int32_t)ag_sext(left, bits) < (int32_t)ag_sext(right, bits);
}

static inline uint32_t ag_shl(uint32_t value, uint32_t count, unsigned bits)
{
    return count >= bits ? 0u : value << count;
}

static inline uint32_t ag_shr(uint32_t value, uint32_t count, unsigned bits)
{
    return count >= bits ? 0u : value >> count;
}

static inline uint32_t ag_sar(uint32_t value, uint32_t count, unsigned bits)
{
    int32_t extended = (int32_t)ag_sext(value, bits);
    return (uint32_t)(extended >> (count >= bits ? bits - 1u : count));
}

/* DIVU's data-dependent cycles: 10 when the quotient cannot fit, else a 15-step restoring
 * division at 4, 6 or 8 cycles a step after 16. */
static inline uint32_t ag_divu_time(uint32_t dividend, uint32_t divisor)
{
    uint32_t shifted = (divisor & 0xFFFFu) << 16;
    uint32_t remainder = dividend;
    uint32_t cycles = 16u;
    if (dividend >= shifted)
        return 10u;
    for (int step = 0; step < 15; step++) {
        uint32_t carry = remainder & 0x80000000u;
        remainder <<= 1;
        if (carry) {
            remainder -= shifted;
            cycles += 4u;
        } else if (remainder >= shifted) {
            remainder -= shifted;
            cycles += 6u;
        } else {
            cycles += 8u;
        }
    }
    return cycles;
}

/* DIVS's data-dependent cycles: 16 (18 for a negative dividend) when the magnitudes'
 * quotient cannot fit, else 150 plus 4 for a negative dividend and 2 for a negative quotient,
 * minus 2 per set bit in bits 15-1 of the magnitudes' quotient. */
static inline uint32_t ag_divs_time(uint32_t dividend, uint32_t divisor)
{
    uint32_t negative_dividend = dividend >> 31;
    uint32_t negative_divisor = divisor >> 15 & 1u;
    uint32_t magnitude = negative_dividend ? 0u - dividend : dividend;
    uint32_t by = negative_divisor ? 0x10000u - (divisor & 0xFFFFu) : divisor & 0xFFFFu;
    uint32_t cycles;
    if (by == 0u || magnitude >> 16 >= by)
        return negative_dividend ? 18u : 16u;
    cycles = 150u - 2u * (uint32_t)__builtin_popcount(magnitude / by & 0xFFFEu);
    if (negative_dividend)
        cycles += 4u;
    if (negative_dividend != negative_divisor)
        cycles += 2u;
    return cycles;
}

static inline uint32_t ag_top(uint32_t value, unsigned bits)
{
    return value >> (bits - 1u) & 1u;
}

static inline uint32_t ag_add_carry(uint32_t l, uint32_t r, uint32_t result, unsigned bits)
{
    return ag_top((l & r) | (~result & (l | r)), bits);
}

static inline uint32_t ag_add_overflow(uint32_t l, uint32_t r, uint32_t result, unsigned bits)
{
    return ag_top((l ^ result) & (r ^ result), bits);
}

static inline uint32_t ag_sub_carry(uint32_t l, uint32_t r, uint32_t result, unsigned bits)
{
    return ag_top((~l & r) | (result & ~(l ^ r)), bits);
}

static inline uint32_t ag_sub_overflow(uint32_t l, uint32_t r, uint32_t result, unsigned bits)
{
    return ag_top((l ^ r) & (l ^ result), bits);
}

/* Memory. Work RAM and ROM are read inline; everything else goes to the runtime's bus. RAM
 * holds 68000 byte order, and every byte index wraps inside it as the mirrors do. */

static inline int ag_in_ram(uint32_t address, uint32_t bytes)
{
    return address >= AG_RAM_START && address <= AG_ADDRESS_MASK - (bytes - 1u);
}

static inline int ag_in_rom(const AgState *s, uint32_t address, uint32_t bytes)
{
    return address + bytes <= s->rom_size;
}

static inline uint32_t ag_ram_read8(const AgState *s, uint32_t address)
{
    return s->ram[address & AG_RAM_MASK];
}

static inline uint32_t ag_ram_read16(const AgState *s, uint32_t address)
{
    return ag_ram_read8(s, address) << 8 | ag_ram_read8(s, address + 1u);
}

static inline uint32_t ag_ram_read32(const AgState *s, uint32_t address)
{
    return ag_ram_read16(s, address) << 16 | ag_ram_read16(s, address + 2u);
}

static inline void ag_ram_write8(AgState *s, uint32_t address, uint32_t value)
{
    s->ram[address & AG_RAM_MASK] = (uint8_t)value;
}

static inline void ag_ram_write16(AgState *s, uint32_t address, uint32_t value)
{
    ag_ram_write8(s, address, value >> 8);
    ag_ram_write8(s, address + 1u, value);
}

static inline void ag_ram_write32(AgState *s, uint32_t address, uint32_t value)
{
    ag_ram_write16(s, address, value >> 16);
    ag_ram_write16(s, address + 2u, value);
}

static inline uint32_t ag_rom_read8(const AgState *s, uint32_t address)
{
    return s->rom[address];
}

static inline uint32_t ag_rom_read16(const AgState *s, uint32_t address)
{
    return ag_rom_read8(s, address) << 8 | ag_rom_read8(s, address + 1u);
}

static inline uint32_t ag_rom_read32(const AgState *s, uint32_t address)
{
    return ag_rom_read16(s, address) << 16 | ag_rom_read16(s, address + 2u);
}

/* Device accesses see the time at the start of their instruction: code charges a segment's
 * cycles before running it, and `ahead` of them lie past that start. */
#define AG_ACCESSORS(bits, bytes)                                                           \
    static inline uint32_t ag_bus_read##bits(AgState *s, uint32_t address, int32_t ahead)   \
    {                                                                                       \
        uint32_t value;                                                                     \
        s->cycles += ahead;                                                                 \
        value = ag_read##bits(s, address);                                                  \
        s->cycles -= ahead;                                                                 \
        return value;                                                                       \
    }                                                                                       \
    static inline void ag_bus_write##bits(AgState *s, uint32_t address, uint32_t value,     \
                                          int32_t ahead)                                    \
    {                                                                                       \
        s->cycles += ahead;                                                                 \
        ag_write##bits(s, address, value);                                                  \
        s->cycles -= ahead;                                                                 \
    }                                                                                       \
    static inline uint32_t ag_load##bits(AgState *s, uint32_t address, int32_t ahead)       \
    {                                                                                       \
        address &= AG_ADDRESS_MASK;                                                         \
        if (ag_in_ram(address, bytes))                                                      \
            return ag_ram_read##bits(s, address);                                           \
        if (ag_in_rom(s, address, bytes))                                                   \
            return ag_rom_read##bits(s, address);                                           \
        return ag_bus_read##bits(s, address, ahead);                                        \
    }                                                                                       \
    static inline void ag_store##bits(AgState *s, uint32_t address, uint32_t value,         \
                                      int32_t ahead)                                        \
    {                                                                                       \
        address &= AG_ADDRESS_MASK;                                                         \
        if (ag_in_ram(address, bytes))                                                      \
            ag_ram_write##bits(s, address, value);                                          \
        else                                                                                \
            ag_bus_write##bits(s, address, value, ahead);                                   \
    }

AG_ACCESSORS(8, 1u)
AG_ACCESSORS(16, 2u)
AG_ACCESSORS(32, 4u)

#endif
