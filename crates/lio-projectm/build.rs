//! Finds libprojectM: the system's (pkg-config `projectM-4`, linked dynamically) if there
//! is one, otherwise the `third_party/libprojectM` submodule built with CMake and linked
//! statically. `LIO_PROJECTM_BUNDLED=1` skips the system library.
//!
//! Tells the crate which OpenGL flavour the library was built for (`projectm_gles`) and
//! tells Lio's build script how it was linked (`DEP_PROJECTM_LINKAGE`).

use std::path::{Path, PathBuf};
use std::{env, fs};

/// Oldest libprojectM with the API in `src/lib.rs`.
const MIN_VERSION: &str = "4.1";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(projectm_gles)");
    println!("cargo::rerun-if-env-changed=LIO_PROJECTM_BUNDLED");

    let bundled = env::var_os("LIO_PROJECTM_BUNDLED").is_some_and(|v| v != "0");
    if !bundled && let Some(gles) = system() {
        if gles {
            println!("cargo::rustc-cfg=projectm_gles");
        }
        println!("cargo::metadata=linkage=dynamic");
        return;
    }
    build_bundled();
    println!("cargo::rustc-cfg=projectm_gles");
    println!("cargo::metadata=linkage=static");
}

/// The system's libprojectM, if any: whether it was built for OpenGL ES.
fn system() -> Option<bool> {
    let library = pkg_config::Config::new()
        .atleast_version(MIN_VERSION)
        .probe("projectM-4")
        .ok()?;
    // pkg-config doesn't say which OpenGL; the CMake package config next to it does.
    let gles = library.link_paths.iter().any(|dir| {
        fs::read_to_string(dir.join("cmake/projectM4/projectM4Config.cmake"))
            .is_ok_and(|config| config.contains("if(\"ON\") # ENABLE_GLES"))
    });
    Some(gles)
}

/// Builds the submodule as a static library, for OpenGL ES 3 (what GTK gives GL areas
/// by default).
fn build_bundled() {
    let source =
        Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../third_party/libprojectM");
    if !source.join("vendor/projectm-eval/CMakeLists.txt").exists() {
        panic!(
            "libprojectM not found: install projectM 4.1 or newer (pkg-config projectM-4), \
             or fetch the bundled copy with `git submodule update --init --recursive`"
        );
    }
    println!("cargo::rerun-if-changed={}", source.display());

    let out = cmake::Config::new(&source)
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("ENABLE_GLES", "ON")
        .define("ENABLE_PLAYLIST", "OFF")
        .define("ENABLE_DEBUG_POSTFIX", "OFF")
        .define("ENABLE_SYSTEM_GLM", "OFF")
        .define("ENABLE_SYSTEM_PROJECTM_EVAL", "OFF")
        .define("BUILD_TESTING", "OFF")
        .define("BUILD_DOCS", "OFF")
        .define("ENABLE_SDL_UI", "OFF")
        // projectm-eval ships its generated parser; with flex and bison around it would
        // regenerate it in the source tree, dirtying the submodule.
        .define("CMAKE_DISABLE_FIND_PACKAGE_BISON", "ON")
        .define("CMAKE_DISABLE_FIND_PACKAGE_FLEX", "ON")
        .build();
    let lib: PathBuf = ["lib", "lib64"]
        .iter()
        .map(|dir| out.join(dir))
        .find(|dir| dir.join("libprojectM-4.a").exists())
        .expect("CMake built libprojectM-4.a");
    println!("cargo::rustc-link-search=native={}", lib.display());
    println!("cargo::rustc-link-lib=static=projectM-4");
    println!("cargo::rustc-link-lib=dylib=stdc++");
    println!("cargo::rustc-link-lib=dylib=GLESv2");
}
