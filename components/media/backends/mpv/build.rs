/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

/// The `mpv` pkg-config module is versioned after the client API, so
/// "2.0" means "client API 2.x", which is what `ffi.rs` is written against.
const MIN_CLIENT_API: &str = "2.0";

fn main() {
    match pkg_config::Config::new()
        .atleast_version(MIN_CLIENT_API)
        .probe("mpv")
    {
        Ok(_) => {},
        Err(error) => {
            // Do not fail the build: type-checking and linting the crate does not need
            // libmpv. Linking anything that uses it will fail with a clear linker error.
            println!("cargo:warning=libmpv >= {MIN_CLIENT_API} not found via pkg-config: {error}");
            println!("cargo:rustc-link-lib=mpv");
        },
    }
}
