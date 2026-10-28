#include "ag_runtime.h"

uint32_t s_000400(AgState *s, uint32_t pc)
{
    (void)pc;
    int32_t ag_c0 = s->cycles;
    AG_CYCLES(s, 4);
    {
        s->d[0] = 0x1u;
        s->n = (uint8_t)0x0u;
        s->z = (uint8_t)0x0u;
        s->v = (uint8_t)0x0u;
        s->c = (uint8_t)0x0u;
    }
    AG_STEP_FAST_END(s, 0x402u, 0u);
    AG_STEP_JUMP(s, 0x402u);
}

uint32_t s_000402(AgState *s, uint32_t pc)
{
    (void)pc;
    int32_t ag_c0 = s->cycles;
    AG_CYCLES(s, 12);
    {
        uint32_t t0 = (s->d[0]) & 0xFFFFu;
        uint32_t t1 = (t0 - 0x1u) & 0xFFFFu;
        s->d[0] = (s->d[0] & 0xFFFF0000u) | t1;
        uint32_t t2 = (uint32_t)(t1 != 0xFFFFu);
        if (t2) {
            AG_CYCLES(s, -2);
            AG_STEP_FAST_END(s, 0x400u, 0u);
            AG_STEP_JUMP(s, 0x400u);
        }
        AG_CYCLES(s, (int32_t)0x2u);
    }
    AG_STEP_FAST_END(s, 0x406u, 0u);
    AG_STEP_JUMP(s, 0x406u);
}

uint32_t s_000406(AgState *s, uint32_t pc)
{
    (void)pc;
    ag_step_begin(s);
    AG_CYCLES(s, 16);
    {
        uint32_t t0 = s->a[7];
        uint32_t t1 = ag_load32(s, t0, 16);
        uint32_t t2 = t0 + 0x4u;
        s->a[7] = t2;
        return ag_step_end(s, t1 & 0xFFFFFFu, 0u, AG_STEP_GO);
    }
}

uint32_t s_000408(AgState *s, uint32_t pc)
{
    (void)pc;
    ag_step_begin(s);
    AG_CYCLES(s, 16);
    {
        uint32_t t0 = (s->d[0]) & 0xFFFFu;
        ag_bus_write16(s, 0xC00000u, t0, 16);
        uint32_t t1 = ag_lts(t0, 0x0u, 16u);
        s->n = (uint8_t)t1;
        uint32_t t2 = (uint32_t)(t0 == 0x0u);
        s->z = (uint8_t)t2;
        s->v = (uint8_t)0x0u;
        s->c = (uint8_t)0x0u;
    }
    AG_STEP_END(s, 0x40Eu, 0u);
    AG_STEP_JUMP(s, 0x40Eu);
}

uint32_t s_00040E(AgState *s, uint32_t pc)
{
    (void)pc;
    ag_step_begin(s);
    AG_CYCLES(s, 18);
    {
        uint32_t t0 = s->a[7];
        uint32_t t1 = t0 - 0x4u;
        ag_store32(s, t1, 0x412u, 18);
        s->a[7] = t1;
        {
            (void)ag_step_end(s, 0x412u, 0u, AG_STEP_CALL);
            AgFn *ag_f = ag_lookup(s, 0x510u);
            uint32_t ag_n = ag_f(s, 0x510u);
            if (ag_n != 0x412u)
                return ag_n;
        }
        AG_STEP_CHECK(s, 0x412u);
        AG_STEP_JUMP(s, 0x412u);
    }
}

uint32_t s_000412(AgState *s, uint32_t pc)
{
    (void)pc;
    ag_step_begin(s);
    AG_CYCLES(s, 12);
    {
        uint32_t t0 = ag_supervisor(s);
        uint32_t t1 = t0 ^ 1u;
        if (t1) {
            AG_CYCLES(s, 22);
            {
                uint32_t e0 = ag_read_sr(s);
                uint32_t e1 = e0 | 0x2000u;
                uint32_t e2 = e1 & 0x7FFFu;
                ag_write_sr(s, e2);
                uint32_t e3 = s->a[7];
                uint32_t e4 = e3 - 0x6u;
                ag_store16(s, e4, e0, 34);
                uint32_t e5 = e4 + 0x2u;
                ag_store32(s, e5, 0x412u, 34);
                s->a[7] = e4;
                uint32_t e6 = ag_rom_read32(s, 0x20u);
                uint32_t ag_t = e6 & 0xFFFFFFu;
                AG_STEP_END(s, ag_t, 0u);
                AG_STEP_JUMP(s, ag_t);
            }
        }
        uint32_t t2 = (s->d[0]) & 0xFFFFu;
        ag_write_sr(s, t2);
    }
    AG_STEP_END(s, 0x414u, 0u);
    AG_STEP_JUMP(s, 0x414u);
}

uint32_t s_000414(AgState *s, uint32_t pc)
{
    (void)pc;
    ag_step_begin(s);
    AG_CYCLES(s, 34);
    {
        {
            uint32_t e0 = ag_read_sr(s);
            uint32_t e1 = e0 | 0x2000u;
            uint32_t e2 = e1 & 0x7FFFu;
            ag_write_sr(s, e2);
            uint32_t e3 = s->a[7];
            uint32_t e4 = e3 - 0x6u;
            ag_store16(s, e4, e0, 34);
            uint32_t e5 = e4 + 0x2u;
            ag_store32(s, e5, 0x416u, 34);
            s->a[7] = e4;
            uint32_t e6 = ag_rom_read32(s, 0x80u);
            uint32_t ag_t = e6 & 0xFFFFFFu;
            AG_STEP_END(s, ag_t, 0u);
            AG_STEP_JUMP(s, ag_t);
        }
    }
}

uint32_t s_000416(AgState *s, uint32_t pc)
{
    (void)pc;
    ag_step_begin(s);
    AG_CYCLES(s, 4);
    {
        uint32_t t0 = ag_supervisor(s);
        uint32_t t1 = t0 ^ 1u;
        if (t1) {
            AG_CYCLES(s, 30);
            {
                uint32_t e0 = ag_read_sr(s);
                uint32_t e1 = e0 | 0x2000u;
                uint32_t e2 = e1 & 0x7FFFu;
                ag_write_sr(s, e2);
                uint32_t e3 = s->a[7];
                uint32_t e4 = e3 - 0x6u;
                ag_store16(s, e4, e0, 34);
                uint32_t e5 = e4 + 0x2u;
                ag_store32(s, e5, 0x416u, 34);
                s->a[7] = e4;
                uint32_t e6 = ag_rom_read32(s, 0x20u);
                uint32_t ag_t = e6 & 0xFFFFFFu;
                AG_STEP_END(s, ag_t, 0u);
                AG_STEP_JUMP(s, ag_t);
            }
        }
        ag_write_sr(s, 0x2700u);
        {
            (void)ag_step_end(s, 0x41Au, 0u, AG_STEP_STOP);
            uint32_t ag_n = ag_stop(s, 0x41Au);
            if (ag_n != 0x41Au)
                return ag_n;
        }
        AG_STEP_CHECK(s, 0x41Au);
        AG_STEP_JUMP(s, 0x41Au);
    }
}
