use std::path::PathBuf;

fn main() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config =
        cbindgen::Config::from_file(crate_dir.join("cbindgen.toml")).expect("read cbindgen.toml");
    let bindings = cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .expect("generate HoloirohIosBridge.h");
    let output = crate_dir.join("include/HoloirohIosBridge.h");
    bindings.write_to_file(&output);
    assert!(
        output.is_file(),
        "cbindgen did not create {}",
        output.display()
    );
    println!("generated {}", output.display());
}
