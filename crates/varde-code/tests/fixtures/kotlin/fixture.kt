// Entity extraction fixture for Kotlin: covers all 14 required kinds.
package com.example.app

import io.ktor.server.engine.embeddedServer
import io.ktor.server.netty.Netty
import io.ktor.server.response.respondText
import io.ktor.server.routing.get
import io.ktor.server.routing.routing

// Add adds two integers (public top-level function -> Function + Export).
public fun add(a: Int, b: Int): Int {
    return a + b
}

// User is a public data class -> Class + Export.
public data class User(val name: String, val age: Int)

// Greeter is an interface (Kotlin's abstraction mechanism) -> Interface.
interface Greeter {
    fun greet(): String
}

// Top-level properties -> Variable.
val version = "1.0.0"
const val maxRetries = 3
val isReady = true

// GreeterImpl implements Greeter; greet() is a method -> Function.
class GreeterImpl : Greeter {
    override fun greet(): String {
        val message = "hello " + version
        return message
    }
}

fun main() {
    val server = embeddedServer(Netty, port = 8080) {
        routing {
            // Route: Ktor-style get("/users") inside a routing block.
            get("/users") {
                // Response: call.respondText(...).
                call.respondText("users")
            }
            get("/health") {
                call.respondText("ok")
            }
        }
    }
    server.start(wait = true)
}

fun compute(x: String, n: Int): String {
    val names = listOf("a", "b")
    for (name in names) {
        if (name.length > 0) {
            throw IllegalArgumentException("bad name")
        }
    }
    var i = 0
    while (i < n) {
        i++
    }
    val result = try {
        x
    } catch (e: Exception) {
        "caught"
    }
    val fallback = result ?: "none"
    if (fallback.length > 0 && n > 1) {
        return fallback
    }
    return "empty"
}
