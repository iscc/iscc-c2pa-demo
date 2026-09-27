//! Desktop entry point; all logic lives in the library crate.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    iscc_c2pa_demo_lib::run()
}
