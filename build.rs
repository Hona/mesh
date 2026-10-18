// Unique id per build, so a running mux from an older build is replaced (SPEC §6.5).
// No rerun-if directives: cargo reruns this whenever a package file changes.
fn main() {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    println!("cargo:rustc-env=MESH_BUILD={t:x}");
}
