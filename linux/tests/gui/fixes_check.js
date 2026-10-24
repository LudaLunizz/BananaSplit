// Checks of the 2.7.1 fixes that show in the page, in a real browser:
//
// * a long status (every connection that failed) keeps to three lines, so
//   the bottom bar never grows into the strips; all of it is the tooltip;
// * a channel button's tooltip says what is connected now, also right after
//   changes in its menu;
// * a multi-track take that continues in new files (the server changed its
//   sample rate) still shows its number of tracks and the file being written.
//
//   node tests/gui/fixes_check.js [path/to/bananasplit]
//
// Like ui_check.js: needs Node.js and Playwright (with Chromium), starts
// BananaSplit itself on the mock JACK server with a throw-away HOME.
"use strict";
const { chromium } = require("playwright");
const { spawn, execFileSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const ROOT = path.resolve(__dirname, "../..");
const BIN = process.argv[2] || path.join(ROOT, "target/release/bananasplit");
const MOCK = path.join(ROOT, "tests/e2e");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
function check(ok, what) {
  console.log((ok ? "ok   " : "FAIL ") + what);
  if (!ok) failures++;
}

// BananaSplit --gui with this config (and environment) on a port of its own.
async function start(ini, extraEnv, port) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "bs-fix-ui-"));
  fs.mkdirSync(path.join(home, ".config/bananasplit"), { recursive: true });
  fs.writeFileSync(path.join(home, ".config/bananasplit/bananasplit.ini"), ini.replace(/\$HOME/g, home));
  const env = Object.assign({}, process.env, {
    HOME: home, XDG_CONFIG_HOME: path.join(home, ".config"), XDG_DATA_HOME: path.join(home, ".local/share"),
    LD_LIBRARY_PATH: MOCK, MOCKJACK_REPORT: path.join(home, "report.txt"), MOCKJACK_REALTIME: "1", MOCKJACK_CYCLES: "100000000",
  }, extraEnv);
  delete env.DISPLAY; delete env.WAYLAND_DISPLAY;
  const log = fs.openSync(path.join(home, "bananasplit.log"), "w");
  const proc = spawn(BIN, ["--gui", "--no-open", "--gui-port", String(port)], { env, stdio: ["ignore", log, log] });
  const url = `http://127.0.0.1:${port}/`;
  for (let i = 0; i < 100; i++) { try { await fetch(url + "api/ping"); break; } catch (e) { await sleep(100); } }
  const post = async (params) => {
    const r = await fetch(url + "api/cmd", { method: "POST", headers: { "X-BananaSplit": "1", "Content-Type": "application/x-www-form-urlencoded" }, body: new URLSearchParams(params).toString() });
    return { status: r.status, text: await r.text() };
  };
  const stop = async () => {
    try { await post({ op: "quit" }); } catch (e) { /* gone */ }
    for (let i = 0; i < 50 && proc.exitCode === null; i++) await sleep(100);
    if (proc.exitCode === null) proc.kill();
  };
  return { home, url, post, stop };
}

async function pollPage(page, fn, pred, what, ms = 5000) {
  const t0 = Date.now();
  let v;
  while (Date.now() - t0 < ms) {
    v = await page.evaluate(fn);
    if (pred(v)) { check(true, what); return v; }
    await sleep(100);
  }
  check(false, what + " (last: " + JSON.stringify(v) + ")");
  return v;
}

(async () => {
  if (!fs.existsSync(path.join(MOCK, "libjack.so.0"))) {
    execFileSync("gcc", ["-shared", "-fPIC", "-O2", "-o", path.join(MOCK, "libjack.so.0"), path.join(MOCK, "mockjack.c"), "-lpthread", "-lm"]);
  }
  const browser = await chromium.launch();

  // ---- a long status --------------------------------------------------------
  // every channel's device is unplugged (a laptop away from its dock)
  const devs = ["Scarlett 2i2 USB Analog Surround 4.0", "HD Pro Webcam C920 Analog Stereo", "Blue Yeti Stereo Microphone Analog Stereo",
    "Jabra Link 380 Mono", "Elgato Wave:3 Analog Stereo", "Logitech G Pro X Gaming Headset Analog Stereo"];
  let ini = "[global]\ncreate_cables = false\n";
  devs.forEach((d, i) => { ini += `[strip${i + 1}]\nconnect = ^${d}:\n`; });
  ["A1", "A2", "A3", "B1", "B2"].forEach((b, i) => { ini += `[bus.${b}]\nconnect = ^${devs[i]}:\n`; });
  for (const [w, h, zoom] of [[1366, 768, 1], [1366, 768, 1.2], [1280, 720, 1.2], [1760, 960, 1]]) {
    const bs = await start(ini + `[gui]\nzoom = ${zoom}\n`, {}, 18700 + Math.floor(Math.random() * 100));
    const page = await browser.newPage({ viewport: { width: w, height: h } });
    await page.goto(bs.url);
    await page.waitForSelector(".strip");
    const m = await pollPage(page, () => {
      const r = (e) => e.getBoundingClientRect();
      const st = document.getElementById("status");
      const lh = parseFloat(getComputedStyle(st).lineHeight) * (r(st).height / st.offsetHeight || 1);
      return {
        len: st.textContent.length, title: st.title === st.textContent, lines: r(st).height / lh,
        foot: Math.max(...[...document.querySelectorAll(".strip .foot")].map((f) => r(f).bottom)),
        bar: r(document.querySelector(".bottom")).top, barBottom: r(document.querySelector(".bottom")).bottom, inner: innerHeight,
      };
    }, (v) => v.len > 900, `${w}x${h} at ${zoom * 100}%: eleven connections fail (a long status)`);
    check(m.lines <= 3.05, `  the status keeps to three lines (${m.lines.toFixed(2)})`);
    check(m.title, "  and all of it is its tooltip");
    check(m.foot <= m.bar, `  the strips end above the bottom bar (${m.foot.toFixed(0)} <= ${m.bar.toFixed(0)})`);
    check(m.barBottom <= m.inner + 1, "  the bottom bar is inside the window");
    await page.close();
    await bs.stop();
  }

  // ---- a channel button's tooltip after changes in its menu -----------------
  {
    const bs = await start("[global]\ncreate_cables = false\n[strip3]\nconnect = ^system:\n", { MOCKJACK_DEVICES: "1" }, 18810);
    const page = await browser.newPage({ viewport: { width: 1760, height: 960 } });
    await page.goto(bs.url);
    await page.waitForSelector(".strip");
    const tip = () => document.querySelectorAll(".devb")[2].title;
    await pollPage(page, tip, (t) => t.includes("Connected: system"), "strip 3's button: Connected: system");
    await bs.post({ op: "dev", ch: 2, client: "system", v: 0 });
    await bs.post({ op: "dev", ch: 2, client: "Razer Kiyo Analog Stereo", v: 1 });
    const t = await pollPage(page, tip, (t) => t.includes("Connected: Razer Kiyo Analog Stereo"), "after changes in its menu: Connected: Razer Kiyo", 3000);
    check(!t.includes("system"), "  and no longer system");
    await page.close();
    await bs.stop();
  }

  // ---- a multi-track take across a change of sample rate ---------------------
  {
    const bs = await start("[global]\ncreate_cables = false\n[strip1]\nroutes = A1\n[recorder]\nfolder = $HOME/rec\nlinks = 1,A1\nmode = multi\n",
      { MOCKJACK_TONE: "1000", MOCKJACK_RATE: "48000", MOCKJACK_RATE_SWITCH: "900:44100" }, 18820);
    const page = await browser.newPage({ viewport: { width: 1760, height: 960 } });
    await page.goto(bs.url);
    await page.waitForSelector(".strip");
    await page.locator("#recbtn").click();
    await pollPage(page, () => document.getElementById("taketitle").textContent, (t) => t === "REC · 2 tracks", "recording: REC · 2 tracks");
    const v = await pollPage(page, () => [document.getElementById("taketitle").textContent, document.getElementById("takeinfo").textContent],
      (v) => v[1].includes("-2_"), "the server changes its rate: the take continues in new files", 8000);
    check(v[0] === "REC · 2 tracks", `  still REC · 2 tracks (${v[0]})`);
    await page.locator("#stopbtn").click();
    await page.close();
    await bs.stop();
  }

  await browser.close();
  console.log(failures ? `GUI FIXES FAIL: ${failures}` : "GUI FIXES PASS");
  process.exit(failures ? 1 : 0);
})().catch((e) => { console.error(e); process.exit(2); });
