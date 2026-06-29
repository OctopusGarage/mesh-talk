// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mesh_talk::run_tauri;

fn main() {
    // Run the Tauri version of the application
    run_tauri();
}
