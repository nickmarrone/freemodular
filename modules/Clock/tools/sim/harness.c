// Runs the clock firmware in simavr, logs every PORTD (output) change to portd.log as
// "<cycle> <value>", and reports how much CPU time the TIMER1 interrupt uses.
//
// usage: harness fw.elf seconds isr_start isr_end [script]
//   script lines: "<ms> <action>", action is one of:
//     enc_down enc_up pause_down pause_up cw ccw   (cw/ccw = one encoder detent)
// env:
//   EEPROM_IN / EEPROM_OUT  load/save a 1024-byte EEPROM image
//   TRACE=<hex>,<hex>       print when execution reaches these addresses
//   TIME_FN=<hex>           print how long each call of that function takes
//
// Note: simavr's SPI takes ~100us per byte (real hardware: ~1us), so display
// updates are ~100x slower than on the module. Hold simulated buttons for 250ms+.
// Inputs are registered as externally driven (AVR_IOCTL_IOPORT_SET_EXTERNAL);
// otherwise simavr's pull-up emulation overrides them on every port write.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sim_avr.h"
#include "sim_elf.h"
#include "avr_ioport.h"
#include "avr_eeprom.h"

static FILE *out;
static avr_t *avr;

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
static void portd_cb(struct avr_irq_t *irq, uint32_t value, void *param) {
    fprintf(out, "%llu %u\n", (unsigned long long)avr->cycle, value & 0xff);
}

typedef struct { uint64_t cycle; char action[16]; } ev_t;

int main(int argc, char **argv) {
    elf_firmware_t fw = {0};
    if (elf_read_firmware(argv[1], &fw)) { fprintf(stderr, "elf load failed\n"); return 1; }
    double seconds = atof(argv[2]);
    uint32_t isr_start = strtoul(argv[3], 0, 0), isr_end = strtoul(argv[4], 0, 0);
    ev_t evs[256]; int nev = 0, next = 0;
    if (argc > 5) {
        FILE *s = fopen(argv[5], "r"); double ms; char a[16];
        while (fscanf(s, "%lf %15s", &ms, a) == 2) { evs[nev].cycle = ms * 16000; strcpy(evs[nev].action, a); nev++; }
        fclose(s);
    }
    avr = avr_make_mcu_by_name("atmega328p");
    avr_init(avr);
    avr_load_firmware(avr, &fw);
    avr->frequency = 16000000;
    static uint8_t ee[1024];
    char *ein = getenv("EEPROM_IN"), *eout = getenv("EEPROM_OUT");
    if (ein) {
        FILE *f = fopen(ein, "rb");
        if (!f || fread(ee, 1, 1024, f) != 1024) { fprintf(stderr, "bad EEPROM_IN\n"); return 1; }
        fclose(f);
        avr_eeprom_desc_t d = { .ee = ee, .offset = 0, .size = 1024 };
        avr_ioctl(avr, AVR_IOCTL_EEPROM_SET, &d);
    }
    out = fopen("portd.log", "w");
    avr_irq_register_notify(avr_io_getirq(avr, AVR_IOCTL_IOPORT_GETIRQ('D'), IOPORT_IRQ_PIN_ALL), portd_cb, 0);
    // buttons/encoder idle high (switches to ground)
    set_pin('C', 3, 1); set_pin('C', 4, 1); set_pin('B', 0, 1); set_pin('B', 1, 1);

    uint64_t end = seconds * 16000000, isr_cycles = 0, isr_max = 0, isr_entry = 0;
    int in_isr = 0, enc_phase = 0, enc_dir = 0; uint64_t enc_next = 0;
    // quadrature sequence for one detent (reversed for the other direction)
    static const int seq[4][2] = {{0,1},{0,0},{1,0},{1,1}};
    while (avr->cycle < end) {
        while (next < nev && avr->cycle >= evs[next].cycle) {
            char *a = evs[next].action;
            if (!strcmp(a, "enc_down")) set_pin('C', 4, 0);
            else if (!strcmp(a, "enc_up")) set_pin('C', 4, 1);
            else if (!strcmp(a, "pause_down")) set_pin('C', 3, 0);
            else if (!strcmp(a, "pause_up")) set_pin('C', 3, 1);
            else if (!strcmp(a, "cw")) { enc_dir = 1; enc_phase = 0; enc_next = avr->cycle; }
            else if (!strcmp(a, "ccw")) { enc_dir = -1; enc_phase = 0; enc_next = avr->cycle; }
            next++;
        }
        if (enc_dir && avr->cycle >= enc_next) {
            int a = seq[enc_phase][0], b = seq[enc_phase][1];
            if (enc_dir > 0) { int t = a; a = b; b = t; }
            set_pin('B', 0, a); set_pin('B', 1, b);
            enc_next = avr->cycle + 16000; // 1ms per quadrature step
            if (++enc_phase == 4) enc_dir = 0;
        }
        static uint32_t trace[16]; static int ntrace = -1;
        if (ntrace < 0) { ntrace = 0; char *t = getenv("TRACE"); while (t && *t) { trace[ntrace++] = strtoul(t, &t, 16); if (*t == ',') t++; } }
        for (int k = 0; k < ntrace; k++) if (avr->pc == trace[k]) fprintf(stderr, "trace %x at %.1f ms\n", trace[k], avr->cycle / 16000.0);
        {   // TIME_FN=<hex addr>: report how long each call to that function takes
            static uint32_t tf = 0; static int init = 0, active = 0; static uint16_t sp0; static uint64_t c0;
            if (!init) { init = 1; char *t = getenv("TIME_FN"); if (t) tf = strtoul(t, 0, 16); }
            uint16_t sp = avr->data[0x5d] | (avr->data[0x5e] << 8);
            if (tf && !active && avr->pc == tf) { active = 1; sp0 = sp; c0 = avr->cycle; }
            else if (active && sp > sp0) { active = 0; fprintf(stderr, "call %x took %.2f ms (at %.1f ms)\n", tf, (avr->cycle - c0) / 16000.0, c0 / 16000.0); }
        }
        int pc_in = avr->pc >= isr_start && avr->pc < isr_end;
        if (pc_in && !in_isr) { in_isr = 1; isr_entry = avr->cycle; }
        if (!pc_in && in_isr) {
            in_isr = 0; uint64_t d = avr->cycle - isr_entry; isr_cycles += d; if (d > isr_max) isr_max = d;
        }
        int state = avr_run(avr);
        if (state == cpu_Done || state == cpu_Crashed) { fprintf(stderr, "cpu state %d at pc %x\n", state, avr->pc); break; }
    }
    fclose(out);
    if (eout) {
        avr_eeprom_desc_t d = { .ee = 0, .offset = 0, .size = 1024 };
        avr_ioctl(avr, AVR_IOCTL_EEPROM_GET, &d);
        FILE *f = fopen(eout, "wb"); fwrite(d.ee, 1, 1024, f); fclose(f);
    }
    printf("cycles %llu isr_load %.1f%% isr_max_cycles %llu pc %x\n", (unsigned long long)avr->cycle,
        100.0 * isr_cycles / avr->cycle, (unsigned long long)isr_max, avr->pc);
    return 0;
}
