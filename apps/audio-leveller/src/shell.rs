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
    NSApplication, NSApplicationDelegate, NSDragOperation, NSDraggingInfo, NSEvent,
    NSGraphicsContext, NSPasteboardTypeFileURL, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSWindowDelegate,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString, NSURL,
};

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
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]);
        this.registerForDraggedTypes(&types);

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
        let window_point = { event.locationInWindow() };
        self.convertPoint_fromView(window_point, None)
    }

    /// Push a message through the model and act on whatever falls out.
    fn send(&self, msg: Msg) {
        let effect = self.ivars().model.borrow_mut().update(msg);
        if let Some(Effect::Process { path, stages }) = effect {
            self.start_worker(path, stages);
        }
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

/// Draw a particular model into a PNG.
pub fn shot_of(path: &Path, model: &Model, _focus: Focus) -> Result<(), std::io::Error> {
    let layout = layout::compute(model, WINDOW_SIZE.0, WINDOW_SIZE.1);
    let height = layout.content_height.max(WINDOW_SIZE.1);
    aqua::render::to_png(path, WINDOW_SIZE.0, height, 2.0, |ctx, bounds| {
        let layout = layout::compute(model, WINDOW_SIZE.0, height);
        draw::scene(ctx, model, &layout, Pointer::default(), bounds);
    })
}
