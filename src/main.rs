fn main() {
    std::process::exit(mesh::cli::run(std::env::args().skip(1).collect()));
}
