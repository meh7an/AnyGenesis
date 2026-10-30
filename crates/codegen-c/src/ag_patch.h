/* What a code patch includes, from the anygenesis-codegen-c crate. Do not edit.

   A code patch is a C function `uint32_t name(AgState *s, uint32_t pc)` run before each
   execution of the instruction at `pc`. It returns where execution goes on: `pc` itself runs
   that instruction, any other address leaves there, with the cycles its pack entry gives
   charged. It may read and write the registers (`s->d`, `s->a`, A7 the active stack pointer),
   the flags (`s->x`, `s->n`, `s->z`, `s->v`, `s->c`, each 0 or 1) and work RAM through
   `ag_ram_read8/16/32` and `ag_ram_write8/16/32`; it reaches no device. */
#ifndef AG_PATCH_H
#define AG_PATCH_H

#include "ag_runtime.h"

/* Pops a return address as RTS does, for a patch that returns to the caller of the code it
   replaces. */
static inline uint32_t ag_patch_rts(AgState *s)
{
    uint32_t to = ag_ram_read32(s, s->a[7]);
    s->a[7] += 4u;
    return to & AG_ADDRESS_MASK;
}

#endif
