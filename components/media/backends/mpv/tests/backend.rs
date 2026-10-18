/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Tests against the libmpv installed on the machine.

use aether_media_mpv::MpvBackend;
use servo_media::{BackendInit, SupportsMediaType};

#[test]
fn can_play_type_reflects_the_linked_libmpv() {
    let backend = MpvBackend::init();

    // Any libmpv build has the mp3 demuxer and decoder, and AAC in MP4.
    assert_eq!(
        backend.can_play_type("audio/mpeg"),
        SupportsMediaType::Maybe
    );
    assert_eq!(
        backend.can_play_type("audio/mp4; codecs=\"mp4a.40.2\""),
        SupportsMediaType::Probably
    );
    assert_eq!(backend.can_play_type("video/x-nope"), SupportsMediaType::No);
    assert_eq!(
        backend.can_play_type("audio/mp4; codecs=\"not-a-codec\""),
        SupportsMediaType::No
    );
}

#[test]
fn backends_can_be_created_repeatedly() {
    // Each init probes with a temporary libmpv instance that must be fully released.
    for _ in 0..3 {
        let backend = MpvBackend::init();
        assert_eq!(
            backend.can_play_type("audio/mpeg"),
            SupportsMediaType::Maybe
        );
    }
}
