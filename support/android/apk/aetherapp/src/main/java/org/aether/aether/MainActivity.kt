/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
package org.aether.aether

import android.content.Intent
import android.content.SharedPreferences
import android.os.Bundle
import android.system.ErrnoException
import android.system.Os
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.component1
import androidx.activity.result.component2
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.selectAll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SearchBar
import androidx.compose.material3.SearchBarDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberSearchBarState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.edit
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.lifecycleScope
import androidx.preference.PreferenceManager
import kotlinx.coroutines.launch
import org.json.JSONObject
import org.servo.servoview.BrowserSettings
import org.servo.servoview.Servo
import org.servo.servoview.ServoNavigator

class MainActivity : ComponentActivity(), Servo.Client {
    private val urlTextFieldState = TextFieldState()
    private var isRefreshing by mutableStateOf(false)
    private lateinit var historyManager: HistoryManager
    private var currentUrl = ""
    private var currentTitle = ""
    private var softKeyboardVisible by mutableStateOf(false)
    private var alertMessage by mutableStateOf<String?>(null)

    private class Settings(preferences: SharedPreferences) {
        var experimental = preferences.getBoolean("experimental", false)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val sharedPreferences = PreferenceManager.getDefaultSharedPreferences(applicationContext)
        val settings = Settings(sharedPreferences)
        val navigator = ServoNavigator()
        var experimentalModeEnabled by mutableStateOf(settings.experimental)
        val servo =
            Servo(
                args = intent.getStringExtra("servoargs"),
                url = if (Intent.ACTION_VIEW == intent.action) intent.data.toString() else null,
                logStr = intent.getStringExtra("servolog"),
                experimentalMode = experimentalModeEnabled,
                scope = lifecycleScope,
                client = this,
                context = this,
                navigator = navigator,
            )

        historyManager = HistoryManager(this)

        val historyActivityResultLauncher =
            registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
                (resultCode, data) ->
                if (resultCode == RESULT_OK && data != null) {
                    val url = data.getStringExtra("url")
                    if (!url.isNullOrEmpty()) {
                        urlTextFieldState.edit { replace(0, length, url) }
                        navigator.navigate(urlTextFieldState.text.toString())
                    }
                }
            }

        setContent {
            LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
                experimentalModeEnabled = Settings(sharedPreferences).experimental
            }

            val servoFocusRequester = remember { FocusRequester() }
            var menuExpanded by remember { mutableStateOf(false) }
            var menuPage by remember { mutableStateOf(AppMenuPage.Root) }
            fun closeMenu() {
                menuExpanded = false
                menuPage = AppMenuPage.Root
            }
            Scaffold(
                topBar = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        IconButton(onClick = { navigator.navigate(homeUrl()) }) {
                            Icon(painterResource(R.drawable.home), stringResource(R.string.home))
                        }
                        Omnibox(
                            urlTextFieldState,
                            onSearch = { search ->
                                navigator.navigate(search)
                                servoFocusRequester.requestFocus()
                            },
                            modifier = Modifier.weight(1f).padding(end = 10.dp),
                        )
                        if (isRefreshing) {
                            CircularProgressIndicator(
                                modifier = Modifier.padding(end = 10.dp).size(20.dp)
                            )
                        }
                        Box {
                            IconButton(
                                onClick = {
                                    menuPage = AppMenuPage.Root
                                    menuExpanded = true
                                }
                            ) {
                                Icon(
                                    painterResource(R.drawable.menu),
                                    stringResource(R.string.menu),
                                )
                            }
                            AppMenu(
                                expanded = menuExpanded,
                                page = menuPage,
                                onDismiss = ::closeMenu,
                                onPageChange = { menuPage = it },
                                canGoBack = navigator.canGoBack,
                                canGoForward = navigator.canGoForward,
                                isRefreshing = isRefreshing,
                                onBack = {
                                    closeMenu()
                                    onHistoryBackMenuItemClicked(navigator)
                                },
                                onForward = {
                                    closeMenu()
                                    onHistoryForwardMenuItemClicked(navigator)
                                },
                                onRefresh = {
                                    closeMenu()
                                    if (isRefreshing) onCancelMenuItemClicked(navigator)
                                    else onRefreshMenuItemClicked(navigator)
                                },
                                onNewTab = {
                                    closeMenu()
                                    navigator.navigate(newTabUrl())
                                },
                                onHistory = {
                                    closeMenu()
                                    onHistoryMenuItemClicked(historyActivityResultLauncher)
                                },
                                onSettings = {
                                    closeMenu()
                                    onSettingsMenuItemClicked()
                                },
                                experimental = experimentalModeEnabled,
                                onExperimentalChange = { enabled ->
                                    experimentalModeEnabled = enabled
                                    sharedPreferences.edit { putBoolean("experimental", enabled) }
                                    navigator.reload()
                                },
                                onExit = {
                                    closeMenu()
                                    finish()
                                },
                            )
                        }
                    }
                }
            ) { innerPadding ->
                Servo(
                    experimentalModeEnabled = experimentalModeEnabled,
                    softKeyboardVisible = softKeyboardVisible,
                    servo = servo,
                    navigator = navigator,
                    modifier = Modifier.padding(innerPadding).focusRequester(servoFocusRequester),
                )
                BackHandler(enabled = menuExpanded || navigator.canGoBack) {
                    when {
                        menuExpanded && menuPage == AppMenuPage.MoreTools ->
                            menuPage = AppMenuPage.Root
                        menuExpanded -> closeMenu()
                        else -> navigator.back()
                    }
                }
                LaunchedEffect(servoFocusRequester) { servoFocusRequester.requestFocus() }
                alertMessage?.let { alertMessage ->
                    AlertDialog(
                        onDismissRequest = { this.alertMessage = null },
                        confirmButton = {
                            TextButton(onClick = { this.alertMessage = null }) {
                                Text(stringResource(android.R.string.ok))
                            }
                        },
                        text = { Text(alertMessage) },
                    )
                }
            }
        }

        val sdcard = getExternalFilesDir("")
        val host = sdcard!!.toPath().resolve("android_hosts").toString()
        try {
            Os.setenv("HOST_FILE", host, false)
        } catch (e: ErrnoException) {
            e.printStackTrace()
        }
    }

    private fun onHistoryBackMenuItemClicked(navigator: ServoNavigator) {
        navigator.back()
    }

    private fun onHistoryForwardMenuItemClicked(navigator: ServoNavigator) {
        navigator.forward()
    }

    private fun onRefreshMenuItemClicked(navigator: ServoNavigator) {
        navigator.reload()
    }

    private fun onCancelMenuItemClicked(navigator: ServoNavigator) {
        navigator.stop()
    }

    private fun onSettingsMenuItemClicked() {
        startActivity(Intent(this, SettingsActivity::class.java))
    }

    private fun onHistoryMenuItemClicked(
        historyActivityResultLauncher: ActivityResultLauncher<Intent>
    ) {
        historyActivityResultLauncher.launch(Intent(this, HistoryActivity::class.java))
    }

    override fun onImeShow() {
        softKeyboardVisible = true
    }

    override fun onImeHide() {
        softKeyboardVisible = false
    }

    override fun onAlert(message: String) {
        alertMessage = message
    }

    override fun onLoadStarted() {
        // This doesn’t seem to actually happen when navigating
        // back to a page that is already cached.
        Log.i(TAG, "onLoadStarted: ")
        isRefreshing = true
    }

    // INFO: This currently gets called multiple times on each load.
    override fun onLoadEnded() {
        Log.i(TAG, "onLoadEnded: ")
        if (currentUrl.isNotEmpty()) {
            // HistoryManager has a basic method of preventing clobbering
            // by the fact that onLoadEnded gets called multiple times
            // per page.
            historyManager.addEntry(currentUrl, currentTitle)
        }
        isRefreshing = false
    }

    override fun onTitleChanged(title: String) {
        currentTitle = title
    }

    override fun onUrlChanged(url: String) {
        urlTextFieldState.edit { replace(0, length, url) }
        currentUrl = url
    }

    companion object {
        private const val TAG = "MainActivity"
    }
}

/** Same pages as the desktop hamburger menu in `ports/aether/desktop/menu.rs`. */
private enum class AppMenuPage {
    Root,
    MoreTools,
}

@Composable
private fun AppMenu(
    expanded: Boolean,
    page: AppMenuPage,
    onDismiss: () -> Unit,
    onPageChange: (AppMenuPage) -> Unit,
    canGoBack: Boolean,
    canGoForward: Boolean,
    isRefreshing: Boolean,
    onBack: () -> Unit,
    onForward: () -> Unit,
    onRefresh: () -> Unit,
    onNewTab: () -> Unit,
    onHistory: () -> Unit,
    onSettings: () -> Unit,
    experimental: Boolean,
    onExperimentalChange: (Boolean) -> Unit,
    onExit: () -> Unit,
) {
    DropdownMenu(
        expanded = expanded,
        onDismissRequest = onDismiss,
        modifier = Modifier.width(280.dp),
    ) {
        when (page) {
            AppMenuPage.Root -> {
                AppMenuItem(
                    label = stringResource(R.string.history_back),
                    enabled = canGoBack,
                    onClick = onBack,
                    leading = { Icon(painterResource(R.drawable.arrow_back), null) },
                )
                AppMenuItem(
                    label = stringResource(R.string.history_forward),
                    enabled = canGoForward,
                    onClick = onForward,
                    leading = { Icon(painterResource(R.drawable.arrow_forward), null) },
                )
                AppMenuItem(
                    label = stringResource(if (isRefreshing) R.string.cancel else R.string.refresh),
                    onClick = onRefresh,
                    leading = {
                        Icon(
                            painterResource(
                                if (isRefreshing) R.drawable.cancel else R.drawable.refresh
                            ),
                            null,
                        )
                    },
                )
                HorizontalDivider()
                AppMenuItem(stringResource(R.string.new_tab), onClick = onNewTab)
                // One WebView per activity; a second window is not supported yet.
                AppMenuItem(stringResource(R.string.new_window), enabled = false, onClick = {})
                AppMenuItem(stringResource(R.string.history_title), onClick = onHistory)
                AppMenuItem(stringResource(R.string.settings_title), onClick = onSettings)
                AppMenuItem(
                    label = stringResource(R.string.settings_experimental_title),
                    onClick = { onExperimentalChange(!experimental) },
                    trailing = { Switch(checked = experimental, onCheckedChange = null) },
                )
                AppMenuItem(
                    label = stringResource(R.string.more_tools),
                    onClick = { onPageChange(AppMenuPage.MoreTools) },
                    trailing = { Icon(painterResource(R.drawable.arrow_forward), null) },
                )
                HorizontalDivider()
                AppMenuItem(stringResource(R.string.exit), onClick = onExit)
            }
            AppMenuPage.MoreTools -> {
                AppMenuItem(
                    label = stringResource(R.string.more_tools),
                    onClick = { onPageChange(AppMenuPage.Root) },
                    leading = {
                        Icon(
                            painterResource(R.drawable.arrow_back),
                            stringResource(R.string.history_back),
                        )
                    },
                )
                // No developer-tools dock, process page, or view-source handler on Android yet.
                AppMenuItem(
                    stringResource(R.string.developer_tools),
                    enabled = false,
                    onClick = {},
                )
                AppMenuItem(stringResource(R.string.processes), enabled = false, onClick = {})
                AppMenuItem(
                    stringResource(R.string.view_page_source),
                    enabled = false,
                    onClick = {},
                )
            }
        }
    }
}

private fun homeUrl(): String {
    val value = homeSettings() ?: return "about:blank"
    return when (value.optString("newWindows", "home")) {
        "blank" -> "about:blank"
        "url" -> value.optString("homepage").ifBlank { "about:blank" }
        else -> "servo:newtab"
    }
}

private fun newTabUrl(): String {
    val value = homeSettings() ?: return "servo:newtab"
    return when (value.optString("newTabs", "home")) {
        "blank" -> "about:blank"
        else -> "servo:newtab"
    }
}

private fun homeSettings(): JSONObject? =
    try {
        JSONObject(BrowserSettings().home())
    } catch (_: Exception) {
        null
    }

@Composable
private fun AppMenuItem(
    label: String,
    onClick: () -> Unit,
    enabled: Boolean = true,
    leading: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    DropdownMenuItem(
        text = { Text(label) },
        onClick = onClick,
        enabled = enabled,
        leadingIcon = leading,
        trailingIcon = trailing,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Omnibox(
    textFieldState: TextFieldState,
    onSearch: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val searchBarState = rememberSearchBarState()

    SearchBar(
        state = searchBarState,
        inputField = {
            val coroutineScope = rememberCoroutineScope()

            SearchBarDefaults.InputField(
                textFieldState = textFieldState,
                searchBarState = searchBarState,
                onSearch = onSearch,
                modifier =
                    Modifier.onFocusChanged { focusState ->
                        if (focusState.isFocused) {
                            coroutineScope.launch { textFieldState.edit { selectAll() } }
                        }
                    },
                placeholder = { Text(stringResource(R.string.url_or_search)) },
            )
        },
        modifier = modifier,
    )
}
