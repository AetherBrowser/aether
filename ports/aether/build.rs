/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Tag used to choose the channel and the suffix of `--version`. Empty means a
/// local `dev` build, which shows the git commit instead.
///
/// CI sets `AETHER_RELEASE_TAG` to one of:
/// - `YYYY-MM-DD` for a nightly
/// - `vX.Y.Z-<prerelease>` (for example `v0.1.0-rc1`) when `regular_release` is off
/// - `vX.Y.Z` (for example `v0.1.0`) for a regular release
fn release_tag() -> Result<String, String> {
    let tag = match std::env::var("AETHER_RELEASE_TAG") {
        Ok(tag) => tag,
        Err(std::env::VarError::NotPresent) => return Ok(String::new()),
        Err(error) => return Err(error.to_string()),
    };
    let tag = tag.trim();
    if tag.is_empty() || is_nightly_tag(tag) || is_release_tag(tag) || is_prerelease_tag(tag) {
        Ok(tag.to_owned())
    } else {
        Err(format!(
            "AETHER_RELEASE_TAG must be empty, YYYY-MM-DD, vX.Y.Z, or vX.Y.Z-<prerelease> (got {tag:?})"
        ))
    }
}

fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_nightly_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(year) = parts.next() else {
        return false;
    };
    let Some(month) = parts.next() else {
        return false;
    };
    let Some(day) = parts.next() else {
        return false;
    };
    parts.next().is_none() &&
        year.len() == 4 &&
        month.len() == 2 &&
        day.len() == 2 &&
        is_ascii_digits(year) &&
        is_ascii_digits(month) &&
        is_ascii_digits(day)
}

fn is_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let mut parts = rest.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none() &&
        is_ascii_digits(major) &&
        is_ascii_digits(minor) &&
        is_ascii_digits(patch)
}

fn is_prerelease_tag(tag: &str) -> bool {
    let Some((version, prerelease)) = tag.split_once('-') else {
        return false;
    };
    is_release_tag(version) &&
        !prerelease.is_empty() &&
        prerelease
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

fn git_sha() -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        let hash = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        Ok(hash.trim().to_owned())
    } else {
        let stderr = String::from_utf8(output.stderr).map_err(|e| e.to_string())?;
        Err(stderr)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rustc-check-cfg=cfg(servo_production)");
    println!("cargo::rustc-check-cfg=cfg(servo_do_not_use_in_production)");
    // The cfg is part of the rustc command line, so sccache will not reuse a
    // binary built for a different tag. The string itself is read via rustc-env.
    println!("cargo::rustc-check-cfg=cfg(aether_release_tag, values(any()))");
    println!("cargo:rerun-if-env-changed=AETHER_RELEASE_TAG");
    let tag = release_tag()?;
    println!("cargo:rustc-env=AETHER_RELEASE_TAG={tag}");
    if !tag.is_empty() {
        println!("cargo:rustc-cfg=aether_release_tag=\"{tag}\"");
    }
    // Cargo does not expose the profile name to crates or their build scripts,
    // but we can extract it from OUT_DIR and set a custom cfg() ourselves.
    let out = std::env::var("OUT_DIR")?;
    let out = Path::new(&out);
    let krate = out.parent().unwrap();
    let build = krate.parent().unwrap();
    let profile = build
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    if profile == "production" || profile.starts_with("production-") {
        println!("cargo:rustc-cfg=servo_production");
    } else {
        println!("cargo:rustc-cfg=servo_do_not_use_in_production");
    }

    // Note: We can't use `#[cfg(windows)]`, since that would check the host platform
    // and not the target platform
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap();

    if target_os == "windows" {
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../../resources/aether.ico");
            res.set_manifest_file("platform/windows/servoshell.exe.manifest");
            res.compile().unwrap();
        }
        #[cfg(not(windows))]
        panic!("Cross-compiling to windows is currently not supported");
    } else if target_os == "android" {
        // FIXME: We need this workaround since jemalloc-sys still links
        // to libgcc instead of libunwind, but Android NDK 23c and above
        // don't have libgcc. We can't disable jemalloc for Android as
        // in 64-bit aarch builds, the system allocator uses tagged
        // pointers by default which causes the assertions in SM & mozjs
        // to fail. See https://github.com/servo/servo/issues/32175.
        let mut libgcc = File::create(out.join("libgcc.a")).unwrap();
        libgcc.write_all(b"INPUT(-lunwind)").unwrap();
        println!("cargo:rustc-link-search=native={}", out.display());
    }

    match git_sha() {
        Ok(hash) => println!("cargo:rustc-env=GIT_SHA={}", hash),
        Err(error) => {
            println!(
                "cargo:warning=Could not generate git version information: {:?}",
                error
            );
            println!("cargo:rustc-env=GIT_SHA=nogit");
        },
    }

    // On MacOS, all dylib dependencies are shipped along with the binary
    // in the "/lib" directory. Setting the rpath here, allows the dynamic
    // linker to locate them. See `man dyld` for more info.
    if target_os == "macos" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/lib/");
    }

    // On OpenHarmony, libservoshell.so is loaded by ArkTS as a NAPI module.
    // Passing a version script allows us to inform the linker about required
    // symbol visibility (only one), which improves stripping of unused sections.
    if target_env == "ohos" {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
        let version_script = Path::new(&manifest_dir)
            .join("platform")
            .join("openharmony")
            .join("libservoshell.ver");
        let version_script_str = version_script.to_str().expect("Expected UTF-8 text");
        assert!(
            version_script.exists(),
            "Expected version script to exist at path `{version_script_str}`"
        );
        println!("cargo:rerun-if-changed={version_script_str}");
        // Using `rustc-link-arg-cdylib` causes a false-positive warning:
        // https://github.com/rust-lang/cargo/issues/16487
        // We work around this by just using the unconditional link-arg, which
        // should be fine, since we always build servo as a cdylib on OpenHarmony.
        println!("cargo:rustc-link-arg=-Wl,--version-script={version_script_str}");
    }
    Ok(())
}
