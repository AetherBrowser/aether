/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

/// The `mpv` pkg-config module is versioned after the client API, so
/// "2.0" means "client API 2.x", which is what `ffi.rs` is written against.
const MIN_CLIENT_API: &str = "2.0";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_libmpv)");
    match pkg_config::Config::new()
        .atleast_version(MIN_CLIENT_API)
        .probe("mpv")
    {
        Ok(_) => {
            println!("cargo::rustc-cfg=has_libmpv");
        },
        Err(error) => {
            // Do not fail the build: type-checking and linting the crate does not need
            // libmpv. Tests that call into it are gated on `has_libmpv`, so
            // `cargo test --workspace` can run on machines without the library.
            println!("cargo:warning=libmpv >= {MIN_CLIENT_API} not found via pkg-config: {error}");
        },
    }
}
