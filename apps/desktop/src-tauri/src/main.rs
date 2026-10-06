// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    path_safety::harden_process();
    velox_desktop_lib::run();
}
