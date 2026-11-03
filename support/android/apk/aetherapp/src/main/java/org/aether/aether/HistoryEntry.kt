package org.aether.aether

import kotlinx.serialization.Serializable

@Serializable class HistoryEntry(val timestamp: Long, val url: String, val title: String?)
