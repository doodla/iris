//! Capture the compilation target triple for `iris --version` / `iris version`.

fn main() {
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=IRIS_TARGET={target}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
