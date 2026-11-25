package app.airhand.android.core

import java.net.InetAddress

/** Canonical endpoint key for looking up a LAN device's saved identity, without DNS. */
object LanAddress {
    fun key(address: String): String? {
        val text = address.trim()
        val host: String
        val port: Int
        if (text.startsWith("[")) {
            val end = text.indexOf(']')
            if (end < 0) return null
            host = text.substring(1, end)
            val tail = text.substring(end + 1)
            port = if (tail.isEmpty()) 4434 else {
                if (!tail.startsWith(":")) return null
                tail.substring(1).toIntOrNull() ?: return null
            }
        } else if (text.count { it == ':' } == 1) {
            host = text.substringBefore(':')
            port = text.substringAfter(':').toIntOrNull() ?: return null
        } else {
            host = text
            port = 4434
        }
        if (port !in 1..65535) return null
        val numeric = if (':' in host) host.matches(Regex("[0-9a-fA-F:.]+"))
        else host.count { it == '.' } == 3 && host.matches(Regex("[0-9.]+"))
        if (!numeric) return null
        val bytes = runCatching { InetAddress.getByName(host).address }.getOrNull() ?: return null
        return bytes.joinToString("") { "%02x".format(it.toInt() and 255) } + ":$port"
    }
}
