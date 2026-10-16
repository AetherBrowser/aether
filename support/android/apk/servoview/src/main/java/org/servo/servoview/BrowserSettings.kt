/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
package org.servo.servoview

/** Reads and writes the same profile settings as the desktop `servo:settings` page. */
class BrowserSettings {
    private val jni = JNIServo()

    fun home(query: String = ""): String = jni.homeSettings(query).orEmpty()

    fun searchEngines(query: String = ""): String = jni.searchEngines(query).orEmpty()

    fun about(): String = jni.aboutInfo().orEmpty()
}
