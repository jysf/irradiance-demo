// Anatomy of a RAW: page logic. All decoding happens in WebAssembly
// (pkg/, built from demo-core with wasm-pack). This file only draws.
import init, { Session } from "./pkg/irradiance_demo_core.js";

const LOUPE_W = 96;
const LOUPE_H = 60;
const PREVIEW_MAX = 1600;
const STRESS_TRIALS = 30;

const SAMPLE = {
  url: "sample/nikon-d300s-cc0.dng",
  name: "Sample: Nikon D300S still life",
  credit: "raw.pixls.us · CC0",
  // Centre of the loupe on the mug's red/white rim (output-frame coordinates).
  loupe: [1989, 876],
};

const ILLUMINANT = {
  1: "Daylight", 2: "Fluorescent", 3: "Tungsten", 4: "Flash", 9: "Fine weather",
  10: "Cloudy", 11: "Shade", 17: "Standard light A", 18: "Standard light B",
  19: "Standard light C", 20: "D55", 21: "D65", 22: "D75", 23: "D50", 24: "ISO studio tungsten",
};
const CH = ["R", "G", "B"];
const CH_CLASS = ["r", "g", "b"];
const CH_NAME = ["red", "green", "blue"];

const $ = (id) => document.getElementById(id);
const state = {
  session: null,
  info: null,
  stage: 0,
  previews: new Map(),
  loupe: [0, 0],       // centre, output-frame px
  bytes: null,         // the original file, kept for the stress test
  fileName: "",
  netDuring: 0,
  wasmBytes: null,
};

// ---------- stage copy ----------

function fmtInt(n) { return n.toLocaleString("en-US"); }
function fmt(n, d = 3) { return Number(n).toFixed(d); }
function mb(bytes) { return (bytes / 1048576).toFixed(bytes > 1048576 * 10 ? 0 : 1) + " MB"; }

function cfaLabel(cfa) {
  if (!cfa) return null;
  const sw = (c) => `<span class="sw ${CH_CLASS[c]}"></span>${CH[c]}`;
  const name = cfa.map((c) => CH[c]).join("");
  return `${sw(cfa[0])} ${sw(cfa[1])} / ${sw(cfa[2])} ${sw(cfa[3])} (${name})`;
}

function stages(info) {
  const bayer = info.channels === 3;
  const px = info.raw_w * info.raw_h;
  const out = info.out_w * info.out_h;
  const wb = info.as_shot_neutral ? info.as_shot_neutral.map((v) => (1 / v)) : null;
  const ill = (n) => (n == null ? "–" : `${ILLUMINANT[n] || "illuminant " + n}`);
  const opcodeNote = (info.opcode_lists[0] || info.opcode_lists[1] || info.opcode_lists[2])
    ? `<p>This file also carries DNG opcode lists (${["1", "2", "3"].filter((_, i) => info.opcode_lists[i]).join(", ")}): gain maps or lens corrections the camera asks for. The library applies the ones it supports during the develop step.</p>` : "";
  return [
    {
      key: "Sensor plane",
      title: "1 · What the sensor recorded",
      tag: "linear, as stored",
      body: `<p>Each photosite counts light and nothing else. This camera stores values from ${fmtInt(info.black)} (black) to ${fmtInt(info.white)} (white), so the plane is one big greyscale grid, and it looks <b>dark</b> because light is stored linearly.</p>` +
        (bayer ? `<p>In the loupe you can already see a fine checkerboard: neighbours sit under different colour filters, so they respond differently to the same colour.</p>` : `<p>This sensor has no colour filters, so every value here is a direct measurement of light.</p>`),
      code: "plane::unpack_into(&s, c.byte_order(), &bytes, &mut raw)?;",
      facts: [
        ["photosites", `${fmtInt(info.raw_w)} × ${fmtInt(info.raw_h)} = ${fmtInt(px)}`],
        ["range", `${fmtInt(info.black)} … ${fmtInt(info.white)} (BlackLevel … WhiteLevel)`],
        ["storage", `${info.bits}-bit · ${info.compression}`],
      ],
    },
    {
      key: "Filter mosaic",
      title: "2 · The colour filter array",
      tag: "same numbers, tinted by filter",
      skip: !bayer,
      body: `<p>Over every photosite sits one red, green or blue filter in a repeating 2 × 2 tile. Here each number is painted the colour of its filter. Half of all sites are green, which is why the frame looks green.</p><p>Each pixel still knows only <b>one</b> of its three colours.</p>`,
      code: "s.color_filter // CFAPattern, repeat 2 × 2",
      facts: [["pattern", cfaLabel(info.cfa) || "–"], ["share", "25% red, 50% green, 25% blue"]],
    },
    {
      key: "Demosaic",
      title: "3 · Demosaic",
      tag: "camera RGB, no white balance",
      skip: !bayer,
      body: `<p>Demosaicing fills in each pixel's two missing colours from its neighbours (bilinear interpolation). The checkerboard disappears and every pixel now has red, green and blue.</p><p>The colours are still the <b>camera's own</b>: unbalanced, green-heavy, and linear, so still dark.</p>`,
      code: "demosaic::demosaic_into(&s, &raw, &mut rgb)?;",
      facts: [["method", "bilinear"], ["values", `${fmtInt(px * 3)}, of which ${fmtInt(px * 2)} interpolated`]],
    },
    bayer ? {
      key: "Colour",
      title: "4 · White balance and colour matrix",
      tag: "linear sRGB, cropped",
      body: `<p>Now the file's own metadata takes over. <b>AsShotNeutral</b> records what grey looked like when the shot was taken, which sets the white balance. The <b>ColorMatrix</b> tags, calibrated under two light sources, are blended for this scene and convert camera RGB to standard sRGB.</p><p>The frame is also cropped to <b>DefaultCrop</b> and rotated to its orientation. Colours are now right, but still linear, so the picture stays dim.</p>${opcodeNote}`,
      code: "color::develop_color_into(&s, &raw, &mut linear)?;",
      facts: [
        ["white balance", wb ? `R × ${fmt(wb[0], 2)}, G × ${fmt(wb[1], 2)}, B × ${fmt(wb[2], 2)}` : "–"],
        ["calibrations", `${ill(info.illuminant_1)} ⟷ ${ill(info.illuminant_2)}`],
        ["matrix source", info.forward_matrix ? "ForwardMatrix" : "inverse ColorMatrix"],
        ["output", `${fmtInt(info.out_w)} × ${fmtInt(info.out_h)}, crop origin (${info.crop_x}, ${info.crop_y}), orientation ${info.orientation}`],
      ],
    } : {
      key: "Levels + crop",
      title: "4 · Levels and crop",
      tag: "linear, cropped",
      body: `<p>With no colour to reconstruct, developing is short and exact: subtract black, scale to white, crop to <b>DefaultCrop</b> and rotate to the orientation. Every output value is a measured value, rescaled. Nothing is interpolated.</p>${opcodeNote}`,
      code: "develop::develop_into(&s, &raw, &mut linear)?;",
      facts: [
        ["output", `${fmtInt(info.out_w)} × ${fmtInt(info.out_h)}, crop origin (${info.crop_x}, ${info.crop_y}), orientation ${info.orientation}`],
        ["interpolated", "0 values"],
      ],
    },
    {
      key: "Tone curve",
      title: "5 · The display curve",
      tag: "display-referred sRGB",
      body: `<p>Screens expect gamma-encoded values, not linear light. The sRGB transfer curve lifts the shadows and midtones, and the picture finally looks like the scene.</p><p>The curve is a precomputed table, so the output is <b>bit-identical</b> on every platform. Compare the SHA-256 below with a native run.</p>`,
      code: "tone::encode_srgb16_into(&linear, &mut srgb)?;",
      facts: [
        ["curve", "sRGB OETF (IEC 61966-2-1), table-driven"],
        ["result", `16-bit, ${fmtInt(out)} pixels × ${info.channels}`],
      ],
    },
  ];
}

// ---------- rendering ----------

function status(msg, isError = false) {
  const el = $("status");
  el.textContent = msg;
  el.classList.toggle("error", isError);
}

function buildKeys() {
  const keys = $("keys");
  keys.innerHTML = "";
  const list = stages(state.info);
  list.forEach((st, i) => {
    const b = document.createElement("button");
    b.className = "key";
    b.type = "button";
    b.setAttribute("role", "tab");
    b.innerHTML = `<span class="n">${i + 1}${st.skip ? " · skipped" : ""}</span><span class="t">${st.key}</span>`;
    if (st.skip) b.setAttribute("aria-disabled", "true");
    b.addEventListener("click", () => { if (!st.skip) showStage(i); });
    keys.appendChild(b);
  });
}

function stageAllowed(i) {
  const st = stages(state.info)[i];
  return st && !st.skip;
}

function step(dir) {
  let i = state.stage + dir;
  while (i >= 0 && i < 5 && !stageAllowed(i)) i += dir;
  if (i >= 0 && i < 5) showStage(i);
}

function getPreview(stage) {
  if (!state.previews.has(stage)) {
    const s = state.session;
    const w = s.preview_width(PREVIEW_MAX);
    const h = s.preview_height(PREVIEW_MAX);
    const data = s.preview(stage, PREVIEW_MAX);
    state.previews.set(stage, new ImageData(new Uint8ClampedArray(data.buffer, data.byteOffset, data.byteLength), w, h));
  }
  return state.previews.get(stage);
}

function showStage(i) {
  state.stage = i;
  const st = stages(state.info)[i];
  [...$("keys").children].forEach((k, j) => {
    k.setAttribute("aria-selected", String(j === i));
    k.tabIndex = j === i ? 0 : -1;
  });
  const img = getPreview(i);
  const c = $("full");
  c.width = img.width;
  c.height = img.height;
  c.getContext("2d").putImageData(img, 0, 0);
  $("fulltag").textContent = st.tag;
  $("title").textContent = st.title;
  $("body").innerHTML = st.body;
  $("code").textContent = st.code;
  $("facts").innerHTML = st.facts.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join("");
  $("prev").disabled = ![0, 1, 2, 3].some((j) => j < i && stageAllowed(j));
  $("next").disabled = ![1, 2, 3, 4].some((j) => j > i && stageAllowed(j));
  drawLoupe();
  trace(...(state.hover || state.loupe));
}

function loupeOrigin() {
  const { out_w, out_h } = state.info;
  const x0 = Math.max(0, Math.min(out_w - LOUPE_W, state.loupe[0] - LOUPE_W / 2));
  const y0 = Math.max(0, Math.min(out_h - LOUPE_H, state.loupe[1] - LOUPE_H / 2));
  return [Math.round(x0), Math.round(y0)];
}

function drawLoupe() {
  const [x0, y0] = loupeOrigin();
  const brighten = state.stage < 4;
  const data = state.session.region(state.stage, x0, y0, LOUPE_W, LOUPE_H, brighten);
  const img = new ImageData(new Uint8ClampedArray(data.buffer, data.byteOffset, data.byteLength), LOUPE_W, LOUPE_H);
  $("zoom").getContext("2d").putImageData(img, 0, 0);
  const { out_w, out_h } = state.info;
  const box = $("loupebox").style;
  box.left = (x0 / out_w) * 100 + "%";
  box.top = (y0 / out_h) * 100 + "%";
  box.width = (LOUPE_W / out_w) * 100 + "%";
  box.height = (LOUPE_H / out_h) * 100 + "%";
  $("zoomcap").textContent = `8× loupe · ${LOUPE_W} × ${LOUPE_H} px at (${x0}, ${y0})` + (brighten ? " · brightened to show detail" : "");
}

// ---------- pixel trace ----------

function trace(x, y) {
  const t = JSON.parse(state.session.trace_json(x, y));
  if (!t) return;
  const info = state.info;
  const bayer = info.channels === 3;
  $("tracewhere").textContent = `output pixel (${t.x}, ${t.y}) ← photosite (${t.rx}, ${t.ry})`;
  const items = [];
  const filter = t.filter;
  const src = (s) => `<span class="src">${s}</span>`;
  const lin = t.lin.map((v) => fmt(v / 65535, 4));

  items.push({
    what: "Photosite",
    val: bayer
      ? `<span class="sw ${CH_CLASS[filter]}"></span>${CH_NAME[filter]} filter · raw <b>${fmtInt(t.raw)}</b> <span class="k">on a scale of ${fmtInt(t.black)}–${fmtInt(t.white)}</span><br>${src("plane::unpack_into")}`
      : `no filter · raw <b>${fmtInt(t.raw)}</b> <span class="k">on a scale of ${fmtInt(t.black)}–${fmtInt(t.white)}</span><br>${src("plane::unpack_into")}`,
  });

  if (bayer) {
    const lines = [0, 1, 2].map((c) => {
      const tag = `<span class="sw ${CH_CLASS[c]}"></span>${CH[c]} <b>${fmtInt(t.cam[c])}</b>`;
      if (c === filter) return `${tag} <span class="k">measured</span>`;
      const ns = t.neighbours[c].map((n) => fmtInt(n[2])).join(", ");
      return `${tag} <span class="k">from ${t.neighbours[c].length} ${CH_NAME[c]} neighbours (${ns})</span>`;
    });
    items.push({ what: "Demosaic", val: lines.join("<br>") + `<br>${src("demosaic::demosaic_into")}` });
    items.push({
      what: "Normalise",
      val: `(value − ${fmtInt(t.black)}) ÷ ${fmtInt(t.white - t.black)} = ${t.cam_norm.map((v) => fmt(v, 4)).join(", ")}<br>${src("derived here for display")}`,
    });
    const m = info.matrix;
    const mtx = m ? `<span class="mtx">${m.flat().map((v) => `<span>${fmt(v, 3)}</span>`).join("")}</span>` : "";
    items.push({
      what: "White balance + matrix",
      val: (m ? `${mtx} × camera RGB<br>` : "") + `→ linear sRGB <b>${lin.join(", ")}</b><br>${src("color::develop_color_into" + (m ? " · matrix recovered from color::to_srgb_linear_into" : ""))}`,
    });
  } else {
    items.push({ what: "Filter mosaic", skip: true, val: "no colour filter: this value is light, not one colour of three" });
    items.push({ what: "Demosaic", skip: true, val: "nothing to interpolate" });
    items.push({
      what: "Levels + crop",
      val: `(raw − ${fmtInt(t.black)}) ÷ ${fmtInt(t.white - t.black)} ≈ ${fmt(t.norm, 4)} → linear <b>${lin[0]}</b><br>${src("develop::develop_into")}`,
    });
  }

  const rgb8 = bayer ? t.srgb8 : [t.srgb8[0], t.srgb8[0], t.srgb8[0]];
  const hex = "#" + rgb8.map((v) => v.toString(16).padStart(2, "0")).join("");
  items.push({
    what: "Tone curve",
    val: `<span class="chip" style="background:${hex}"></span><b>${rgb8.join(", ")}</b> <span class="k">${hex} · 16-bit ${t.srgb16.map(fmtInt).join(", ")}</span><br>${src("tone::encode_srgb16_into")}`,
  });

  // Which ticket rows belong to the current stage.
  const on = { 0: [0], 1: [0], 2: [1], 3: [2, 3], 4: [4] }[state.stage] || [];
  $("ticket").innerHTML = items.map((it, i) => `
    <li class="${it.skip ? "skip" : ""} ${on.includes(i) ? "on" : ""}">
      <span class="no">${i + 1}</span>
      <div><div class="what">${it.what}</div><div class="val">${it.val}</div></div>
    </li>`).join("");

  const ops = info.opcode_lists;
  $("tracenote").textContent = (ops[0] || ops[1] || ops[2])
    ? "This file has opcode lists (gain maps or lens corrections). They act inside the develop step, so the derived values above are approximate there; every value marked with a library call is exact."
    : "Every value marked with a library call is exactly what irradiance produced for this pixel.";
}

function plateCoords(ev, el) {
  const r = el.getBoundingClientRect();
  const { out_w, out_h } = state.info;
  const x = Math.min(out_w - 1, Math.max(0, Math.floor(((ev.clientX - r.left) / r.width) * out_w)));
  const y = Math.min(out_h - 1, Math.max(0, Math.floor(((ev.clientY - r.top) / r.height) * out_h)));
  return [x, y, (ev.clientX - r.left) / r.width, (ev.clientY - r.top) / r.height];
}

function wireInteractions() {
  const plate = $("plate");
  const cross = $("crosshair");
  plate.addEventListener("mousemove", (ev) => {
    if (!state.session) return;
    const [x, y, fx, fy] = plateCoords(ev, $("full"));
    state.hover = [x, y];
    cross.style.display = "block";
    cross.style.left = fx * 100 + "%";
    cross.style.top = fy * 100 + "%";
    trace(x, y);
  });
  plate.addEventListener("mouseleave", () => {
    state.hover = null;
    cross.style.display = "none";
    if (state.session) trace(...state.loupe);
  });
  plate.addEventListener("click", (ev) => {
    if (!state.session) return;
    const [x, y] = plateCoords(ev, $("full"));
    state.loupe = [x, y];
    drawLoupe();
    trace(x, y);
  });
  const zoom = $("zoom");
  zoom.addEventListener("mousemove", (ev) => {
    if (!state.session) return;
    const r = zoom.getBoundingClientRect();
    const [x0, y0] = loupeOrigin();
    const x = x0 + Math.min(LOUPE_W - 1, Math.floor(((ev.clientX - r.left) / r.width) * LOUPE_W));
    const y = y0 + Math.min(LOUPE_H - 1, Math.floor(((ev.clientY - r.top) / r.height) * LOUPE_H));
    state.hover = [x, y];
    trace(x, y);
  });
  zoom.addEventListener("mouseleave", () => { state.hover = null; if (state.session) trace(...state.loupe); });

  $("prev").addEventListener("click", () => step(-1));
  $("next").addEventListener("click", () => step(1));
  document.addEventListener("keydown", (e) => {
    if (!state.session || e.target.closest("input, textarea")) return;
    if (e.key === "ArrowRight") step(1);
    if (e.key === "ArrowLeft") step(-1);
  });

  $("fileinput").addEventListener("change", async (e) => {
    const f = e.target.files && e.target.files[0];
    if (f) await openFile(f);
    e.target.value = "";
  });
  const drop = $("drop");
  let depth = 0;
  window.addEventListener("dragenter", (e) => { if (hasFiles(e)) { depth++; drop.hidden = false; e.preventDefault(); } });
  window.addEventListener("dragover", (e) => { if (hasFiles(e)) e.preventDefault(); });
  window.addEventListener("dragleave", () => { depth = Math.max(0, depth - 1); if (!depth) drop.hidden = true; });
  window.addEventListener("drop", async (e) => {
    e.preventDefault();
    depth = 0;
    drop.hidden = true;
    const f = e.dataTransfer && e.dataTransfer.files[0];
    if (f) await openFile(f);
  });
  $("breakbtn").addEventListener("click", runStress);
}

function hasFiles(e) {
  return e.dataTransfer && [...e.dataTransfer.types].includes("Files");
}

// ---------- performance panel ----------

function renderPerf() {
  const info = state.info;
  const total = info.timings.reduce((a, [, ms]) => a + ms, 0);
  const max = Math.max(...info.timings.map(([, ms]) => ms), 1);
  $("bars").innerHTML = info.timings.map(([name, ms]) => `
    <div class="bar"><span class="lbl">${name}</span>
      <span class="track"><span class="fill" style="width:${(ms / max) * 100}%"></span></span>
      <span class="ms">${ms.toFixed(0)} ms</span></div>`).join("");
  const mp = (info.out_w * info.out_h) / 1e6;
  const bufTotal = info.buffers.reduce((a, [, b]) => a + b, 0);
  const rows = [
    ["total", `${total.toFixed(0)} ms for ${mp.toFixed(1)} MP · ${(mp / (total / 1000)).toFixed(0)} MP/s`],
    ["network", `${state.netDuring} requests while developing`],
    ["buffers", `${mb(bufTotal)}, all allocated by this page and filled by the library: ` +
      info.buffers.map(([n, b]) => `${n} ${mb(b)}`).join(" · ")],
  ];
  if (state.wasmBytes) rows.push(["decoder", state.wasmBytes]);
  $("perf").innerHTML = rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join("");
  $("hash").textContent = info.sha256;
}

// ---------- loading ----------

async function openFile(file) {
  const buf = new Uint8Array(await file.arrayBuffer());
  await develop(buf, { name: file.name, credit: `${mb(file.size)} · read locally` });
}

// Yield once so the status line paints before the (synchronous) develop.
// Not requestAnimationFrame: it never fires in a background tab.
function nextFrame() {
  return new Promise((r) => setTimeout(r, 30));
}

async function develop(bytes, meta) {
  status(`Developing ${meta.name}…`);
  await nextFrame();
  const before = performance.getEntriesByType("resource").length;
  let session;
  try {
    session = new Session(bytes);
  } catch (e) {
    status(`Could not develop ${meta.name}: ${e && e.message ? e.message : e}`, true);
    return false;
  }
  state.netDuring = performance.getEntriesByType("resource").length - before;
  if (state.session) state.session.free();
  state.session = session;
  state.info = JSON.parse(session.info_json());
  state.previews.clear();
  state.bytes = bytes;
  state.fileName = meta.name;
  const { out_w, out_h } = state.info;
  state.loupe = meta.loupe && meta.loupe[0] < out_w && meta.loupe[1] < out_h
    ? meta.loupe : [Math.floor(out_w / 2), Math.floor(out_h / 2)];
  state.hover = null;

  const cam = [state.info.make, state.info.model].filter(Boolean).join(" ").replace(/^(\w+) \1 /i, "$1 ") || "Unknown camera";
  $("filename").textContent = meta.name;
  $("filemeta").textContent = `${cam} · ${meta.credit}`;
  $("eyebrow").textContent = `irradiance 0.2.6 · ${cam} · ${fmtInt(state.info.raw_w)} × ${fmtInt(state.info.raw_h)} photosites`;
  buildKeys();
  if (!stageAllowed(state.stage)) state.stage = 0;
  showStage(state.stage);
  renderPerf();
  $("breakbtn").disabled = false;
  resetStress();
  status("");
  return true;
}

async function boot() {
  status("Loading the decoder…");
  try {
    await init();
  } catch (e) {
    status(`This browser could not load WebAssembly: ${e.message || e}`, true);
    return;
  }
  const entry = performance.getEntriesByType("resource").find((r) => r.name.endsWith(".wasm"));
  if (entry && entry.decodedBodySize) {
    const kb = (n) => Math.round(n / 1024) + " KB";
    state.wasmBytes = `${kb(entry.decodedBodySize)} of WebAssembly` +
      (entry.encodedBodySize && entry.encodedBodySize < entry.decodedBodySize ? ` (${kb(entry.encodedBodySize)} over the wire)` : "");
    $("wasmsize").textContent = kb(entry.decodedBodySize);
  }
  wireInteractions();
  status("Fetching the sample DNG…");
  try {
    const res = await fetch(SAMPLE.url);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const bytes = new Uint8Array(await res.arrayBuffer());
    await develop(bytes, SAMPLE);
  } catch (e) {
    status(`Could not load the sample (${e.message || e}). Open your own DNG instead.`, true);
  }
}

// ---------- stress test ----------

let worker = null;
let jobId = 0;
const pending = new Map();

function newWorker() {
  if (worker) worker.terminate();
  worker = new Worker(new URL("./corrupt-worker.js", import.meta.url), { type: "module" });
  worker.onmessage = (e) => {
    const p = pending.get(e.data.id);
    if (p) { pending.delete(e.data.id); p(e.data); }
  };
  worker.onerror = (e) => {
    // An uncaught trap kills the module; fail everything in flight.
    for (const [id, p] of pending) { pending.delete(id); p({ id, crash: e.message || "worker error" }); }
  };
}

function runInWorker(bytes) {
  if (!worker) newWorker();
  const id = ++jobId;
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      newWorker();
      resolve({ id, crash: "timed out after 30 s" });
    }, 30000);
    pending.set(id, (d) => { clearTimeout(timer); resolve(d); });
    worker.postMessage({ id, bytes }, [bytes.buffer]);
  });
}

// Small seeded PRNG so every run is reproducible from its seed.
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6D2B79F5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// Entries of IFD0 and its SubIFDs (one level down). In most DNGs the raw
// sensor data lives in a SubIFD, so that is where the meanest edits land.
function ifdEntries(b) {
  if (b.length < 8) return null;
  const le = b[0] === 0x49;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const entries = [];
  const readIfd = (off, where) => {
    if (off + 2 > b.length) return [];
    const n = dv.getUint16(off, le);
    const found = [];
    for (let i = 0; i < n && off + 2 + 12 * (i + 1) <= b.length; i++) {
      const at = off + 2 + 12 * i;
      found.push({ at, tag: dv.getUint16(at, le), where });
    }
    return found;
  };
  const ifd0 = readIfd(dv.getUint32(4, le), "IFD0");
  entries.push(...ifd0);
  const sub = ifd0.find((e) => e.tag === 0x014a);
  if (sub) {
    const count = dv.getUint32(sub.at + 4, le);
    const offs = count === 1 ? [dv.getUint32(sub.at + 8, le)]
      : Array.from({ length: Math.min(count, 8) }, (_, k) => dv.getUint32(dv.getUint32(sub.at + 8, le) + 4 * k, le));
    offs.forEach((o, k) => entries.push(...readIfd(o, `SubIFD ${k}`)));
  }
  return { le, dv, entries };
}

// Prefer the sensor's SubIFD when the file has one.
function pickEntry(d, r) {
  const subs = d.entries.filter((e) => e.where !== "IFD0");
  const pool = subs.length && r() < 0.75 ? subs : d.entries;
  return pool[Math.floor(r() * pool.length)];
}

const hex = (t) => "0x" + t.toString(16).padStart(4, "0");

const MUTATIONS = [
  ["truncate", (b, r) => {
    const n = Math.floor(r() * b.length);
    return [b.slice(0, n), `truncated to ${fmtInt(n)} of ${fmtInt(b.length)} bytes`];
  }],
  ["flip header bits", (b, r) => {
    const c = b.slice();
    const k = 1 + Math.floor(r() * 8);
    const span = Math.min(65536, c.length);
    for (let i = 0; i < k; i++) c[Math.floor(r() * span)] ^= 1 << Math.floor(r() * 8);
    return [c, `${k} random bit${k > 1 ? "s" : ""} flipped in the first 64 KB`];
  }],
  ["lie about a count", (b, r) => {
    const c = b.slice();
    const d = ifdEntries(c);
    if (!d || !d.entries.length) return [c, "no IFD found"];
    const e = pickEntry(d, r);
    d.dv.setUint32(e.at + 4, 0xffffffff, d.le);
    return [c, `${e.where} tag ${hex(e.tag)}: count set to 4,294,967,295`];
  }],
  ["point an offset off the end", (b, r) => {
    const c = b.slice();
    const d = ifdEntries(c);
    if (!d || !d.entries.length) return [c, "no IFD found"];
    const e = pickEntry(d, r);
    d.dv.setUint32(e.at + 8, 0xfffffff0, d.le);
    return [c, `${e.where} tag ${hex(e.tag)}: value offset set to 0xFFFFFFF0`];
  }],
  ["scramble an IFD entry", (b, r) => {
    const c = b.slice();
    const d = ifdEntries(c);
    if (!d || !d.entries.length) return [c, "no IFD found"];
    const e = pickEntry(d, r);
    for (let i = 0; i < 12; i++) c[e.at + i] = Math.floor(r() * 256);
    return [c, `${e.where} entry for tag ${hex(e.tag)} overwritten with 12 random bytes`];
  }],
  ["zero a block", (b, r) => {
    const c = b.slice();
    const at = Math.floor(r() * Math.max(1, c.length - 4096));
    c.fill(0, at, at + 4096);
    return [c, `4 KB zeroed at byte ${fmtInt(at)}`];
  }],
  ["random bytes anywhere", (b, r) => {
    const c = b.slice();
    for (let i = 0; i < 64; i++) c[Math.floor(r() * c.length)] = Math.floor(r() * 256);
    return [c, "64 random bytes overwritten across the file"];
  }],
];

function resetStress() {
  $("tally").hidden = true;
  $("log").innerHTML = "";
  ["t-err", "t-ok", "t-crash"].forEach((id) => { $(id).textContent = "0"; });
  $("t-crash").className = "num";
}

async function runStress() {
  if (!state.bytes) return;
  const btn = $("breakbtn");
  btn.disabled = true;
  resetStress();
  $("tally").hidden = false;
  const counts = { err: 0, ok: 0, crash: 0 };
  const seed = (Date.now() & 0xffffff) >>> 0;
  const r = rng(seed);
  const log = $("log");
  for (let i = 0; i < STRESS_TRIALS; i++) {
    const [name, fn] = MUTATIONS[i % MUTATIONS.length];
    const [bytes, how] = fn(state.bytes, r);
    btn.textContent = `Running ${i + 1} of ${STRESS_TRIALS}…`;
    const d = await runInWorker(bytes);
    let cls, text;
    if (d.crash) {
      counts.crash++; cls = "crash"; text = d.crash;
    } else if (d.result.ok) {
      counts.ok++; cls = "ok"; text = `decoded ${fmtInt(d.result.w)} × ${fmtInt(d.result.h)}`;
    } else {
      counts.err++; cls = "err";
      text = d.result.demo_limit ? `refused by this page's size cap: ${d.result.message}` : `${d.result.variant}: ${d.result.message}`;
    }
    $("t-err").textContent = counts.err;
    $("t-ok").textContent = counts.ok;
    $("t-crash").textContent = counts.crash;
    $("t-crash").className = "num " + (counts.crash ? "some" : "zero");
    const li = document.createElement("li");
    li.innerHTML = `<span class="how">${i + 1}. ${name}: ${how}</span><span class="res ${cls}"></span>`;
    li.querySelector(".res").textContent = text;
    log.prepend(li);
  }
  btn.textContent = `Corrupt the file ${STRESS_TRIALS} times again`;
  btn.disabled = false;
}

boot();
