// Drives every control of the GUI in a real browser and checks, after each
// action, that BananaSplit's own state (GET /api/state) changed the way the
// control says. Optional: needs Node.js and Playwright (with Chromium).
//
//   node tests/gui/ui_check.js [path/to/bananasplit]
//
// It starts BananaSplit itself on the mock JACK server of the end-to-end
// test (build it first: tests/e2e/run_e2e.sh does), with a throw-away HOME,
// so nothing on this computer is touched.
"use strict";
const { chromium } = require("playwright");
const { spawn, execFileSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const ROOT = path.resolve(__dirname, "../..");
const BIN = process.argv[2] || path.join(ROOT, "target/release/bananasplit");
const MOCK = path.join(ROOT, "tests/e2e");
const PORT = 18950;
const URL = `http://127.0.0.1:${PORT}/`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0, checks = 0;
function check(ok, what) {
  checks++;
  if (ok) console.log("ok   " + what);
  else { failures++; console.log("FAIL " + what); }
}
async function state() { return (await fetch(URL + "api/state")).json(); }
async function devices() { return (await fetch(URL + "api/devices")).json(); }
async function untilDev(pred, what, ms = 4000) {
  const t0 = Date.now();
  let d;
  while (Date.now() - t0 < ms) {
    d = await devices();
    try { if (pred(d)) { check(true, what); return d; } } catch (e) { /* not yet */ }
    await sleep(80);
  }
  check(false, what);
  return d;
}
async function post(params) {
  const r = await fetch(URL + "api/cmd", { method: "POST", headers: { "X-BananaSplit": "1", "Content-Type": "application/x-www-form-urlencoded" }, body: new URLSearchParams(params).toString() });
  return { status: r.status, text: await r.text() };
}
async function until(pred, what, ms = 4000) {
  const t0 = Date.now();
  let s;
  while (Date.now() - t0 < ms) {
    s = await state();
    try { if (pred(s)) { check(true, what); return s; } } catch (e) { /* not yet */ }
    await sleep(60);
  }
  check(false, what);
  return s;
}

(async () => {
  if (!fs.existsSync(path.join(MOCK, "libjack.so.0"))) {
    execFileSync("gcc", ["-shared", "-fPIC", "-O2", "-o", path.join(MOCK, "libjack.so.0"), path.join(MOCK, "mockjack.c"), "-lpthread", "-lm"]);
  }
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "bs-ui-"));
  const rec = path.join(home, "rec");
  fs.mkdirSync(path.join(home, ".config/bananasplit"), { recursive: true });
  fs.writeFileSync(path.join(home, ".config/bananasplit/bananasplit.ini"),
    `[global]\ncreate_cables = false\n[recorder]\nfolder = ${rec}\nlinks = A1\n`);
  const env = Object.assign({}, process.env, {
    HOME: home, XDG_CONFIG_HOME: path.join(home, ".config"), XDG_DATA_HOME: path.join(home, ".local/share"),
    LD_LIBRARY_PATH: MOCK, MOCKJACK_REPORT: path.join(home, "report.txt"), MOCKJACK_REALTIME: "1", MOCKJACK_CYCLES: "100000000",
    // a webcam, a USB mic and its headphones, and a 440 Hz tone on system:capture
    MOCKJACK_DEVICES: "1", MOCKJACK_TONE: "440",
  });
  delete env.DISPLAY; delete env.WAYLAND_DISPLAY;
  const log = fs.openSync(path.join(home, "bananasplit.log"), "w");
  const proc = spawn(BIN, ["--gui", "--no-open", "--gui-port", String(PORT)], { env, stdio: ["ignore", log, log] });
  let exited = null;
  proc.on("exit", (c) => { exited = c; });
  process.on("exit", () => { if (exited === null) proc.kill(); });
  for (let i = 0; i < 100; i++) { try { await fetch(URL + "api/ping"); break; } catch (e) { await sleep(100); } }

  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1760, height: 960 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  await page.goto(URL);
  await page.waitForSelector(".strip");
  await sleep(600);
  const strip = (i) => page.locator(".strip").nth(i);

  // ---- strip buttons ----
  await strip(1).locator(".btn.mute").click();
  await until((s) => s.ch[1].mute, "MUTE mutes strip 2");
  await strip(1).locator(".btn.mute").click();
  await until((s) => !s.ch[1].mute, "MUTE again unmutes it");
  await strip(2).locator(".btn.solo").click();
  await until((s) => s.ch[2].solo && s.anySolo, "SOLO solos strip 3");
  check(await strip(0).evaluate((e) => e.classList.contains("dim")), "other strips dim while one is soloed");
  await strip(2).locator(".btn.solo").click();
  await until((s) => !s.anySolo, "SOLO again releases it");
  await strip(0).locator(".rt", { hasText: "B2" }).click();
  await until((s) => (s.ch[0].routes & 16) !== 0, "route chip B2 sends strip 1 to B2");
  await strip(0).locator(".rt", { hasText: "B2" }).click();
  await until((s) => (s.ch[0].routes & 16) === 0, "route chip B2 again stops it");
  await strip(1).locator(".pill", { hasText: "MONO" }).click();
  await until((s) => s.ch[1].mono, "MONO makes strip 2 mono");
  await strip(7).locator(".btn.mute").click();
  await until((s) => s.ch[7].mute, "bus A2 MUTE");
  await strip(7).locator(".btn.mute").click();

  // ---- keyboard: the selected strip ----
  await strip(4).locator(".btn.mute").click();
  await until((s) => s.ch[4].mute, "MUTE on strip 5");
  check(await strip(4).evaluate((e) => e.classList.contains("sel")), "clicking into a strip selects it");
  await page.keyboard.press("m");
  await until((s) => !s.ch[4].mute, "key M unmutes the selected strip");
  await page.keyboard.press("2");
  await until((s) => (s.ch[4].routes & 2) !== 0, "key 2 routes it to A2");
  await page.keyboard.press("2");
  await until((s) => (s.ch[4].routes & 2) === 0, "key 2 again stops it");
  await page.keyboard.press("o");
  await until((s) => s.ch[4].mono, "key O makes it mono");
  await page.keyboard.press("o");
  await until((s) => !s.ch[4].mono, "key O again: stereo");
  await page.keyboard.press("]");
  check(await strip(5).evaluate((e) => e.classList.contains("sel")), "key ] selects the next strip");

  // ---- fader ----
  const cap = strip(3).locator(".capf");
  const fb = await strip(3).locator(".fader").boundingBox();
  const cb = await cap.boundingBox();
  await page.mouse.move(cb.x + cb.width / 2, cb.y + cb.height / 2);
  await page.mouse.down();
  await page.mouse.move(cb.x + cb.width / 2, fb.y - 40, { steps: 8 });
  await page.mouse.up();
  await until((s) => s.ch[3].gain === 12, "dragging the fader to the top gives +12 dB");
  await strip(3).locator(".fader").dblclick();
  await until((s) => s.ch[3].gain === 0, "double-click on the fader: 0 dB");
  await strip(3).locator(".fval").click();
  await page.keyboard.press("Control+A");
  await page.keyboard.type("-12.5");
  await page.keyboard.press("Enter");
  await until((s) => s.ch[3].gain === -12.5, "typing -12.5 into the level box");
  check((await strip(3).locator(".fval").inputValue()) === "−12.5", "level box shows −12.5");
  await strip(3).locator(".fader").hover();
  await page.mouse.wheel(0, -100);
  await until((s) => s.ch[3].gain === -12, "wheel up on the fader: +0.5 dB");

  // ---- knobs ----
  const knob = strip(4).locator(".knob").first();
  const kb = await knob.boundingBox();
  await page.mouse.move(kb.x + kb.width / 2, kb.y + kb.height / 2);
  await page.mouse.down();
  await page.mouse.move(kb.x + kb.width / 2, kb.y + kb.height / 2 - 50, { steps: 6 });
  await page.mouse.up();
  await until((s) => s.ch[4].trim === 12, "dragging GAIN up 50 px: +12 dB input gain");
  check((await strip(4).locator(".kval").first().textContent()) === "+12 dB", "GAIN shows +12 dB");
  await knob.dblclick();
  await until((s) => s.ch[4].trim === 0, "double-click on GAIN: 0 dB");
  await knob.focus();
  await page.keyboard.press("ArrowUp");
  await until((s) => s.ch[4].trim === 0.5, "arrow up on GAIN: +0.5 dB");
  const pan = strip(4).locator(".knob").nth(1);
  const pb = await pan.boundingBox();
  await page.mouse.move(pb.x + pb.width / 2, pb.y + pb.height / 2);
  await page.mouse.down();
  await page.mouse.move(pb.x + pb.width / 2, pb.y + pb.height / 2 + 50, { steps: 6 });
  await page.mouse.up();
  await until((s) => s.ch[4].pan === -0.5, "dragging PAN down 50 px: L50");
  check((await strip(4).locator(".kval").nth(1).textContent()) === "L50", "PAN shows L50");
  await pan.dblclick();
  await until((s) => s.ch[4].pan === 0, "double-click on PAN: centre");
  const bal = strip(6).locator(".knob").nth(1);
  await bal.focus();
  await page.keyboard.press("ArrowRight");
  await until((s) => s.ch[6].pan === 0.05, "bus A1 BALANCE right");

  // ---- rename ----
  await strip(5).locator(".name").dblclick();
  await page.keyboard.press("Control+A");
  await page.keyboard.type("Guitar");
  await page.keyboard.press("Enter");
  await until((s) => s.ch[5].name === "Guitar", "double-click on a name renames the strip");

  // ---- EQ ----
  await strip(2).locator(".pill", { hasText: "EQ" }).click();
  check(await page.locator("#eqdlg").evaluate((d) => d.open), "EQ pill opens the editor");
  await page.locator("#eqon").click();
  await until((s) => s.ch[2].eq.on, "EQ on in the editor");
  const h = page.locator("#eqhandles .h").nth(2);
  const hb = await h.boundingBox();
  await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2);
  await page.mouse.down();
  await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2 - 40, { steps: 5 });
  await page.mouse.up();
  await until((s) => s.ch[2].eq.b[2][3] > 3, "dragging band 3 up boosts it");
  check(await strip(2).locator(".pill", { hasText: "EQ" }).getAttribute("aria-pressed") === "true", "EQ pill lit");
  await page.locator("#eqclose").click();

  // ---- VBAN ----
  await page.locator("#vbanbtn").click();
  await page.locator("#vbanon").click();
  await until((s) => s.vban.on, "VBAN switch on");
  await page.locator("#vbanon").click();
  await until((s) => !s.vban.on, "VBAN switch off");
  await page.locator("#vbanclose").click();

  // ---- recorder settings ----
  await page.locator(".arm").nth(1).click();
  await until((s) => s.ch[1].link, "arming track 2");
  await page.locator(".arm").nth(1).click();
  await until((s) => !s.ch[1].link, "disarming track 2");
  await page.locator("[data-format=wav16]").click();
  await until((s) => s.rec.format === "wav16", "format 16-bit");
  await page.locator("[data-mode=multi]").click();
  await until((s) => s.rec.mode === "multi", "mode multi");
  await page.locator("[data-mode=mix]").click();
  await until((s) => s.rec.mode === "mix", "mode mix");
  await page.locator("#playto .rt", { hasText: "B1" }).click();
  await until((s) => s.playTo === 9, "play to A1 + B1");
  await page.locator("#playto .rt", { hasText: "B1" }).click();
  await until((s) => s.playTo === 1, "play to A1 only");

  // ---- record, then play it back ----
  for (let i = 0; i < 6; i++) await strip(i).locator(".btn.mute").click();
  await until((s) => s.ch.slice(0, 6).every((c) => c.mute), "every strip muted (A1 carries only the player now)");
  // a recording to play: A1 is silent, so write a loud one by hand
  fs.mkdirSync(rec, { recursive: true });
  const n = 3 * 48000, data = Buffer.alloc(n * 4);
  for (let i = 0; i < n; i++) { const v = Math.round(16000 * Math.sin(i * 0.05)); data.writeInt16LE(v, 4 * i); data.writeInt16LE(v, 4 * i + 2); }
  const hdr = Buffer.alloc(44);
  hdr.write("RIFF", 0); hdr.writeUInt32LE(36 + data.length, 4); hdr.write("WAVEfmt ", 8); hdr.writeUInt32LE(16, 16);
  hdr.writeUInt16LE(1, 20); hdr.writeUInt16LE(2, 22); hdr.writeUInt32LE(48000, 24); hdr.writeUInt32LE(192000, 28);
  hdr.writeUInt16LE(4, 32); hdr.writeUInt16LE(16, 34); hdr.write("data", 36); hdr.writeUInt32LE(data.length, 40);
  fs.writeFileSync(path.join(rec, "tone.part"), Buffer.concat([hdr, data]));
  fs.renameSync(path.join(rec, "tone.part"), path.join(rec, "2026-01-01_10-00-00_mix.wav"));
  await page.locator("#recbtn").click();
  await until((s) => s.rec.on, "REC starts recording");
  check(await page.locator("#recbtn").evaluate((b) => b.classList.contains("on")), "REC button lit");
  await sleep(1200);
  await page.locator("#stopbtn").click();
  let s = await until((s) => !s.rec.on, "STOP ends the recording");
  s = await until((s) => s.takes.count === 2, "the new take shows up next to the old one", 8000);
  await page.locator("#takeprev").click();
  s = await until((s) => s.player.take === "2026-01-01_10-00-00", "‹ selects the earlier take");
  check((await page.locator("#taketitle").textContent()).includes("Take 01"), "waveform title shows TAKE 01");
  await page.locator("#playbtn").click();
  s = await until((s) => s.player.state === "playing", "PLAY plays it");
  await until((s) => s.ch[6].m[0] > -20, "the take reaches A1 (meter above -20 dB)");
  await page.locator("#playbtn").click();
  await until((s) => s.player.state === "paused", "PAUSE pauses it");
  const pausedAt = (await state()).player.pos;
  await until((s) => s.ch[6].m[0] < -50, "A1 goes quiet while paused", 5000);
  check(Math.abs((await state()).player.pos - pausedAt) < 0.01, "the position holds while paused");
  const wb = await page.locator("#wavec").boundingBox();
  await page.mouse.click(wb.x + wb.width * 0.75, wb.y + wb.height / 2);
  await until((s) => s.player.state === "playing" && s.player.pos >= 2.2, "clicking the waveform while paused plays from there");
  await page.mouse.click(wb.x + wb.width * 0.25, wb.y + wb.height / 2);
  await until((s) => s.player.state === "playing" && s.player.pos >= 0.7 && s.player.pos < 1.6, "clicking it while playing jumps there");
  await page.locator("#stopbtn").click();
  await until((s) => s.player.state === "stopped" && s.player.pos === 0, "STOP stops and rewinds");
  check(/^\d\d:\d\d:\d\d\.\d\d$/.test(await page.locator("#tc").textContent()), "time code shows hh:mm:ss.cc");

  // ---- scenes ----
  await page.locator("#scenebtn").click();
  await page.locator("#scenemenu .mi", { hasText: "Save the mix" }).click();
  await page.locator("#scenein").fill("Test Scene");
  await page.locator("#sceneform button[type=submit]").click();
  await until((s) => s.scene === "Test Scene" && s.scenes.includes("Test Scene"), "Save as new scene");
  check(fs.existsSync(path.join(home, ".config/bananasplit/scenes/Test Scene.ini")), "scene file written");
  await strip(3).locator(".fader").dblclick();
  await until((s) => s.ch[3].gain === 0, "a fader moved after saving");
  await page.locator("#scenebtn").click();
  await page.locator("#scenemenu [role=menuitemradio]", { hasText: "Test Scene" }).click();
  await until((s) => s.ch[3].gain === -12, "loading the scene puts the fader back");
  check((await page.locator("#scenename").textContent()) === "Test Scene", "header shows the scene");

  // ---- menu ----
  await page.locator("#menubtn").click();
  await page.locator("[data-theme-v=light]").click();
  await until((s) => s.app.theme === "light", "theme Light");
  check(await page.evaluate(() => document.documentElement.dataset.theme) === "light", "page is light");
  await page.locator("[data-theme-v=dark]").click();
  await until((s) => s.app.theme === "dark", "theme Dark");
  await page.locator("#zoomin").click();
  await until((s) => s.app.zoom === 1.1, "zoom +");
  await page.locator("#zoomval").click();
  await until((s) => s.app.zoom === 1, "zoom back to 100%");
  await page.locator("#m-tray").click();
  await until((s) => !s.app.closeToTray, "closing the window quits");
  await page.locator("#m-tray").click();
  await until((s) => s.app.closeToTray, "closing the window keeps it running");
  await page.locator("#m-autostart").click();
  await until((s) => s.app.autostart, "Start with the system");
  const auto = path.join(home, ".config/autostart/bananasplit.desktop");
  check(fs.existsSync(auto) && fs.readFileSync(auto, "utf8").includes("--gui --hidden"), "autostart entry written");
  await page.locator("#m-autostart").click();
  await until((s) => !s.app.autostart, "Start with the system off");
  check(!fs.existsSync(auto), "autostart entry removed");
  await page.locator("#m-install").click();
  await until((s) => s.app.installed, "Add BananaSplit to the app menu");
  const desk = path.join(home, ".local/share/applications/bananasplit.desktop");
  check(fs.existsSync(desk) && fs.existsSync(path.join(home, ".local/bin/bananasplit")), "menu entry and program installed");
  check(fs.existsSync(path.join(home, ".local/share/icons/hicolor/256x256/apps/bananasplit.png")), "icon installed");
  await page.locator("#menubtn").click();
  await page.locator("#mainmenu [data-op=save]").click();
  await sleep(300);
  const ini = fs.readFileSync(path.join(home, ".config/bananasplit/bananasplit.ini"), "utf8");
  check(/\[strip5\][^[]*trim_db = 0.5/.test(ini) && /\[strip6\][^[]*name = Guitar/.test(ini), "Save settings writes gain and names");


  // ---- 2.7: a channel's menu: what it takes in ----
  if ((await state()).ch[2].mute) await strip(2).locator(".btn.mute").click(); // (muted for the player test)
  await until((s) => !s.ch[2].mute, "strip 3 unmuted again");
  const menuItem = (label) => page.locator("#cmenu .ci").filter({ has: page.locator("b", { hasText: new RegExp("^" + label + "$") }) });
  await strip(2).locator(".devb").click();
  await page.waitForSelector("#cmenu .ci");
  check(await page.locator("#cmenu").isVisible(), "strip 3's menu button opens its menu");
  check((await page.locator("#cmenu h4").textContent()).includes("what comes in"), "a strip's menu lists what it can take in");
  check(await menuItem("Razer Kiyo Analog Stereo").count() === 1 && await menuItem("Yeti Stereo Microphone Analog Stereo").count() === 1,
    "it lists the input devices");
  check(await menuItem("Firefox").count() === 1, "and the JACK programs");
  check(await page.locator("#cmenu .ci").first().isDisabled(), "its virtual cable is greyed out while cables are off");
  check((await page.locator("#cmenu .ci").first().textContent()).includes("create_cables = false"), "and says why");
  await menuItem("system").click();
  await untilDev((d) => d.ch[2].conn.includes("system"), "ticking system connects strip 3 to it");
  await until((s) => s.ch[2].m[0] > -20, "its tone reaches strip 3's meter");
  await until((s) => s.ch[2].dev === "system", "strip 3's menu button says what it takes in");
  // (the menu is built again when the mixer's answer comes in: wait for it)
  let ticked = false;
  for (let i = 0; i < 40 && !ticked; i++) {
    ticked = await menuItem("system").getAttribute("aria-checked", { timeout: 500 }).catch(() => null) === "true";
    if (!ticked) await sleep(100);
  }
  check(ticked, "the menu shows system ticked");
  await page.keyboard.press("Escape");
  check(!(await page.locator("#cmenu").isVisible()), "Escape closes the menu");
  // ---- 2.7: a muted channel shows what comes in, greyed ----
  await strip(2).locator(".btn.mute").click();
  await until((s) => s.ch[2].mute && s.ch[2].mi[0] > -20 && s.ch[2].m[0] < -50, "muted, strip 3 still reports its input level");
  await sleep(250);
  let segs = await strip(2).locator(".meter .seg").evaluateAll((xs) => xs.map((x) => x.className));
  check(segs.some((c) => / x$/.test(c)) && !segs.some((c) => / [gar]$/.test(c)), "a muted strip's meter shows its input, greyed");
  check(await page.locator(".qbar").nth(2).evaluate((e) => e.classList.contains("muted")), "so does its quick-mix bar");
  await strip(2).locator(".btn.mute").click();
  await until((s) => !s.ch[2].mute && s.ch[2].m[0] > -20, "unmuted, it sends again");
  await sleep(250);
  segs = await strip(2).locator(".meter .seg").evaluateAll((xs) => xs.map((x) => x.className));
  check(segs.some((c) => / g$/.test(c)) && !segs.some((c) => / x$/.test(c)), "and its meter is in colour again");
  // untick, the default input, a config line, disconnect all
  await strip(2).locator(".devb").click();
  await page.waitForSelector("#cmenu .ci");
  await menuItem("system").click();
  await untilDev((d) => !d.ch[2].conn.includes("system"), "unticking system disconnects it");
  await menuItem("Razer Kiyo Analog Stereo").click();
  await untilDev((d) => d.ch[2].conn.includes("Razer Kiyo Analog Stereo") && d.ch[2].connect.includes("^Razer Kiyo Analog Stereo:"), "ticking a device connects it and keeps it in the config");
  await menuItem("Default input").click();
  await untilDev((d) => d.ch[2].connect.includes("@default"), "Default input adds connect = @default");
  await page.locator("#cmenu .cfoot button").click();
  await untilDev((d) => d.ch[2].conn.length === 0 && d.ch[2].connect.length === 0, "Disconnect all leaves strip 3 with nothing");
  await page.keyboard.press("Escape");
  // a bus's menu: where it plays
  await strip(7).locator(".devb").click();
  await page.waitForSelector("#cmenu .ci");
  check((await page.locator("#cmenu h4").textContent()).includes("where it plays"), "a bus's menu lists where it can play");
  await menuItem("system").click();
  await untilDev((d) => d.ch[7].conn.includes("system"), "ticking system plays A2 there");
  await menuItem("OBS").click();
  await untilDev((d) => d.ch[7].conn.includes("OBS"), "a bus can feed a JACK program too");
  await page.locator("#cmenu .cfoot button").click();
  await untilDev((d) => d.ch[7].conn.length === 0, "Disconnect all on A2");
  await page.mouse.click(5, 5);
  check(!(await page.locator("#cmenu").isVisible()), "clicking elsewhere closes the menu");
  const bad = await post({ op: "dev", ch: 0, client: "No Such Device", v: 1 });
  check(bad.status === 400 && bad.text.includes("gone"), "connecting something that is not there is refused");

  // ---- 2.7: a bus feeds another bus ----
  await strip(9).locator(".rt", { hasText: "A1" }).click();
  await until((s) => (s.ch[9].routes & 1) !== 0, "B1's TO A1 key sends B1 into A1");
  await sleep(150);
  check(await strip(6).locator(".rt", { hasText: "B1" }).evaluate((e) => e.classList.contains("loop")), "A1's B1 key is blocked (it would be a loop)");
  await strip(6).locator(".rt", { hasText: "B1" }).click();
  await sleep(400);
  check(((await state()).ch[6].routes & 8) === 0, "clicking it does not make the loop");
  const loop = await post({ op: "route", ch: 6, bus: 3, v: 1 });
  check(loop.status === 400 && loop.text.includes("loop"), "the server refuses a loop too");
  const self = await post({ op: "route", ch: 7, bus: 1, v: 1 });
  check(self.status === 400, "and a bus feeding itself");
  check(await strip(6).locator(".rt.self").count() === 1 && await strip(6).locator(".rt.self").isDisabled(), "a bus has no key for itself");
  await strip(9).locator(".rt", { hasText: "A1" }).click();
  await until((s) => (s.ch[9].routes & 1) === 0, "B1's A1 key again stops it");
  // ---- 2.7: every output in the bottom bar ----
  check(await page.locator("#outs .orow").count() === 5, "the bottom bar shows all five outputs");
  check((await page.locator("#outs .orow").nth(4).textContent()).startsWith("B2"), "B2 last");
  // ---- 2.7: VBAN shows this computer's addresses ----
  await page.locator("#vbanbtn").click();
  const want = Object.values(os.networkInterfaces()).flat().filter((a) => a && a.family === "IPv4" && !a.internal).map((a) => a.address);
  const shown = await page.locator("#vbanips .ip b").allTextContents();
  check(want.every((ip) => shown.includes(ip)) && shown.length === new Set(want).size, "the VBAN window shows this computer's IP addresses (" + (shown.join(", ") || "none") + ")");
  check((await page.locator("#vbanhint").textContent()).includes("UDP port"), "and how to send to them");
  await page.locator("#vbanclose").click();
  // ---- 2.7: the console fills the window, whatever its size ----
  for (const [w, h] of [[1280, 720], [1366, 768], [1440, 900], [1920, 1080], [2560, 1440], [3440, 1440], [1024, 640], [1280, 1024]]) {
    await page.setViewportSize({ width: w, height: h });
    await sleep(250);
    const m = await page.evaluate(() => {
      const st = [...document.querySelectorAll(".strip")].map((e) => e.getBoundingClientRect());
      const b = document.querySelector(".bottom").getBoundingClientRect(), t = document.querySelector(".top").getBoundingClientRect();
      return { tops: st.map((x) => x.top), widths: st.map((x) => x.width), bottom: b.bottom, right: b.right, left: t.left,
               sw: document.documentElement.scrollWidth, sh: document.documentElement.scrollHeight };
    });
    const one = Math.max(...m.tops) - Math.min(...m.tops) < 1, same = Math.max(...m.widths) - Math.min(...m.widths) < 1.5;
    const fills = Math.abs(m.bottom - h) < 0.05 * h && Math.abs(m.right - (w - m.left)) < 0.03 * w;
    check(one && same && fills && m.sw <= w && m.sh <= h, `${w}x${h}: one row of equal strips, filling the window, no scrolling`);
  }
  await page.setViewportSize({ width: 800, height: 560 });
  await sleep(250);
  check(await page.evaluate(() => document.body.classList.contains("scroll") && document.documentElement.scrollWidth > innerWidth),
    "smaller than the smallest layout: the window scrolls instead of squeezing it");
  await page.setViewportSize({ width: 1760, height: 960 });
  await sleep(250);
  // a long name gets a smaller font instead of being cut; a short one keeps its size
  const nameFit = (i) => strip(i).locator(".name").evaluate((e) => {
    const box = e.getBoundingClientRect().width;
    e.style.width = "max-content";
    const full = e.getBoundingClientRect().width;
    e.style.width = "";
    return { box, full, px: parseFloat(getComputedStyle(e).fontSize), text: e.textContent };
  });
  const longName = await nameFit(7), shortName = await nameFit(0);
  check(longName.full <= longName.box + 0.01 && longName.px < 12.5 && shortName.px === 12.5,
    `a long name fits its strip in a smaller font (${longName.text}: ${longName.px}px, ${shortName.text}: ${shortName.px}px)`);

  // ---- keyboard ----
  await strip(0).locator(".sh").click();
  await page.keyboard.press("m");
  await until((s) => !s.ch[0].mute, "key M toggles mute of the selected strip");

  check(errors.length === 0, "no errors in the page" + (errors.length ? ": " + errors.join(" | ") : ""));

  // ---- quit ----
  await page.locator("#menubtn").click();
  await page.locator("#m-quit").click();
  await page.locator("#m-quit").click();
  for (let i = 0; i < 50 && exited === null; i++) await sleep(100);
  check(exited === 0, "Quit (clicked twice) stops BananaSplit cleanly");
  await browser.close();
  if (exited === null) proc.kill();
  if (failures) console.log("BananaSplit's output:\n" + fs.readFileSync(path.join(home, "bananasplit.log"), "utf8"));
  fs.rmSync(home, { recursive: true, force: true });
  console.log(`\n${checks - failures} of ${checks} checks passed`);
  process.exit(failures ? 1 : 0);
})().catch((e) => { console.error(e); process.exit(2); });
