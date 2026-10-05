use gpui::{
    Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, Styled, Window, div, px,
};

use crate::theme;

pub const TEXT_FIELD_CONTEXT: &str = "TextField";

const CARET_WIDTH: f32 = 1.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextFieldEvent {
    Changed(String),
    Submitted(String),
    Cancelled,
}

pub struct TextField {
    value: String,
    placeholder: SharedString,
    focus_handle: FocusHandle,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl TextField {
    pub fn new(
        value: impl Into<String>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            value: value.into(),
            placeholder: placeholder.into(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.value.is_empty() {
            self.value.clear();
            cx.emit(TextFieldEvent::Changed(String::new()));
            cx.notify();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        match keystroke.key.as_str() {
            "enter" => cx.emit(TextFieldEvent::Submitted(self.value.clone())),
            "escape" => cx.emit(TextFieldEvent::Cancelled),
            "backspace" => {
                if self.value.pop().is_some() {
                    cx.emit(TextFieldEvent::Changed(self.value.clone()));
                }
            }
            _ => {
                let typed = keystroke
                    .key_char
                    .as_deref()
                    .filter(|typed| !typed.chars().any(char::is_control));
                if let Some(typed) = typed {
                    self.value.push_str(typed);
                    cx.emit(TextFieldEvent::Changed(self.value.clone()));
                }
            }
        }
        cx.notify();
        cx.stop_propagation();
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let shown = if self.value.is_empty() {
            div()
                .text_color(theme::text_muted())
                .child(self.placeholder.clone())
        } else {
            div().child(self.value.clone())
        };
        div()
            .key_context(TEXT_FIELD_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|field, event, _, cx| field.key_down(event, cx)))
            .px_1()
            .h_full()
            .min_h(px(18.))
            .flex()
            .items_center()
            .rounded_sm()
            .border_1()
            .border_color(if focused {
                theme::selection()
            } else {
                theme::border()
            })
            .bg(theme::background())
            .text_color(theme::text())
            .child(shown)
            .children(focused.then(|| div().w(px(CARET_WIDTH)).h(px(12.)).bg(theme::text())))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    fn field_in_window<'a>(
        cx: &'a mut TestAppContext,
        value: &str,
    ) -> (gpui::Entity<TextField>, &'a mut VisualTestContext) {
        let value = value.to_owned();
        let (field, cx) = cx.add_window_view(|_, cx| TextField::new(value, "type here", cx));
        field.update_in(cx, |field, window, _| field.focus(window));
        (field, cx)
    }

    fn events_of(
        field: &gpui::Entity<TextField>,
        cx: &mut VisualTestContext,
    ) -> std::rc::Rc<std::cell::RefCell<Vec<TextFieldEvent>>> {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = events.clone();
        let subscription = cx.update(|_, cx| {
            cx.subscribe(field, move |_, event: &TextFieldEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
        });
        std::mem::forget(subscription);
        events
    }

    #[gpui::test]
    fn typing_appends_and_backspace_removes(cx: &mut TestAppContext) {
        let (field, cx) = field_in_window(cx, "ab");
        let events = events_of(&field, cx);

        cx.simulate_keystrokes("c space d");
        cx.simulate_keystrokes("backspace");

        assert_eq!(cx.read(|cx| field.read(cx).value().to_owned()), "abc ");
        assert_eq!(
            events.borrow().last(),
            Some(&TextFieldEvent::Changed("abc ".into()))
        );
    }

    #[gpui::test]
    fn enter_submits_and_escape_cancels(cx: &mut TestAppContext) {
        let (field, cx) = field_in_window(cx, "name");
        let events = events_of(&field, cx);

        cx.simulate_keystrokes("enter escape");

        assert_eq!(
            *events.borrow(),
            [
                TextFieldEvent::Submitted("name".into()),
                TextFieldEvent::Cancelled
            ]
        );
    }

    #[gpui::test]
    fn modified_keys_are_left_to_the_shortcuts(cx: &mut TestAppContext) {
        let (field, cx) = field_in_window(cx, "");
        let events = events_of(&field, cx);

        cx.simulate_keystrokes("ctrl-a");

        assert_eq!(cx.read(|cx| field.read(cx).value().to_owned()), "");
        assert!(events.borrow().is_empty());
    }
}
