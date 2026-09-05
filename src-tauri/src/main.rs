//! BEASTUBE desktop entry point.
//!
//! The real work lives in the library crate so that integration tests and benchmarks can drive the
//! application without going through the binary.

// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    beastube_app_lib::run();
}
