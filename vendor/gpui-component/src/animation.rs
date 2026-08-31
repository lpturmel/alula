use std::time::Instant;

use gpui::{
    AnyElement, App, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, Window,
};

/// A cubic bezier function like CSS `cubic-bezier`.
///
/// Builder:
///
/// https://cubic-bezier.com
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 {
    move |t: f32| {
        let one_t = 1.0 - t;
        let one_t2 = one_t * one_t;
        let t2 = t * t;
        let t3 = t2 * t;

        // The Bezier curve function for x and y, where x0 = 0, y0 = 0, x3 = 1, y3 = 1
        let _x = 3.0 * x1 * one_t2 * t + 3.0 * x2 * one_t * t2 + t3;
        let y = 3.0 * y1 * one_t2 * t + 3.0 * y2 * one_t * t2 + t3;

        y
    }
}

/// Adds an animation whose restart key is separate from its stable element identity.
///
/// GPUI namespaces all interactive state below an animation by the animation's ID. Using a
/// changing ID to restart feedback can therefore replace a control between mouse-down and
/// mouse-up. This variant keeps the namespace stable while restarting its clock when
/// `restart_key` changes.
pub trait StableAnimationExt {
    fn with_stable_animation(
        self,
        id: impl Into<ElementId>,
        restart_key: impl Into<ElementId>,
        animation: gpui::Animation,
        animator: impl Fn(Self, f32) -> Self + 'static,
    ) -> StableAnimationElement<Self>
    where
        Self: Sized,
    {
        StableAnimationElement {
            id: id.into(),
            restart_key: restart_key.into(),
            element: Some(self),
            animation,
            animator: Box::new(animator),
        }
    }
}

impl<E: IntoElement + 'static> StableAnimationExt for E {}

pub struct StableAnimationElement<E> {
    id: ElementId,
    restart_key: ElementId,
    element: Option<E>,
    animation: gpui::Animation,
    animator: Box<dyn Fn(E, f32) -> E + 'static>,
}

struct StableAnimationState {
    restart_key: ElementId,
    start: Instant,
}

impl<E: IntoElement + 'static> IntoElement for StableAnimationElement<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: IntoElement + 'static> Element for StableAnimationElement<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        window.with_element_state(
            global_id.expect("stable animation requires an element ID"),
            |state, window| {
                let now = Instant::now();
                let mut state = state.unwrap_or_else(|| StableAnimationState {
                    restart_key: self.restart_key.clone(),
                    start: now,
                });
                if state.restart_key != self.restart_key {
                    state.restart_key = self.restart_key.clone();
                    state.start = now;
                }

                let duration = self.animation.duration.as_secs_f32();
                let elapsed = state.start.elapsed().as_secs_f32();
                let raw_delta = if duration > 0.0 {
                    elapsed / duration
                } else {
                    1.0
                };
                let done = self.animation.oneshot && raw_delta >= 1.0;
                let delta = if self.animation.oneshot {
                    raw_delta.min(1.0)
                } else {
                    raw_delta % 1.0
                };
                let delta = (self.animation.easing)(delta);

                let element = self
                    .element
                    .take()
                    .expect("layout should only be requested once");
                let mut element = (self.animator)(element, delta).into_any_element();
                if !done {
                    window.request_animation_frame();
                }

                ((element.request_layout(window, cx), element), state)
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, time::Duration};

    use gpui::{
        Animation, ClickEvent, Context, InteractiveElement as _, Modifiers, MouseButton, Render,
        StatefulInteractiveElement as _, Styled as _, TestAppContext, Window, div, point, px,
    };

    use super::StableAnimationExt as _;

    struct ClickProbe {
        animation_generation: u64,
        clicks: Rc<Cell<usize>>,
    }

    impl Render for ClickProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            div()
                .id("click-probe")
                .size(px(100.))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _, _| {
                        this.clicks.set(this.clicks.get() + 1)
                    }),
                )
                .with_stable_animation(
                    "click-feedback",
                    self.animation_generation as usize,
                    Animation::new(Duration::from_millis(120)),
                    |this, delta| this.opacity(delta),
                )
        }
    }

    #[gpui::test]
    fn click_survives_feedback_rerender_between_press_and_release(cx: &mut TestAppContext) {
        let clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, _| ClickProbe {
                animation_generation: 0,
                clicks,
            }
        });
        let position = point(px(50.), px(50.));

        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
        view.update(cx, |this, cx| {
            this.animation_generation += 1;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());

        assert_eq!(clicks.get(), 1);
    }
}
