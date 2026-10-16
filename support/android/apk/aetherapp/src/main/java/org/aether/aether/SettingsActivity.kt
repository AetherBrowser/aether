/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
package org.aether.aether

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import java.net.URLEncoder
import org.json.JSONObject
import org.servo.servoview.BrowserSettings

class SettingsActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val browserSettings = BrowserSettings()
        setContent {
            var section by remember { mutableStateOf<SettingsSection?>(null) }
            var home by remember { mutableStateOf(HomeSettings.parse(browserSettings.home())) }
            var search by remember {
                mutableStateOf(SearchSettings.parse(browserSettings.searchEngines()))
            }
            val about = remember { AboutInfo.parse(browserSettings.about()) }

            BackHandler(enabled = section != null) { section = null }

            Scaffold(
                topBar = {
                    @OptIn(ExperimentalMaterial3Api::class)
                    TopAppBar(
                        title = {
                            Text(
                                stringResource(
                                    when (section) {
                                        SettingsSection.Home -> R.string.settings_home
                                        SettingsSection.Search -> R.string.settings_search
                                        SettingsSection.Downloads -> R.string.settings_downloads
                                        SettingsSection.Appearance -> R.string.settings_appearance
                                        SettingsSection.About -> R.string.settings_about
                                        null -> R.string.settings_title
                                    }
                                )
                            )
                        },
                        navigationIcon = {
                            IconButton(
                                onClick = {
                                    if (section == null) finish() else section = null
                                }
                            ) {
                                Icon(
                                    painterResource(R.drawable.arrow_back),
                                    stringResource(R.string.back),
                                )
                            }
                        },
                    )
                }
            ) { innerPadding ->
                Column(
                    modifier =
                        Modifier.padding(innerPadding)
                            .fillMaxSize()
                            .verticalScroll(rememberScrollState())
                ) {
                    when (section) {
                        null ->
                            SettingsSection.entries.forEach { entry ->
                                ListItem(
                                    headlineContent = { Text(stringResource(entry.title)) },
                                    modifier = Modifier.clickable { section = entry },
                                )
                            }
                        SettingsSection.Home ->
                            HomeSection(
                                home = home,
                                onChange = { query ->
                                    home = HomeSettings.parse(browserSettings.home(query))
                                },
                            )
                        SettingsSection.Search ->
                            SearchSection(
                                search = search,
                                onChange = { query ->
                                    SearchSettings.parse(browserSettings.searchEngines(query))
                                        .also { search = it }
                                },
                            )
                        SettingsSection.Downloads ->
                            SectionHeading(stringResource(R.string.settings_downloads))
                        SettingsSection.Appearance ->
                            SectionHeading(stringResource(R.string.settings_appearance))
                        SettingsSection.About -> AboutSection(about)
                    }
                }
            }
        }
    }
}

private enum class SettingsSection(val title: Int) {
    Home(R.string.settings_home),
    Search(R.string.settings_search),
    Downloads(R.string.settings_downloads),
    Appearance(R.string.settings_appearance),
    About(R.string.settings_about),
}

private data class HomeSettings(
    val newWindows: String,
    val homepage: String,
    val newTabs: String,
    val showSearch: Boolean,
    val showLogo: Boolean,
    val error: String,
) {
    companion object {
        fun parse(json: String): HomeSettings {
            val value = jsonObject(json)
            return HomeSettings(
                newWindows = value.optString("newWindows", "home"),
                homepage = value.optString("homepage", ""),
                newTabs = value.optString("newTabs", "home"),
                showSearch = value.optBoolean("showSearch", true),
                showLogo = value.optBoolean("showLogo", true),
                error = value.optString("error", ""),
            )
        }
    }
}

private data class SearchEngine(val id: String, val name: String, val url: String)

private data class SearchSettings(
    val defaultId: String,
    val engines: List<SearchEngine>,
    val available: List<SearchEngine>,
    val error: String,
) {
    companion object {
        fun parse(json: String): SearchSettings {
            val value = jsonObject(json)
            return SearchSettings(
                defaultId = value.optString("defaultId", ""),
                engines = value.optJSONArray("engines").toEngines(includeUrl = true),
                available = value.optJSONArray("available").toEngines(includeUrl = false),
                error = value.optString("error", ""),
            )
        }
    }
}

private data class AboutInfo(val version: String, val channel: String, val architecture: String) {
    companion object {
        fun parse(json: String): AboutInfo {
            val value = jsonObject(json)
            return AboutInfo(
                version = value.optString("version", ""),
                channel = value.optString("channel", ""),
                architecture = value.optString("architecture", ""),
            )
        }
    }
}

@Composable
private fun HomeSection(home: HomeSettings, onChange: (String) -> Unit) {
    if (home.error.isNotEmpty()) {
        ErrorText(home.error)
    }
    var newWindows by remember(home.newWindows) { mutableStateOf(home.newWindows) }
    SectionHeading(stringResource(R.string.settings_homepage))
    ChoiceRow(
        label = stringResource(R.string.settings_new_windows),
        value = newWindows,
        options =
            listOf(
                "home" to stringResource(R.string.settings_aether_home),
                "url" to stringResource(R.string.settings_url),
                "blank" to stringResource(R.string.settings_blank_page),
            ),
        onSelect = { choice ->
            newWindows = choice
            if (choice != "url") {
                onChange("newWindows=$choice")
            }
        },
    )
    if (newWindows == "url") {
        var draft by remember(home.homepage) { mutableStateOf(home.homepage) }
        OutlinedTextField(
            value = draft,
            onValueChange = { draft = it },
            label = { Text(stringResource(R.string.settings_url)) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { onChange(homepageQuery(draft)) }),
            modifier =
                Modifier.fillMaxWidth().padding(horizontal = 16.dp).onFocusChanged { focus ->
                    if (!focus.isFocused && draft != home.homepage) {
                        onChange(homepageQuery(draft))
                    }
                },
        )
    }

    SectionHeading(stringResource(R.string.settings_new_tabs))
    ChoiceRow(
        label = stringResource(R.string.settings_new_tabs),
        value = home.newTabs,
        options =
            listOf(
                "home" to stringResource(R.string.settings_aether_home),
                "blank" to stringResource(R.string.settings_blank_page),
            ),
        onSelect = { onChange("newTabs=$it") },
    )

    SectionHeading(stringResource(R.string.settings_aether_home))
    SwitchRow(
        title = stringResource(R.string.settings_search_box),
        checked = home.showSearch,
        onCheckedChange = { onChange("showSearch=$it") },
    )
    SwitchRow(
        title = stringResource(R.string.settings_logo),
        checked = home.showLogo,
        onCheckedChange = { onChange("showLogo=$it") },
    )
}

@Composable
private fun SearchSection(
    search: SearchSettings,
    onChange: (String) -> SearchSettings,
) {
    if (search.error.isNotEmpty()) {
        ErrorText(search.error)
    }
    ChoiceRow(
        label = stringResource(R.string.settings_default_engine),
        value = search.defaultId,
        options = search.engines.map { it.id to it.name },
        onSelect = {
            onChange("default=${encode(it)}")
            Unit
        },
    )

    SectionHeading(stringResource(R.string.settings_configured_engines))
    val onlyOne = search.engines.size < 2
    search.engines.forEach { engine ->
        ListItem(
            headlineContent = { Text(engine.name) },
            supportingContent = { Text(engine.url) },
            trailingContent = {
                TextButton(
                    onClick = {
                        onChange("remove=${encode(engine.id)}")
                        Unit
                    },
                    enabled = !onlyOne,
                ) {
                    Text(stringResource(R.string.settings_remove))
                }
            },
        )
    }

    SectionHeading(stringResource(R.string.settings_add_engine))
    Text(
        stringResource(R.string.settings_add_engine_hint),
        modifier = Modifier.padding(horizontal = 16.dp),
    )
    var name by remember { mutableStateOf("") }
    var url by remember { mutableStateOf("") }
    OutlinedTextField(
        value = name,
        onValueChange = { name = it },
        label = { Text(stringResource(R.string.settings_engine_name)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    )
    OutlinedTextField(
        value = url,
        onValueChange = { url = it },
        label = { Text(stringResource(R.string.settings_engine_url)) },
        placeholder = { Text("https://example.com/search?q=%s") },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
    )
    TextButton(
        onClick = {
            val updated = onChange("name=${encode(name)}&url=${encode(url)}")
            if (updated.error.isEmpty()) {
                name = ""
                url = ""
            }
        },
        enabled = name.isNotBlank() && url.isNotBlank(),
        modifier = Modifier.padding(horizontal = 8.dp),
    ) {
        Text(stringResource(R.string.settings_add))
    }

    if (search.available.isNotEmpty()) {
        var builtinId by remember(search.available) { mutableStateOf(search.available.first().id) }
        SectionHeading(stringResource(R.string.settings_builtin_engines))
        ChoiceRow(
            label = stringResource(R.string.settings_builtin_engines),
            value = builtinId,
            options = search.available.map { it.id to it.name },
            onSelect = { builtinId = it },
        )
        TextButton(
            onClick = {
                onChange("addBuiltin=${encode(builtinId)}")
                Unit
            },
            modifier = Modifier.padding(horizontal = 8.dp),
        ) {
            Text(stringResource(R.string.settings_add))
        }
    }
}

@Composable
private fun AboutSection(about: AboutInfo) {
    ListItem(
        headlineContent = { Text(stringResource(R.string.settings_version)) },
        supportingContent = { Text(about.version) },
    )
    ListItem(
        headlineContent = { Text(stringResource(R.string.settings_channel)) },
        supportingContent = { Text(about.channel) },
    )
    ListItem(
        headlineContent = { Text(stringResource(R.string.settings_architecture)) },
        supportingContent = { Text(about.architecture) },
    )
}

@Composable
private fun ChoiceRow(
    label: String,
    value: String,
    options: List<Pair<String, String>>,
    onSelect: (String) -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    val selected = options.firstOrNull { it.first == value }?.second ?: value
    ListItem(
        headlineContent = { Text(label) },
        supportingContent = { Text(selected) },
        trailingContent = {
            TextButton(onClick = { open = true }) { Text(selected) }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                options.forEach { (id, name) ->
                    DropdownMenuItem(
                        text = { Text(name) },
                        onClick = {
                            open = false
                            if (id != value) onSelect(id)
                        },
                    )
                }
            }
        },
    )
}

@Composable
private fun SwitchRow(title: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        trailingContent = { Switch(checked = checked, onCheckedChange = onCheckedChange) },
    )
}

@Composable
private fun SectionHeading(title: String) {
    Text(title, modifier = Modifier.padding(start = 16.dp, top = 20.dp, end = 16.dp, bottom = 4.dp))
}

@Composable
private fun ErrorText(message: String) {
    Text(message, modifier = Modifier.padding(16.dp))
}

private fun jsonObject(json: String): JSONObject {
    return try {
        if (json.isEmpty()) JSONObject() else JSONObject(json)
    } catch (_: Exception) {
        JSONObject().put("error", "Could not read settings.")
    }
}

private fun homepageQuery(url: String) = "newWindows=url&homepage=${encode(url.trim())}"

private fun encode(value: String) = URLEncoder.encode(value, Charsets.UTF_8.name())

private fun org.json.JSONArray?.toEngines(includeUrl: Boolean): List<SearchEngine> {
    if (this == null) return emptyList()
    return buildList {
        for (index in 0 until length()) {
            val item = optJSONObject(index) ?: continue
            add(
                SearchEngine(
                    id = item.optString("id"),
                    name = item.optString("name"),
                    url = if (includeUrl) item.optString("url") else "",
                )
            )
        }
    }
}
