// Runs damaged copies of a DNG through the full pipeline, isolated from the
// page: if anything here ever traps, only this worker dies and the page
// counts it as a crash.
import init, { try_develop } from "./pkg/irradiance_demo_core.js";

const ready = init();

self.onmessage = async (e) => {
  const { id, bytes } = e.data;
  try {
    await ready;
    const result = JSON.parse(try_develop(bytes));
    self.postMessage({ id, result });
  } catch (err) {
    self.postMessage({ id, crash: String((err && err.message) || err) });
  }
};
