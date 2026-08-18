mod allocator;
mod api;
mod app;
mod login;
mod model;
mod mpv;
mod search_input;

fn main() {
    allocator::configure();
    app::launch();
}
