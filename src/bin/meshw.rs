//! Windowless build of `mesh` (SPEC §7.2): scheduled tasks and the elevated setup step.
#![windows_subsystem = "windows"]

fn main() {
    std::process::exit(mesh::cli::run_background(
        std::env::args().skip(1).collect(),
    ));
}
