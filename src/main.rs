#![cfg_attr(windows, windows_subsystem = "windows")]

mod allocator;
mod api;
mod app;
mod login;
mod model;
mod mpv;
mod network;
mod power;

fn main() {
    allocator::configure();
    app::launch();
}
