package app.airhand.android.ui.session

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** 手势：触控板 / 触屏 / 直通三种方式下，手指动作变成什么鼠标操作 */
class GesturesTest {
    private val log = ArrayList<String>()
    private val pressingStates = ArrayList<Boolean>()
    private val timers = FakeTimers()
    private lateinit var g: Gestures

    /** 视图 1000×500，画面占满（没有黑边） */
    private val out = object : Gestures.Out {
        override fun toRemote(x: Float, y: Float) = (x / 1000f) to (y / 500f)
        override fun contentSize() = 1000f to 500f
        override fun move(nx: Float, ny: Float) {
            log += "move %.2f %.2f".format(nx, ny)
        }

        override fun delta(nx: Float, ny: Float) {
            log += "delta %.3f %.3f".format(nx, ny)
        }

        override fun button(button: String, down: Boolean, click: Boolean) {
            log += "$button ${if (click) "click " else ""}${if (down) "down" else "up"}"
        }

        override fun click(button: String) { log += "$button click" }
        override fun pressingChanged() { pressingStates += g.pressing }

        override fun wheel(dx: Int, dy: Int) {
            log += "wheel $dx $dy"
        }

        override fun zoom(factor: Float, fx: Float, fy: Float) {
            log += "zoom"
        }

        override fun pan(dx: Float, dy: Float) {}
        override fun follow() {}
        override fun haptic() {}
        override fun ripple(x: Float, y: Float) {}
        override fun toggleKeyboard() {
            log += "keyboard"
        }

        override fun touch(id: Int, phase: String, nx: Float, ny: Float) {
            log += "touch %d %s %.2f %.2f".format(id, phase, nx, ny)
        }
    }

    class FakeTimers : Gestures.Timers {
        var now = 0L
        private val pending = ArrayList<Pair<Long, () -> Unit>>()
        override fun after(ms: Long, r: () -> Unit): Any {
            val p = (now + ms) to r
            pending += p
            return p
        }

        override fun cancel(token: Any?) {
            pending.remove(token)
        }

        fun advance(ms: Long) {
            val target = now + ms
            while (true) {
                val next = pending.filter { it.first <= target }.minByOrNull { it.first } ?: break
                pending.remove(next)
                now = next.first
                next.second()
            }
            now = target
        }
    }

    private fun p(x: Float, y: Float, id: Int = 0) = Gestures.P(id, x, y)
    private fun buttons() = log.filter { it.startsWith("left") || it.startsWith("right") }

    @Before
    fun setUp() {
        g = Gestures(out, timers, slop = 10f)
    }

    @Test
    fun trackpadTapClicksAtThePointer() {
        g.mode = Gestures.Mode.TRACKPAD
        g.setCursor(0.25f, 0.5f)
        g.down(p(800f, 400f), 0)
        g.up(p(800f, 400f), 80)
        assertEquals(listOf("move 0.25 0.50", "left click"), log)
    }

    @Test
    fun trackpadSlideMovesThePointerRelatively() {
        g.mode = Gestures.Mode.TRACKPAD
        g.down(p(100f, 100f), 0)
        g.move(listOf(p(150f, 100f)), 100)
        g.move(listOf(p(200f, 100f)), 200)
        g.up(p(200f, 100f), 250)
        assertTrue(g.cursorX > 0.55f)
        assertEquals(0.5f, g.cursorY, 0.001f)
        assertTrue("no clicks: $log", buttons().isEmpty())
    }

    @Test
    fun hiddenRemoteCursorMakesTheTrackpadRelative() {
        g.mode = Gestures.Mode.TRACKPAD
        g.setCursor(0.25f, 0.5f)
        g.relative = true
        // 点一下：不先把光标挪到本机指针的位置（游戏里视角会跳）
        g.down(p(800f, 400f), 0)
        g.up(p(800f, 400f), 80)
        assertEquals(listOf("left click"), log)
        log.clear()
        // 滑动：发移动量（画面的几分之几），本机画的指针不动
        g.down(p(100f, 100f), 1000)
        g.move(listOf(p(150f, 100f)), 1100)
        g.move(listOf(p(200f, 120f)), 1200)
        g.up(p(200f, 120f), 1250)
        assertTrue("$log", log.isNotEmpty() && log.all { it.startsWith("delta ") })
        assertTrue("$log", log.any { it.startsWith("delta 0.") && !it.startsWith("delta 0.000") })
        assertEquals(0.25f, g.cursorX, 0.001f)
        assertEquals(0.5f, g.cursorY, 0.001f)
    }

    @Test
    fun touchDragBecomesRelativeWhenTheGameHidesTheCursor() {
        g.mode = Gestures.Mode.TOUCH
        g.down(p(500f, 250f), 0)
        assertFalse(g.pressing)
        g.move(listOf(p(540f, 250f)), 50)
        // 拖动开始时还是绝对坐标：移到按下的位置、按下左键
        assertEquals(listOf("move 0.50 0.50", "move 0.50 0.50", "left down", "move 0.54 0.50"), log)
        // 按着左键（RemoteView 据此在松开之前不因为光标出现就改回发位置）
        assertTrue(g.pressing)
        log.clear()
        // 游戏开始转视角，光标隐藏了：之后发手指移动了多少
        g.relative = true
        g.move(listOf(p(640f, 300f)), 100)
        g.up(p(640f, 300f), 150)
        assertEquals(listOf("delta 0.100 0.100", "left up"), log)
        assertFalse(g.pressing)
    }

    @Test
    fun trackpadLongPressHoldsLeftForDragging() {
        g.mode = Gestures.Mode.TRACKPAD
        g.down(p(100f, 100f), 0)
        timers.advance(Gestures.LONG_PRESS_MS + 10)
        g.up(p(100f, 100f), 600)
        assertEquals(listOf("left down", "left up"), buttons())
        assertEquals(listOf(true, false), pressingStates)
    }

    @Test
    fun trackpadTapThenHoldDrags() {
        g.mode = Gestures.Mode.TRACKPAD
        g.down(p(100f, 100f), 0)
        g.up(p(100f, 100f), 60)
        g.down(p(102f, 101f), 150)
        g.move(listOf(p(160f, 101f)), 200)
        g.move(listOf(p(220f, 101f)), 250)
        g.up(p(220f, 101f), 300)
        timers.advance(Gestures.DRAG_RESUME_MS)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
        // 拖动期间指针在动
        val i = log.indexOfLast { it == "left down" }
        assertTrue(log.subList(i, log.size).any { it.startsWith("move") })
    }

    @Test
    fun twoFingerTapIsARightClick() {
        g.mode = Gestures.Mode.TRACKPAD
        g.down(p(100f, 100f), 0)
        g.pointerDown(p(200f, 100f, 1), listOf(p(100f, 100f), p(200f, 100f, 1)), 20)
        g.pointerUp(p(0f, 0f, 1), 1, 120)
        g.up(p(100f, 100f), 130)
        assertEquals(listOf("right click"), buttons())
    }

    @Test
    fun twoFingerDragScrollsNaturally() {
        g.mode = Gestures.Mode.TRACKPAD
        g.down(p(100f, 100f), 0)
        g.pointerDown(p(200f, 100f, 1), listOf(p(100f, 100f), p(200f, 100f, 1)), 10)
        // 手指往下 = 内容往下 = 往上滚（滚轮为正）
        g.move(listOf(p(100f, 130f), p(200f, 130f, 1)), 40)
        g.move(listOf(p(100f, 160f), p(200f, 160f, 1)), 70)
        g.pointerUp(p(0f, 0f, 1), 1, 80)
        g.up(p(100f, 160f), 90)
        val wheel = log.filter { it.startsWith("wheel") }.map { it.split(" ")[2].toInt() }
        assertTrue("wheel: $log", wheel.isNotEmpty() && wheel.all { it > 0 })
        assertTrue(buttons().isEmpty())
    }

    @Test
    fun pinchZoomsTheView() {
        g.mode = Gestures.Mode.TOUCH
        g.down(p(400f, 200f), 0)
        g.pointerDown(p(500f, 200f, 1), listOf(p(400f, 200f), p(500f, 200f, 1)), 10)
        g.move(listOf(p(350f, 200f), p(550f, 200f, 1)), 40)
        g.pointerUp(p(0f, 0f, 1), 1, 60)
        g.up(p(350f, 200f), 70)
        assertTrue(log.contains("zoom"))
        assertTrue(buttons().isEmpty())
    }

    @Test
    fun touchTapClicksWhereTheFingerIs() {
        g.mode = Gestures.Mode.TOUCH
        g.down(p(500f, 250f), 0)
        g.up(p(500f, 250f), 60)
        assertEquals(listOf("move 0.50 0.50", "move 0.50 0.50", "left click"), log)
    }

    @Test
    fun touchLongPressIsARightClickThere() {
        g.mode = Gestures.Mode.TOUCH
        g.down(p(250f, 125f), 0)
        timers.advance(Gestures.LONG_PRESS_MS + 10)
        g.up(p(250f, 125f), 600)
        assertEquals(listOf("right click"), buttons())
        assertEquals(0.25f, g.cursorX, 0.001f)
    }

    @Test
    fun touchDragHoldsTheLeftButtonFromTheStart() {
        g.mode = Gestures.Mode.TOUCH
        g.down(p(100f, 100f), 0)
        g.move(listOf(p(300f, 100f)), 50)
        g.up(p(400f, 100f), 100)
        val down = log.indexOf("left down")
        assertEquals("move 0.10 0.20", log[down - 1])
        assertEquals("left up", log.last())
        assertEquals(0.4f, g.cursorX, 0.001f)
    }

    @Test
    fun passthroughQuickTap() {
        g.mode = Gestures.Mode.PASSTHROUGH
        g.down(p(100f, 100f), 0)
        g.up(p(100f, 100f), 30)
        assertEquals(listOf("left click down", "left click up"), buttons())
    }

    @Test
    fun passthroughHoldIsALongPressOnTheOtherSide() {
        g.mode = Gestures.Mode.PASSTHROUGH
        g.down(p(100f, 100f), 0)
        timers.advance(Gestures.PASSTHROUGH_DELAY_MS + 5)
        assertEquals(listOf("left down"), buttons())
        timers.advance(800)
        g.up(p(100f, 100f), 900)
        assertEquals(listOf("left down", "left up"), buttons())
    }

    @Test
    fun passthroughPinchDoesNotTouchTheOtherPhone() {
        g.mode = Gestures.Mode.PASSTHROUGH
        g.down(p(400f, 200f), 0)
        g.pointerDown(p(500f, 200f, 1), listOf(p(400f, 200f), p(500f, 200f, 1)), 20)
        timers.advance(200)
        g.move(listOf(p(380f, 200f), p(520f, 200f, 1)), 60)
        g.pointerUp(p(0f, 0f, 1), 1, 100)
        g.up(p(380f, 200f), 110)
        assertTrue(buttons().isEmpty())
        assertTrue(log.contains("zoom"))
    }

    @Test
    fun passthroughSwipeStartsRightAway() {
        g.mode = Gestures.Mode.PASSTHROUGH
        g.down(p(500f, 400f), 0)
        g.move(listOf(p(500f, 300f)), 16)
        g.up(p(500f, 100f), 40)
        assertEquals(listOf("left down", "left up"), buttons())
        val down = log.indexOf("left down")
        assertEquals("move 0.50 0.80", log[down - 1])
    }

    @Test
    fun threeFingerTapTogglesTheKeyboard() {
        g.mode = Gestures.Mode.TRACKPAD
        g.down(p(100f, 100f), 0)
        g.pointerDown(p(200f, 100f, 1), listOf(p(100f, 100f), p(200f, 100f, 1)), 10)
        g.pointerDown(p(300f, 100f, 2), listOf(p(100f, 100f), p(200f, 100f, 1), p(300f, 100f, 2)), 20)
        g.pointerUp(p(0f, 0f, 2), 2, 100)
        g.pointerUp(p(0f, 0f, 1), 1, 110)
        g.up(p(100f, 100f), 120)
        assertEquals(listOf("keyboard"), log.filter { it == "keyboard" })
        assertTrue(buttons().isEmpty())
    }

    @Test
    fun multitouchPassesEveryFingerThrough() {
        g.mode = Gestures.Mode.MULTITOUCH
        g.down(p(400f, 250f, id = 5), 0)
        g.pointerDown(p(600f, 250f, id = 9), listOf(p(400f, 250f, 5), p(600f, 250f, 9)), 10)
        g.move(listOf(p(300f, 250f, 5), p(700f, 250f, 9)), 30)
        // 没动的手指不重复发
        g.move(listOf(p(300f, 250f, 5), p(750f, 250f, 9)), 40)
        g.pointerUp(p(750f, 250f, 9), 1, 60)
        g.up(p(300f, 250f, 5), 70)
        assertEquals(
            listOf(
                "touch 0 down 0.40 0.50",
                "touch 1 down 0.60 0.50",
                "touch 0 move 0.30 0.50",
                "touch 1 move 0.70 0.50",
                "touch 1 move 0.75 0.50",
                "touch 1 up 0.75 0.50",
                "touch 0 up 0.30 0.50",
            ),
            log,
        )
    }

    @Test
    fun multitouchReusesFreeSlotsAndCancels() {
        g.mode = Gestures.Mode.MULTITOUCH
        g.down(p(100f, 100f, id = 0), 0)
        g.pointerDown(p(200f, 100f, id = 1), listOf(p(100f, 100f, 0), p(200f, 100f, 1)), 10)
        g.pointerUp(p(100f, 100f, 0), 1, 20)
        // 新手指用空出来的 0 号
        g.pointerDown(p(300f, 100f, id = 2), listOf(p(200f, 100f, 1), p(300f, 100f, 2)), 30)
        // 三指手势也传给对方，不开键盘
        g.pointerDown(p(400f, 100f, id = 3), listOf(p(200f, 100f, 1), p(300f, 100f, 2), p(400f, 100f, 3)), 35)
        g.cancel()
        val events = log.map { it.split(" ").take(3).joinToString(" ") }
        assertEquals(listOf("touch 0 down", "touch 1 down", "touch 0 up", "touch 0 down", "touch 2 down"), events.take(5))
        // 打断时还按着的三个点都松开（顺序不重要）
        assertEquals(setOf("touch 0 cancel", "touch 1 cancel", "touch 2 cancel"), events.drop(5).toSet())
        assertEquals(8, events.size)
        assertTrue(log.none { it == "keyboard" })
        assertTrue(buttons().isEmpty())
    }

    // ───────────── 开着游戏按键：空白处拖动转视角 ─────────────

    @Test
    fun gameKeysTouchDragTurnsTheViewWithoutTheLeftButton() {
        g.mode = Gestures.Mode.TOUCH
        g.relative = true
        g.lookGain = 2f
        g.down(p(500f, 250f), 0)
        g.move(listOf(p(540f, 250f)), 50)
        g.move(listOf(p(590f, 275f)), 100)
        g.up(p(590f, 275f), 150)
        // 不按左键（拖一下视角不该开枪），移动量乘上灵敏度；松手也不算点击
        assertEquals(listOf("delta 0.080 0.000", "delta 0.100 0.100"), log)
        assertFalse(g.pressing)
    }

    @Test
    fun gameKeysTapStillClicks() {
        g.mode = Gestures.Mode.TOUCH
        g.relative = true
        g.lookGain = 1f
        g.down(p(500f, 250f), 0)
        g.up(p(500f, 250f), 80)
        assertEquals(listOf("left click"), log)
    }

    @Test
    fun gameKeysRestingThumbNeitherRightClicksNorClicks() {
        // 转视角时拇指常常搭在屏幕上不动：不能因此开镜（右键）或者松手时开枪（左键）
        for (m in listOf(Gestures.Mode.TOUCH, Gestures.Mode.TRACKPAD)) {
            log.clear()
            g.mode = m
            g.relative = true
            g.lookGain = 1f
            val t0 = timers.now
            g.down(p(500f, 250f), t0)
            timers.advance(Gestures.LONG_PRESS_MS + 500)
            g.up(p(500f, 250f), timers.now)
            assertTrue("$m: $log", buttons().isEmpty())
        }
    }

    @Test
    fun gameKeysTrackpadLookHasNoAcceleration() {
        g.mode = Gestures.Mode.TRACKPAD
        g.relative = true
        g.lookGain = 1f
        g.down(p(100f, 100f), 0)
        g.move(listOf(p(120f, 100f)), 10)
        // 很快地划：平时会加速，转视角时不加速
        g.move(listOf(p(320f, 100f)), 20)
        g.up(p(320f, 100f), 30)
        assertEquals(listOf("delta 0.020 0.000", "delta 0.200 0.000"), log)
    }

    @Test
    fun gameKeysLeaveTheVisibleCursorAlone() {
        // 被控端显示着光标（游戏菜单）：和平时一样，拖动 = 按着左键拖
        g.mode = Gestures.Mode.TOUCH
        g.lookGain = 1f
        g.down(p(500f, 250f), 0)
        g.move(listOf(p(540f, 250f)), 50)
        g.up(p(540f, 250f), 100)
        assertEquals(listOf("left down", "left up"), buttons())
    }

    @Test
    fun accelerationIsBounded() {
        assertEquals(0.7f, Gestures.accel(0f), 0.001f)
        assertEquals(2.4f, Gestures.accel(100f), 0.001f)
        assertTrue(Gestures.accel(1f) > 1f)
    }

    private fun at(t: Long, action: () -> Unit = {}) {
        timers.advance(t - timers.now)
        action()
    }

    private fun beginTrackpadDrag() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(150) { g.down(p(101f, 100f), 150) }
        at(200) { g.move(listOf(p(160f, 100f)), 200) }
    }

    @Test
    fun delayedSecondPressFallsBackToLeftDrag() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(380) { g.down(p(100f, 100f), 380) } // Exactly 300 ms after the tap.
        at(830)
        assertEquals(listOf("left click", "left down"), buttons())
        at(880) { g.move(listOf(p(160f, 100f)), 880) }
        at(900) { g.up(p(160f, 100f), 900) }
        at(1400)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
        assertEquals(listOf(true, false), pressingStates)
    }

    @Test
    fun quickTapDragUsesPointerPositionNotFingerLandingPosition() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(200) { g.down(p(800f, 400f), 200) }
        at(240) { g.move(listOf(p(850f, 400f)), 240) }
        at(260) { g.up(p(850f, 400f), 260) }
        at(760)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
    }

    @Test
    fun movingThePointerAfterATapDoesNotStartAnUnwantedDrag() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        g.setCursor(0.25f, 0.25f)
        at(200) { g.down(p(100f, 100f), 200) }
        at(240) { g.move(listOf(p(160f, 100f)), 240) }
        at(260) { g.up(p(160f, 100f), 260) }
        at(1000)
        assertEquals(listOf("left click"), buttons())
    }

    @Test
    fun a350msTapIsNotSilentlyDiscarded() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(350) { g.up(p(100f, 100f), 350) }
        assertEquals(listOf("left click"), buttons())
        at(430) { g.down(p(800f, 400f), 430) }
        at(500) { g.move(listOf(p(850f, 400f)), 500) }
        g.cancel()
        assertEquals(listOf("left click", "left down", "left up"), buttons())
    }

    @Test
    fun holdingTheQuickSecondPressStartsOneLeftPress() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(200) { g.down(p(100f, 100f), 200) }
        at(650)
        assertTrue(g.pressing)
        at(700) { g.move(listOf(p(160f, 100f)), 700) }
        at(720) { g.up(p(160f, 100f), 720) }
        at(1220)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
    }

    @Test
    fun longPressStartsLeftDragDespiteSmallInitialMovement() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(150) { g.move(listOf(p(103f, 100f)), 150) }
        at(300) { g.move(listOf(p(107f, 100f)), 300) }
        at(449) { g.move(listOf(p(109f, 100f)), 449) }
        at(450)
        assertEquals(listOf("left down"), buttons())
        assertTrue(g.pressing)
        assertEquals(listOf(true), pressingStates)
        val x = g.cursorX
        at(451) { g.move(listOf(p(108f, 100f)), 451) }
        assertTrue(g.cursorX != x)
        at(460) { g.up(p(108f, 100f), 460) }
        assertEquals(listOf("left down", "left up"), buttons())
    }

    @Test
    fun liftingAfterADragKeepsTheButtonOnlyForTheResumeWindow() {
        beginTrackpadDrag()
        at(300) { g.up(p(160f, 100f), 300) }
        at(799)
        assertTrue(g.pressing)
        assertEquals(listOf("left click", "left down"), buttons())
        at(800)
        assertFalse(g.pressing)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
        assertEquals(listOf(true, false), pressingStates)
    }

    @Test
    fun regrippingResumesWithoutASecondPressOrPointerJump() {
        beginTrackpadDrag()
        at(300) { g.up(p(160f, 100f), 300) }
        val x = g.cursorX
        at(500) { g.down(p(900f, 100f), 500) }
        assertEquals(x, g.cursorX, 0.00001f)
        at(520) { g.move(listOf(p(920f, 100f)), 520) }
        assertTrue(g.cursorX > x && g.cursorX - x < 0.1f)
        at(540) { g.up(p(920f, 100f), 540) }
        at(800) // The old resume timer must have been cancelled.
        assertTrue(g.pressing)
        at(1040)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
        assertEquals(listOf(true, false), pressingStates)
    }

    @Test
    fun tappingDuringTheResumeWindowDropsWithoutAnotherClick() {
        beginTrackpadDrag()
        at(300) { g.up(p(160f, 100f), 300) }
        at(450) { g.down(p(900f, 100f), 450) }
        at(470) { g.up(p(900f, 100f), 470) }
        assertFalse(g.pressing)
        at(1200)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
    }

    @Test
    fun cancelReleasesAPendingDragImmediately() {
        beginTrackpadDrag()
        at(300) { g.up(p(160f, 100f), 300) }
        at(350) { g.cancel() }
        assertFalse(g.pressing)
        at(1200)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
        assertEquals(listOf(true, false), pressingStates)
    }

    @Test
    fun changingModeReleasesAPendingDragImmediately() {
        beginTrackpadDrag()
        at(300) { g.up(p(160f, 100f), 300) }
        at(350) { g.mode = Gestures.Mode.TOUCH }
        assertFalse(g.pressing)
        at(1200)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
    }

    @Test
    fun aNewDownWithoutAnUpReleasesTheOldButton() {
        beginTrackpadDrag()
        at(250) { g.down(p(800f, 400f), 250) }
        assertFalse(g.pressing)
        assertEquals(listOf("left click", "left down", "left up"), buttons())
        at(400) { g.up(p(800f, 400f), 400) }
        at(1200)
        assertEquals(listOf("left click", "left down", "left up", "left click"), buttons())
    }

    @Test
    fun aLateSecondFingerCannotTurnAPendingDragIntoRightClick() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(200) { g.down(p(100f, 100f), 200) }
        at(400) { g.pointerDown(p(200f, 100f, 1), listOf(p(100f, 100f), p(200f, 100f, 1)), 400) }
        at(500) { g.pointerUp(p(200f, 100f, 1), 1, 500) }
        at(520) { g.up(p(100f, 100f), 520) }
        at(1200)
        assertEquals(listOf("left click"), buttons())
    }

    @Test
    fun anExtraFingerAfterAStationaryLongPressReleasesLeftWithoutRight() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(450)
        at(600) { g.pointerDown(p(200f, 100f, 1), listOf(p(100f, 100f), p(200f, 100f, 1)), 600) }
        at(700) { g.pointerUp(p(200f, 100f, 1), 1, 700) }
        at(720) { g.up(p(100f, 100f), 720) }
        assertEquals(listOf("left down", "left up"), buttons())
    }

    @Test
    fun aNormalTwoFingerTapAfterALeftClickStillRightClicks() {
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(300) { g.down(p(100f, 100f), 300) }
        at(310) { g.pointerDown(p(200f, 100f, 1), listOf(p(100f, 100f), p(200f, 100f, 1)), 310) }
        at(400) { g.pointerUp(p(200f, 100f, 1), 1, 400) }
        at(410) { g.up(p(100f, 100f), 410) }
        assertEquals(listOf("left click", "right click"), buttons())
    }

    @Test
    fun aGameLookSwipeAfterATapNeverHoldsLeft() {
        g.relative = true
        g.lookGain = 1f
        at(0) { g.down(p(100f, 100f), 0) }
        at(80) { g.up(p(100f, 100f), 80) }
        at(150) { g.down(p(100f, 100f), 150) }
        at(200) { g.move(listOf(p(160f, 100f)), 200) }
        at(250) { g.up(p(160f, 100f), 250) }
        at(1200)
        assertEquals(listOf("left click"), buttons())
        assertFalse(g.pressing)
    }
}
