//! Typeset — a live LaTeX editor for macOS, written in Rust.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

mod compile;
mod complete;
mod convert;
mod fixes;
mod highlight;
mod hints;
mod sync;

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod editor;
#[cfg(target_os = "macos")]
mod paste;
#[cfg(target_os = "macos")]
mod preview;
#[cfg(target_os = "macos")]
mod selfcheck;
#[cfg(target_os = "macos")]
mod theme;

#[cfg(target_os = "macos")]
fn main() {
    if std::env::args().any(|a| a == "--security-check") {
        std::process::exit(selfcheck::run());
    }
    app::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Typeset is a macOS app (AppKit + PDFKit). Build it on a Mac with `cargo run --release`.");
}
