use gpui::{
    App, Bounds, DispatchPhase, Entity, Hitbox, HitboxBehavior, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Styled, Window, canvas, fill,
    point, px, size,
};

use super::TimelinePanel;
use crate::theme;

pub const SCROLLBAR_THICKNESS: f32 = 8.;
const MIN_THUMB_FRACTION: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub start: f32,
    pub size: f32,
}

impl Thumb {
    pub fn new(offset: f64, visible: f64, extent: f64) -> Option<Self> {
        if !(extent > visible && extent > 0.) {
            return None;
        }
        let size = ((visible / extent) as f32).clamp(MIN_THUMB_FRACTION, 1.);
        let start = ((offset / extent) as f32).clamp(0., 1. - size);
        Some(Self { start, size })
    }

    pub fn grab_at(self, fraction: f32) -> f32 {
        if (self.start..=self.start + self.size).contains(&fraction) {
            fraction - self.start
        } else {
            self.size / 2.
        }
    }

    pub fn start_for(self, fraction: f32, grab: f32) -> f32 {
        (fraction - grab).clamp(0., 1. - self.size)
    }
}

pub fn scrollbar(
    panel: Entity<TimelinePanel>,
    axis: Axis,
    thumb: Option<Thumb>,
) -> impl IntoElement {
    let bar = canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::BlockMouse),
        move |bounds, hitbox, window, cx| {
            let Some(thumb) = thumb else {
                return;
            };
            window.paint_quad(fill(bounds, theme::panel()));
            window.paint_quad(fill(thumb_bounds(bounds, axis, thumb), theme::border()));
            listen(panel, axis, bounds, hitbox, window, cx);
        },
    );
    match axis {
        Axis::Horizontal => bar.h(px(SCROLLBAR_THICKNESS)).w_full().flex_none(),
        Axis::Vertical => bar.w(px(SCROLLBAR_THICKNESS)).h_full().flex_none(),
    }
}

fn thumb_bounds(bounds: Bounds<Pixels>, axis: Axis, thumb: Thumb) -> Bounds<Pixels> {
    match axis {
        Axis::Horizontal => Bounds::new(
            point(
                bounds.left() + bounds.size.width * thumb.start,
                bounds.top(),
            ),
            size(bounds.size.width * thumb.size, bounds.size.height),
        ),
        Axis::Vertical => Bounds::new(
            point(
                bounds.left(),
                bounds.top() + bounds.size.height * thumb.start,
            ),
            size(bounds.size.width, bounds.size.height * thumb.size),
        ),
    }
}

pub fn fraction_along(bounds: Bounds<Pixels>, axis: Axis, position: Point<Pixels>) -> f32 {
    let (along, length) = match axis {
        Axis::Horizontal => (position.x - bounds.left(), bounds.size.width),
        Axis::Vertical => (position.y - bounds.top(), bounds.size.height),
    };
    if length <= px(0.) {
        0.
    } else {
        f32::from(along) / f32::from(length)
    }
}

fn listen(
    panel: Entity<TimelinePanel>,
    axis: Axis,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
    _: &mut App,
) {
    window.on_mouse_event({
        let panel = panel.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                let fraction = fraction_along(bounds, axis, event.position);
                panel.update(cx, |panel, cx| panel.press_scrollbar(axis, fraction, cx));
            }
        }
    });
    window.on_mouse_event({
        let panel = panel.clone();
        move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && panel.read(cx).is_scrolling_with(axis) {
                let fraction = fraction_along(bounds, axis, event.position);
                panel.update(cx, |panel, cx| {
                    if event.dragging() {
                        panel.drag_scrollbar(axis, fraction, cx);
                    } else {
                        panel.release_scrollbar(cx);
                    }
                });
            }
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble
            && event.button == MouseButton::Left
            && panel.read(cx).is_scrolling_with(axis)
        {
            panel.update(cx, |panel, cx| panel.release_scrollbar(cx));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumb_shows_the_visible_share_and_hides_when_everything_fits() {
        assert_eq!(Thumb::new(0., 100., 100.), None);

        let thumb = Thumb::new(50., 100., 200.).unwrap();

        assert_eq!(
            thumb,
            Thumb {
                start: 0.25,
                size: 0.5
            }
        );
        assert_eq!(
            Thumb::new(0., 1., 1_000_000.).unwrap().size,
            MIN_THUMB_FRACTION
        );
    }

    #[test]
    fn dragging_keeps_the_grab_and_pressing_beside_the_thumb_centres_it() {
        let thumb = Thumb {
            start: 0.25,
            size: 0.5,
        };

        assert_eq!(thumb.grab_at(0.5), 0.25);
        assert_eq!(thumb.grab_at(0.9), 0.25);
        assert_eq!(thumb.start_for(0.75, 0.25), 0.5);
        assert_eq!(thumb.start_for(0.0, 0.25), 0.);
        assert_eq!(thumb.start_for(1.0, 0.1), 0.5);
    }
}
