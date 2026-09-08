//! The AppKit shell: one window, one view, one audio device.
//!
//! Everything platform-specific lives here. The view holds the model and the
//! player, turns events into messages, and asks [`crate::draw`] to paint the
//! result — so swapping this file for a GTK or Win32 one would leave the model,
//! the layout, the drawing and the accessibility tree untouched.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use aqua::window::{WindowCommand, WindowDesc};
use leveller_audio::{Clip, Player};
use leveller_listen::{Answer, ClipQuestion, Peaks, Store, TrialQuestion};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAccessibilityButtonRole, NSAccessibilityCheckBoxRole, NSAccessibilityGroupRole,
    NSAccessibilityRadioButtonRole, NSAccessibilityRadioGroupRole, NSAccessibilityStaticTextRole,
    NSApplication, NSApplicationDelegate, NSEvent, NSGraphicsContext, NSTextField,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowDelegate,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSCopying, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString};

use crate::access;
use crate::draw::{self, Pointer, Transport};
use crate::layout::{self, Hit, Layout, TextKey};
use crate::model::{Effect, Model, Msg, Route};

const WINDOW_SIZE: (f64, f64) = (900.0, 760.0);
const MIN_SIZE: (f64, f64) = (620.0, 520.0);

pub struct State {
    model: RefCell<Model>,
    pointer: RefCell<Pointer>,
    /// The audio device, once something is loaded.
    player: RefCell<Option<Player>>,
    /// One waveform per loaded clip, in the same order.
    peaks: RefCell<Vec<Peaks>>,
    /// A region being dragged out on the annotation waveform, in seconds.
    dragging: Cell<Option<(f64, f64)>>,
    /// One invisible subview per accessibility element.
    elements: RefCell<Vec<Retained<AxProxy>>>,
    /// The real text fields, and what each one is for.
    fields: RefCell<Vec<(Retained<NSTextField>, TextKey)>>,
}

/// What one accessibility proxy knows about itself.
#[derive(Default)]
struct ProxyState {
    role: RefCell<Retained<NSString>>,
    label: RefCell<Retained<NSString>>,
    value: RefCell<Option<Retained<NSString>>>,
    help: RefCell<Option<Retained<NSString>>>,
    enabled: Cell<bool>,
    index: Cell<usize>,
}

define_class!(
    /// An invisible view standing in for one drawn control, so the
    /// accessibility system has something real to find.
    ///
    /// `hitTest:` refuses, so the mouse goes to the drawing view underneath.
    /// Synthetic `NSAccessibilityElement`s would be tidier and do not work: a
    /// custom view's synthetic children are filtered out somewhere inside
    /// AppKit. Real subviews are included because they are views.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "ListenAccessibilityProxy"]
    #[ivars = ProxyState]
    struct AxProxy;

    impl AxProxy {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> *mut NSView {
            std::ptr::null_mut()
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method(isAccessibilityEnabled))]
        fn is_accessibility_enabled(&self) -> bool {
            self.ivars().enabled.get()
        }

        #[unsafe(method(accessibilityRole))]
        fn accessibility_role(&self) -> *mut NSString {
            Retained::autorelease_return(self.ivars().role.borrow().clone())
        }

        #[unsafe(method(accessibilityLabel))]
        fn accessibility_label(&self) -> *mut NSString {
            Retained::autorelease_return(self.ivars().label.borrow().clone())
        }

        #[unsafe(method(accessibilityValue))]
        fn accessibility_value(&self) -> *mut NSString {
            match self.ivars().value.borrow().clone() {
                Some(value) => Retained::autorelease_return(value),
                None => std::ptr::null_mut(),
            }
        }

        #[unsafe(method(accessibilityHelp))]
        fn accessibility_help(&self) -> *mut NSString {
            match self.ivars().help.borrow().clone() {
                Some(help) => Retained::autorelease_return(help),
                None => std::ptr::null_mut(),
            }
        }

        /// The same path a click takes, so there is one way for a thing to
        /// happen rather than two that can disagree.
        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> objc2::runtime::Bool {
            let Some(parent) = (unsafe { self.superview() }) else {
                return objc2::runtime::Bool::NO;
            };
            let Ok(view) = parent.downcast::<ListenView>() else {
                return objc2::runtime::Bool::NO;
            };
            view.activate_element(self.ivars().index.get());
            objc2::runtime::Bool::YES
        }
    }

    unsafe impl NSObjectProtocol for AxProxy {}
);

impl AxProxy {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ProxyState {
            role: RefCell::new(NSString::from_str("AXUnknown")),
            label: RefCell::new(NSString::new()),
            ..ProxyState::default()
        });
        unsafe { msg_send![super(this), init] }
    }

    fn describe(&self, element: &access::Element) {
        let role = match element.role {
            access::Role::Button => unsafe { NSAccessibilityButtonRole },
            access::Role::Checkbox => unsafe { NSAccessibilityCheckBoxRole },
            access::Role::RadioGroup => unsafe { NSAccessibilityRadioGroupRole },
            access::Role::Radio => unsafe { NSAccessibilityRadioButtonRole },
            access::Role::StaticText => unsafe { NSAccessibilityStaticTextRole },
            access::Role::Group => unsafe { NSAccessibilityGroupRole },
        };
        *self.ivars().role.borrow_mut() = role.copy();
        *self.ivars().label.borrow_mut() = NSString::from_str(&element.label);
        *self.ivars().value.borrow_mut() = element.value.as_deref().map(NSString::from_str);
        *self.ivars().help.borrow_mut() = element.help.as_deref().map(NSString::from_str);
        self.ivars().enabled.set(element.enabled);
        self.ivars().index.set(0);
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "ListenView"]
    #[ivars = State]
    struct ListenView;

    impl ListenView {
        /// Top-left origin, so every offset in the layout reads the way the
        /// stylesheet wrote it.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Some(context) = NSGraphicsContext::currentContext() else {
                return;
            };
            let bounds = self.bounds();
            let model = self.ivars().model.borrow();
            let layout = layout::compute(&model, bounds.size.width, bounds.size.height);
            let peaks = self.ivars().peaks.borrow();
            let player = self.ivars().player.borrow();
            let selected = player.as_ref().map_or(0, Player::selected);
            let transport = Transport {
                playing: player.as_ref().is_some_and(Player::is_playing),
                position: player.as_ref().map_or(0.0, Player::position_secs),
                duration: player.as_ref().map_or(0.0, Player::duration_secs),
                peaks: peaks.get(selected),
                dragging: self.ivars().dragging.get(),
                // Real text fields are on top of the wells, so the drawing must
                // not put a second copy of the text underneath them.
                fields: false,
            };
            draw::scene(
                &context.CGContext(),
                &model,
                &layout,
                *self.ivars().pointer.borrow(),
                transport,
                bounds,
            );
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.point_of(event);
            let hit = self.layout().hit(point.x, point.y);

            // The title strip, anywhere but a light, drags the window.
            if hit == Some(Hit::TitleBar) {
                if let Some(window) = self.window() {
                    if event.clickCount() >= 2 {
                        window.performZoom(None);
                    } else {
                        window.performWindowDragWithEvent(event);
                    }
                }
                return;
            }

            // A drag across the annotation waveform marks a region, so the
            // press has to remember where it started.
            if hit == Some(Hit::Waveform)
                && let Some(t) = self.time_at(point.x)
            {
                self.ivars().dragging.set(Some((t, t)));
            }

            self.ivars().pointer.borrow_mut().pressed = hit;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let point = self.point_of(event);
            if let Some((from, _)) = self.ivars().dragging.get()
                && let Some(to) = self.time_at(point.x)
            {
                self.ivars().dragging.set(Some((from, to)));
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let pressed = self.ivars().pointer.borrow_mut().pressed.take();
            let point = self.point_of(event);
            let hit = self.layout().hit(point.x, point.y);

            if let Some((from, to)) = self.ivars().dragging.take() {
                // A drag of nearly nothing is a click that missed a region, and
                // marking a one-millisecond breath is never what was meant.
                if (to - from).abs() >= 0.01 {
                    self.send(Msg::AddAnnotation {
                        start: from,
                        end: to,
                    });
                } else {
                    self.send(Msg::SelectAnnotation(None));
                }
                self.setNeedsDisplay(true);
                return;
            }

            // A press and a release on the same thing is a click; anywhere else
            // is a cancelled one, which is what every control here does.
            if let Some(pressed) = pressed
                && Some(pressed) == hit
            {
                self.activate(pressed, point.x);
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let point = self.point_of(event);
            let hit = self.layout().hit(point.x, point.y);
            let mut pointer = self.ivars().pointer.borrow_mut();
            if pointer.hovered != hit {
                pointer.hovered = hit;
                drop(pointer);
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().pointer.borrow_mut().hovered = None;
            self.setNeedsDisplay(true);
        }

        /// The keyboard, which is how the app is actually meant to be used.
        ///
        /// Someone comparing four clips presses 1, 2, 3, 4 and space; someone
        /// marking breaths presses B. Reaching for the mouse between every
        /// judgement is what makes a listening test take an hour.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(characters) = event.charactersIgnoringModifiers() else {
                return;
            };
            let characters = characters.to_string();
            let Some(key) = characters.chars().next() else {
                return;
            };

            // Digits pick a clip on a trial, and nothing anywhere else.
            if let Some(digit) = key.to_digit(10)
                && digit >= 1
                && self.ivars().model.borrow().trial().is_some()
            {
                self.select_clip(digit as usize - 1);
                return;
            }

            match key {
                ' ' => self.toggle_play(),
                '+' | '=' => self.zoom(0.5),
                '-' | '_' => self.zoom(2.0),
                '\u{7f}' | '\u{8}' => self.delete_selected(),
                '\u{f702}' => self.scroll_view(-0.25),
                '\u{f703}' => self.scroll_view(0.25),
                letter => {
                    // A label's first letter selects it, which is why the label
                    // set is stored with the file rather than hard-coded: a
                    // project with different labels gets different shortcuts.
                    let index = {
                        let model = self.ivars().model.borrow();
                        model.annotate().and_then(|state| {
                            state.file.labels.iter().position(|name| {
                                name.chars()
                                    .next()
                                    .is_some_and(|c| c.eq_ignore_ascii_case(&letter))
                            })
                        })
                    };
                    if let Some(index) = index {
                        self.send(Msg::SetAnnotationLabel(index));
                    }
                }
            }
        }

        /// A text field finished editing.
        #[unsafe(method(textChanged:))]
        fn text_changed(&self, sender: &NSTextField) {
            let text = sender.stringValue().to_string();
            let key = self
                .ivars()
                .fields
                .borrow()
                .iter()
                .find(|(field, _)| std::ptr::eq(&**field, sender))
                .map(|(_, key)| key.clone());
            let Some(key) = key else { return };

            match key {
                TextKey::Listener => self.send(Msg::SetListener(text)),
                TextKey::Clip { label, question } => self.send(Msg::AnswerClip {
                    label,
                    question,
                    answer: Answer::Text(text),
                }),
                TextKey::Trial { question } => self.send(Msg::AnswerTrial {
                    question,
                    answer: Answer::Text(text),
                }),
                TextKey::Note => self.set_note(text),
            }
        }

        /// Called by a timer while something is playing, to move the playhead.
        #[unsafe(method(tick))]
        fn tick(&self) {
            if self
                .ivars()
                .player
                .borrow()
                .as_ref()
                .is_some_and(Player::is_playing)
            {
                self.setNeedsDisplay(true);
            }
        }
    }

    unsafe impl NSObjectProtocol for ListenView {}
);

impl ListenView {
    fn new(mtm: MainThreadMarker, store: Store) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(State {
            model: RefCell::new(Model::new(store)),
            pointer: RefCell::new(Pointer::default()),
            player: RefCell::new(None),
            peaks: RefCell::new(Vec::new()),
            dragging: Cell::new(None),
            elements: RefCell::new(Vec::new()),
            fields: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        // 30 Hz while something plays: enough for the playhead to look
        // continuous, and nothing at all when it does not.
        let _: Retained<objc2::runtime::AnyObject> = unsafe {
            msg_send![
                class!(NSTimer),
                scheduledTimerWithTimeInterval: 0.033f64,
                target: &*this,
                selector: sel!(tick),
                userInfo: std::ptr::null::<objc2::runtime::AnyObject>(),
                repeats: true,
            ]
        };

        this
    }

    fn layout(&self) -> Layout {
        let bounds = self.bounds();
        layout::compute(
            &self.ivars().model.borrow(),
            bounds.size.width,
            bounds.size.height,
        )
    }

    fn point_of(&self, event: &NSEvent) -> NSPoint {
        let window_point = event.locationInWindow();
        self.convertPoint_fromView(window_point, None)
    }

    /// Where an x position falls in the annotation file, in seconds.
    fn time_at(&self, x: f64) -> Option<f64> {
        let layout = self.layout();
        let page = layout.annotate()?;
        let model = self.ivars().model.borrow();
        let state = model.annotate()?;
        let inner = page.waveform.inset(3.0, 3.0);
        let fraction = ((x - inner.x) / inner.width.max(1.0)).clamp(0.0, 1.0);
        Some(state.view_start + fraction * state.view_secs)
    }

    /// Push a message through the model and act on whatever falls out.
    fn send(&self, msg: Msg) {
        let effect = self.ivars().model.borrow_mut().update(msg);
        match effect {
            Some(Effect::LoadClips(paths)) => self.load(&paths),
            Some(Effect::StopPlaying) => {
                *self.ivars().player.borrow_mut() = None;
                self.ivars().peaks.borrow_mut().clear();
            }
            None => {}
        }
        self.rebuild_elements();
        self.rebuild_fields();
        self.setNeedsDisplay(true);
    }

    /// Decode the clips and hand them to one player.
    ///
    /// All of them at once, into one device: switching between clips has to be
    /// instant and has to keep its place in the passage, and a player per clip
    /// could do neither.
    fn load(&self, paths: &[PathBuf]) {
        let mut clips = Vec::new();
        let mut peaks = Vec::new();
        for path in paths {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Ok(audio) = leveller_wav::decode(&bytes) else {
                continue;
            };
            peaks.push(Peaks::build(&audio.signal));
            clips.push(Clip {
                sample_rate: audio.signal.sample_rate(),
                channels: audio.signal.into_channels(),
            });
        }

        *self.ivars().peaks.borrow_mut() = peaks;
        *self.ivars().player.borrow_mut() = match Player::new(clips) {
            Ok(player) => Some(player),
            Err(error) => {
                eprintln!("listen: no audio output ({error})");
                None
            }
        };
    }

    fn toggle_play(&self) {
        {
            let held = self.ivars().player.borrow();
            let Some(player) = held.as_ref() else { return };
            if player.is_playing() {
                player.pause();
            } else {
                // Playing on from the end would be silence, so a finished clip
                // starts again.
                if player.finished() {
                    player.seek_secs(0.0);
                }
                player.play();
            }
        }
        self.setNeedsDisplay(true);
    }

    /// Switch clip without stopping, which is the whole point of the app.
    fn select_clip(&self, index: usize) {
        let count = {
            let model = self.ivars().model.borrow();
            model
                .trial()
                .and_then(|s| s.trial())
                .map_or(0, |t| t.clips.len())
        };
        if index >= count {
            return;
        }
        if let Some(player) = self.ivars().player.borrow().as_ref() {
            player.select(index);
        }
        self.send(Msg::SelectClip(index));
    }

    fn zoom(&self, by: f64) {
        let model = self.ivars().model.borrow();
        let Some(state) = model.annotate() else {
            return;
        };
        // Around the middle of what is showing, so zooming does not walk off
        // the thing being looked at.
        let centre = state.view_start + state.view_secs / 2.0;
        let secs = state.view_secs * by;
        let start = centre - secs / 2.0;
        drop(model);
        self.send(Msg::SetView { start, secs });
    }

    fn scroll_view(&self, by: f64) {
        let model = self.ivars().model.borrow();
        let Some(state) = model.annotate() else {
            return;
        };
        let secs = state.view_secs;
        let start = state.view_start + secs * by;
        drop(model);
        self.send(Msg::SetView { start, secs });
    }

    fn delete_selected(&self) {
        let id = {
            let model = self.ivars().model.borrow();
            model.annotate().and_then(|s| s.selected.clone())
        };
        if let Some(id) = id {
            self.send(Msg::DeleteAnnotation(id));
        }
    }

    fn set_note(&self, text: String) {
        // The note belongs to whichever region is selected, and there is no
        // sensible place to put it when none is.
        let mut model = self.ivars().model.borrow_mut();
        let Some(state) = model.annotate_mut() else {
            return;
        };
        let Some(id) = state.selected.clone() else {
            return;
        };
        if let Some(annotation) = state.file.annotations.iter_mut().find(|a| a.id == id) {
            annotation.note = (!text.is_empty()).then_some(text);
            state.dirty = true;
        }
        drop(model);
        self.send(Msg::Save);
    }

    /// Activate an element from the accessibility side, which is the same path
    /// a click takes.
    fn activate_element(&self, index: usize) {
        let hit = {
            let model = self.ivars().model.borrow();
            let bounds = self.bounds();
            let layout = layout::compute(&model, bounds.size.width, bounds.size.height);
            access::elements(&model, &layout)
                .get(index)
                .and_then(|e| e.hit)
        };
        if let Some(hit) = hit {
            // The centre of whatever it is, for the controls that care where
            // they were clicked.
            self.activate(hit, 0.0);
        }
    }

    fn activate(&self, hit: Hit, x: f64) {
        match hit {
            Hit::Light(light) => {
                if let Some(window) = self.window() {
                    WindowCommand::from(light).perform(&window);
                }
            }
            Hit::TitleBar => {}
            Hit::Home => self.send(Msg::Go(Route::Home)),

            Hit::OpenSession(index) => {
                let name = self.ivars().model.borrow().sessions().get(index).cloned();
                if let Some(session) = name {
                    self.send(Msg::Go(Route::Trial { session, index: 0 }));
                }
            }
            Hit::OpenResults(index) => {
                let name = self.ivars().model.borrow().sessions().get(index).cloned();
                if let Some(session) = name {
                    self.send(Msg::Go(Route::Results { session }));
                }
            }
            Hit::OpenAnnotate(index) => {
                let name = self.ivars().model.borrow().annotatable().get(index).cloned();
                if let Some(file) = name {
                    self.send(Msg::Go(Route::Annotate { file }));
                }
            }

            Hit::Clip(index) => self.select_clip(index),
            Hit::PlayPause => self.toggle_play(),
            Hit::Scrub => {
                let layout = self.layout();
                if let Some(page) = layout.trial()
                    && let Some(player) = self.ivars().player.borrow().as_ref()
                {
                    let inner = page.waveform.inset(3.0, 3.0);
                    let fraction = ((x - inner.x) / inner.width.max(1.0)).clamp(0.0, 1.0);
                    player.seek_secs(fraction * player.duration_secs());
                }
                self.setNeedsDisplay(true);
            }

            Hit::ClipScale(question, step) => self.answer_scale(question, step),
            Hit::ClipTag(question, option) => self.toggle_tag(question, option),
            Hit::TrialPick(question, clip) => self.pick(question, clip),
            Hit::PreviousTrial => self.send(Msg::PreviousTrial),
            Hit::NextTrial => self.send(Msg::NextTrial),
            Hit::Reveal => self.send(Msg::Reveal),

            Hit::Waveform => {}
            Hit::Region(index) => {
                let id = {
                    let model = self.ivars().model.borrow();
                    model
                        .annotate()
                        .and_then(|s| s.visible().get(index).map(|a| a.id.clone()))
                };
                self.send(Msg::SelectAnnotation(id));
            }
            Hit::Label(index) => self.send(Msg::SetAnnotationLabel(index)),
            Hit::Confidence(which) => self.send(Msg::SetAnnotationConfidence(which)),
            Hit::DeleteRegion => self.delete_selected(),
            Hit::ZoomIn => self.zoom(0.5),
            Hit::ZoomOut => self.zoom(2.0),
        }
    }

    /// Which clip the questions are about: whichever one is selected.
    fn current_clip(&self) -> Option<String> {
        let model = self.ivars().model.borrow();
        let state = model.trial()?;
        state
            .trial()?
            .clips
            .get(state.clip)
            .map(|c| c.label.clone())
    }

    fn answer_scale(&self, question: usize, step: usize) {
        let Some(label) = self.current_clip() else {
            return;
        };
        let asked = {
            let model = self.ivars().model.borrow();
            model
                .trial()
                .and_then(|s| s.clip_questions().get(question).cloned())
        };
        let Some(ClipQuestion::Scale { id, min, .. }) = asked else {
            return;
        };
        self.send(Msg::AnswerClip {
            label,
            question: id,
            answer: Answer::Number(min + step as f64),
        });
    }

    fn toggle_tag(&self, question: usize, option: usize) {
        let Some(label) = self.current_clip() else {
            return;
        };
        let (id, name, mut chosen) = {
            let model = self.ivars().model.borrow();
            let Some(state) = model.trial() else { return };
            let Some(ClipQuestion::Tags { id, options, .. }) = state.clip_questions().get(question)
            else {
                return;
            };
            let Some(name) = options.get(option).cloned() else {
                return;
            };
            let chosen = match state.clip_answer(&label, id) {
                Some(Answer::Tags(tags)) => tags.clone(),
                _ => Vec::new(),
            };
            (id.clone(), name, chosen)
        };

        // A tag is a toggle: pressing the one already on takes it off, which is
        // the only way to correct a mis-click.
        if let Some(at) = chosen.iter().position(|t| *t == name) {
            chosen.remove(at);
        } else {
            chosen.push(name);
        }
        self.send(Msg::AnswerClip {
            label,
            question: id,
            answer: Answer::Tags(chosen),
        });
    }

    fn pick(&self, question: usize, clip: usize) {
        let (id, label) = {
            let model = self.ivars().model.borrow();
            let Some(state) = model.trial() else { return };
            let Some(TrialQuestion::Pick { id, .. }) = state.trial_questions().get(question) else {
                return;
            };
            let Some(label) = state
                .trial()
                .and_then(|t| t.clips.get(clip))
                .map(|c| c.label.clone())
            else {
                return;
            };
            (id.clone(), label)
        };
        self.send(Msg::AnswerTrial {
            question: id,
            answer: Answer::Text(label),
        });
    }

    /// Rebuild the accessibility proxies from the model as it stands.
    fn rebuild_elements(&self) {
        let model = self.ivars().model.borrow();
        let bounds = self.bounds();
        let layout = layout::compute(&model, bounds.size.width, bounds.size.height);
        let wanted = access::elements(&model, &layout);
        drop(model);

        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let mut proxies = self.ivars().elements.borrow_mut();

        // Reuse the views rather than rebuilding them: an element that keeps
        // its identity keeps VoiceOver's place in the window.
        while proxies.len() > wanted.len() {
            if let Some(extra) = proxies.pop() {
                extra.removeFromSuperview();
            }
        }
        while proxies.len() < wanted.len() {
            let proxy = AxProxy::new(mtm);
            self.addSubview(&proxy);
            proxies.push(proxy);
        }
        for (index, (proxy, element)) in proxies.iter().zip(&wanted).enumerate() {
            proxy.setFrame(CGRect::new(
                CGPoint::new(element.rect.x, element.rect.y),
                CGSize::new(element.rect.width, element.rect.height),
            ));
            proxy.describe(element);
            proxy.ivars().index.set(index);
        }
    }

    /// Put a real `NSTextField` wherever the layout asked for one.
    fn rebuild_fields(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let bounds = self.bounds();
        let (layout, values) = {
            let model = self.ivars().model.borrow();
            let layout = layout::compute(&model, bounds.size.width, bounds.size.height);
            let values: Vec<String> = layout
                .fields
                .iter()
                .map(|field| layout::value_of(&model, &field.key))
                .collect();
            (layout, values)
        };

        let mut fields = self.ivars().fields.borrow_mut();
        while fields.len() > layout.fields.len() {
            if let Some((extra, _)) = fields.pop() {
                extra.removeFromSuperview();
            }
        }
        while fields.len() < layout.fields.len() {
            let field = NSTextField::new(mtm);
            field.setBordered(false);
            field.setDrawsBackground(false);
            unsafe {
                field.setTarget(Some(self));
                field.setAction(Some(sel!(textChanged:)));
            }
            self.addSubview(&field);
            fields.push((field, TextKey::Listener));
        }

        for ((field, key), (wanted, value)) in
            fields.iter_mut().zip(layout.fields.iter().zip(&values))
        {
            *key = wanted.key.clone();
            field.setFrame(CGRect::new(
                CGPoint::new(wanted.rect.x + 4.0, wanted.rect.y + 3.0),
                CGSize::new(wanted.rect.width - 8.0, wanted.rect.height - 6.0),
            ));
            field.setPlaceholderString(Some(&NSString::from_str(wanted.placeholder)));
            // A field with a live field editor is the one being typed in, and
            // replacing its contents under the caret would eat what is half
            // written.
            if field.currentEditor().is_none() {
                field.setStringValue(&NSString::from_str(value));
            }
        }
    }

    /// Track the mouse, so hover states work.
    fn refresh_tracking(&self) {
        for area in self.trackingAreas().iter() {
            self.removeTrackingArea(&area);
        }
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                self.bounds(),
                NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::ActiveInKeyWindow
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(self),
                None,
            )
        };
        self.addTrackingArea(&area);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ListenDelegate"]
    #[ivars = RefCell<Option<Retained<ListenView>>>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        /// Aqua flattens and mutes a window that is not in front, and the whole
        /// interface is painted, so it has to be told which way round it is.
        #[unsafe(method(windowDidBecomeMain:))]
        fn did_become_main(&self, _notification: &NSNotification) {
            self.set_active(true);
        }

        #[unsafe(method(windowDidResignMain:))]
        fn did_resign_main(&self, _notification: &NSNotification) {
            self.set_active(false);
        }

        /// Whatever is open is written before the window goes.
        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _notification: &NSNotification) {
            if let Some(view) = self.ivars().borrow().as_ref() {
                view.send(Msg::Save);
            }
        }
    }

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn set_active(&self, active: bool) {
        if let Some(view) = self.ivars().borrow().as_ref() {
            view.send(Msg::WindowActive(active));
        }
    }
}

/// Where the sessions and the files to annotate live.
pub fn default_store() -> Store {
    // Beside the project by default, and overridable — the listening material
    // is gigabytes of WAV and does not belong in the repository.
    if let Ok(root) = std::env::var("LISTENING_ROOT") {
        return Store::new(root);
    }
    Store::new(
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("listening"),
    )
}

/// Bring up the window and run.
pub fn run() -> ! {
    let mtm = MainThreadMarker::new().expect("the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Regular);
    aqua::window::install_menu(mtm, "Listen");

    let window = aqua::window::make_window(
        mtm,
        &WindowDesc {
            title: "Listen".into(),
            size: WINDOW_SIZE,
            min_size: MIN_SIZE,
        },
    );

    let view = ListenView::new(mtm, default_store());
    view.setFrame(CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1),
    ));
    view.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    view.refresh_tracking();
    window.setContentView(Some(&view));

    let delegate = Delegate::alloc(mtm).set_ivars(RefCell::new(Some(view.clone())));
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    window.makeKeyAndOrderFront(None);
    // A window can open behind another one, so the first paint has to know
    // rather than assume.
    view.send(Msg::WindowActive(window.isMainWindow()));

    app.activate();
    app.run();
    unreachable!("the run loop does not return")
}

/// Draw a page into a PNG without opening a window.
///
/// The same drawing code the window runs, so the picture is the app rather than
/// a mock-up of it — which is how the interface gets looked at on a machine
/// with no screen-recording permission.
pub fn shot(path: &Path, page: &str, store: Store) -> Result<(), std::io::Error> {
    // Onto a copy, because drawing a page runs the real model and the real
    // model saves as it goes — and a screenshot must not put invented scores
    // into somebody's session.
    let scratch = std::env::temp_dir().join(format!("listen-shot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_tree(store.root(), &scratch)?;
    let mut model = Model::new(Store::new(&scratch));
    let session = model.sessions().first().cloned().unwrap_or_default();
    let file = model.annotatable().first().cloned().unwrap_or_default();

    match page {
        "trial" => {
            model.update(Msg::Go(Route::Trial {
                session,
                index: 0,
            }));
            // A trial part-way through being scored, so the drawing's answered
            // and unanswered branches are both in the picture.
            model.update(Msg::AnswerClip {
                label: "A".into(),
                question: "distortion".into(),
                answer: Answer::Number(1.0),
            });
            model.update(Msg::AnswerClip {
                label: "A".into(),
                question: "artefacts".into(),
                answer: Answer::Tags(vec!["dull".into(), "thin".into()]),
            });
            model.update(Msg::AnswerTrial {
                question: "best".into(),
                answer: Answer::Text("B".into()),
            });
        }
        "results" => {
            // Two listeners' scores, so the table has numbers in it rather than
            // a column of dashes. Written through the model, which means they
            // are read back the way a real session's would be.
            for (listener, bias) in [("nino", 0.0f64), ("sam", 1.0)] {
                model.update(Msg::SetListener(listener.into()));
                for (trial, scores) in [
                    ("t01", [1.0f64, 3.0, 4.0]),
                    ("t02", [0.0, 2.0, 4.0]),
                    ("t03", [1.0, 2.0, 5.0]),
                ] {
                    model.update(Msg::Go(Route::Trial {
                        session: session.clone(),
                        index: match trial {
                            "t01" => 0,
                            "t02" => 1,
                            _ => 2,
                        },
                    }));
                    for (label, score) in ["A", "B", "C"].iter().zip(scores) {
                        model.update(Msg::AnswerClip {
                            label: (*label).into(),
                            question: "distortion".into(),
                            answer: Answer::Number((score + bias).min(5.0)),
                        });
                    }
                }
            }
            model.update(Msg::Go(Route::Results { session }));
            model.update(Msg::Reveal);
        }
        "annotate" => {
            model.update(Msg::Go(Route::Annotate { file }));
            // A couple of regions, so the picture shows what a marked-up file
            // looks like rather than an empty one.
            model.update(Msg::SetView {
                start: 0.0,
                secs: 12.0,
            });
            model.update(Msg::AddAnnotation {
                start: 1.9,
                end: 2.35,
            });
            model.update(Msg::SetAnnotationLabel(1));
            model.update(Msg::AddAnnotation {
                start: 5.1,
                end: 5.6,
            });
            model.update(Msg::SetAnnotationLabel(0));
            model.update(Msg::AddAnnotation {
                start: 8.4,
                end: 9.0,
            });
            model.update(Msg::SetAnnotationConfidence(
                leveller_listen::Confidence::Maybe,
            ));
        }
        _ => {}
    }

    // The waveform the picture shows is the real one, built from the clip on
    // disk the same way the running app builds it.
    let loaded = model
        .trial()
        .and_then(|state| state.paths.first().cloned())
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| leveller_wav::decode(&bytes).ok())
        .map(|audio| Peaks::build(&audio.signal));
    let peaks = model.annotate().map(|state| &state.peaks).or(loaded.as_ref());
    let _ = &scratch;
    let first = layout::compute(&model, WINDOW_SIZE.0, WINDOW_SIZE.1);
    let height = first.content_height.max(WINDOW_SIZE.1);
    aqua::render::to_png(path, WINDOW_SIZE.0, height, 2.0, |ctx, bounds| {
        let layout = layout::compute(&model, WINDOW_SIZE.0, height);
        let transport = Transport {
            playing: false,
            position: 0.0,
            duration: 0.0,
            peaks,
            dragging: None,
            fields: true,
        };
        draw::scene(
            ctx,
            &model,
            &layout,
            Pointer::default(),
            transport,
            bounds,
        );
    })
}

/// Copy a directory, for the throwaway store the screenshots run against.
fn copy_tree(from: &Path, to: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
