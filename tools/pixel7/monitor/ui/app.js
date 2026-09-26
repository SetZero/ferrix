// The window: the phone's state, a boot's stages and console, the runs, and
// live charts of the phone's stats. Everything arrives from the backend as
// the `sample` event once a second, and the guest's console as `vm-line`.

"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const KEEP = 900; // samples kept: fifteen minutes
const FOLLOW_LINES = 10;

const samples = [];
let lastSample = null;
let lastAndroid = null;
let vmRunning = false;
let vmPaused = false;
let vmStarted = 0;
// The helper's last run as first seen, so that one is not loaded as if it had
// just happened: `undefined` until the helper has answered once.
let lastHelperRun = undefined;
let selectedRun = null;

// ---------------------------------------------------------- Ferrix's stats

// What `ferrix-statd` prints inside Ferrix: one `FERRIX-STAT {json}` line a
// sample, `t` being Ferrix's uptime. Kept per run: a new START begins again.
const ferrix = { start: null, samples: [], end: null };

function isStatLine(line) {
  return line.startsWith("FERRIX-STAT");
}

function takeStatLine(line, live) {
  const space = line.indexOf(" ");
  if (space < 0) return;
  let value;
  try {
    value = JSON.parse(line.slice(space + 1));
  } catch {
    return;
  }
  const tag = line.slice(0, space);
  if (tag === "FERRIX-STAT-START") {
    ferrix.start = value;
    ferrix.samples = [];
    ferrix.end = null;
    if (live) showTab("ferrix");
  } else if (tag === "FERRIX-STAT") {
    ferrix.samples.push(value);
    if (ferrix.samples.length > 20000) ferrix.samples.shift();
  } else if (tag === "FERRIX-STAT-END") {
    ferrix.end = value;
  }
  $("ferrix-badge").textContent = ferrix.samples.length ? `● ${ferrix.samples.length}` : "";
}

const ferrixCharts = {
  cpu: new LineChart($("chart-f-cpu"), { max: 100, unit: "%" }),
  mem: new LineChart($("chart-f-mem"), { unit: " MiB" }),
  rate: new LineChart($("chart-f-rate"), { unit: "" }),
  tasks: new LineChart($("chart-f-tasks"), { unit: "" }),
};

function ferrixSeries(pick, label, color) {
  const points = [];
  for (const s of ferrix.samples) {
    const v = pick(s);
    if (v !== null && v !== undefined && !Number.isNaN(v)) points.push([s.t, v]);
  }
  return { label, color, points };
}

function drawFerrix() {
  const samples = ferrix.samples;
  if (!samples.length) return;
  const first = samples[0].t;
  const last = samples[samples.length - 1].t;
  // The whole run, however long, and a minute at least.
  const span = Math.max(60, last - first + 1);
  for (const chart of Object.values(ferrixCharts)) chart.span = span;
  const cores = samples[samples.length - 1].cpu.length;
  ferrixCharts.cpu.draw([
    ferrixSeries((s) => s.all, "all", COLORS[0]),
    ...Array.from({ length: cores }, (_, i) =>
      ferrixSeries((s) => s.cpu[i], `cpu${i}`, COLORS[(i + 1) % COLORS.length])),
  ], last);
  ferrixCharts.mem.draw([
    ferrixSeries((s) => (s.mem.total - s.mem.available) / 1024, "used", COLORS[0]),
    ferrixSeries((s) => s.mem.cached / 1024, "cached", COLORS[1]),
  ], last);
  ferrixCharts.rate.draw([
    ferrixSeries((s) => s.rate.irq, "interrupts", COLORS[3]),
    ferrixSeries((s) => s.rate.ctxt, "switches", COLORS[5]),
  ], last);
  ferrixCharts.tasks.draw([
    ferrixSeries((s) => s.tasks.threads, "threads", COLORS[0]),
    ferrixSeries((s) => s.tasks.processes, "processes", COLORS[2]),
    ferrixSeries((s) => s.tasks.running, "running", COLORS[4]),
  ], last);

  const now = samples[samples.length - 1];
  $("f-cpu-now").textContent = `${now.all.toFixed(1)}% of ${cores} · at ${now.t.toFixed(1)} s`;
  $("f-mem-now").textContent = `${((now.mem.total - now.mem.available) / 1024).toFixed(0)} / ${(now.mem.total / 1024).toFixed(0)} MiB`;
  $("f-rate-now").textContent = `${now.rate.irq.toFixed(0)} irq · ${now.rate.ctxt.toFixed(0)} ctxt`;
  $("f-tasks-now").textContent = `${now.tasks.processes} processes · ${now.tasks.threads} threads`;
  $("f-top-when").textContent = ferrix.end ? `ended after ${ferrix.end.seconds} s` : `at ${now.t.toFixed(1)} s`;
  const rows = [`<div class="row head"><span>pid</span><span>command</span><span>CPU</span><span>RSS</span></div>`];
  for (const task of now.top) {
    rows.push(`<div class="row"><span>${task.pid}</span><span></span><span>${task.cpu.toFixed(1)}%</span><span>${(task.rss_kib / 1024).toFixed(1)} MiB</span></div>`);
  }
  $("f-top").innerHTML = rows.join("");
  now.top.forEach((task, i) => {
    $("f-top").children[i + 1].children[1].textContent = `${task.comm}${task.threads > 1 ? ` (${task.threads})` : ""}`;
  });
}

function showTab(name) {
  document.querySelectorAll(".tab").forEach((tab) => tab.classList.toggle("selected", tab.dataset.tab === name));
  $("tab-phone").classList.toggle("hidden", name !== "phone");
  $("tab-ferrix").classList.toggle("hidden", name !== "ferrix");
  drawCharts();
  drawFerrix();
}

document.querySelectorAll(".tab").forEach((tab) => (tab.onclick = () => showTab(tab.dataset.tab)));

// ------------------------------------------------------------------ charts

const charts = {
  cpu: new LineChart($("chart-cpu"), { max: 100, unit: "%" }),
  freq: new LineChart($("chart-freq"), { unit: " MHz" }),
  mem: new LineChart($("chart-mem"), { unit: " GiB", decimals: 1 }),
  temp: new LineChart($("chart-temp"), { min: 20, unit: "°", decimals: 1 }),
  gpu: new LineChart($("chart-gpu"), { unit: " MHz" }),
  bat: new LineChart($("chart-bat"), { min: null, unit: " mA" }),
  vm: new LineChart($("chart-vm"), { unit: "%" }),
};

const COLORS = ["#cfbcff", "#64b5f6", "#3ddc84", "#ffb74d", "#ff6b6b", "#4dd0e1", "#f48fb1", "#aed581"];
const CLUSTER = { 0: "LITTLE", 4: "MID", 6: "BIG" };
const THERMAL = ["BIG", "MID", "LITTLE", "G3D", "TPU", "battery", "neutral_therm", "disp_therm"];

// Ferrix's own samples, placed in the PC's time, for the stretch of a native
// boot when the phone was in Ferrix and Android had nothing to say: what
// `fillFromFerrix` makes of a run's record once it is back.
let ferrixFill = [];

// The spans the phone was away from Android, from the states the samples
// carry: each shaded and named behind the charts, so a gap reads as what it
// was rather than as a line drawn across it.
const BANDS = {
  fastboot: ["fastboot", "#ffb74d"],
  ferrix: ["in Ferrix", "#64b5f6"],
  booting: ["Android booting", "#ffd54f"],
  absent: ["not connected", "#8a8898"],
  offline: ["adb offline", "#ff6b6b"],
  unauthorized: ["adb unauthorized", "#ff6b6b"],
};

function bands() {
  const out = [];
  for (const s of samples) {
    const kind = BANDS[s.state];
    const last = out[out.length - 1];
    if (!kind) continue;
    if (last && last.state === s.state && s.t - last.to <= 3) {
      last.to = s.t + 1;
    } else {
      out.push({ state: s.state, from: s.t, to: s.t + 1, label: kind[0], color: kind[1] });
    }
  }
  for (const band of out) {
    if (band.state === "ferrix" && ferrixFill.some((f) => f.t >= band.from && f.t <= band.to)) {
      band.label = "in Ferrix · its own stats";
    }
  }
  return out;
}

// Place the loaded record's `ferrix-statd` samples in the last stretch the
// phone was in Ferrix. The loader logs the counter as it starts, which is as
// the phone leaves fastboot, and Ferrix's `t` is that counter: so a sample's
// time is the stretch's start plus how far the counter got past the loader's.
function fillFromFerrix(leftFastboot = null) {
  let away = bands().filter((b) => b.state === "ferrix").pop();
  // The helper saw the phone leave fastboot, which beats the monitor's own
  // view when the monitor started while the phone was already away.
  if (leftFastboot) away = { from: leftFastboot, to: away ? Math.max(away.to, leftFastboot + 600) : leftFastboot + 600 };
  const anchor = consoleLines
    .map((line) => /counter at \d+ Hz, (\d+\.\d+) s since it started/.exec(line))
    .find(Boolean);
  if (!away || !anchor || !ferrix.samples.length) return;
  const loaderAt = Number(anchor[1]);
  const clusters = lastAndroid ? lastAndroid.clusters.map((c) => ({ first_cpu: c.first_cpu, mhz: null, max_mhz: null })) : [];
  const placed = ferrix.samples.map((s) => ({
    t: away.from + (s.t - loaderAt),
    state: "ferrix",
    fromFerrix: true,
    cores: s.cpu,
    total: s.all,
    clusters,
    memory: {
      total: s.mem.total / 1024,
      available: s.mem.available / 1024,
      cached: s.mem.cached / 1024,
      swap_total: 0,
      swap_free: 0,
    },
    thermal: [],
    vms: [],
  }));
  // A record that does not fit the stretch is some other run's.
  if (placed[0].t < away.from - 5 || placed[placed.length - 1].t > away.to + 60) return;
  ferrixFill = placed;
  drawCharts();
}

function allSamples() {
  return ferrixFill.length ? [...samples, ...ferrixFill].sort((a, b) => a.t - b.t) : samples;
}

function seriesOf(pick, label, color) {
  const points = [];
  for (const s of allSamples()) {
    const v = pick(s);
    if (v !== null && v !== undefined && !Number.isNaN(v)) points.push([s.t, v]);
  }
  return { label, color, points };
}

function drawCharts() {
  const now = lastSample ? lastSample.t : Date.now() / 1000;
  const away = bands();
  const clusters = (s, first) => {
    const members = (s.cores || []).filter((_, i) => clusterOf(s, i) === first);
    return members.length ? members.reduce((a, b) => a + b, 0) / members.length : null;
  };
  charts.cpu.draw([
    seriesOf((s) => s.total, "all", COLORS[0]),
    seriesOf((s) => clusters(s, 6), "BIG", COLORS[4]),
    seriesOf((s) => clusters(s, 4), "MID", COLORS[3]),
    seriesOf((s) => clusters(s, 0), "LITTLE", COLORS[1]),
  ], now, away);
  charts.freq.draw([0, 4, 6].map((first, i) =>
    seriesOf((s) => (s.clusters.find((c) => c.first_cpu === first) || {}).mhz, CLUSTER[first], [COLORS[1], COLORS[3], COLORS[4]][i])), now, away);
  charts.mem.draw([
    seriesOf((s) => s.memory && (s.memory.total - s.memory.available) / 1024, "used", COLORS[0]),
    seriesOf((s) => s.memory && s.memory.cached / 1024, "cached", COLORS[1]),
    seriesOf((s) => s.memory && (s.memory.swap_total - s.memory.swap_free) / 1024, "swap", COLORS[3]),
  ], now, away);
  const zones = THERMAL.filter((name) => lastSample && lastSample.thermal.some(([z]) => z === name)).slice(0, 6);
  charts.temp.draw(zones.map((name, i) =>
    seriesOf((s) => { const z = s.thermal.find(([n]) => n === name); return z ? z[1] : null; }, name.replace("_therm", ""), COLORS[i])), now, away);
  charts.gpu.draw([
    seriesOf((s) => s.gpu && s.gpu[0], "GPU", COLORS[2]),
    seriesOf((s) => s.mif_mhz, "memory bus", COLORS[5]),
  ], now, away);
  charts.bat.draw([
    seriesOf((s) => s.battery && s.battery.current_ma !== null ? -s.battery.current_ma : null, "draw", COLORS[3]),
  ], now, away);
  charts.vm.draw([
    seriesOf((s) => s.vms && s.vms.length ? s.vms.reduce((a, v) => a + v.cpu, 0) : null, "CPU", COLORS[0]),
  ], now, away);
}

function clusterOf(s, core) {
  let first = 0;
  for (const c of s.clusters || []) if (c.first_cpu <= core && c.first_cpu >= first) first = c.first_cpu;
  return first;
}

// --------------------------------------------------------------- the sample

const STATES = {
  android: ["ok", "Android"],
  booting: ["warn", "Android booting"],
  fastboot: ["warn", "fastboot"],
  ferrix: ["info", "running Ferrix"],
  unauthorized: ["bad", "adb unauthorized"],
  offline: ["bad", "adb offline"],
  recovery: ["warn", "recovery"],
  absent: ["bad", "not connected"],
};
const TIMELINE_COLORS = { android: "#3ddc84", booting: "#ffd54f", fastboot: "#ffb74d", ferrix: "#64b5f6", absent: "#3a3a48" };

function pill(id, kind, text) {
  const el = $(id);
  el.className = "pill " + kind;
  el.querySelector("b").textContent = text;
}

function onSample(s) {
  samples.push(s);
  if (samples.length > KEEP) samples.shift();
  lastSample = s;
  if (s.state === "android" && s.clusters.length) lastAndroid = s;
  if (ferrixFill.length && ferrixFill[ferrixFill.length - 1].t < samples[0].t) ferrixFill = [];

  const [kind, text] = STATES[s.state] || ["bad", s.state];
  pill("state", kind, vmRunning ? `${text} · VM ${vmPaused ? "paused" : "running"}` : text);
  pill("root", "quiet", s.state === "android" ? (s.root ? "root shell" : "shell, no root") : "no shell");

  const helper = s.helper;
  if (helper) {
    const phase = helper.phase === "idle" ? "helper ready" : `helper: ${helper.phase}`;
    pill("helper", helper.phase === "idle" ? "ok" : "info", phase);
  } else {
    pill("helper", "bad", "helper not running");
  }
  $("boot").disabled = s.state !== "android" || (helper && helper.phase !== "idle");
  $("vm-run").disabled = s.state !== "android" || vmRunning;

  // The helper's native boot: its phase while it runs, and its record once
  // Android is back.
  if (helper && helper.phase !== "idle") {
    const away = helper.phase.startsWith("Ferrix")
      ? " The phone is off USB while Ferrix runs, so the stages and Ferrix's stats arrive with its record when Android is back."
      : "";
    showResult("busy", `Native boot: ${helper.phase}…${away}`);
    if (!consoleLines.length) document.querySelectorAll(".stage").forEach((el) => el.classList.add("pending"));
    setSource(`native boot of ${shortImage(helper.image)}`);
  }
  const last = helper && helper.last && helper.last.when ? helper.last : null;
  if (helper && lastHelperRun === undefined) {
    lastHelperRun = last ? last.when : null;
  } else if (last && last.when !== lastHelperRun) {
    lastHelperRun = last.when;
    if (last.record) loadRun(last.record, `native boot ${last.when}`, last.left_fastboot ? Number(last.left_fastboot) : true);
    refreshRuns();
  }

  // The header's summary.
  const bits = [];
  if (s.memory) bits.push(`${(s.memory.total / 1024).toFixed(1)} GiB`);
  if (s.load) bits.push(`load ${s.load[0].toFixed(2)}`);
  if (s.battery && s.battery.level !== null) bits.push(`battery ${s.battery.level}%`);
  $("uptime").textContent = bits.length ? bits.join(" · ") : text;

  // Now-values and the core bars.
  $("cpu-now").textContent = s.total !== null && s.total !== undefined ? `${s.total.toFixed(0)}%` : "—";
  const cores = $("cores");
  if (cores.children.length !== (s.cores || []).length) {
    cores.innerHTML = (s.cores || []).map((_, i) =>
      `<div class="core"><div class="bar"><i></i></div>cpu${i} <span></span></div>`).join("");
  }
  (s.cores || []).forEach((load, i) => {
    const core = cores.children[i];
    core.querySelector("i").style.height = `${Math.min(100, load)}%`;
    core.querySelector("span").textContent = `${load.toFixed(0)}%`;
  });
  $("freq-now").textContent = s.clusters.length ? s.clusters.map((c) => `${(c.mhz / 1000).toFixed(2)}`).join(" / ") + " GHz" : "—";
  $("mem-now").textContent = s.memory ? `${((s.memory.total - s.memory.available) / 1024).toFixed(2)} / ${(s.memory.total / 1024).toFixed(1)} GiB` : "—";
  const hottest = s.thermal.filter(([n]) => ["BIG", "MID", "LITTLE", "G3D", "TPU"].includes(n)).sort((a, b) => b[1] - a[1])[0];
  $("temp-now").textContent = hottest ? `${hottest[0]} ${hottest[1].toFixed(1)}°C` : (s.root ? "—" : "needs root");
  $("gpu-now").textContent = s.gpu ? `${s.gpu[0].toFixed(0)} MHz · ${s.gpu[1].toFixed(0)}%` : (s.root ? "—" : "needs root");
  $("bat-now").textContent = s.battery ? `${s.battery.level ?? "—"}% · ${s.battery.voltage ? s.battery.voltage.toFixed(2) + " V" : ""}` : "—";
  $("bat-now").title = s.battery && s.battery.status ? s.battery.status : "";
  $("vm-now").textContent = s.vms && s.vms.length
    ? s.vms.map((v) => `${v.cpu.toFixed(0)}% CPU · ${(v.rss_mib / 1024).toFixed(2)} GiB RSS`).join(", ")
    : "not running";

  drawTimeline();
  drawCharts();
}

function drawTimeline() {
  const line = $("timeline");
  const recent = samples.slice(-600);
  if (line.children.length !== recent.length) {
    line.innerHTML = recent.map(() => "<span></span>").join("");
  }
  recent.forEach((s, i) => {
    line.children[i].style.background = TIMELINE_COLORS[s.state] || "#ff6b6b";
  });
}

// --------------------------------------------------------- boot and console

const consoleLines = [];

function setSource(text) { $("source").textContent = text; }

function showResult(kind, text) {
  const el = $("result");
  el.className = "result " + kind;
  el.textContent = text;
}

function resetBoot() {
  $("stages").innerHTML = Array.from({ length: 12 }, (_, i) =>
    `<div class="stage" id="stage-${i + 1}"><b>${i + 1}</b><span></span></div>`).join("");
  showResult("", "");
}

function classify(line) {
  if (line.startsWith("FERRIX-BOOT-OK")) return "ok";
  if (line.startsWith("FERRIX-PANIC")) return "bad";
  if (/^\s*stage\s+\d+\s/.test(line)) return "stage-line";
  return "";
}

// Mark a line's effect on the stage row, with its time since the boot began
// when there is one.
function trackStages(line, t) {
  const stage = /^\s*stage\s+(\d+)\s/.exec(line);
  if (stage) {
    const n = Number(stage[1]);
    for (let i = 1; i <= n; i++) $(`stage-${i}`).className = "stage done";
    const next = $(`stage-${n + 1}`);
    if (next) next.className = "stage now";
    if (t !== null && vmStarted) $(`stage-${n}`).querySelector("span").textContent = `${(t - vmStarted).toFixed(1)} s`;
  }
  if (line.startsWith("FERRIX-BOOT-OK")) {
    document.querySelectorAll(".stage").forEach((el) => (el.className = "stage done"));
    showResult("ok", line);
  }
  if (line.startsWith("FERRIX-PANIC")) {
    const failing = document.querySelector(".stage.now");
    if (failing) failing.className = "stage failed";
    showResult("bad", line);
  }
  const code = /^\s+code\s+(FX-\d+)\s+(.*)/.exec(line);
  if (code && $("result").classList.contains("bad")) {
    $("result").textContent += `\n${code[1]}: ${code[2]}`;
  }
}

function nearEnd(pre) {
  const lineHeight = 12 * 1.45;
  return pre.scrollHeight - pre.scrollTop - pre.clientHeight <= lineHeight * FOLLOW_LINES;
}

function shown(line, filter) {
  if (isStatLine(line) && !$("show-stats").checked) return false;
  return !filter || line.toLowerCase().includes(filter);
}

function appendLine(line, t = null) {
  consoleLines.push(line);
  if (isStatLine(line)) {
    takeStatLine(line, true);
    drawFerrix();
  }
  trackStages(line, t);
  const pre = $("console");
  const follow = nearEnd(pre);
  const filter = $("filter").value.trim().toLowerCase();
  if (shown(line, filter)) pre.appendChild(renderLine(line, filter));
  $("lines").textContent = `${consoleLines.length} lines`;
  if (follow) pre.scrollTop = pre.scrollHeight;
}

function renderLine(line, filter) {
  const span = document.createElement("span");
  const kind = classify(line);
  if (kind) span.className = kind;
  if (filter) {
    const at = line.toLowerCase().indexOf(filter);
    span.append(line.slice(0, at));
    const mark = document.createElement("mark");
    mark.textContent = line.slice(at, at + filter.length);
    span.append(mark, line.slice(at + filter.length));
  } else {
    span.textContent = line;
  }
  span.append("\n");
  return span;
}

function rerender() {
  const pre = $("console");
  const filter = $("filter").value.trim().toLowerCase();
  pre.textContent = "";
  const fragment = document.createDocumentFragment();
  for (const line of consoleLines) {
    if (shown(line, filter)) fragment.appendChild(renderLine(line, filter));
  }
  pre.appendChild(fragment);
  pre.scrollTop = pre.scrollHeight;
}

function clearConsole() {
  consoleLines.length = 0;
  $("console").textContent = "";
  $("lines").textContent = "0 lines";
  resetBoot();
}

async function loadRun(path, label, fill = false) {
  try {
    const text = await invoke("read_run", { path });
    clearConsole();
    vmStarted = 0;
    // A phone record holds ABL's log before the loader's first line, and the
    // next boot's after Ferrix's last: `[   12.070097] [I] ...` lines.
    const loader = text.indexOf("ferrix-pixel7 loader");
    let lines = (loader >= 0 ? text.slice(loader) : text).split("\n");
    const next = lines.findIndex((line) => /^\[\s*\d+\.\d+\] \[[IWE]\]/.test(line));
    if (next > 0) lines = lines.slice(0, next);
    ferrix.samples = [];
    ferrix.start = null;
    ferrix.end = null;
    for (const line of lines) {
      consoleLines.push(line);
      if (isStatLine(line)) takeStatLine(line, false);
      trackStages(line, null);
    }
    rerender();
    $("lines").textContent = `${consoleLines.length} lines`;
    if (fill) fillFromFerrix(typeof fill === "number" ? fill : null);
    if (ferrix.samples.length) {
      showTab("ferrix");
      drawFerrix();
    }
    setSource(label);
    if (!$("result").textContent) showResult("bad", "no FERRIX-BOOT-OK or FERRIX-PANIC in the record");
  } catch (error) {
    showResult("bad", String(error));
  }
}

// ------------------------------------------------------------------- runs

function shortImage(path) {
  if (!path) return "—";
  const parts = path.split("/");
  return parts.slice(-2).join("/");
}

async function refreshRuns() {
  const runs = await invoke("list_runs");
  $("runs-count").textContent = `${runs.length} runs`;
  $("runs").innerHTML = "";
  for (const run of runs.slice(0, 200)) {
    const row = document.createElement("div");
    row.className = "run" + (run.path === selectedRun ? " selected" : "");
    const kind = run.result ? (run.result.startsWith("FERRIX-BOOT-OK") ? "ok" : "bad") : "";
    const when = new Date(run.when * 1000);
    row.innerHTML = `<span class="dot ${kind}"></span><span class="name"></span><span class="when"></span><span class="res"></span><span class="elapsed"></span>`;
    row.querySelector(".name").textContent = (run.vm ? "VM " : "") + run.name;
    row.querySelector(".when").textContent = when.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
    row.querySelector(".res").textContent = run.result || "no result line";
    row.querySelector(".elapsed").textContent = run.elapsed || "";
    row.onclick = () => {
      selectedRun = run.path;
      document.querySelectorAll(".run").forEach((el) => el.classList.remove("selected"));
      row.classList.add("selected");
      loadRun(run.path, `record: ${run.name}`);
    };
    $("runs").appendChild(row);
  }
}

// ---------------------------------------------------------------- actions

$("boot").onclick = async () => {
  try {
    if (!lastSample || !lastSample.helper) {
      showResult("busy", "Starting the helper…");
      await invoke("start_helper", { image: null });
      for (let i = 0; i < 20 && !(lastSample && lastSample.helper); i++) await sleep(500);
    }
    // On the phone the service must end, or the phone stays in Ferrix: the
    // kernel feeds the watchdog while pid 1 runs. "Until stopped" is a minute.
    const choice = $("stats").value;
    const stats = choice === "" ? null : (Number(choice) || 60);
    const what = stats === null
      ? "Android comes back about 75 s later."
      : `Ferrix's stat service runs for ${stats} s after the boot checks, and Android comes back about ${75 + stats} s later. The graphs fill in from the record then: nothing reaches the PC while Ferrix runs.`;
    if (!confirm(`Reboot the phone into Ferrix? ${what}`)) return;
    clearConsole();
    setSource(stats === null ? "native boot" : `native boot · ferrix-statd ${stats} s`);
    showResult("busy", "Rebooting into Ferrix…");
    await invoke("boot_native", { stats });
  } catch (error) {
    showResult("bad", String(error));
  }
};

$("vm-run").onclick = async () => {
  clearConsole();
  vmStarted = Date.now() / 1000;
  vmRunning = true;
  vmPaused = false;
  setVmButtons();
  setSource(`VM · ${$("cpus").value} vCPUs · ${Number($("memory").value) / 1024} GiB${$("stats").value !== "" ? " · ferrix-statd" : ""}`);
  ferrix.samples = [];
  $("ferrix-badge").textContent = "";
  showResult("busy", "Starting the guest…");
  try {
    const stats = $("stats").value;
    await invoke("vm_start", {
      cpus: Number($("cpus").value),
      memory: Number($("memory").value),
      stats: stats === "" ? null : Number(stats),
    });
  } catch (error) {
    vmRunning = false;
    setVmButtons();
    showResult("bad", String(error));
  }
};

$("vm-pause").onclick = async () => {
  try {
    await invoke("vm_control", { command: vmPaused ? "resume" : "suspend" });
    vmPaused = !vmPaused;
    setVmButtons();
  } catch (error) {
    showResult("bad", String(error));
  }
};

$("vm-stop").onclick = () => invoke("vm_control", { command: "stop" }).catch((error) => showResult("bad", String(error)));

function setVmButtons() {
  $("vm-pause").disabled = !vmRunning;
  $("vm-stop").disabled = !vmRunning;
  $("vm-pause").textContent = vmPaused ? "Resume" : "Pause";
  $("vm-run").disabled = vmRunning;
}

$("filter").oninput = rerender;
$("show-stats").onchange = rerender;
$("to-end").onclick = () => { const pre = $("console"); pre.scrollTop = pre.scrollHeight; };
$("runs-refresh").onclick = refreshRuns;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------- startup

listen("sample", (event) => onSample(event.payload));
listen("vm-line", (event) => {
  if (consoleLines.length === 0) showResult("busy", "The guest is booting…");
  appendLine(event.payload.line, event.payload.t);
});
// Ferrix's USB serial port during a native boot: the same lines a guest
// sends, from the phone itself.
listen("usb-state", (event) => {
  const { port, seconds } = event.payload;
  $("boot-android").disabled = !port;
  if (port) {
    clearConsole();
    resetBoot();
    ferrix.samples = [];
    $("ferrix-badge").textContent = "";
    setSource(`Native · USB serial ${port}`);
    showResult("busy", `Ferrix is on USB (${port})…`);
    return;
  }
  $("result").textContent += `  ·  USB gone after ${seconds.toFixed(1)} s`;
  refreshRuns();
});
listen("usb-line", (event) => appendLine(event.payload.line, event.payload.t));

// Ferrix restarts when its USB port is sent usbdev's one command, and on the
// phone a restart is the watchdog reset that brings Android back.
$("boot-android").onclick = async () => {
  try {
    await invoke("usb_reboot");
    $("boot-android").disabled = true;
    showResult("busy", "Asked Ferrix to restart; Android comes back in about a minute…");
  } catch (error) {
    showResult("bad", String(error));
  }
};
listen("vm-ended", (event) => {
  vmRunning = false;
  vmPaused = false;
  setVmButtons();
  const { status, seconds } = event.payload;
  if (!$("result").classList.contains("ok") && !$("result").classList.contains("bad")) {
    showResult("bad", `The guest ended with no FERRIX-BOOT-OK (status ${status ?? "?"})`);
  }
  $("result").textContent += `  ·  ${seconds.toFixed(1)} s`;
  refreshRuns();
});

window.addEventListener("resize", () => { drawCharts(); drawFerrix(); });
resetBoot();
refreshRuns();
