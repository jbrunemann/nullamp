//! Nullamp: a retro-inspired music and DJ mix player.

// Release builds on Windows are GUI-subsystem programs so no console window opens behind the player.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    nullamp::run()
}
