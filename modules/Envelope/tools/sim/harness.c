// Runs the envelope firmware in simavr and logs:
//   dac.log      every DAC channel A sample: "<cycle> <value 0-4095>"
//   io.log       LED (0-3) and aux (X) output changes: "<cycle> <pin> <level>"
//   compute.log  per sample, cycles from the DAC latch until the firmware starts
//                writing the next sample, i.e. how long computing it took
//
// usage: harness_env fw.elf seconds [script]
//   script lines: "<ms> <action> [args]":
//     gate_on gate_off trig btn_down btn_up
//     cv <0-3> <0-1000>   knob position in permille (0 = min, 1000 = max)
// env: EEPROM_IN / EEPROM_OUT  load/save a 1024-byte EEPROM image
//
// Note: simavr's SPI takes ~100us per byte (real hardware: ~1us), so the sample
// loop is slower than on the module and can skip samples that wouldn't be skipped
// on hardware. Use compute.log rather than dac.log gaps to judge CPU headroom.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sim_avr.h"
#include "sim_elf.h"
#include "avr_ioport.h"
#include "avr_adc.h"
#include "avr_spi.h"
#include "avr_eeprom.h"

static avr_t *avr;
static FILE *dac_log, *io_log;
static uint16_t spi_word; static int spi_bytes;
static uint64_t cs_rise_cycle;
static FILE *compute_log;
static void spi_cb(struct avr_irq_t *irq, uint32_t value, void *param) {
    spi_word = (spi_word << 8) | (value & 0xff); spi_bytes++;
}
static void cs_cb(struct avr_irq_t *irq, uint32_t value, void *param) {
    if (!value && cs_rise_cycle) {
        // CS falls when the firmware starts writing the next sample, so the time
        // since the last latch is how long computing that sample took
        fprintf(compute_log, "%llu %llu\n", (unsigned long long)avr->cycle, (unsigned long long)(avr->cycle - cs_rise_cycle));
    }
    if (value) cs_rise_cycle = avr->cycle;
    if (value) { // rising edge latches the DAC
        if (spi_bytes >= 2 && !(spi_word & 0x8000)) fprintf(dac_log, "%llu %u\n", (unsigned long long)avr->cycle, spi_word & 0xfff);
        spi_bytes = 0;
    }
}
// Drive an input pin from outside. Registering it as externally driven stops
// simavr's pull-up emulation from overriding it whenever the port is written.
static uint8_t ext_mask[3], ext_value[3];
static void set_pin(char port, int bit, int level) {
    int i = port - 'B';
    ext_mask[i] |= 1 << bit;
    ext_value[i] = level ? ext_value[i] | (1 << bit) : ext_value[i] & ~(1 << bit);
    avr_ioport_external_t ext = { .name = port, .mask = ext_mask[i], .value = ext_value[i] };
    avr_ioctl(avr, AVR_IOCTL_IOPORT_SET_EXTERNAL(port), &ext);
    avr_raise_irq(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ(port), bit), level);
}

static void io_cb(struct avr_irq_t *irq, uint32_t value, void *param) {
    fprintf(io_log, "%llu %c %u\n", (unsigned long long)avr->cycle, (char)(long)param, value);
}

int main(int argc, char **argv) {
    elf_firmware_t fw = {0};
    if (elf_read_firmware(argv[1], &fw)) { fprintf(stderr, "elf load failed\n"); return 1; }
    double seconds = atof(argv[2]);
    struct { uint64_t cycle; char action[16]; int a, b; } evs[512]; int nev = 0, next = 0;
    if (argc > 3) {
        FILE *s = fopen(argv[3], "r"); char line[128];
        while (fgets(line, sizeof line, s)) {
            double ms; evs[nev].a = evs[nev].b = 0;
            if (sscanf(line, "%lf %15s %d %d", &ms, evs[nev].action, &evs[nev].a, &evs[nev].b) >= 2) { evs[nev].cycle = ms * 16000; nev++; }
        }
        fclose(s);
    }
    avr = avr_make_mcu_by_name("atmega328p");
    avr_init(avr);
    avr_load_firmware(avr, &fw);
    avr->frequency = 16000000; avr->vcc = avr->avcc = avr->aref = 5000;
    static uint8_t ee[1024];
    char *ein = getenv("EEPROM_IN"), *eout = getenv("EEPROM_OUT");
    if (ein) {
        FILE *f = fopen(ein, "rb");
        if (!f || fread(ee, 1, 1024, f) != 1024) { fprintf(stderr, "bad EEPROM_IN\n"); return 1; }
        fclose(f);
        avr_eeprom_desc_t d = { .ee = ee, .offset = 0, .size = 1024 };
        avr_ioctl(avr, AVR_IOCTL_EEPROM_SET, &d);
    }
    dac_log = fopen("dac.log", "w"); io_log = fopen("io.log", "w"); compute_log = fopen("compute.log", "w");
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_SPI_GETIRQ(0), SPI_IRQ_OUTPUT), spi_cb, 0);
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ('B'), 2), cs_cb, 0);
    for (int i = 4; i < 8; i++)
        avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ('D'), i), io_cb, (void *)(long)('0' + i - 4));
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ('C'), 3), io_cb, (void *)(long)'X');
    // Knob i is read from ADC4..ADC7. The firmware sets ADMUX two conversions ahead
    // because real hardware only applies it when the next conversion starts, but
    // simavr applies it at once, so in the simulator every reading lands one slot
    // early. Feed each knob's voltage to the next channel to compensate.
    static const int adc_ch[4] = {ADC_IRQ_ADC5, ADC_IRQ_ADC6, ADC_IRQ_ADC7, ADC_IRQ_ADC4};
    // inputs are inverted in hardware: gate/trig/button active low; CV reads 977 at minimum
    uint64_t trig_release = 0;
    set_pin('D', 2, 1); set_pin('D', 3, 1); set_pin('B', 0, 1);
    int cv[4] = {200, 300, 600, 300};
    for (int i = 0; i < 4; i++) avr_raise_irq(avr_io_getirq(avr, AVR_IOCTL_ADC_GETIRQ, adc_ch[i]), 4770 - cv[i] * 4770 / 1000);
    uint64_t end = seconds * 16000000;
    while (avr->cycle < end) {
        while (next < nev && avr->cycle >= evs[next].cycle) {
            char *a = evs[next].action;
            if (!strcmp(a, "gate_on")) set_pin('D', 2, 0);
            else if (!strcmp(a, "gate_off")) set_pin('D', 2, 1);
            else if (!strcmp(a, "trig")) { set_pin('D', 3, 0); trig_release = avr->cycle + 16000; }
            else if (!strcmp(a, "btn_down")) set_pin('B', 0, 0);
            else if (!strcmp(a, "btn_up")) set_pin('B', 0, 1);
            else if (!strcmp(a, "cv")) { cv[evs[next].a] = evs[next].b;
                avr_raise_irq(avr_io_getirq(avr, AVR_IOCTL_ADC_GETIRQ, adc_ch[evs[next].a]), 4770 - evs[next].b * 4770 / 1000); }
            next++;
        }
        if (trig_release && avr->cycle >= trig_release) { set_pin('D', 3, 1); trig_release = 0; }
        int state = avr_run(avr);
        if (state == cpu_Done || state == cpu_Crashed) { fprintf(stderr, "cpu state %d at pc %x\n", state, avr->pc); break; }
    }
    fclose(dac_log); fclose(io_log); fclose(compute_log);
    if (eout) {
        avr_eeprom_desc_t d = { .ee = 0, .offset = 0, .size = 1024 };
        avr_ioctl(avr, AVR_IOCTL_EEPROM_GET, &d);
        FILE *f = fopen(eout, "wb"); fwrite(d.ee, 1, 1024, f); fclose(f);
    }
    printf("cycles %llu pc %x\n", (unsigned long long)avr->cycle, avr->pc);
    return 0;
}
