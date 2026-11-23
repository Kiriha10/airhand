//! 合成点击的时序。真实鼠标、拖动仍原样注入；时间从平台注入调用完成后开始算。
//! 同一会话的后续事件排在点击释放后，避免一次轻点之后的移动变成拖动。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use airhand_proto::session::{InputEvent, MouseButton};

/// 与常见游戏的当前按钮状态轮询兼容；GameViewer 的合成点击在控制端等待 70–99 ms。
const HOLD: Duration = Duration::from_millis(80);
/// 连点时也必须能观察到松开，不能让相邻的 UP、DOWN 再挤在一起。
const RELEASE: Duration = Duration::from_millis(40);

#[derive(Default)]
struct Button {
    down: Option<(Instant, bool)>,
    up: Option<Instant>,
}

#[derive(Default)]
pub(super) struct ClickTiming {
    buttons: HashMap<MouseButton, Button>,
}

impl ClickTiming {
    /// `None`：忽略这个合成点击事件（例如鼠标键已由拖动或游戏按键按住）。
    pub(super) fn delay(&self, event: &InputEvent, now: Instant) -> Option<Duration> {
        let InputEvent::MouseButton {
            button,
            down,
            click,
        } = event
        else {
            return Some(Duration::ZERO);
        };
        if !button.is_known() {
            return None;
        }
        if !click {
            return Some(Duration::ZERO);
        }
        let state = self.buttons.get(button);
        if *down {
            // 合成点击不能松开已由真实输入按住的同一个按钮。
            if state.is_some_and(|s| matches!(s.down, Some((_, false)))) {
                return None;
            }
            Some(state.and_then(|s| s.up).map_or(Duration::ZERO, |up| {
                (up + RELEASE).saturating_duration_since(now)
            }))
        } else {
            let (pressed, synthetic) = state?.down?;
            synthetic.then(|| (pressed + HOLD).saturating_duration_since(now))
        }
    }

    /// 只在事件实际注入完后记录；被忽略的事件不改变状态。
    pub(super) fn injected(&mut self, event: &InputEvent, now: Instant) {
        match event {
            InputEvent::MouseButton {
                button,
                down,
                click,
            } => {
                let state = self.buttons.entry(*button).or_default();
                if *down {
                    state.down = Some((now, *click));
                } else {
                    state.down = None;
                    state.up = Some(now);
                }
            }
            InputEvent::ReleaseAll => self.clear(),
            _ => {}
        }
    }

    pub(super) fn clear(&mut self) {
        self.buttons.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(button: MouseButton, down: bool, click: bool) -> InputEvent {
        InputEvent::MouseButton {
            button,
            down,
            click,
        }
    }

    #[test]
    fn a_synthetic_click_holds_after_injection_not_after_arrival() {
        let mut timing = ClickTiming::default();
        let arrived = Instant::now();
        let down = button(MouseButton::Left, true, true);
        let up = button(MouseButton::Left, false, true);
        assert_eq!(timing.delay(&down, arrived), Some(Duration::ZERO));
        // 平台队列堵了两秒：仍要从实际注入完按下之后保持完整的 HOLD。
        let injected = arrived + Duration::from_secs(2);
        timing.injected(&down, injected);
        assert_eq!(timing.delay(&up, injected), Some(HOLD));
        assert_eq!(
            timing.delay(&up, injected + Duration::from_millis(25)),
            Some(Duration::from_millis(55))
        );
        assert_eq!(timing.delay(&up, injected + HOLD), Some(Duration::ZERO));
    }

    #[test]
    fn consecutive_clicks_have_a_release_interval_for_each_button() {
        let mut timing = ClickTiming::default();
        let t = Instant::now();
        let down = button(MouseButton::Right, true, true);
        let up = button(MouseButton::Right, false, true);
        timing.injected(&down, t);
        timing.injected(&up, t + HOLD);
        assert_eq!(timing.delay(&down, t + HOLD), Some(RELEASE));
        assert_eq!(
            timing.delay(&down, t + HOLD + Duration::from_millis(10)),
            Some(Duration::from_millis(30))
        );
        assert_eq!(
            timing.delay(&down, t + HOLD + RELEASE),
            Some(Duration::ZERO)
        );
        assert_eq!(
            timing.delay(&button(MouseButton::Left, true, true), t + HOLD),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn real_buttons_and_drag_release_are_not_delayed() {
        let mut timing = ClickTiming::default();
        let t = Instant::now();
        let down = button(MouseButton::Left, true, false);
        let up = button(MouseButton::Left, false, false);
        timing.injected(&down, t);
        assert_eq!(timing.delay(&up, t), Some(Duration::ZERO));
        timing.injected(&up, t);
        assert_eq!(timing.delay(&down, t), Some(Duration::ZERO));
    }

    #[test]
    fn tapping_does_not_release_a_button_held_by_real_input() {
        let mut timing = ClickTiming::default();
        let t = Instant::now();
        timing.injected(&button(MouseButton::Left, true, false), t);
        assert_eq!(
            timing.delay(&button(MouseButton::Left, true, true), t),
            None
        );
        assert_eq!(
            timing.delay(&button(MouseButton::Left, false, true), t),
            None
        );
        assert_eq!(
            timing.delay(&button(MouseButton::Left, false, false), t),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn release_all_forgets_clicks_and_ignores_an_old_synthetic_release() {
        let mut timing = ClickTiming::default();
        let t = Instant::now();
        let down = button(MouseButton::Left, true, true);
        let up = button(MouseButton::Left, false, true);
        timing.injected(&down, t);
        timing.injected(&InputEvent::ReleaseAll, t);
        assert_eq!(timing.delay(&up, t), None);
        assert_eq!(timing.delay(&down, t), Some(Duration::ZERO));
        timing.injected(&button(MouseButton::Left, true, false), t);
        assert_eq!(timing.delay(&up, t), None);
    }
}
