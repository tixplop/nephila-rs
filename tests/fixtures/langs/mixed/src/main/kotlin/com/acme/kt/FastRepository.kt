package com.acme.kt

import com.acme.core.BaseRepository

data class Stats(val hits: Int)

object Registry {
    fun all(): List<Stats> = emptyList()
}

class FastRepository : BaseRepository() {
    fun flush() = println("flush")
}

fun helperTop(x: Int): Int = x + 1
