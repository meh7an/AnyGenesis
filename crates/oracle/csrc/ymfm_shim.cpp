// A C interface to the vendored ymfm's YM2612 for the Rust oracle. ymfm leaves timing to
// its host: this host keeps the chip's two timers and its busy flag in chip clocks and
// advances them 144 clocks after each output sample, so a timer runs out on a sample
// boundary.

#include <cstdint>

#include "ymfm_opn.h"

namespace {

// One YM2612 output sample in chip clocks (prescale 6 times 24 operator slots).
constexpr int32_t SAMPLE_CLOCKS = 144;

class Host : public ymfm::ymfm_interface {
public:
    void ymfm_set_timer(uint32_t tnum, int32_t duration_in_clocks) override {
        m_timers[tnum] = duration_in_clocks;
    }

    void ymfm_set_busy_end(uint32_t clocks) override { m_busy = static_cast<int32_t>(clocks); }

    bool ymfm_is_busy() override { return m_busy > 0; }

    // Moves time on by `clocks`, firing timers that run out.
    void advance(int32_t clocks) {
        m_busy -= clocks;
        for (uint32_t tnum = 0; tnum < 2; tnum++) {
            if (m_timers[tnum] < 0) {
                continue;
            }
            m_timers[tnum] -= clocks;
            if (m_timers[tnum] <= 0) {
                // The engine reloads the timer through ymfm_set_timer when it keeps running.
                m_timers[tnum] = -1;
                m_engine->engine_timer_expired(tnum);
            }
        }
    }

private:
    int32_t m_timers[2] = {-1, -1};
    int32_t m_busy = 0;
};

struct Chip {
    Host host;
    ymfm::ym2612 chip;

    Chip() : chip(host) { chip.reset(); }
};

} // namespace

extern "C" {

void *ag_ymfm_new() { return new Chip(); }

void ag_ymfm_free(void *chip) { delete static_cast<Chip *>(chip); }

void ag_ymfm_reset(void *chip) { static_cast<Chip *>(chip)->chip.reset(); }

void ag_ymfm_write(void *chip, uint32_t offset, uint8_t data) {
    static_cast<Chip *>(chip)->chip.write(offset, data);
}

uint8_t ag_ymfm_read_status(void *chip) { return static_cast<Chip *>(chip)->chip.read_status(); }

// Writes `count` stereo samples to `out` (left, right interleaved).
void ag_ymfm_generate(void *chip, int32_t *out, uint32_t count) {
    Chip *state = static_cast<Chip *>(chip);
    for (uint32_t index = 0; index < count; index++) {
        ymfm::ym2612::output_data output;
        state->chip.generate(&output, 1);
        out[2 * index] = output.data[0];
        out[2 * index + 1] = output.data[1];
        state->host.advance(SAMPLE_CLOCKS);
    }
}

} // extern "C"
