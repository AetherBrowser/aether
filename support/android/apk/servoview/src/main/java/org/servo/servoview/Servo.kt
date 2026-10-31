/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
package org.servo.servoview

import android.content.Context
import android.util.Size
import android.view.KeyEvent as AndroidKeyEvent
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import android.view.View
import android.view.inputmethod.BaseInputConnection
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.Interaction
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.nativeKeyCode
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.input.pointer.pointerInteropFilter
import androidx.compose.ui.node.ModifierNodeElement
import androidx.compose.ui.platform.PlatformTextInputModifierNode
import androidx.compose.ui.platform.establishTextInputSession
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.LifecycleResumeEffect
import java.text.BreakIterator
import java.util.concurrent.Executors
import kotlin.coroutines.EmptyCoroutineContext
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.launch

@Composable
fun Servo(
    softKeyboardVisible: Boolean,
    servo: Servo,
    navigator: ServoNavigator,
    modifier: Modifier = Modifier,
) {
    LifecycleResumeEffect(servo) {
        servo.suspend(false)
        onPauseOrDispose { servo.suspend(true) }
    }
    LaunchedEffect(Unit) {
        while (true) {
            withFrameNanos { servo.onDoFrame() }
        }
    }
    LaunchedEffect(servo, navigator) { navigator.consumeNavigationEvents(servo) }

    val focusRequester = remember { FocusRequester() }
    AndroidView(
        factory = { context -> ServoView(context, servo) },
        modifier =
            modifier
                .focusRequester(focusRequester)
                .focusable()
                .softKeyboardVisible(softKeyboardVisible, servo)
                .onKeyEvent { keyEvent ->
                    when (keyEvent.type) {
                        KeyEventType.KeyDown if keyEvent.key != Key.Back -> {
                            servo.onKeyDown(
                                keyEvent.key.nativeKeyCode,
                                keyEvent.nativeKeyEvent,
                            )
                            true
                        }

                        KeyEventType.KeyUp if keyEvent.key != Key.Back -> {
                            servo.onKeyUp(
                                keyEvent.key.nativeKeyCode,
                                keyEvent.nativeKeyEvent,
                            )
                            true
                        }

                        else -> false
                    }
                }
                .pointerInteropFilter(null) { motionEvent ->
                    focusRequester.requestFocus()

                    val action = motionEvent.actionMasked
                    val pointerIndex = motionEvent.actionIndex
                    val pointerId = motionEvent.getPointerId(pointerIndex)
                    val x = motionEvent.getX(pointerIndex)
                    val y = motionEvent.getY(pointerIndex)

                    when (action) {
                        MotionEvent.ACTION_DOWN,
                        MotionEvent.ACTION_POINTER_DOWN -> servo.touchDown(x, y, pointerId)
                        MotionEvent.ACTION_MOVE -> servo.touchMove(x, y, pointerId)
                        MotionEvent.ACTION_UP,
                        MotionEvent.ACTION_POINTER_UP -> servo.touchUp(x, y, pointerId)
                        MotionEvent.ACTION_CANCEL -> servo.touchCancel(x, y, pointerId)
                    }

                    true
                },
        onRelease = { servo.glDispatcher.close() },
    )
}

@Stable
class ServoNavigator {
    private sealed interface NavigationEvent : Interaction {
        data class Navigate(val uri: String) : NavigationEvent

        data object Back : NavigationEvent

        data object Forward : NavigationEvent

        data object Reload : NavigationEvent

        data object Stop : NavigationEvent
    }

    private val coroutineScope = CoroutineScope(EmptyCoroutineContext)
    private val navigationEvents = MutableSharedFlow<NavigationEvent>()

    internal suspend fun consumeNavigationEvents(servo: Servo) {
        navigationEvents.collect { navigationEvent ->
            when (navigationEvent) {
                is NavigationEvent.Navigate -> servo.loadUri(navigationEvent.uri)
                NavigationEvent.Back -> servo.goBack()
                NavigationEvent.Forward -> servo.goForward()
                NavigationEvent.Reload -> servo.reload()
                NavigationEvent.Stop -> servo.stop()
            }
        }
    }

    var canGoBack by mutableStateOf(false)
        internal set

    var canGoForward by mutableStateOf(false)
        internal set

    fun navigate(uri: String) {
        coroutineScope.launch { navigationEvents.emit(NavigationEvent.Navigate(uri)) }
    }

    fun back() {
        coroutineScope.launch { navigationEvents.emit(NavigationEvent.Back) }
    }

    fun forward() {
        coroutineScope.launch { navigationEvents.emit(NavigationEvent.Forward) }
    }

    fun reload() {
        coroutineScope.launch { navigationEvents.emit(NavigationEvent.Reload) }
    }

    fun stop() {
        coroutineScope.launch { navigationEvents.emit(NavigationEvent.Stop) }
    }
}

private fun Modifier.softKeyboardVisible(visible: Boolean, servo: Servo) =
    this then SoftKeyboardVisible(visible, servo)

private data class SoftKeyboardVisible(val visible: Boolean, val servo: Servo) :
    ModifierNodeElement<SoftKeyboardVisibleNode>() {
    override fun create() = SoftKeyboardVisibleNode(servo)

    override fun update(node: SoftKeyboardVisibleNode) {
        node.servo = servo
        node.update(visible)
    }
}

private class SoftKeyboardVisibleNode(var servo: Servo) :
    Modifier.Node(), PlatformTextInputModifierNode {
    private var textInputSessionJob: Job? = null

    fun update(visible: Boolean) {
        textInputSessionJob?.cancel()
        if (visible) {
            val servo = servo
            textInputSessionJob = coroutineScope.launch {
                establishTextInputSession {
                    startInputMethod { _ -> ServoInputConnection(view, servo) }
                }
            }
        }
    }
}

/**
 * Soft keyboards deliver typed text here, not as key events. The previous [BaseInputConnection]
 * dropped that text, so only Enter (as a newline) reached the field and was stored as a space.
 */
private class ServoInputConnection(view: View, private val servo: Servo) :
    BaseInputConnection(view, true) {
    private var composing = false
    private var composingText = ""

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        super.commitText(text, newCursorPosition)
        val raw = text?.toString().orEmpty()
        if (raw.isEmpty()) {
            replacePreeditAndEnd()
            return true
        }
        var index = 0
        while (index < raw.length) {
            val breakAt = raw.indexOfAny(charArrayOf('\r', '\n'), index)
            val end = if (breakAt < 0) raw.length else breakAt
            val segment = raw.substring(index, end)
            if (segment.isNotEmpty()) {
                commitSegment(segment)
            }
            if (breakAt < 0) {
                break
            }
            // An empty segment still has to clear an open preedit before Enter.
            replacePreeditAndEnd()
            servo.sendKey(AndroidKeyEvent.KEYCODE_ENTER, 0)
            index = breakAt + 1
            if (raw[breakAt] == '\r' && index < raw.length && raw[index] == '\n') {
                index++
            }
        }
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        super.setComposingText(text, newCursorPosition)
        val value = text?.toString().orEmpty()
        if (!composing) {
            servo.imeComposition(COMPOSITION_START, "")
            composing = true
        }
        composingText = value
        servo.imeComposition(COMPOSITION_UPDATE, value)
        return true
    }

    override fun finishComposingText(): Boolean {
        super.finishComposingText()
        if (composing) {
            servo.imeComposition(COMPOSITION_END, composingText)
            composing = false
            composingText = ""
        }
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        // Lengths are UTF-16 code units. Backspace deletes one grapheme, so an
        // emoji (two code units) must not become two deletions.
        val before =
            if (beforeLength > 0) {
                getTextBeforeCursor(beforeLength, 0)?.toString().orEmpty()
            } else {
                ""
            }
        val after =
            if (afterLength > 0) {
                getTextAfterCursor(afterLength, 0)?.toString().orEmpty()
            } else {
                ""
            }
        super.deleteSurroundingText(beforeLength, afterLength)
        repeat(deletionCount(before, beforeLength)) {
            servo.sendKey(AndroidKeyEvent.KEYCODE_DEL, 0)
        }
        repeat(deletionCount(after, afterLength)) {
            servo.sendKey(AndroidKeyEvent.KEYCODE_FORWARD_DEL, 0)
        }
        return true
    }

    private fun commitSegment(value: String) {
        if (!composing) {
            servo.imeComposition(COMPOSITION_START, "")
        }
        servo.imeComposition(COMPOSITION_UPDATE, value)
        servo.imeComposition(COMPOSITION_END, value)
        composing = false
        composingText = ""
    }

    private fun replacePreeditAndEnd() {
        if (!composing) {
            return
        }
        servo.imeComposition(COMPOSITION_UPDATE, "")
        servo.imeComposition(COMPOSITION_END, "")
        composing = false
        composingText = ""
    }
}

private fun deletionCount(text: String, requested: Int): Int {
    if (requested <= 0 || text.isEmpty()) {
        return requested.coerceAtLeast(0)
    }
    val iterator = BreakIterator.getCharacterInstance()
    iterator.setText(text)
    var graphemes = 0
    while (iterator.next() != BreakIterator.DONE) {
        graphemes++
    }
    // Code units the connection never stored are still deleted one per unit.
    return graphemes + (requested - text.length).coerceAtLeast(0)
}

private const val COMPOSITION_START = 0
private const val COMPOSITION_UPDATE = 1
private const val COMPOSITION_END = 2

class Servo(
    args: String?,
    url: String?,
    logStr: String?,
    experimentalMode: Boolean,
    private val scope: CoroutineScope,
    client: Client,
    context: Context,
    navigator: ServoNavigator,
) {
    private val jni = JNIServo()
    internal val glDispatcher = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val servoCallbacks = Callbacks(client, jni, scope, glDispatcher, navigator)

    init {
        scope.launch(glDispatcher) {
            jni.init(
                context,
                args,
                url,
                logStr,
                experimentalMode,
                servoCallbacks,
            )
        }
    }

    fun addPlatformWindow(
        size: Size,
        density: Float,
        surface: Surface,
    ) {
        scope.launch(glDispatcher) {
            jni.addPlatformWindow(
                size,
                density,
                surface,
            )
        }
    }

    fun resize(size: Size) {
        scope.launch(glDispatcher) { jni.resize(size) }
    }

    fun reload() {
        scope.launch(glDispatcher) { jni.reload() }
    }

    fun stop() {
        scope.launch(glDispatcher) { jni.stop() }
    }

    fun goBack() {
        scope.launch(glDispatcher) { jni.goBack() }
    }

    fun goForward() {
        scope.launch(glDispatcher) { jni.goForward() }
    }

    fun loadUri(uri: String) {
        scope.launch(glDispatcher) { jni.loadUri(uri) }
    }

    fun onKeyDown(keyCode: Int, event: KeyEvent) {
        scope.launch(glDispatcher) { jni.keydown(keyCode, event.unicodeChar) }
    }

    fun sendKey(keyCode: Int, unicode: Int) {
        scope.launch(glDispatcher) {
            jni.keydown(keyCode, unicode)
            jni.keyup(keyCode, unicode)
        }
    }

    fun imeComposition(state: Int, text: String) {
        scope.launch(glDispatcher) { jni.imeComposition(state, text) }
    }

    fun onKeyUp(keyCode: Int, event: KeyEvent) {
        scope.launch(glDispatcher) { jni.keyup(keyCode, event.unicodeChar) }
    }

    fun touchDown(x: Float, y: Float, pointerId: Int) {
        scope.launch(glDispatcher) { jni.touchDown(x, y, pointerId) }
    }

    fun touchMove(x: Float, y: Float, pointerId: Int) {
        scope.launch(glDispatcher) { jni.touchMove(x, y, pointerId) }
    }

    fun touchUp(x: Float, y: Float, pointerId: Int) {
        scope.launch(glDispatcher) { jni.touchUp(x, y, pointerId) }
    }

    fun touchCancel(x: Float, y: Float, pointerId: Int) {
        scope.launch(glDispatcher) { jni.touchCancel(x, y, pointerId) }
    }

    fun pausePainting() {
        scope.launch(glDispatcher) { jni.pausePainting() }
    }

    fun resumePainting(surface: Surface, size: Size) {
        scope.launch(glDispatcher) { jni.resumePainting(surface, size) }
    }

    fun suspend(suspended: Boolean) {
        servoCallbacks.suspended = suspended
    }

    fun mediaSessionAction(action: Int) {
        scope.launch(glDispatcher) { jni.mediaSessionAction(action) }
    }

    fun setExperimentalMode(enable: Boolean) {
        scope.launch(glDispatcher) { jni.setExperimentalMode(enable) }
    }

    fun onDoFrame() {
        scope.launch(glDispatcher) { jni.doFrame() }
    }

    interface Client {
        fun onAlert(message: String)

        fun onLoadStarted()

        fun onLoadEnded()

        fun onTitleChanged(title: String)

        fun onUrlChanged(url: String)

        fun onImeShow()

        fun onImeHide()

        fun onMediaSessionMetadata(title: String, artist: String, album: String)

        fun onMediaSessionPlaybackStateChange(state: Int)

        fun onMediaSessionSetPositionState(duration: Float, position: Float, playbackRate: Float)
    }

    private class Callbacks(
        private var client: Client,
        private val jni: JNIServo,
        private val scope: CoroutineScope,
        private val glDispatcher: CoroutineDispatcher,
        private val navigator: ServoNavigator,
    ) : JNIServo.Callbacks, Client {
        var suspended: Boolean = false

        override fun wakeup() {
            if (!suspended) {
                scope.launch(glDispatcher) { jni.performUpdates() }
            }
        }

        override fun onAlert(message: String) {
            scope.launch { client.onAlert(message) }
        }

        override fun onImeShow() {
            scope.launch { client.onImeShow() }
        }

        override fun onImeHide() {
            scope.launch { client.onImeHide() }
        }

        override fun onLoadStarted() {
            scope.launch { client.onLoadStarted() }
        }

        override fun onLoadEnded() {
            scope.launch { client.onLoadEnded() }
        }

        override fun onTitleChanged(title: String) {
            scope.launch { client.onTitleChanged(title) }
        }

        override fun onUrlChanged(url: String) {
            scope.launch { client.onUrlChanged(url) }
        }

        override fun onHistoryChanged(canGoBack: Boolean, canGoForward: Boolean) {
            navigator.canGoBack = canGoBack
            navigator.canGoForward = canGoForward
        }

        override fun onMediaSessionMetadata(title: String, artist: String, album: String) {
            scope.launch { client.onMediaSessionMetadata(title, artist, album) }
        }

        override fun onMediaSessionPlaybackStateChange(state: Int) {
            scope.launch { client.onMediaSessionPlaybackStateChange(state) }
        }

        override fun onMediaSessionSetPositionState(
            duration: Float,
            position: Float,
            playbackRate: Float,
        ) {
            scope.launch { client.onMediaSessionSetPositionState(duration, position, playbackRate) }
        }
    }
}
