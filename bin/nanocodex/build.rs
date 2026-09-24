mod build_version;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build_version.rs");
    build_version::emit()
}
