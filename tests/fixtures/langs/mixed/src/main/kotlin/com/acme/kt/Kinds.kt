package com.acme.core

interface Auditable {
    fun audit()
}

enum class Level { LOW, HIGH }

open class Base

class S(private val repo: OrderRepository) : Base(), Auditable {
    companion object {
        const val NAME = "s"
    }

    override fun audit() {}

    fun m(o: Order) {
        repo.save(o)
    }
}
