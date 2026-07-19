//! `eos-notify` — send a desktop notification: `eos-notify <title> [body]`.
//!
//! Minimal transport: write `"title\nbody"` to `/tmp/eos-notify`, which the
//! `eos-notifyd` daemon picks up and shows as a toast. The file is a deliberate
//! placeholder for a proper `notify:` scheme / socket (see eos-notifyd).

use std::{env, fs, process};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: eos-notify <title> [body]");
        process::exit(2);
    }
    let body = args.get(1).cloned().unwrap_or_default();
    // The daemon splits on the first newline, so a title can't contain one.
    let msg = format!("{}\n{}", args[0].replace('\n', " "), body);
    if let Err(err) = fs::write("/tmp/eos-notify", msg) {
        eprintln!("eos-notify: {err}");
        process::exit(1);
    }
}
