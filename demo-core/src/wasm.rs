//! Browser bindings. Thin: every method forwards to `Developed`.

use crate::{try_develop as try_develop_core, Developed};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    // `performance.now()` exists in both windows and workers.
    #[wasm_bindgen(js_namespace = performance)]
    fn now() -> f64;
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct Session {
    inner: Developed,
}

#[wasm_bindgen]
impl Session {
    /// Develop a whole DNG file. Throws the library's error message on failure.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>) -> Result<Session, JsError> {
        let clock = || now();
        Developed::run(bytes, &clock).map(|inner| Session { inner }).map_err(|e| JsError::new(&e))
    }

    pub fn info_json(&self) -> String {
        self.inner.info_json()
    }

    pub fn is_bayer(&self) -> bool {
        self.inner.is_bayer()
    }

    pub fn out_width(&self) -> u32 {
        self.inner.geo.out_w
    }

    pub fn out_height(&self) -> u32 {
        self.inner.geo.out_h
    }

    pub fn preview_width(&self, max_w: u32) -> u32 {
        self.inner.preview_dims(max_w).0
    }

    pub fn preview_height(&self, max_w: u32) -> u32 {
        self.inner.preview_dims(max_w).1
    }

    /// RGBA8, `preview_width x preview_height`.
    pub fn preview(&self, stage: u32, max_w: u32) -> Vec<u8> {
        self.inner.preview(stage, max_w)
    }

    /// RGBA8, `w x h`, full resolution, output-frame coordinates.
    pub fn region(&self, stage: u32, x: u32, y: u32, w: u32, h: u32, brighten: bool) -> Vec<u8> {
        self.inner.region(stage, x, y, w, h, brighten)
    }

    pub fn trace_json(&self, x: u32, y: u32) -> String {
        self.inner.trace_json(x, y)
    }
}

/// Run the pipeline on (possibly corrupted) bytes; returns a JSON outcome.
#[wasm_bindgen]
pub fn try_develop(bytes: &[u8]) -> String {
    try_develop_core(bytes)
}
