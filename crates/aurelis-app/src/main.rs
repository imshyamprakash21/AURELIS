mod application;
mod cli;
mod input;
mod library_browser;
mod library_play_test;
mod library_search;
mod library_test;
mod player_controller;
mod presentation;

/* ============================================================
 * MAIN
 * ============================================================
 */

fn main() {
    aurelis_core::initialize();
    cli::run();
}
