# Anatomy of a RAW

**Live demo: https://jysf.github.io/irradiance-demo/**

An interactive demo of [irradiance](https://crates.io/crates/irradiance), a
pure-Rust camera RAW library, compiled to WebAssembly. Open a DNG (or use the
bundled sample) and step through what happens to it:

1. **Sensor plane:** one number per photosite (`plane::unpack_into`)
2. **Filter mosaic:** the same numbers, painted by colour filter
3. **Demosaic:** two of every three colour values interpolated (`demosaic::demosaic_into`)
4. **White balance + colour matrix**, crop and orientation (`color::develop_color_into`,
   or `develop::develop_into` for monochrome sensors)
5. **Tone curve:** sRGB encoding (`tone::encode_srgb16_into`)

It also has:

- **One pixel, all the way through.** Hover anywhere to see that pixel's raw
  count, its filter colour, the neighbours the demosaic used, the white
  balance and colour matrix, and the final sRGB value. Values labelled with a
  library call are exactly what irradiance produced.
- **What it cost.** Per-step timings, the buffers the page allocated, the
  network requests made while developing (zero), and a SHA-256 of the output.
- **Try to break it.** Damages the file 30 ways (truncation, bit flips,
  lying tag counts, offsets past the end, scrambled IFD entries) and runs
  every copy through the full pipeline in a separate worker. Each one either
  decodes or returns a typed error.

Nothing is uploaded: the file is read and developed inside the browser tab.

## Layout

```
demo-core/        Rust crate: the pipeline, previews, pixel trace (native + wasm32)
  src/lib.rs      everything the page calls, as plain Rust
  src/wasm.rs     wasm-bindgen wrapper (compiled only for wasm32)
  src/bin/hash.rs native twin: prints the same SHA-256 the page shows
web/              the static site
  index.html, style.css, app.js, corrupt-worker.js
  pkg/            built WebAssembly (committed, so the site is plain static files)
  sample/         the default DNG
build.sh          rebuilds web/pkg
```

## Build and run

Requires `rustup` with the `wasm32-unknown-unknown` target and `wasm-pack`.

```sh
./build.sh
python3 -m http.server 8765 --directory web
# open http://localhost:8765
```

`build.sh` calls rustup's stable toolchain explicitly. If a Homebrew `rustc`
comes first on `PATH`, a plain `cargo build --target wasm32-unknown-unknown`
fails with "can't find crate for `std`", because Homebrew's Rust ships no
wasm32 standard library.

## Same bytes, native and in the browser

```sh
cd demo-core
cargo run --release --bin hash -- ../web/sample/nikon-d300s-cc0.dng
# sha256  72f319601d913714e32539146582e4869513375c43f3ba08f33fdbd52a97807e
```

The page shows the same hash for the same file. irradiance's tone curve is a
lookup table rather than `powf`, so the output does not depend on the
platform's maths library.

`--verify` also checks the page's own bookkeeping (mapping output pixels back
to photosites, and the colour matrix it recovers for the trace) against the
library's output on 5,000 random pixels. On files without opcode lists the
median difference is about 13/65535. Files with gain maps or lens corrections
(most phone DNGs, some Leicas) differ more, because those corrections happen
inside the develop step; the trace says so on those files.

## Limits

- DNG only. Vendor formats (NEF, CR2, ARW, PEF, ...) need converting first,
  with `dnglab convert` or Adobe DNG Converter, keeping the mosaic (not
  "linear"). Apple ProRAW is already demosaiced, so stages 2 and 3 have
  nothing to show.
- The page refuses files over 100 megapixels before allocating anything:
  irradiance never allocates on the caller's behalf, so the caller sets the
  ceiling, and a browser tab has a few GB of WebAssembly memory at most.

## Deploying

Every push to `main` publishes `web/` to GitHub Pages
(`.github/workflows/pages.yml`). Run `./build.sh` and commit `web/pkg` after
changing `demo-core`.

## Licence

MIT OR Apache-2.0, like irradiance. The sample photo is CC0 (see
`web/sample/README.md`).
