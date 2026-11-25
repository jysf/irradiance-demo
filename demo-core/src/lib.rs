//! Core of the "Anatomy of a RAW" demo.
//!
//! Runs irradiance's pipeline on DNG bytes and KEEPS every stage, so a UI can
//! show the sensor plane, the filter mosaic, the demosaic, the colour transform
//! and the tone curve side by side, and trace one pixel through all of them.
//! The same code runs natively (the `hash` binary) and in the browser (wasm32,
//! see `wasm.rs`), which is what lets the page compare output hashes.
//!
//! Everything shown as "the library's" value comes straight out of an
//! irradiance call. Values this crate derives for display (normalised camera
//! RGB, the recovered colour matrix) are labelled as derived in the UI.

use irradiance::ifd::{ByteOrder, Container, Sensor};
use sha2::{Digest, Sha256};

#[cfg(target_arch = "wasm32")]
mod wasm;

/// Stage indices, in pipeline order.
pub const STAGE_PLANE: u32 = 0;
pub const STAGE_MOSAIC: u32 = 1;
pub const STAGE_DEMOSAIC: u32 = 2;
pub const STAGE_LINEAR: u32 = 3;
pub const STAGE_SRGB: u32 = 4;

/// Maps output-frame pixels (after crop and orientation) back to the raw plane.
///
/// irradiance applies `ActiveArea` -> `DefaultCrop` -> `Orientation`; this is
/// the inverse for the 8 TIFF orientations. Lens-correction opcodes (warps) are
/// not modelled, so on a file that carries one the stage 1-3 views are
/// approximate. `hash --verify` checks this mapping against the library.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub raw_w: u32,
    pub raw_h: u32,
    pub out_w: u32,
    pub out_h: u32,
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_w: u32,
    pub crop_h: u32,
    pub orientation: u32,
}

impl Geometry {
    fn new(s: &Sensor, out_w: u32, out_h: u32) -> Geometry {
        let orientation = s.orientation.unwrap_or(1);
        let (crop_w, crop_h) = if (5..=8).contains(&orientation) {
            (out_h, out_w)
        } else {
            (out_w, out_h)
        };
        let (aa_x, aa_y) = s.active_area.as_ref().map(|a| (a.left, a.top)).unwrap_or((0, 0));
        let (co_x, co_y) = s.default_crop_origin.as_ref().map(|o| (o.x, o.y)).unwrap_or((0, 0));
        Geometry {
            raw_w: s.width,
            raw_h: s.height,
            out_w,
            out_h,
            crop_x: aa_x + co_x,
            crop_y: aa_y + co_y,
            crop_w,
            crop_h,
            orientation,
        }
    }

    /// Output pixel -> raw-plane photosite. Inputs must be inside the output frame.
    pub fn out_to_raw(&self, x: u32, y: u32) -> (u32, u32) {
        let (cw, ch) = (self.crop_w, self.crop_h);
        let (sx, sy) = match self.orientation {
            2 => (cw - 1 - x, y),
            3 => (cw - 1 - x, ch - 1 - y),
            4 => (x, ch - 1 - y),
            5 => (y, x),
            6 => (y, ch - 1 - x),
            7 => (cw - 1 - y, ch - 1 - x),
            8 => (cw - 1 - y, x),
            _ => (x, y),
        };
        let rx = (sx + self.crop_x).min(self.raw_w - 1);
        let ry = (sy + self.crop_y).min(self.raw_h - 1);
        (rx, ry)
    }
}

/// What the page shows in its header and fact lists.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub make: String,
    pub model: String,
    pub bits: u32,
    pub black: f64,
    pub white: f64,
    /// 2x2 CFA as colour indices (0 R, 1 G, 2 B), row-major; `None` = monochrome.
    pub cfa: Option<[u8; 4]>,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub illuminant_1: Option<u32>,
    pub illuminant_2: Option<u32>,
    pub forward_matrix: bool,
    pub compression: String,
    /// Which of OpcodeList1/2/3 the file carries (gain maps, lens corrections).
    pub opcode_lists: [bool; 3],
}

/// The library never allocates for the caller, so the CALLER decides how big a
/// plane it is willing to hold. A damaged header can claim any size; this demo
/// refuses anything over this many photosites before allocating. (Above it, a
/// browser tab would run out of WebAssembly memory, which is a crash of the
/// page, not of the library.)
pub const MAX_PIXELS: u64 = 100_000_000;

fn check_size(w: u32, h: u32) -> Result<(), String> {
    let n = u64::from(w) * u64::from(h);
    if n > MAX_PIXELS {
        return Err(format!(
            "DemoSizeLimit: the file claims {w} x {h} photosites ({:.0} MP); this demo allocates at most {} MP",
            n as f64 / 1e6,
            MAX_PIXELS / 1_000_000
        ));
    }
    Ok(())
}

/// Wall-clock milliseconds per step. Supplied by the caller so the same code
/// works natively (`Instant`) and in the browser (`performance.now()`).
pub type Clock<'a> = &'a dyn Fn() -> f64;

pub struct Developed {
    pub info: Info,
    pub geo: Geometry,
    pub raw: Vec<u16>,
    /// Camera-space RGB from `demosaic_into`, raw-plane sized; Bayer only.
    pub cam: Option<Vec<u16>>,
    /// Linear output (3 channels for Bayer, 1 for monochrome), output-frame sized.
    pub lin: Vec<u16>,
    /// sRGB-encoded output, same shape as `lin`.
    pub srgb: Vec<u16>,
    pub channels: u32,
    /// Combined white balance x camera->linear-sRGB matrix, recovered by probing
    /// `color::to_srgb_linear_into`. Bayer only.
    pub matrix: Option<[[f64; 3]; 3]>,
    pub timings: Vec<(&'static str, f64)>,
    pub sha256: String,
    pub input_bytes: usize,
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn ascii_tag(c: &Container, bytes: &[u8], tag: u16) -> String {
    // irradiance exposes no ASCII-tag reader, so walk IFD0's 12-byte entries
    // (TIFF 6.0 section 2) for this one display string. Any inconsistency just
    // yields an empty string.
    let Some(ifd) = c.ifd0() else { return String::new() };
    let little = matches!(c.byte_order(), ByteOrder::Little);
    let rd16 = |o: usize| bytes.get(o..o + 2).map(|b| if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let rd32 = |o: usize| bytes.get(o..o + 4).map(|b| if little { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let base = ifd.offset() as usize;
    let Some(n) = rd16(base) else { return String::new() };
    for k in 0..n as usize {
        let e = base + 2 + 12 * k;
        if rd16(e) != Some(tag) {
            continue;
        }
        let (Some(count), Some(field)) = (rd32(e + 4), rd32(e + 8)) else { return String::new() };
        let count = count as usize;
        let raw = if count <= 4 { bytes.get(e + 8..e + 8 + count) } else { bytes.get(field as usize..(field as usize).saturating_add(count)) };
        let Some(raw) = raw else { return String::new() };
        let s: String = raw.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        return s.trim().to_string();
    }
    String::new()
}

fn rat(r: &irradiance::ifd::Rational) -> f64 {
    if r.den == 0 {
        0.0
    } else {
        f64::from(r.num) / f64::from(r.den)
    }
}

fn info_from(c: &Container, bytes: &[u8], s: &Sensor) -> Info {
    let bits = s.bits_per_sample;
    let black = f64::from(s.black_level.unwrap_or(0));
    let white = f64::from(s.white_level.unwrap_or(if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 }));
    let cfa = s.color_filter.as_ref().and_then(|c| {
        if c.repeat_dim == [2, 2] && c.pattern.len() == 4 {
            Some([c.pattern[0], c.pattern[1], c.pattern[2], c.pattern[3]])
        } else {
            None
        }
    });
    let as_shot_neutral = s.as_shot_neutral.as_ref().and_then(|v| {
        if v.len() == 3 {
            Some([rat(&v[0]), rat(&v[1]), rat(&v[2])])
        } else {
            None
        }
    });
    Info {
        make: ascii_tag(c, bytes, 271),
        model: ascii_tag(c, bytes, 272),
        bits,
        black,
        white,
        cfa,
        as_shot_neutral,
        illuminant_1: s.calibration_illuminant_1,
        illuminant_2: s.calibration_illuminant_2,
        forward_matrix: s.forward_matrix_1.is_some(),
        compression: format!("{:?}", s.compression),
        opcode_lists: s.opcode_lists,
    }
}

/// Recover the 3x3 matrix `to_srgb_linear_into` applies, by finite differences
/// on four probe pixels. The base pixel is the camera's own neutral
/// (`AsShotNeutral`), which maps to grey, so no output clamps at 0 or 65535.
fn probe_matrix(s: &Sensor, neutral: Option<[f64; 3]>) -> Result<Option<[[f64; 3]; 3]>, String> {
    let n = (s.width as usize) * (s.height as usize) * 3;
    if n < 12 {
        return Ok(None);
    }
    let nv = neutral.unwrap_or([1.0, 1.0, 1.0]);
    let base: [u16; 3] = [0, 1, 2].map(|c| (8192.0 * nv[c].clamp(0.05, 1.0)).round() as u16);
    let delta = 1024u16;
    let mut src = vec![0u16; n];
    let mut dst = vec![0u16; n];
    for p in 0..4 {
        src[p * 3..p * 3 + 3].copy_from_slice(&base);
        if p > 0 {
            src[p * 3 + p - 1] += delta;
        }
    }
    irradiance::color::to_srgb_linear_into(s, &src, &mut dst).map_err(err)?;
    if dst[0..12].iter().any(|&v| v == 0 || v == u16::MAX) {
        return Ok(None);
    }
    let mut m = [[0.0; 3]; 3];
    for j in 0..3 {
        let p = (j + 1) * 3;
        for i in 0..3 {
            m[i][j] = (f64::from(dst[p + i]) - f64::from(dst[i])) / f64::from(delta);
        }
    }
    Ok(Some(m))
}

impl Developed {
    /// Run the full pipeline. `bytes` is the whole DNG file.
    pub fn run(bytes: Vec<u8>, now: Clock) -> Result<Developed, String> {
        let mut timings = Vec::new();
        let mut t = now();
        let lap = |name: &'static str, t: &mut f64, timings: &mut Vec<(&'static str, f64)>| {
            let n = now();
            timings.push((name, n - *t));
            *t = n;
        };

        let c = Container::parse(&bytes).map_err(err)?;
        let s = c.sensor().map_err(err)?;
        let info = info_from(&c, &bytes, &s);
        lap("parse", &mut t, &mut timings);
        check_size(s.width, s.height)?;

        let npx = (s.width as usize) * (s.height as usize);
        let mut raw = vec![0u16; npx];
        irradiance::plane::unpack_into(&s, c.byte_order(), &bytes, &mut raw).map_err(err)?;
        lap("unpack", &mut t, &mut timings);

        let (ow, oh) = irradiance::develop::output_dimensions(&s).map_err(err)?;
        let geo = Geometry::new(&s, ow, oh);
        let onpx = (ow as usize) * (oh as usize);

        let bayer = s.color_filter.is_some();
        let (cam, lin, channels, matrix) = if bayer {
            // Demosaic on its own is only for display and the trace; the colour
            // develop below does its own (fused, byte-identical) demosaic.
            let mut cam = vec![0u16; npx * 3];
            irradiance::demosaic::demosaic_into(&s, &raw, &mut cam).map_err(err)?;
            lap("demosaic", &mut t, &mut timings);
            let mut lin = vec![0u16; onpx * 3];
            irradiance::color::develop_color_into(&s, &raw, &mut lin).map_err(err)?;
            lap("colour", &mut t, &mut timings);
            let matrix = probe_matrix(&s, info.as_shot_neutral)?;
            t = now(); // the probe is demo bookkeeping, not pipeline time
            (Some(cam), lin, 3, matrix)
        } else {
            let mut lin = vec![0u16; onpx];
            irradiance::develop::develop_into(&s, &raw, &mut lin).map_err(err)?;
            lap("develop", &mut t, &mut timings);
            (None, lin, 1, None)
        };

        let mut srgb = vec![0u16; lin.len()];
        irradiance::tone::encode_srgb16_into(&lin, &mut srgb).map_err(err)?;
        lap("tone", &mut t, &mut timings);

        let sha256 = hash_u16(&srgb);
        Ok(Developed {
            info,
            geo,
            raw,
            cam,
            lin,
            srgb,
            channels,
            matrix,
            timings,
            sha256,
            input_bytes: bytes.len(),
        })
    }

    pub fn is_bayer(&self) -> bool {
        self.cam.is_some()
    }

    fn norm(&self, v: u16) -> f32 {
        let (b, w) = (self.info.black, self.info.white);
        if w <= b {
            return 0.0;
        }
        (((f64::from(v) - b) / (w - b)).clamp(0.0, 1.0)) as f32
    }

    fn cfa_at(&self, rx: u32, ry: u32) -> Option<u8> {
        self.info.cfa.map(|p| p[((ry % 2) * 2 + (rx % 2)) as usize])
    }

    /// One output pixel of a stage, as linear-ish floats in 0..1.
    fn stage_pixel(&self, stage: u32, x: u32, y: u32) -> [f32; 3] {
        let g = &self.geo;
        match stage {
            STAGE_PLANE | STAGE_MOSAIC | STAGE_DEMOSAIC => {
                let (rx, ry) = g.out_to_raw(x, y);
                let i = (ry as usize) * (g.raw_w as usize) + rx as usize;
                match stage {
                    STAGE_PLANE => {
                        let v = self.norm(self.raw[i]);
                        [v, v, v]
                    }
                    STAGE_MOSAIC => {
                        let v = self.norm(self.raw[i]);
                        let mut px = [0.0; 3];
                        match self.cfa_at(rx, ry) {
                            Some(ch) if ch < 3 => px[ch as usize] = v,
                            _ => px = [v, v, v],
                        }
                        px
                    }
                    _ => match &self.cam {
                        Some(cam) => [self.norm(cam[i * 3]), self.norm(cam[i * 3 + 1]), self.norm(cam[i * 3 + 2])],
                        None => {
                            let v = self.norm(self.raw[i]);
                            [v, v, v]
                        }
                    },
                }
            }
            _ => {
                let buf = if stage == STAGE_LINEAR { &self.lin } else { &self.srgb };
                let i = (y as usize) * (g.out_w as usize) + x as usize;
                if self.channels == 3 {
                    let k = i * 3;
                    [buf[k] as f32 / 65535.0, buf[k + 1] as f32 / 65535.0, buf[k + 2] as f32 / 65535.0]
                } else {
                    let v = buf[i] as f32 / 65535.0;
                    [v, v, v]
                }
            }
        }
    }

    /// Preview size for a given maximum width (keeps aspect, never upscales).
    pub fn preview_dims(&self, max_w: u32) -> (u32, u32) {
        let (w, h) = (self.geo.out_w, self.geo.out_h);
        if w <= max_w {
            return (w, h);
        }
        let ph = ((u64::from(h) * u64::from(max_w)) / u64::from(w)).max(1) as u32;
        (max_w, ph)
    }

    /// Box-downscaled RGBA8 view of a whole stage. Stages 0-3 are shown linear
    /// (honest: that is why they look dark); stage 4 is display-encoded.
    pub fn preview(&self, stage: u32, max_w: u32) -> Vec<u8> {
        let (pw, ph) = self.preview_dims(max_w);
        let (ow, oh) = (self.geo.out_w, self.geo.out_h);
        let mut out = vec![0u8; (pw as usize) * (ph as usize) * 4];
        for py in 0..ph {
            let y0 = (u64::from(py) * u64::from(oh) / u64::from(ph)) as u32;
            let y1 = ((u64::from(py + 1) * u64::from(oh) / u64::from(ph)) as u32).max(y0 + 1);
            for px in 0..pw {
                let x0 = (u64::from(px) * u64::from(ow) / u64::from(pw)) as u32;
                let x1 = ((u64::from(px + 1) * u64::from(ow) / u64::from(pw)) as u32).max(x0 + 1);
                let mut acc = [0.0f32; 3];
                let mut n = 0.0f32;
                for y in y0..y1.min(oh) {
                    for x in x0..x1.min(ow) {
                        let p = self.stage_pixel(stage, x, y);
                        acc[0] += p[0];
                        acc[1] += p[1];
                        acc[2] += p[2];
                        n += 1.0;
                    }
                }
                let k = ((py as usize) * (pw as usize) + px as usize) * 4;
                for c in 0..3 {
                    out[k + c] = ((acc[c] / n.max(1.0)) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                }
                out[k + 3] = 255;
            }
        }
        out
    }

    /// Full-resolution RGBA8 crop of a stage for the loupe. `brighten` applies
    /// a plain 1/2.2 display gamma so detail in the linear stages is visible.
    pub fn region(&self, stage: u32, x0: u32, y0: u32, w: u32, h: u32, brighten: bool) -> Vec<u8> {
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..h {
            for x in 0..w {
                let (ox, oy) = (x0 + x, y0 + y);
                let k = ((y as usize) * (w as usize) + x as usize) * 4;
                if ox >= self.geo.out_w || oy >= self.geo.out_h {
                    out[k + 3] = 255;
                    continue;
                }
                let p = self.stage_pixel(stage, ox, oy);
                for c in 0..3 {
                    let v = if brighten && stage != STAGE_SRGB { p[c].max(0.0).powf(1.0 / 2.2) } else { p[c] };
                    out[k + c] = (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                }
                out[k + 3] = 255;
            }
        }
        out
    }

    /// Everything the page needs to explain one output pixel, as JSON.
    pub fn trace_json(&self, x: u32, y: u32) -> String {
        let g = &self.geo;
        if x >= g.out_w || y >= g.out_h {
            return "null".into();
        }
        let (rx, ry) = g.out_to_raw(x, y);
        let i = (ry as usize) * (g.raw_w as usize) + rx as usize;
        let rawv = self.raw[i];
        let oi = (y as usize) * (g.out_w as usize) + x as usize;
        let lin: Vec<u16> = (0..self.channels as usize).map(|c| self.lin[oi * self.channels as usize + c]).collect();
        let srgb: Vec<u16> = (0..self.channels as usize).map(|c| self.srgb[oi * self.channels as usize + c]).collect();
        let srgb8: Vec<u16> = srgb.iter().map(|&v| ((u32::from(v) * 255 + 32767) / 65535) as u16).collect();

        let mut s = String::new();
        s.push_str(&format!(
            "{{\"x\":{x},\"y\":{y},\"rx\":{rx},\"ry\":{ry},\"raw\":{rawv},\"black\":{},\"white\":{},",
            self.info.black, self.info.white
        ));
        s.push_str(&format!("\"filter\":{},", self.cfa_at(rx, ry).map(|c| c.to_string()).unwrap_or("null".into())));

        if let Some(cam) = &self.cam {
            let camv = [cam[i * 3], cam[i * 3 + 1], cam[i * 3 + 2]];
            // Same-colour photosites in the 3x3 neighbourhood, per channel: the
            // inputs a bilinear demosaic averages.
            let mut neigh: [Vec<(i32, i32, u16)>; 3] = [vec![], vec![], vec![]];
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = rx as i32 + dx;
                    let ny = ry as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= g.raw_w as i32 || ny >= g.raw_h as i32 {
                        continue;
                    }
                    if let Some(ch) = self.cfa_at(nx as u32, ny as u32) {
                        if ch < 3 {
                            let v = self.raw[(ny as usize) * (g.raw_w as usize) + nx as usize];
                            neigh[ch as usize].push((dx, dy, v));
                        }
                    }
                }
            }
            s.push_str(&format!("\"cam\":[{},{},{}],", camv[0], camv[1], camv[2]));
            s.push_str("\"neighbours\":[");
            for (c, list) in neigh.iter().enumerate() {
                if c > 0 {
                    s.push(',');
                }
                s.push('[');
                for (k, (dx, dy, v)) in list.iter().enumerate() {
                    if k > 0 {
                        s.push(',');
                    }
                    s.push_str(&format!("[{dx},{dy},{v}]"));
                }
                s.push(']');
            }
            s.push_str("],");
            let n: Vec<String> = camv.iter().map(|&v| format!("{:.5}", self.norm(v))).collect();
            s.push_str(&format!("\"cam_norm\":[{}],", n.join(",")));
        } else {
            s.push_str(&format!("\"norm\":{:.5},", self.norm(rawv)));
        }
        let j = |v: &[u16]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        s.push_str(&format!("\"lin\":[{}],\"srgb16\":[{}],\"srgb8\":[{}]}}", j(&lin), j(&srgb), j(&srgb8)));
        s
    }

    /// Buffers this demo allocated (the library writes into them).
    pub fn buffers(&self) -> Vec<(&'static str, usize)> {
        let mut v = vec![("file bytes", self.input_bytes), ("raw plane", self.raw.len() * 2)];
        if let Some(c) = &self.cam {
            v.push(("camera RGB", c.len() * 2));
        }
        v.push(("linear output", self.lin.len() * 2));
        v.push(("sRGB output", self.srgb.len() * 2));
        v
    }

    pub fn info_json(&self) -> String {
        let i = &self.info;
        let g = &self.geo;
        let opt_u = |o: Option<u32>| o.map(|v| v.to_string()).unwrap_or("null".into());
        let cfa = i.cfa.map(|p| format!("[{},{},{},{}]", p[0], p[1], p[2], p[3])).unwrap_or("null".into());
        let neutral = i
            .as_shot_neutral
            .map(|n| format!("[{:.5},{:.5},{:.5}]", n[0], n[1], n[2]))
            .unwrap_or("null".into());
        let matrix = self
            .matrix
            .map(|m| {
                let rows: Vec<String> = m.iter().map(|r| format!("[{:.4},{:.4},{:.4}]", r[0], r[1], r[2])).collect();
                format!("[{}]", rows.join(","))
            })
            .unwrap_or("null".into());
        let timings: Vec<String> = self.timings.iter().map(|(n, ms)| format!("[\"{n}\",{ms:.2}]")).collect();
        let buffers: Vec<String> = self.buffers().iter().map(|(n, b)| format!("[\"{n}\",{b}]")).collect();
        format!(
            concat!(
                "{{\"make\":{},\"model\":{},\"bits\":{},\"black\":{},\"white\":{},\"cfa\":{},",
                "\"as_shot_neutral\":{},\"illuminant_1\":{},\"illuminant_2\":{},\"forward_matrix\":{},",
                "\"compression\":{},\"raw_w\":{},\"raw_h\":{},\"out_w\":{},\"out_h\":{},\"crop_x\":{},",
                "\"crop_y\":{},\"orientation\":{},\"matrix\":{},\"timings\":[{}],\"buffers\":[{}],",
                "\"sha256\":\"{}\",\"channels\":{},\"opcode_lists\":[{},{},{}]}}"
            ),
            json_str(&i.make),
            json_str(&i.model),
            i.bits,
            i.black,
            i.white,
            cfa,
            neutral,
            opt_u(i.illuminant_1),
            opt_u(i.illuminant_2),
            i.forward_matrix,
            json_str(&i.compression),
            g.raw_w,
            g.raw_h,
            g.out_w,
            g.out_h,
            g.crop_x,
            g.crop_y,
            g.orientation,
            matrix,
            timings.join(","),
            buffers.join(","),
            self.sha256,
            self.channels,
            i.opcode_lists[0],
            i.opcode_lists[1],
            i.opcode_lists[2]
        )
    }

    /// Checks the output->raw mapping and the recovered matrix against the
    /// library's own output on `samples` random pixels. Returns
    /// (pixels compared, max |error| in 1/65535 units).
    pub fn self_check(&self, samples: u32) -> (u32, f64) {
        let errs = self.check_errors(samples);
        (errs.len() as u32, errs.last().copied().unwrap_or(0.0))
    }

    /// Per-pixel |mapped - library| errors, sorted ascending.
    pub fn check_errors(&self, samples: u32) -> Vec<f64> {
        let mut errs: Vec<f64> = Vec::new();
        let g = &self.geo;
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..samples {
            let mut worst = 0.0f64;
            let x = (next() % u64::from(g.out_w)) as u32;
            let y = (next() % u64::from(g.out_h)) as u32;
            let (rx, ry) = g.out_to_raw(x, y);
            let i = (ry as usize) * (g.raw_w as usize) + rx as usize;
            let oi = (y as usize) * (g.out_w as usize) + x as usize;
            if self.cam.is_some() && self.matrix.is_none() {
                continue;
            }
            if let (Some(cam), Some(m)) = (&self.cam, &self.matrix) {
                let c = [self.norm(cam[i * 3]) as f64, self.norm(cam[i * 3 + 1]) as f64, self.norm(cam[i * 3 + 2]) as f64];
                for r in 0..3 {
                    let lib = f64::from(self.lin[oi * 3 + r]);
                    if lib == 0.0 || lib == 65535.0 {
                        continue;
                    }
                    let mine = (m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]) * 65535.0;
                    worst = worst.max((mine - lib).abs());
                }
            } else {
                let lib = f64::from(self.lin[oi]);
                let mine = f64::from(self.norm(self.raw[i])) * 65535.0;
                worst = worst.max((mine - lib).abs());
            }
            errs.push(worst);
        }
        errs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        errs
    }
}

pub fn hash_u16(v: &[u16]) -> String {
    let mut h = Sha256::new();
    // Little-endian bytes on every platform, so native and wasm hash the same thing.
    let mut buf = Vec::with_capacity(64 * 1024);
    for chunk in v.chunks(32 * 1024) {
        buf.clear();
        for s in chunk {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        h.update(&buf);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Run the pipeline on possibly-damaged bytes and report how it ended, as JSON:
/// `{"ok":true,"w":..,"h":..}` or `{"ok":false,"variant":"Truncated","message":".."}`.
/// Used by the "corrupt this file" test; nothing is kept.
pub fn try_develop(bytes: &[u8]) -> String {
    let limit = |s: &Sensor| check_size(s.width, s.height);
    let result = (|| -> Result<Result<(u32, u32), String>, irradiance::Error> {
        let c = Container::parse(bytes)?;
        let s = c.sensor()?;
        if let Err(e) = limit(&s) {
            return Ok(Err(e));
        }
        let mut raw = vec![0u16; (s.width as usize) * (s.height as usize)];
        irradiance::plane::unpack_into(&s, c.byte_order(), bytes, &mut raw)?;
        let (ow, oh) = irradiance::develop::output_dimensions(&s)?;
        let onpx = (ow as usize) * (oh as usize);
        let lin = if s.color_filter.is_some() {
            let mut lin = vec![0u16; onpx * 3];
            irradiance::color::develop_color_into(&s, &raw, &mut lin)?;
            lin
        } else {
            let mut lin = vec![0u16; onpx];
            irradiance::develop::develop_into(&s, &raw, &mut lin)?;
            lin
        };
        let mut srgb = vec![0u16; lin.len()];
        irradiance::tone::encode_srgb16_into(&lin, &mut srgb)?;
        Ok(Ok((ow, oh)))
    })();
    match result {
        Ok(Ok((w, h))) => format!("{{\"ok\":true,\"w\":{w},\"h\":{h}}}"),
        Ok(Err(demo)) => format!(
            "{{\"ok\":false,\"demo_limit\":true,\"variant\":\"DemoSizeLimit\",\"message\":{}}}",
            json_str(&demo)
        ),
        Err(e) => {
            let dbg = format!("{e:?}");
            let variant: String = dbg.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            format!("{{\"ok\":false,\"variant\":{},\"message\":{}}}", json_str(&variant), json_str(&e.to_string()))
        }
    }
}

/// Keep `ByteOrder` in the public surface used by bins without another import.
pub fn byte_order_name(b: ByteOrder) -> &'static str {
    match b {
        ByteOrder::Little => "little-endian",
        ByteOrder::Big => "big-endian",
    }
}
