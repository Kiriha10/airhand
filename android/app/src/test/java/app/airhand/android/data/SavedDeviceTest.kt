package app.airhand.android.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertNotNull
import org.junit.Test

class SavedDeviceTest {
    @Test
    fun equivalentLocalAddressesUseTheSameSavedIdentity() {
        val address = app.airhand.android.core.LanAddress
        assertNotNull(address.key("fd00::20"))
        assertEquals(address.key("192.168.1.20"), address.key("192.168.1.20:4434"))
        assertEquals(address.key("fd00::20"), address.key("[FD00:0:0:0:0:0:0:20]:4434"))
        assertFalse(address.key("192.168.1.20:4434") == address.key("192.168.1.20:4435"))
        assertNull(address.key("example.com:4434"))
    }
    @Test
    fun lanAddressAndTokenSurviveSavingWithoutReplacingTheRelayToken() {
        val old = SavedDevice.parse(JSONObject().put("code", "482913").put("resume", "ab".repeat(16)))
        assertNull(old.lanAddr)
        assertNull(old.lanResume)
        val local = old.copy(lanAddr = "192.168.1.20:4434", lanResume = "cd".repeat(16))
        val restored = SavedDevice.parse(local.toJson())
        assertEquals("192.168.1.20:4434", restored.lanAddr)
        assertEquals("cd".repeat(16), restored.lanResume)
        assertEquals("ab".repeat(16), restored.resume)
    }
    /** 互换 Ctrl 和 ⌘：默认关闭（以前保存的设备没有这一项也是关闭），打开之后记得住 */
    @Test
    fun ctrlCommandSwapIsOffByDefaultAndRemembered() {
        val old = SavedDevice.parse(JSONObject().put("code", "482913").put("hostOs", "macOS"))
        assertFalse(old.swapCtrlCommand)
        val on = old.copy(swapCtrlCommand = true)
        assertEquals(on, SavedDevice.parse(on.toJson()))
    }
}
