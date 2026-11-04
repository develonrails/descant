//! Compiles the GSettings schema, so `cargo run` from a checkout finds it.
//! The Flatpak installs and compiles its own copy.

use std::{env, fs, path::PathBuf, process::Command};

const SCHEMA: &str = "data/io.github.develonrails.Descant.gschema.xml";

fn main() {
    println!("cargo:rerun-if-changed={SCHEMA}");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("schemas");
    fs::create_dir_all(&out).expect("schema directory");
    fs::copy(
        SCHEMA,
        out.join("io.github.develonrails.Descant.gschema.xml"),
    )
    .expect("schema copy");
    let status = Command::new("glib-compile-schemas")
        .arg("--strict")
        .arg(&out)
        .status()
        .expect("glib-compile-schemas (part of GLib) is needed to build Descant");
    assert!(status.success(), "the GSettings schema does not compile");
    println!("cargo:rustc-env=DESCANT_SCHEMA_DIR={}", out.display());
}
