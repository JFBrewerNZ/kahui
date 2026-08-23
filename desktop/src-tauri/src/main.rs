// Release builds have no console: this is a window, and a stray terminal behind
// it on Windows looks like something has gone wrong.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kahui_desktop_lib::run()
}
