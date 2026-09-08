//! The AppKit shell: one window, one view, and a worker thread.
//!
//! Everything platform-specific lives here. The view holds the model, turns
//! events into messages, and asks [`crate::draw`] to paint the result — so
//! swapping this file for a GTK or Win32 one would leave the model, the layout
//! and the drawing untouched.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use aqua::palette::Focus;
use aqua::window::{WindowCommand, WindowDesc};
use leveller_ui::{Effect, Model, Msg};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibilityButtonRole, NSAccessibilityCheckBoxRole, NSAccessibilityGroupRole,
    NSAccessibilityProgressIndicatorRole, NSAccessibilityRadioButtonRole,
    NSAccessibilityRadioGroupRole, NSAccessibilitySliderRole, NSAccessibilityStaticTextRole,
    NSApplication, NSApplicationDelegate, NSDragOperation, NSDraggingInfo, NSEvent,
    NSGraphicsContext, NSPasteboardTypeFileURL, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSWindowDelegate,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSCopying, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString,
    NSURL,
};

use crate::access;
use crate::draw::{self, Pointer};
use crate::layout::{self, Hit, Layout};

const WINDOW_SIZE: (f64, f64) = (760.0, 900.0);
const MIN_SIZE: (f64, f64) = (560.0, 560.0);

/// What the worker thread sends back.
enum FromWorker {
    Progress {
        stage: String,
        index: usize,
        total: usize,
        overall: f64,
    },
    Finished(Result<Box<leveller_io::ProcessResult>, String>),
}

pub struct State {
    model: RefCell<Model>,
    pointer: RefCell<Pointer>,
    /// The worker's channel, drained on the main thread by a timer.
    inbox: RefCell<Option<Receiver<FromWorker>>>,
    /// One invisible subview per accessibility element, rebuilt whenever the
    /// model changes.
    elements: RefCell<Vec<Retained<AxProxy>>>,
}

/// What one accessibility proxy knows about itself.
#[derive(Default)]
struct ProxyState {
    role: RefCell<Retained<NSString>>,
    label: RefCell<Retained<NSString>>,
    value: RefCell<Option<Retained<NSString>>>,
    help: RefCell<Option<Retained<NSString>>>,
    enabled: std::cell::Cell<bool>,
    /// Which element this is, so activating it can be routed back.
    index: std::cell::Cell<usize>,
}

define_class!(
    /// An invisible view standing in for one drawn control, so the
    /// accessibility system has something real to find.
    ///
    /// It draws nothing and takes no clicks — `hitTest:` refuses, so the mouse
    /// goes to the view underneath, which is the one that knows how to handle
    /// it. What it does have is a role, a name, a value and an action, which is
    /// everything a screen reader wants and none of what a mouse does.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "AudioLevellerAccessibilityProxy"]
    #[ivars = ProxyState]
    struct AxProxy;

    impl AxProxy {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> *mut NSView {
            // Transparent to the mouse: the drawing view handles every click.
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
            // SAFETY: called on the main thread, from the accessibility system.
            let Some(parent) = (unsafe { self.superview() }) else {
                return objc2::runtime::Bool::NO;
            };
            let Ok(view) = parent.downcast::<LevellerView>() else {
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
            access::Role::Button | access::Role::Disclosure => unsafe { NSAccessibilityButtonRole },
            access::Role::Checkbox => unsafe { NSAccessibilityCheckBoxRole },
            access::Role::RadioGroup => unsafe { NSAccessibilityRadioGroupRole },
            access::Role::Radio => unsafe { NSAccessibilityRadioButtonRole },
            access::Role::Slider => unsafe { NSAccessibilitySliderRole },
            access::Role::StaticText => unsafe { NSAccessibilityStaticTextRole },
            access::Role::Group => unsafe { NSAccessibilityGroupRole },
            access::Role::ProgressIndicator => unsafe { NSAccessibilityProgressIndicatorRole },
        };
        *self.ivars().role.borrow_mut() = role.copy();
        *self.ivars().label.borrow_mut() = NSString::from_str(&element.label);
        *self.ivars().value.borrow_mut() = element.value.as_deref().map(NSString::from_str);
        *self.ivars().help.borrow_mut() = element.help.as_deref().map(NSString::from_str);
        self.ivars().enabled.set(element.enabled);
    }

    fn set_index(&self, index: usize) {
        self.ivars().index.set(index);
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "AudioLevellerView"]
    #[ivars = State]
    struct LevellerView;

    impl LevellerView {
        /// Top-left origin, so every offset in the layout and the drawing reads
        /// the way the stylesheet wrote it.
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
            draw::scene(
                &context.CGContext(),
                &model,
                &layout,
                *self.ivars().pointer.borrow(),
                bounds,
            );
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.point_of(event);
            let hit = self.layout().hit(point.x, point.y);

            // The title strip, anywhere but a light, drags the window — and a
            // double-click there does whatever the system is set to do.
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

            self.ivars().pointer.borrow_mut().pressed = hit;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            // A slider tracks while the button is down; everything else only
            // cares where the mouse comes up.
            let pressed = self.ivars().pointer.borrow().pressed;
            if let Some(Hit::Slider(stage, param)) = pressed {
                let point = self.point_of(event);
                self.drag_slider(stage, param, point.x);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let pressed = self.ivars().pointer.borrow_mut().pressed.take();
            let point = self.point_of(event);
            let hit = self.layout().hit(point.x, point.y);

            // A press and a release on the same thing is a click; anywhere else
            // is a cancelled one, which is what every control on this platform
            // does.
            if let Some(pressed) = pressed
                && (pressed == hit.unwrap_or(pressed) || matches!(pressed, Hit::Slider(..)))
            {
                self.activate(pressed);
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

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if dropped_paths(sender).is_empty() {
                return NSDragOperation::None;
            }
            self.send(Msg::DragOver(true));
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            self.send(Msg::DragOver(false));
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> objc2::runtime::Bool {
            let Some(path) = dropped_paths(sender).into_iter().next() else {
                self.send(Msg::DragOver(false));
                return objc2::runtime::Bool::NO;
            };
            self.send(Msg::Dropped(path));
            objc2::runtime::Bool::YES
        }

        /// Called by a timer while a run is going, to move what the worker sent
        /// onto the main thread.
        #[unsafe(method(drainWorker))]
        fn drain_worker(&self) {
            let mut finished = false;
            let messages: Vec<FromWorker> = {
                let inbox = self.ivars().inbox.borrow();
                let Some(receiver) = inbox.as_ref() else {
                    return;
                };
                receiver.try_iter().collect()
            };

            for message in messages {
                match message {
                    FromWorker::Progress {
                        stage,
                        index,
                        total,
                        overall,
                    } => self.send(Msg::Progress {
                        stage,
                        index,
                        total,
                        overall,
                    }),
                    FromWorker::Finished(result) => {
                        finished = true;
                        self.send(Msg::Finished(result));
                    }
                }
            }
            if finished {
                *self.ivars().inbox.borrow_mut() = None;
            }
        }
    }

    unsafe impl NSObjectProtocol for LevellerView {}
);

impl LevellerView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(State {
            model: RefCell::new(Model::new()),
            pointer: RefCell::new(Pointer::default()),
            inbox: RefCell::new(None),
            elements: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]);
        this.registerForDraggedTypes(&types);

        this
    }

    /// Rebuild the accessibility proxies from the model as it stands.
    ///
    /// One invisible subview per element. Synthetic `NSAccessibilityElement`s
    /// would be the tidier answer and do not work: AppKit asks the container
    /// for them, receives them, and reports none — a custom view's synthetic
    /// children are filtered out somewhere inside. Real subviews are included
    /// because they are views, which is the whole trick.
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
        // its identity keeps VoiceOver's place in the window, and rebuilding
        // them all on every parameter tick would move the cursor to the top.
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
            proxy.set_index(index);
            proxy.setFrame(CGRect::new(
                CGPoint::new(element.rect.x, element.rect.y),
                CGSize::new(element.rect.width, element.rect.height),
            ));
            proxy.describe(element);
        }
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
            self.activate(hit);
        }
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
        let window_point = { event.locationInWindow() };
        self.convertPoint_fromView(window_point, None)
    }

    /// Push a message through the model and act on whatever falls out.
    fn send(&self, msg: Msg) {
        let effect = self.ivars().model.borrow_mut().update(msg);
        if let Some(Effect::Process { path, stages }) = effect {
            self.start_worker(path, stages);
        }
        self.rebuild_elements();
        self.setNeedsDisplay(true);
    }

    fn activate(&self, hit: Hit) {
        match hit {
            Hit::Light(light) => {
                if let Some(window) = self.window() {
                    WindowCommand::from(light).perform(&window);
                }
            }
            Hit::TitleBar | Hit::Slider(..) => {}
            Hit::DropZone => self.open_panel(),
            Hit::Preset(index) => {
                let name = self
                    .ivars()
                    .model
                    .borrow()
                    .preset_names()
                    .get(index)
                    .map(|n| (*n).to_string());
                if let Some(name) = name {
                    self.send(Msg::Preset(name));
                }
            }
            Hit::Revert => self.send(Msg::Revert),
            Hit::Rerender => self.send(Msg::Rerender),
            Hit::StageCheckbox(index) => self.send(Msg::ToggleStage(index)),
            Hit::StageDisclosure(index) => self.send(Msg::ToggleExpanded(index)),
        }
    }

    /// Drag a slider: turn an x position into a value on that parameter's own
    /// scale, snapped to its step.
    fn drag_slider(&self, stage_index: usize, param_index: usize, x: f64) {
        let layout = self.layout();
        let Some(row) = layout.rows.get(stage_index) else {
            return;
        };
        let Some(param) = row.params.get(param_index) else {
            return;
        };

        let (name, spec) = {
            let model = self.ivars().model.borrow();
            let Some(stage) = model.stages().get(stage_index) else {
                return;
            };
            let Some(spec) = stage.params.get(param_index) else {
                return;
            };
            (stage.name.to_string(), spec.clone())
        };

        let leveller_stages::params::ParamSpec::Number {
            key,
            min,
            max,
            step,
            ..
        } = spec
        else {
            return;
        };

        // The knob is 14pt wide and its centre is what tracks, so the usable
        // travel is the track less one knob.
        let travel = (param.control.width - 14.0).max(1.0);
        let fraction = ((x - param.control.x - 7.0) / travel).clamp(0.0, 1.0);
        let raw = min + fraction * (max - min);
        let snapped = if step > 0.0 {
            (raw / step).round() * step
        } else {
            raw
        };

        self.send(Msg::SetParam {
            stage: name,
            key: key.to_string(),
            value: serde_json::json!(snapped.clamp(min, max)),
        });
    }

    /// The drop zone is also a button: clicking it opens a file panel, so the
    /// app is usable without dragging — which the Electron version was not.
    fn open_panel(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm);
        panel.setAllowsMultipleSelection(false);
        panel.setCanChooseDirectories(false);
        panel.setMessage(Some(&NSString::from_str("Choose a .wav file to level")));
        if panel.runModal() == objc2_app_kit::NSModalResponseOK
            && let Some(url) = { panel.URL() }
            && let Some(path) = { url.path() }
        {
            self.send(Msg::Dropped(PathBuf::from(path.to_string())));
        }
    }

    /// Run the chain on a background thread, reporting through a channel that a
    /// timer drains on the main thread.
    ///
    /// A channel and a poll rather than dispatching each message to the main
    /// queue: progress arrives thousands of times a second from inside the DSP,
    /// and hopping the queue that often costs more than the processing.
    fn start_worker(&self, path: PathBuf, stages: Vec<leveller_pipeline::StageSpec>) {
        let (sender, receiver): (Sender<FromWorker>, Receiver<FromWorker>) = channel();
        *self.ivars().inbox.borrow_mut() = Some(receiver);

        std::thread::spawn(move || {
            let registry = leveller_stages::default_registry();
            let progress = sender.clone();
            let result = leveller_io::process_file(&path, &stages, &registry, |p| {
                let _ = progress.send(FromWorker::Progress {
                    stage: p.stage.to_string(),
                    index: p.index,
                    total: p.total,
                    overall: p.overall,
                });
            });
            let _ = sender.send(FromWorker::Finished(
                result.map(Box::new).map_err(|e| e.to_string()),
            ));
        });

        // 20 Hz: fast enough that a progress bar looks continuous, slow enough
        // that the main thread is doing nothing most of the time.
        let _: Retained<objc2::runtime::AnyObject> = unsafe {
            msg_send![
                class!(NSTimer),
                scheduledTimerWithTimeInterval: 0.05f64,
                target: self,
                selector: objc2::sel!(drainWorker),
                userInfo: std::ptr::null::<objc2::runtime::AnyObject>(),
                repeats: true,
            ]
        };
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

use objc2::class;

/// The file paths in a drag, ignoring anything that is not a `.wav`.
fn dropped_paths(sender: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<PathBuf> {
    let pasteboard = { sender.draggingPasteboard() };
    let Some(items) = ({ pasteboard.pasteboardItems() }) else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let string = unsafe { item.stringForType(NSPasteboardTypeFileURL) }?;
            let url = { NSURL::URLWithString(&string) }?;
            let path = { url.path() }?;
            let path = PathBuf::from(path.to_string());
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
                .then_some(path)
        })
        .collect()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AudioLevellerDelegate"]
    #[ivars = RefCell<Option<Retained<LevellerView>>>]
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
    }

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            // One window, and closing it is how you quit — which is what a
            // single-document app of this vintage did.
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

/// Bring up the window and run.
pub fn run() -> ! {
    let mtm = MainThreadMarker::new().expect("the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Regular);
    aqua::window::install_menu(mtm, "Audio Leveller");

    let window = aqua::window::make_window(
        mtm,
        &WindowDesc {
            title: "Audio Leveller".into(),
            size: WINDOW_SIZE,
            min_size: MIN_SIZE,
        },
    );

    let view = LevellerView::new(mtm);
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

/// Draw the window into a PNG without opening one.
pub fn shot(path: &Path) -> Result<(), std::io::Error> {
    shot_of(path, &Model::new(), Focus::Active)
}

/// A shot of a window that has done something: a file processed, a stage
/// opened, a parameter moved.
///
/// It processes a synthetic recording for real rather than faking a report,
/// because a fake one would drift from the real shape the first time a stage's
/// report changed.
pub fn shot_busy(path: &Path) -> Result<(), std::io::Error> {
    let mut model = Model::new();
    model.update(Msg::ToggleExpanded(7));
    model.update(Msg::SetParam {
        stage: "level".into(),
        key: "targetLufs".into(),
        value: serde_json::json!(-20.0),
    });
    model.update(Msg::ToggleStage(2));

    // A real run over a real file, so the numbers and the decisions are the
    // ones the app would actually show.
    let fixture = std::env::temp_dir().join("audio-leveller-shot.wav");
    write_fixture(&fixture)?;
    let stages = model.chain();
    // Through the model, so it remembers the file — which is what turns
    // Re-render into the default button. The effect it returns is the run this
    // does by hand below.
    let _ = model.update(Msg::Dropped(fixture.clone()));
    let registry = leveller_stages::default_registry();
    let result = leveller_io::process_file(&fixture, &stages, &registry, |_| {})
        .map_err(std::io::Error::other)?;
    let _ = std::fs::remove_file(&fixture);
    model.update(Msg::Finished(Ok(Box::new(result))));

    shot_of(path, &model, Focus::Active)
}

/// A short synthetic recording for the shot to process.
fn write_fixture(path: &Path) -> Result<(), std::io::Error> {
    use leveller_dsp::Signal;
    let sample_rate = 48_000u32;
    let seconds = 8.0;
    let n = (seconds * f64::from(sample_rate)) as usize;

    // Two spurts at different levels, with pauses, so the leveller has
    // something to segment and the room-tone harvester something to harvest.
    let mut samples = vec![0.0f32; n];
    let mut state = 1u32;
    for (i, sample) in samples.iter_mut().enumerate() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = f64::from(state >> 8) / f64::from(1u32 << 23) - 1.0;
        let t = i as f64 / f64::from(sample_rate);
        let voiced = (1.5..3.5).contains(&t) || (5.0..7.0).contains(&t);
        let level = if (1.5..3.5).contains(&t) { 0.06 } else { 0.22 };
        let envelope = if voiced {
            0.5 - 0.5 * (std::f64::consts::TAU * 4.0 * t).cos()
        } else {
            0.0
        };
        let tone: f64 = (1..14)
            .map(|h| (std::f64::consts::TAU * 120.0 * f64::from(h) * t).sin() / f64::from(h))
            .sum();
        *sample = (tone * envelope * level + noise * 0.0015) as f32;
    }

    let audio = leveller_wav::Audio {
        signal: Signal::mono(sample_rate, samples),
        bit_depth: 24,
        format: leveller_wav::SampleFormat::Int,
    };
    std::fs::write(
        path,
        leveller_wav::encode(&audio).map_err(std::io::Error::other)?,
    )
}

/// Draw a particular model into a PNG.
pub fn shot_of(path: &Path, model: &Model, _focus: Focus) -> Result<(), std::io::Error> {
    let layout = layout::compute(model, WINDOW_SIZE.0, WINDOW_SIZE.1);
    let height = layout.content_height.max(WINDOW_SIZE.1);
    aqua::render::to_png(path, WINDOW_SIZE.0, height, 2.0, |ctx, bounds| {
        let layout = layout::compute(model, WINDOW_SIZE.0, height);
        draw::scene(ctx, model, &layout, Pointer::default(), bounds);
    })
}
