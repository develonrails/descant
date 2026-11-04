//! Compiles the GSettings schema, so `cargo run` from a checkout finds it
//! (the Flatpak installs and compiles its own copy), and the resources: the
//! icons Adwaita lacks, built into the binary.

use std::{env, fs, path::PathBuf, process::Command};

const SCHEMA: &str = "data/io.github.develonrails.Descant.gschema.xml";
const RESOURCES: &str = "data/descant.gresource.xml";
const ICONS: &str = "data/icons/actions";

fn main() {
    println!("cargo:rerun-if-changed={SCHEMA}");
    println!("cargo:rerun-if-changed={RESOURCES}");
    println!("cargo:rerun-if-changed={ICONS}");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(ICONS)
        .arg("--target")
        .arg(out_dir.join("descant.gresource"))
        .arg(RESOURCES)
        .status()
        .expect("glib-compile-resources (part of GLib) is needed to build Descant");
    assert!(status.success(), "the resources do not compile");

    let out = out_dir.join("schemas");
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
