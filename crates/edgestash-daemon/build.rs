use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifest = if cfg!(feature = "uiaccess") {
        manifest_dir.join("manifests/edgestash-daemon-uiaccess.manifest")
    } else {
        manifest_dir.join("manifests/edgestash-daemon.manifest")
    };

    println!("cargo:rerun-if-changed={}", manifest.display());

    #[cfg(target_env = "msvc")]
    {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
