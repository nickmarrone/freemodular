// Runs the Drift firmware in simavr and logs:
//   dac.log      every DAC channel A sample: "<cycle> <value 0-4095>"
//   compute.log  per sample, cycles from the DAC latch until the firmware starts
//                writing the next sample, i.e. how long computing it took
//
// usage: harness fw.elf seconds [script]
//   script lines: "<ms> cv <0-3> <0-1000>"   input voltage in permille of 5V
//     0: speed CV, 1: texture CV, 2: speed knob, 3: texture knob
// env: MODE=perlin|brownian|bezier|lfo selects the algorithm (DIP switches)
//
// Note: simavr's SPI takes ~100us per byte (real hardware: ~1us), so writing a
// sample takes much longer than on the module. compute.log starts timing at the
// latch, after the SPI transfer, so it isn't affected.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sim_avr.h"
#include "sim_elf.h"
#include "avr_ioport.h"
#include "avr_adc.h"
#include "avr_spi.h"

static avr_t *avr;
static FILE *dac_log, *compute_log;
static uint16_t spi_word; static int spi_bytes;
static uint64_t cs_rise_cycle;
static void spi_cb(struct avr_irq_t *irq, uint32_t value, void *param) {
    spi_word = (spi_word << 8) | (value & 0xff); spi_bytes++;
}
static void cs_cb(struct avr_irq_t *irq, uint32_t value, void *param) {
    if (!value && cs_rise_cycle) {
        // CS falls when the firmware starts writing the next sample, so the time
        // since the last latch is how long computing that sample took
        fprintf(compute_log, "%llu %llu\n", (unsigned long long)avr->cycle, (unsigned long long)(avr->cycle - cs_rise_cycle));
    }
    if (value) { // rising edge latches the DAC
        cs_rise_cycle = avr->cycle;
        if (spi_bytes >= 2 && !(spi_word & 0x8000)) fprintf(dac_log, "%llu %u\n", (unsigned long long)avr->cycle, spi_word & 0xfff);
        spi_bytes = 0;
    }
}
// Drive an input pin from outside. Registering it as externally driven stops
// simavr's pull-up emulation from overriding it whenever the port is written.
static void set_pin(char port, uint8_t mask, uint8_t value) {
    avr_ioport_external_t ext = { .name = port, .mask = mask, .value = value };
    avr_ioctl(avr, AVR_IOCTL_IOPORT_SET_EXTERNAL(port), &ext);
    for (int bit = 0; bit < 8; bit++)
        if (mask & (1 << bit)) avr_raise_irq(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ(port), bit), (value >> bit) & 1);
}

int main(int argc, char **argv) {
    elf_firmware_t fw = {0};
    if (elf_read_firmware(argv[1], &fw)) { fprintf(stderr, "elf load failed\n"); return 1; }
    double seconds = atof(argv[2]);
    struct { uint64_t cycle; char action[16]; int a, b; } evs[512]; int nev = 0, next = 0;
    if (argc > 3) {
        FILE *s = fopen(argv[3], "r"); char line[128];
        while (fgets(line, sizeof line, s) && nev < 512) {
            double ms; evs[nev].a = evs[nev].b = 0;
            if (sscanf(line, "%lf %15s %d %d", &ms, evs[nev].action, &evs[nev].a, &evs[nev].b) >= 2) { evs[nev].cycle = ms * 16000; nev++; }
        }
        fclose(s);
    }
    avr = avr_make_mcu_by_name("atmega328p");
    avr_init(avr);
    avr_load_firmware(avr, &fw);
    avr->frequency = 16000000; avr->vcc = avr->avcc = avr->aref = 5000;
    dac_log = fopen("dac.log", "w"); compute_log = fopen("compute.log", "w");
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_SPI_GETIRQ(0), SPI_IRQ_OUTPUT), spi_cb, 0);
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ('B'), 2), cs_cb, 0);

    // DIP switches pull D5 (switch 1) and D4 (switch 2) low when on
    const char *mode = getenv("MODE") ? getenv("MODE") : "perlin";
    int sw1 = !strcmp(mode, "bezier") || !strcmp(mode, "lfo");
    int sw2 = !strcmp(mode, "brownian") || !strcmp(mode, "lfo");
    set_pin('D', (1 << 5) | (1 << 4), (sw1 ? 0 : 1 << 5) | (sw2 ? 0 : 1 << 4));

    // Input i is read from ADC4..ADC7. The firmware sets ADMUX two conversions ahead
    // because real hardware only applies it when the next conversion starts, but
    // simavr applies it at once, so in the simulator every reading lands one slot
    // early. Feed each input's voltage to the next channel to compensate.
    static const int adc_ch[4] = {ADC_IRQ_ADC5, ADC_IRQ_ADC6, ADC_IRQ_ADC7, ADC_IRQ_ADC4};
    int cv[4] = {0, 0, 500, 500};
    for (int i = 0; i < 4; i++) avr_raise_irq(avr_io_getirq(avr, AVR_IOCTL_ADC_GETIRQ, adc_ch[i]), cv[i] * 5);
    uint64_t end = seconds * 16000000;
    while (avr->cycle < end) {
        while (next < nev && avr->cycle >= evs[next].cycle) {
            if (!strcmp(evs[next].action, "cv"))
                avr_raise_irq(avr_io_getirq(avr, AVR_IOCTL_ADC_GETIRQ, adc_ch[evs[next].a & 3]), evs[next].b * 5);
            next++;
        }
        int state = avr_run(avr);
        if (state == cpu_Done || state == cpu_Crashed) { fprintf(stderr, "cpu state %d at pc %x\n", state, avr->pc); break; }
    }
    fclose(dac_log); fclose(compute_log);
    printf("cycles %llu pc %x\n", (unsigned long long)avr->cycle, avr->pc);
    return 0;
}
