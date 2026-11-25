# Sample DNG

`nikon-d300s-cc0.dng` is a Nikon D300S raw file from
[raw.pixls.us](https://raw.pixls.us/), dedicated to the public domain (CC0).
It was converted from the original NEF with `dnglab convert --embed-raw false`
(lossless-JPEG compressed, 11.6 MB), which develops byte-identically to the
uncompressed conversion.

To use a different default photo, replace this file, then update `SAMPLE` at the
top of `web/app.js` (file name, caption, and the loupe's starting point).
