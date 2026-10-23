/* Mock libjack.so.0 for end-to-end testing of the BananaSplit binary without an
 * audio server. It plays the role of the JACK server: owns port buffers,
 * performs connections, runs the client's process callback in a separate
 * thread and checks what comes out. Results go to $MOCKJACK_REPORT. */
#include <errno.h>
#include <math.h>
#include <pthread.h>
#include <regex.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define NF 128
#define MAXP 128
#define MAXC 256
typedef int (*proc_cb)(uint32_t, void *);
typedef int (*xrun_cb)(void *);
typedef void (*preg_cb)(uint32_t, int, void *);
typedef void (*shut_cb)(void *);
typedef int (*srate_cb)(uint32_t, void *);
typedef void (*conn_cb)(uint32_t, uint32_t, int, void *);

typedef struct { char name[128]; unsigned long flags; float buf[NF]; int ours; } port_t;
static port_t ports[MAXP];
static int nports;
static struct { int src, dst; } conns[MAXC];
static int nconns;
static proc_cb pcb; static void *parg;
static xrun_cb xcb; static void *xarg;
static preg_cb rcb; static void *rarg;
static srate_cb scb; static void *sarg;
static conn_cb ccb; static void *carg;
/* The graph's sample rate. MOCKJACK_RATE sets it; MOCKJACK_PRE_RATE is what
   jack_get_sample_rate says until the first cycle ran (PipeWire's libjack
   reports the PIPEWIRE_LATENCY denominator there); MOCKJACK_RATE_SWITCH=C:R
   changes the rate to R before cycle C (like pw-metadata clock.force-rate).
   MOCKJACK_NO_SRATE_CB: never call the sample-rate callback. */
static volatile uint32_t rate = 48000, pre_rate = 0;
static volatile int cycled = 0;
/* MOCKJACK_TONE=HZ: system:capture_1/2 play a sine of HZ at 0.5;
   with MOCKJACK_TONE_ALL every other port that sends audio plays one too,
   each at its own level (for screenshots) */
static double tone_hz = 0.0; static unsigned long long tone_pos = 0; static int tone_all = 0;
static char cname[64];
static pthread_t th;
static volatile int running;
static int impulse_cycle = 500, max_cycles = 2000; /* MOCKJACK_IMPULSE_CYCLE / MOCKJACK_CYCLES */
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;

static int add_port(const char *full, unsigned long flags, int ours) {
  snprintf(ports[nports].name, sizeof ports[nports].name, "%s", full);
  ports[nports].flags = flags; ports[nports].ours = ours;
  return nports++;
}
static int find(const char *n) { for (int i = 0; i < nports; i++) if (!strcmp(ports[i].name, n)) return i; return -1; }

void *jack_client_open(const char *name, int opts, int *status, ...) {
  if (getenv("MOCKJACK_RATE")) rate = (uint32_t)atoi(getenv("MOCKJACK_RATE"));
  if (getenv("MOCKJACK_PRE_RATE")) pre_rate = (uint32_t)atoi(getenv("MOCKJACK_PRE_RATE"));
#ifndef MOCK_PIPEWIRE
  /* plays JACK2's libjack with no jackd running (the PipeWire fallback test) */
  if (getenv("MOCKJACK_NOSERVER")) { if (status) *status = 0x11; return NULL; }
#else
  if (getenv("MOCKJACK_REPORT_PW")) { FILE *f = fopen(getenv("MOCKJACK_REPORT_PW"), "a"); if (f) { fprintf(f, "PIPEWIRE LIBJACK %s\n", name); fclose(f); } }
#endif
  (void)opts; strncpy(cname, name, 63); if (status) *status = 0;
  if (getenv("MOCKJACK_DEVICES")) { /* the review's box: a webcam listed before the USB mic */
    add_port("Razer Kiyo Analog Stereo:capture_FL", 0x2 | 0x4, 0); add_port("Razer Kiyo Analog Stereo:capture_FR", 0x2 | 0x4, 0);
    add_port("Yeti Stereo Microphone Analog Stereo:capture_FL", 0x2 | 0x4, 0);
    add_port("Yeti Stereo Microphone Analog Stereo:capture_FR", 0x2 | 0x4, 0);
    /* the mic's headphone output: same description, so PipeWire adds -63 */
    add_port("Yeti Stereo Microphone Analog Stereo-63:playback_FL", 0x1 | 0x4, 0);
    add_port("Yeti Stereo Microphone Analog Stereo-63:playback_FR", 0x1 | 0x4, 0);
    add_port("Yeti Stereo Microphone Analog Stereo-63:monitor_FL", 0x2 | 0x4, 0);
    add_port("Yeti Stereo Microphone Analog Stereo-63:monitor_FR", 0x2 | 0x4, 0);
  }
  /* external ports: physical capture (outputs), physical playback (inputs), an app */
  add_port("Speakers:monitor_FL", 0x2 | 0x4, 0); add_port("Speakers:monitor_FR", 0x2 | 0x4, 0);
  add_port("system:capture_1", 0x2 | 0x4, 0); add_port("system:capture_2", 0x2 | 0x4, 0);
  add_port("system:playback_1", 0x1 | 0x4, 0); add_port("system:playback_2", 0x1 | 0x4, 0);
  add_port("Firefox:output_FL", 0x2, 0); add_port("Firefox:output_FR", 0x2, 0);
  add_port("OBS:input_1", 0x1, 0);
  if (getenv("MOCKJACK_CABLES")) { /* what PipeWire's JACK layer shows for BananaSplit's cables */
    const char *in[6] = {"BananaSplit-In1-Mic","BananaSplit-In2-Discord","BananaSplit-In3-Desktop","BananaSplit-In4-Game","BananaSplit-In5-Music","BananaSplit-In6-Aux"};
    char b[128];
    for (int i = 0; i < 6; i++) {
      snprintf(b, sizeof b, "%s:playback_FL", in[i]); add_port(b, 0x1, 0);
      snprintf(b, sizeof b, "%s:playback_FR", in[i]); add_port(b, 0x1, 0);
      snprintf(b, sizeof b, "%s:monitor_FL", in[i]);  add_port(b, 0x2, 0);
      snprintf(b, sizeof b, "%s:monitor_FR", in[i]);  add_port(b, 0x2, 0);
    }
    add_port("BananaSplit-Out-B1:input_FL", 0x1, 0); add_port("BananaSplit-Out-B1:input_FR", 0x1, 0);
    add_port("BananaSplit-Out-B1:capture_FL", 0x2, 0); add_port("BananaSplit-Out-B1:capture_FR", 0x2, 0);
    add_port("BananaSplit-Out-B2:input_FL", 0x1, 0); add_port("BananaSplit-Out-B2:input_FR", 0x1, 0);
  }
  return (void *)1;
}
int jack_client_close(void *c) { (void)c; running = 0; if (th) pthread_join(th, NULL); th = 0; return 0; }
const char *jack_get_client_name(void *c) { (void)c; return cname; }
int jack_set_process_callback(void *c, proc_cb f, void *a) { (void)c; pcb = f; parg = a; return 0; }
int jack_set_xrun_callback(void *c, xrun_cb f, void *a) { (void)c; xcb = f; xarg = a; return 0; }
int jack_set_port_registration_callback(void *c, preg_cb f, void *a) { (void)c; rcb = f; rarg = a; return 0; }
int jack_set_sample_rate_callback(void *c, srate_cb f, void *a) { (void)c; scb = f; sarg = a; return 0; }
int jack_set_port_connect_callback(void *c, conn_cb f, void *a) { (void)c; ccb = f; carg = a; return 0; }
void jack_on_shutdown(void *c, shut_cb f, void *a) { (void)c; (void)f; (void)a; }
void *jack_port_register(void *c, const char *s, const char *t, unsigned long fl, unsigned long b) {
  (void)c; (void)b;
  if (strcmp(t, "32 bit float mono audio")) return NULL;
  char full[128]; snprintf(full, sizeof full, "%s:%s", cname, s);
  return &ports[add_port(full, fl, 1)];
}
void *jack_port_get_buffer(void *p, uint32_t n) { (void)n; return ((port_t *)p)->buf; }
const char *jack_port_name(void *p) { return ((port_t *)p)->name; }
typedef struct { uint32_t min, max; } lr_t;
void jack_port_get_latency_range(void *p, int mode, lr_t *r) {
  (void)p; const char *l = getenv("MOCKJACK_LATENCY"); /* frames, both directions */
  r->min = r->max = l ? (uint32_t)atoi(l) : (mode == 0 ? NF : 2 * NF);
}
int jack_disconnect(void *c, const char *a, const char *b) {
  (void)c; pthread_mutex_lock(&mu);
  int s = find(a), d = find(b), rc = -1;
  for (int i = 0; i < nconns; i++) if (conns[i].src == s && conns[i].dst == d) {
    conns[i] = conns[--nconns]; rc = 0; break;
  }
  pthread_mutex_unlock(&mu);
  if (!rc && ccb) ccb((uint32_t)s, (uint32_t)d, 0, carg);
  return rc;
}
const char **jack_port_get_all_connections(void *c, void *port) {
  (void)c; int me = (int)((port_t *)port - ports), k = 0;
  const char **out = calloc(MAXC + 1, sizeof *out);
  pthread_mutex_lock(&mu);
  for (int i = 0; i < nconns; i++) {
    if (conns[i].src == me) out[k++] = ports[conns[i].dst].name;
    else if (conns[i].dst == me) out[k++] = ports[conns[i].src].name;
  }
  pthread_mutex_unlock(&mu);
  if (!k) { free(out); return NULL; }
  return out;
}
int jack_connect(void *c, const char *a, const char *b) {
  (void)c; pthread_mutex_lock(&mu);
  int s = find(a), d = find(b), rc = 0;
  if (s < 0 || d < 0 || !(ports[s].flags & 2) || !(ports[d].flags & 1)) rc = EINVAL;
  else {
    for (int i = 0; i < nconns; i++) if (conns[i].src == s && conns[i].dst == d) rc = EEXIST;
    if (!rc) { conns[nconns].src = s; conns[nconns].dst = d; nconns++; }
  }
  pthread_mutex_unlock(&mu);
  if (!rc && ccb) ccb((uint32_t)s, (uint32_t)d, 1, carg);
  return rc;
}
const char **jack_get_ports(void *c, const char *pat, const char *type, unsigned long flags) {
  (void)c; (void)type; regex_t re; int use = pat && *pat;
  if (use && regcomp(&re, pat, REG_EXTENDED | REG_NOSUB)) return NULL;
  const char **out = calloc(MAXP + 1, sizeof *out); int k = 0;
  for (int i = 0; i < nports; i++) {
    if ((ports[i].flags & flags) != flags) continue;
    if (use && regexec(&re, ports[i].name, 0, NULL, 0)) continue;
    out[k++] = ports[i].name;
  }
  if (use) regfree(&re);
  if (!k) { free(out); return NULL; }
  return out;
}
void jack_free(void *p) { free(p); }
uint32_t jack_get_sample_rate(void *c) { (void)c; return (cycled || !pre_rate) ? rate : pre_rate; }
uint32_t jack_get_buffer_size(void *c) { (void)c; return NF; }
int jack_set_buffer_size(void *c, uint32_t n) { (void)c; (void)n; return 0; }
float jack_cpu_load(void *c) { (void)c; return 1.0f; }
void jack_set_error_function(void (*f)(const char *)) { (void)f; }
void jack_set_info_function(void (*f)(const char *)) { (void)f; }
int jack_deactivate(void *c) { (void)c; running = 0; if (th) pthread_join(th, NULL); th = 0; return 0; }

/* ---- the "server" --------------------------------------------------- */
static float ext_signal(const char *name, int cyc, int i) {
  if (tone_hz > 0 && !strncmp(name, "system:capture_", 15))
    return (float)(0.5 * sin(2.0 * M_PI * tone_hz * (double)(tone_pos + (unsigned)i) / (double)rate));
  if (tone_hz > 0 && tone_all) {
    unsigned h = 5381;
    for (const char *p = name; *p && *p != ':'; p++) h = h * 33u + (unsigned char)*p;
    double level = 0.02 + 0.12 * (double)(h % 97u) / 97.0;
    return (float)(level * sin(2.0 * M_PI * tone_hz * (double)(tone_pos + (unsigned)i) / (double)rate));
  }
  /* every port fires a distinct impulse at cycle 30, frame 77 */
  if (cyc != impulse_cycle || i != 77) return 0.0f;
  if (!strcmp(name, "system:capture_1")) return 0.5f;
  if (!strcmp(name, "system:capture_2")) return 0.25f;
  if (!strcmp(name, "Firefox:output_FL")) return 0.125f;
  if (!strcmp(name, "Firefox:output_FR")) return 0.0625f;
  return 0.0f;
}

static void *server(void *u) {
  (void)u; FILE *rep = fopen(getenv("MOCKJACK_REPORT"), "w");
  int hits = 0, cycles = 0;
  /* MOCKJACK_REALTIME: one cycle every 128/48000 s, like a sound card
     (for tests that exchange audio with real time, such as VBAN) */
  int realtime = getenv("MOCKJACK_REALTIME") != NULL;
  int switch_at = -1; uint32_t switch_rate = 0;
  if (getenv("MOCKJACK_RATE_SWITCH")) sscanf(getenv("MOCKJACK_RATE_SWITCH"), "%d:%u", &switch_at, &switch_rate);
  int no_cb = getenv("MOCKJACK_NO_SRATE_CB") != NULL;
  struct timespec t0; clock_gettime(CLOCK_MONOTONIC, &t0);
  long long due = 0; /* ns after t0 at which this cycle starts */
  for (int cyc = 0; running && cyc < max_cycles; cyc++) {
    if (cyc == switch_at && switch_rate) {
      rate = switch_rate;
      if (scb && !no_cb) scb(rate, sarg);
    }
    if (realtime) {
      long long ns = (long long)t0.tv_nsec + due;
      struct timespec at = { t0.tv_sec + ns / 1000000000LL, ns % 1000000000LL };
      while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &at, NULL) == EINTR) {}
      due += (long long)NF * 1000000000LL / rate;
    }
    pthread_mutex_lock(&mu);
    for (int i = 0; i < nports; i++) {
      if (ports[i].ours && (ports[i].flags & 1)) {          /* our inputs: sum of sources */
        memset(ports[i].buf, 0, sizeof ports[i].buf);
        for (int c = 0; c < nconns; c++) if (conns[c].dst == i)
          for (int f = 0; f < NF; f++) ports[i].buf[f] += ext_signal(ports[conns[c].src].name, cyc, f);
      }
      if (ports[i].ours && (ports[i].flags & 2)) for (int f = 0; f < NF; f++) ports[i].buf[f] = 999.0f; /* garbage */
    }
    pthread_mutex_unlock(&mu);
    if (!cycled) {
      cycled = 1;
      /* PipeWire tells the client the graph's real rate once it runs it */
      if (pre_rate && pre_rate != rate && scb && !no_cb) scb(rate, sarg);
    }
    pcb(NF, parg);
    tone_pos += NF;
    cycles++;
    if (cyc == 5 && xcb) xcb(xarg);
    if (cyc == 10 && rcb) rcb(0, 1, rarg);
    /* inspect what reached system:playback_1/2 and OBS:input_1 */
    pthread_mutex_lock(&mu);
    for (int c = 0; c < nconns; c++) {
      port_t *s = &ports[conns[c].src], *d = &ports[conns[c].dst];
      if (!s->ours) continue;
      for (int f = 0; f < NF; f++) {
        float v = s->buf[f];
        if (v == 999.0f) { fprintf(rep, "GARBAGE %s\n", s->name); break; }
        if (v != 0.0f && cyc > 3) { fprintf(rep, "SIGNAL cycle=%d frame=%d %s -> %s value=%.6f\n", cyc, f, s->name, d->name, v); hits++; }
      }
    }
    pthread_mutex_unlock(&mu);
    if (!realtime) usleep(2000);
  }
  for (int c = 0; c < nconns; c++) fprintf(rep, "CONN %s -> %s\n", ports[conns[c].src].name, ports[conns[c].dst].name);
  fprintf(rep, "CYCLES %d HITS %d\n", cycles, hits);
  fclose(rep);
  return NULL;
}

int jack_activate(void *c) {
  (void)c; running = 1;
  if (getenv("MOCKJACK_TONE")) tone_hz = atof(getenv("MOCKJACK_TONE"));
  tone_all = getenv("MOCKJACK_TONE_ALL") != NULL;
  if (getenv("MOCKJACK_IMPULSE_CYCLE")) impulse_cycle = atoi(getenv("MOCKJACK_IMPULSE_CYCLE"));
  if (getenv("MOCKJACK_CYCLES")) max_cycles = atoi(getenv("MOCKJACK_CYCLES"));
  usleep(300000); /* let BananaSplit auto-connect first */
  return pthread_create(&th, NULL, server, NULL);
}
