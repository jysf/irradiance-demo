//! Native twin of the browser demo: develops a DNG with the same code and
//! prints the SHA-256 the web page shows, so the two can be compared.
//!
//!     cargo run --release --bin hash -- photo.dng [--verify]
//!
//! `--verify` also checks the page's output->raw mapping and recovered colour
//! matrix against the library's own output on random pixels.

use irradiance_demo_core::Developed;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: hash <file.dng> [--verify]");
        std::process::exit(2);
    };
    let verify = args.iter().any(|a| a == "--verify");
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(1);
    });
    let t0 = Instant::now();
    let clock = || t0.elapsed().as_secs_f64() * 1000.0;
    let d = match Developed::run(bytes, &clock) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    println!("sha256  {}", d.sha256);
    println!("output  {} x {} x {}", d.geo.out_w, d.geo.out_h, d.channels);
    for (name, ms) in &d.timings {
        println!("{name:<8}{ms:>9.1} ms");
    }
    if verify {
        let e = d.check_errors(5000);
        let pct = |q: f64| e[((e.len() as f64 - 1.0) * q) as usize];
        println!(
            "verify  {} px  |mapped - library| /65535: median {:.1}  p90 {:.1}  p99 {:.1}  max {:.1}",
            e.len(), pct(0.5), pct(0.9), pct(0.99), pct(1.0)
        );
        if let Some(m) = d.matrix {
            println!("matrix  {:?}", m);
        }
    }
}
