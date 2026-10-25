/*
 * Bridges Musashi's global host interface to the Rust side. Musashi resolves memory access
 * through fixed extern functions and keeps all CPU state in globals, so the Rust wrapper
 * attaches its bus here for the duration of each call into the core.
 */

#include <stddef.h>

#include "m68k.h"
#include "m68kcpu.h"

/* Mirrors `Registers` in src/lib.rs; the field order is part of the FFI contract. */
typedef struct {
    unsigned int d[8];
    unsigned int a[7];
    unsigned int usp;
    unsigned int ssp;
    unsigned int sr;
    unsigned int pc;
} oracle_registers;

typedef unsigned int (*oracle_read_fn)(void *bus, unsigned int address, unsigned int size);
typedef void (*oracle_write_fn)(void *bus, unsigned int address, unsigned int size,
                                unsigned int value);

static void *g_bus;
static oracle_read_fn g_read;
static oracle_write_fn g_write;

void oracle_attach_bus(void *bus, oracle_read_fn read, oracle_write_fn write) {
    g_bus = bus;
    g_read = read;
    g_write = write;
}

unsigned int m68k_read_memory_8(unsigned int address) { return g_read(g_bus, address, 1); }
unsigned int m68k_read_memory_16(unsigned int address) { return g_read(g_bus, address, 2); }
unsigned int m68k_read_memory_32(unsigned int address) { return g_read(g_bus, address, 4); }

void m68k_write_memory_8(unsigned int address, unsigned int value) {
    g_write(g_bus, address, 1, value);
}

void m68k_write_memory_16(unsigned int address, unsigned int value) {
    g_write(g_bus, address, 2, value);
}

void m68k_write_memory_32(unsigned int address, unsigned int value) {
    g_write(g_bus, address, 4, value);
}

void oracle_init(void) {
    m68k_init();
    m68k_set_cpu_type(M68K_CPU_TYPE_68000);
}

/*
 * A reset clears stop, trace, pending interrupts and the run mode left by the previous
 * case. It also schedules reset cycles that the next m68k_execute would consume instead of
 * running an instruction, so they are dropped here.
 */
void oracle_load(const oracle_registers *regs) {
    int i;

    m68k_pulse_reset();
    RESET_CYCLES = 0;

    /* SR first: it selects which of USP and ISP currently lives in A7. */
    m68k_set_reg(M68K_REG_SR, regs->sr);
    m68k_set_reg(M68K_REG_USP, regs->usp);
    m68k_set_reg(M68K_REG_ISP, regs->ssp);
    for (i = 0; i < 8; i++) {
        m68k_set_reg((m68k_register_t)(M68K_REG_D0 + i), regs->d[i]);
    }
    for (i = 0; i < 7; i++) {
        m68k_set_reg((m68k_register_t)(M68K_REG_A0 + i), regs->a[i]);
    }
    m68k_set_reg(M68K_REG_PC, regs->pc);
}

void oracle_save(oracle_registers *regs) {
    int i;

    for (i = 0; i < 8; i++) {
        regs->d[i] = m68k_get_reg(NULL, (m68k_register_t)(M68K_REG_D0 + i));
    }
    for (i = 0; i < 7; i++) {
        regs->a[i] = m68k_get_reg(NULL, (m68k_register_t)(M68K_REG_A0 + i));
    }
    regs->usp = m68k_get_reg(NULL, M68K_REG_USP);
    regs->ssp = m68k_get_reg(NULL, M68K_REG_ISP);
    regs->sr = m68k_get_reg(NULL, M68K_REG_SR);
    regs->pc = m68k_get_reg(NULL, M68K_REG_PC);
}

/* With a budget of one cycle the execute loop runs exactly one instruction. */
int oracle_step(void) { return m68k_execute(1); }

int oracle_stopped(void) { return CPU_STOPPED != 0; }

/*
 * The disassembler fetches through these hooks instead of the CPU bus. The Rust side lends
 * a byte buffer for one call under its own lock; bytes outside it read as zero, so a short
 * buffer can never be overrun.
 */
static const unsigned char *g_dasm_bytes;
static unsigned int g_dasm_base;
static unsigned int g_dasm_length;

static unsigned int dasm_byte(unsigned int address) {
    unsigned int offset = address - g_dasm_base;
    return offset < g_dasm_length ? g_dasm_bytes[offset] : 0;
}

unsigned int m68k_read_disassembler_8(unsigned int address) { return dasm_byte(address); }

unsigned int m68k_read_disassembler_16(unsigned int address) {
    return (dasm_byte(address) << 8) | dasm_byte(address + 1);
}

unsigned int m68k_read_disassembler_32(unsigned int address) {
    return (m68k_read_disassembler_16(address) << 16) | m68k_read_disassembler_16(address + 2);
}

unsigned int oracle_is_valid_68000(unsigned int word) {
    return m68k_is_valid_instruction(word, M68K_CPU_TYPE_68000);
}

unsigned int oracle_disassemble_68000(unsigned int pc, const unsigned char *bytes,
                                      unsigned int length, char *text) {
    unsigned int consumed;

    g_dasm_bytes = bytes;
    g_dasm_base = pc;
    g_dasm_length = length;
    consumed = m68k_disassemble(text, pc, M68K_CPU_TYPE_68000);
    g_dasm_bytes = NULL;
    g_dasm_length = 0;
    return consumed;
}
