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

#define AG_ACCESSORS(bits, bytes)                                                           \
    static inline uint32_t ag_load##bits(AgState *s, uint32_t address)                      \
    {                                                                                       \
        address &= AG_ADDRESS_MASK;                                                         \
        if (ag_in_ram(address, bytes))                                                      \
            return ag_ram_read##bits(s, address);                                           \
        if (ag_in_rom(s, address, bytes))                                                   \
            return ag_rom_read##bits(s, address);                                           \
        return ag_read##bits(s, address);                                                   \
    }                                                                                       \
    static inline void ag_store##bits(AgState *s, uint32_t address, uint32_t value)         \
    {                                                                                       \
        address &= AG_ADDRESS_MASK;                                                         \
        if (ag_in_ram(address, bytes))                                                      \
            ag_ram_write##bits(s, address, value);                                          \
        else                                                                                \
            ag_write##bits(s, address, value);                                              \
    }

AG_ACCESSORS(8, 1u)
AG_ACCESSORS(16, 2u)
AG_ACCESSORS(32, 4u)

#endif
