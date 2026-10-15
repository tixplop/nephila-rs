package com.acme.shop

import com.acme.base.Base as B
import java.util.*

sealed class Shape
data class Circle(val r: Double) : Shape()
interface Named {
    val name: String
}
enum class Color { RED, GREEN }
annotation class Tag
object Registry {
    fun all(): List<Shape> = emptyList()
}

class Service(private val repo: Repo) : B(), Named {
    override val name = "svc"
    companion object {
        const val X = 1
    }
    init {
        println("init")
    }
    fun run() = repo.save(Circle(1.0))
}

fun top(x: Int): Int = x + 1
