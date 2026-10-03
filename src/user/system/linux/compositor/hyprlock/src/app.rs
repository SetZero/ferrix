//! The program: `CHyprlock::run`, on `src/user/system/linux/compositor/toolkit`.
//!
//! Connect; take a picture of every screen first if the fade or a
//! `path = screenshot` background needs one; take the lock and put a lock
//! surface on every screen; then turn the loop -- keys into the
//! [`Session`], the password check on a thread of its own, the widgets'
//! commands through the toolkit's children, a frame whenever something
//! changed or is still moving -- until the password is accepted, the lock
//! has faded out and is let go.

use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use compositor_toolkit::protocol::screencopy::{
    self, zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as manager,
};
use compositor_toolkit::{
    ChildId, ChildOutput, Client, Command, Event, KeyboardEvent, ObjectId, OutputId, PointerEvent,
    SurfaceId, Value, tiny_skia,
};

use crate::assets::System;
use crate::auth::{Account, Backend, Next, Prompt, Secret, Verdict};
use crate::config::Config;
use crate::format::Context;
use crate::scene::{Scene, Screen, View};
use crate::session::{KeyPress, Keyed, Session};
use crate::tween::Tween;

/// How often the loop looks for the password check's answer while one is
/// out, in milliseconds.
const CHECK_POLL_MS: u64 = 20;

/// What the thread the backend is asked on sends back.
#[derive(Debug)]
enum Answer {
    /// A conversation began again, after the last one ended.
    Began(Prompt),
    /// What the answer led to.
    Next(Next),
}

/// What the command line asked for.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `--grace`: seconds in which any key unlocks.
    pub grace: u64,
    /// `--immediate-render`.
    pub immediate_render: bool,
    /// `--display`.
    pub display: Option<String>,
    /// `-v`: every event the compositor sends, on standard error.
    pub verbose: bool,
}

/// One screen's lock surface.
#[derive(Debug)]
struct Surface {
    output: OutputId,
    scene: Option<Scene>,
    dirty: bool,
    waiting_frame: bool,
    moving: bool,
}

/// A screenshot being taken.
#[derive(Debug)]
struct Capture {
    output: OutputId,
    frame: ObjectId,
    buffer: Option<(ObjectId, u32, u32, u32, u32)>,
    inverted: bool,
}

/// The program's state.
struct App {
    client: Client,
    config: Config,
    assets: System,
    session: Session,
    account: Option<Account>,
    backend: Arc<dyn Backend>,
    surfaces: BTreeMap<SurfaceId, Surface>,
    screenshots: BTreeMap<OutputId, tiny_skia::Pixmap>,
    children: BTreeMap<ChildId, (SurfaceId, usize)>,
    opacity: Tween,
    fading_out: bool,
    locked: bool,
    started: Instant,
    checks: mpsc::Receiver<Answer>,
    check_sender: mpsc::Sender<Answer>,
    /// The last prompt's text: `$PAMPROMPT`.
    prompt: Option<String>,
    /// Whether a conversation is open, so an answer goes to it rather than
    /// to a new one.
    open: bool,
    verbose: bool,
}

/// Run hyprlock with `config` until it unlocks.
///
/// # Errors
///
/// A sentence for anything that stops it before or during the lock.
pub fn run(config: Config, options: &Options, backend: Arc<dyn Backend>) -> Result<(), String> {
    let client = match &options.display {
        Some(name) => {
            let path = if name.contains('/') {
                std::path::PathBuf::from(name)
            } else {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
                    .join(name)
            };
            Client::connect_to(&path)
        }
        None => Client::connect(),
    }
    .map_err(|error| format!("Couldn't connect to a wayland compositor: {error}"))?;
    let account = crate::auth::account(std::path::Path::new("/"), crate::auth::uid());
    if account.is_none() {
        crate::say("Failed to get username for the current uid (getpwuid)");
    }
    // The conversation begins before the lock is taken: `$PAMPROMPT` is its
    // first prompt, and a lock nothing can open is not taken at all
    // (docs/AUTH.md §5.4, decision 4).
    let prompt = if config.auth.pam {
        if let Err(verdict) = backend.ready() {
            return Err(format!("not locking: {}", verdict.fail_text()));
        }
        match backend.begin() {
            Ok(prompt) => Some(prompt.text),
            Err(verdict) => {
                return Err(format!("not locking: {}", verdict.fail_text()));
            }
        }
    } else {
        return Err("not locking: auth:pam:enabled is off, and nothing else can unlock".to_owned());
    };
    if options.grace > 0 {
        // docs/AUTH.md decision 11: the grace is hyprix's `misc:lock_grace`,
        // which hyprix enforces from when it took the lock, and a client
        // cannot extend it.
        crate::say(
            "[WARN] --grace: on Ferrix the grace is hyprix's misc:lock_grace; hyprlock's own is not honoured",
        );
    }
    let session = Session::new(
        config.general.ignore_empty_input,
        config.general.fail_timeout,
        0,
        0,
    );
    let (check_sender, checks) = mpsc::channel();
    let mut app = App {
        opacity: Tween::new(0.0, &config.animations, "fadeIn"),
        client,
        assets: System::new(),
        session,
        account,
        backend,
        surfaces: BTreeMap::new(),
        screenshots: BTreeMap::new(),
        children: BTreeMap::new(),
        fading_out: false,
        locked: false,
        started: Instant::now(),
        checks,
        check_sender,
        prompt,
        open: true,
        verbose: options.verbose,
        config,
    };
    app.screenshots()?;
    app.lock()?;
    app.run()
}

impl App {
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn context(&self) -> Context {
        let (user, gecos) = self
            .account
            .as_ref()
            .map(|account| (account.name.clone(), account.gecos.clone()))
            .unwrap_or_default();
        let layout = self
            .client
            .keyboard_layout()
            .map(|(layout, group)| (layout.label.to_owned(), usize::try_from(group).unwrap_or(0)))
            .unwrap_or_else(|| ("error".to_owned(), 0));
        Context {
            user,
            gecos,
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(0)),
            utc_offset: 0,
            attempts: self.session.attempts,
            fail: self.session.fail_text.clone(),
            pam_fail: if self.config.auth.pam {
                self.session.pam_fail.clone()
            } else {
                None
            },
            pam_prompt: self.prompt.clone(),
            layout,
        }
    }

    /// `screencopyRequired`: a fade, or a `path = screenshot` background.
    fn wants_screenshots(&self) -> bool {
        let fades = self.config.animations.enabled
            && (self.config.animations.get("fadeIn").enabled
                || self.config.animations.get("fadeOut").enabled);
        let shot = self.config.widgets.iter().any(|widget| {
            matches!(widget, crate::config::Widget::Background(background) if background.path == "screenshot")
        });
        fades || shot
    }

    /// Take a picture of every screen before locking, as upstream does,
    /// through `zwlr_screencopy_v1`.
    fn screenshots(&mut self) -> Result<(), String> {
        if !self.wants_screenshots() {
            return Ok(());
        }
        let Ok((manager_id, _)) =
            self.client
                .bind(&screencopy::ZWLR_SCREENCOPY_MANAGER_V1, 3, None)
        else {
            crate::say(
                "No screencopy support! path=screenshot won't work. Falling back to background color.",
            );
            return Ok(());
        };
        let mut captures = Vec::new();
        let outputs: Vec<OutputId> = self
            .client
            .outputs()
            .iter()
            .filter_map(|output| output.id)
            .collect();
        for output in outputs {
            let Some(wl_output) = self.client.output_object(output) else {
                continue;
            };
            let frame_id = self
                .client
                .new_object(&screencopy::ZWLR_SCREENCOPY_FRAME_V1, 3);
            self.client
                .request(
                    manager_id,
                    manager::request::CAPTURE_OUTPUT,
                    &[
                        Value::NewId(frame_id),
                        Value::Int(0),
                        Value::Object(wl_output),
                    ],
                )
                .map_err(|error| error.to_string())?;
            captures.push(Capture {
                output,
                frame: frame_id,
                buffer: None,
                inverted: false,
            });
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !captures.is_empty() && Instant::now() < deadline {
            let events = self
                .client
                .dispatch(Some(Duration::from_millis(100)))
                .map_err(|error| error.to_string())?;
            for event in events {
                let Event::Object {
                    object,
                    opcode,
                    args,
                    ..
                } = event
                else {
                    continue;
                };
                let Some(at) = captures.iter().position(|capture| capture.frame == object) else {
                    continue;
                };
                self.capture_event(&mut captures, at, opcode, &args)?;
            }
        }
        if !captures.is_empty() {
            crate::say(
                "screencopy: a screen gave no picture in time; its fade starts from the colour",
            );
        }
        Ok(())
    }

    fn capture_event(
        &mut self,
        captures: &mut Vec<Capture>,
        at: usize,
        opcode: u16,
        args: &[Value],
    ) -> Result<(), String> {
        let Some(capture) = captures.get_mut(at) else {
            return Ok(());
        };
        let uint = |index: usize| args.get(index).and_then(Value::as_uint).unwrap_or(0);
        match opcode {
            frame::event::BUFFER if capture.buffer.is_none() => {
                let (format, width, height, stride) = (uint(0), uint(1), uint(2), uint(3));
                let (buffer, _) = self
                    .client
                    .new_buffer(width, height, format)
                    .map_err(|error| error.to_string())?;
                capture.buffer = Some((buffer, format, width, height, stride));
                self.client
                    .request(
                        capture.frame,
                        frame::request::COPY,
                        &[Value::Object(buffer)],
                    )
                    .map_err(|error| error.to_string())?;
            }
            frame::event::FLAGS => {
                capture.inverted = uint(0) & frame::flags::Y_INVERT != 0;
            }
            frame::event::READY => {
                let capture = captures.remove(at);
                if let Some((buffer, format, width, height, stride)) = capture.buffer {
                    if let Some(bytes) = self.client.buffer_bytes(buffer)
                        && let Some(pixmap) =
                            to_pixmap(bytes, format, width, height, stride, capture.inverted)
                    {
                        let _ = self.screenshots.insert(capture.output, pixmap);
                    }
                    self.client.destroy_buffer(buffer);
                }
                let _ = self
                    .client
                    .request(capture.frame, frame::request::DESTROY, &[]);
            }
            frame::event::FAILED => {
                let capture = captures.remove(at);
                crate::say("screencopy: the compositor could not copy a screen");
                let _ = self
                    .client
                    .request(capture.frame, frame::request::DESTROY, &[]);
            }
            _ => {}
        }
        Ok(())
    }

    fn lock(&mut self) -> Result<(), String> {
        self.client
            .lock()
            .map_err(|error| format!("Couldn't lock the session: {error}"))?;
        let outputs: Vec<OutputId> = self
            .client
            .outputs()
            .iter()
            .filter_map(|output| output.id)
            .collect();
        for output in outputs {
            self.cover(output)?;
        }
        if self.config.general.hide_cursor {
            self.client.hide_cursor();
        }
        Ok(())
    }

    fn cover(&mut self, output: OutputId) -> Result<(), String> {
        let surface = self
            .client
            .lock_surface(output)
            .map_err(|error| format!("a lock surface: {error}"))?;
        let _ = self.surfaces.insert(
            surface,
            Surface {
                output,
                scene: None,
                dirty: true,
                waiting_frame: false,
                moving: false,
            },
        );
        Ok(())
    }

    fn run(&mut self) -> Result<(), String> {
        // `startFadeIn`, as the loop begins.
        let now = self.now();
        self.opacity.set(1.0, now);
        loop {
            let now = self.now();
            let timeout = self
                .next_deadline(now)
                .map(|at| Duration::from_millis(at.saturating_sub(now)));
            let events = self
                .client
                .dispatch(timeout)
                .map_err(|error| error.to_string())?;
            for event in events {
                if self.event(event)? {
                    return Ok(());
                }
            }
            let now = self.now();
            while let Ok(answer) = self.checks.try_recv() {
                self.answered(answer, now);
            }
            if self.session.tick(now) {
                self.force_update(now);
                self.mark_all();
            }
            self.timers(now);
            if self.fading_out && !self.opacity.moving(now) {
                self.client.unlock().map_err(|error| error.to_string())?;
                crate::say("hyprlock: unlocked");
                let _ = self.client.roundtrip();
                return Ok(());
            }
            self.draw_all(now);
        }
    }

    fn next_deadline(&self, now: u64) -> Option<u64> {
        let mut soonest = self.session.next_deadline();
        for surface in self.surfaces.values() {
            if let Some(scene) = &surface.scene
                && let Some(at) = scene.next_deadline()
            {
                soonest = Some(soonest.map_or(at, |other| other.min(at)));
            }
        }
        if self.opacity.moving(now) || self.fading_out {
            soonest = Some(now + 16);
        }
        if self.session.checking() {
            let at = now + CHECK_POLL_MS;
            soonest = Some(soonest.map_or(at, |other| other.min(at)));
        }
        soonest
    }

    /// One event; whether the program is done.
    fn event(&mut self, event: Event) -> Result<bool, String> {
        let now = self.now();
        if self.verbose {
            crate::say(&format!("[TRACE] {event:?}"));
        }
        match event {
            Event::Configure {
                surface,
                width,
                height,
            } => self.configure(surface, width, height, now),
            Event::Locked => {
                self.locked = true;
                crate::say("hyprlock: locked");
                self.mark_all();
            }
            Event::LockFinished => {
                crate::say("hyprlock: the compositor ended the lock");
                return Ok(true);
            }
            Event::Frame { surface, .. } => {
                if let Some(state) = self.surfaces.get_mut(&surface) {
                    state.waiting_frame = false;
                    if state.moving {
                        state.dirty = true;
                    }
                }
            }
            Event::OutputAdded(output) if self.locked => self.cover(output)?,
            Event::Closed(surface) => {
                let _ = self.surfaces.remove(&surface);
            }
            Event::Keyboard(KeyboardEvent::Key(key)) => self.key(&key, now),
            Event::Pointer(PointerEvent::Button {
                surface,
                x,
                y,
                pressed: true,
                ..
            }) => self.click(surface, x, y),
            Event::ChildExited { child, output, .. } => {
                if let Some((surface, widget)) = self.children.remove(&child)
                    && let Some(scene) = self
                        .surfaces
                        .get_mut(&surface)
                        .and_then(|state| state.scene.as_mut())
                {
                    scene.finished(widget, &output, now);
                    self.mark(surface);
                }
            }
            Event::Woken => {}
            _ => {}
        }
        Ok(false)
    }

    fn configure(&mut self, surface: SurfaceId, width: u32, height: u32, now: u64) {
        let scale = self.client.scale(surface).max(1);
        let context = self.context();
        let Some(state) = self.surfaces.get_mut(&surface) else {
            return;
        };
        let Some(output) = self.client.output(state.output) else {
            return;
        };
        let screen = Screen {
            name: output.name.clone(),
            description: output.description.clone(),
            width: width.saturating_mul(scale),
            height: height.saturating_mul(scale),
        };
        let scene = Scene::new(&self.config, &screen, &context, now);
        crate::say(&format!(
            "hyprlock: {} ({}) {}x{}: {} widget(s)",
            screen.name,
            screen.description,
            screen.width,
            screen.height,
            scene.len()
        ));
        state.scene = Some(scene);
        state.dirty = true;
    }

    /// A click: every `onclick` under it runs, detached, as upstream's
    /// `spawnAsync`.
    fn click(&mut self, surface: SurfaceId, x: f64, y: f64) {
        let scale = f64::from(self.client.scale(surface).max(1));
        let Some(scene) = self
            .surfaces
            .get(&surface)
            .and_then(|state| state.scene.as_ref())
        else {
            return;
        };
        #[expect(clippy::cast_possible_truncation, reason = "a position on a screen")]
        let commands = scene.click((x * scale) as f32, (y * scale) as f32);
        for command in commands {
            if let Err(error) = compositor_toolkit::spawn(&command) {
                crate::say(&format!("Failed to start \"{command}\": {error}"));
            }
        }
    }

    fn key(&mut self, key: &compositor_toolkit::Key, now: u64) {
        let press = KeyPress {
            code: key.code,
            keysym: key.keysym.unwrap_or(""),
            text: &key.text,
            pressed: key.pressed,
            repeat: key.repeat,
            control: key.modifiers.control,
            caps_lock: key.modifiers.caps_lock,
            num_lock: key.modifiers.num_lock,
        };
        match self.session.key(&press, now) {
            Keyed::Nothing => {}
            Keyed::Redraw => self.mark_all(),
            Keyed::Grace => self.fade_out(now),
            Keyed::Submit(password) => {
                crate::say("hyprlock: authenticating");
                self.check(password);
                self.mark_all();
            }
        }
    }

    /// Answer the backend on a thread of its own, as upstream's PAM thread
    /// does: a check takes a moment, and the field must keep drawing while
    /// it runs. A conversation that ended is begun again first. The loop
    /// looks for the answer every [`CHECK_POLL_MS`] while one is out.
    fn check(&mut self, password: String) {
        let sender = self.check_sender.clone();
        let backend = Arc::clone(&self.backend);
        let open = self.open;
        self.open = true;
        // Into the fixed buffer authd's records use, and the typed copy
        // zeroed.
        let mut typed = password.into_bytes();
        let secret = Secret::from_bytes(&typed);
        typed.fill(0);
        let _ = std::hint::black_box(&typed);
        drop(typed);
        let _ = std::thread::spawn(move || {
            let Some(secret) = secret else {
                let _ = sender.send(Answer::Next(Next::Verdict(Verdict::Failed {
                    text: "a password is at most 256 bytes".to_owned(),
                    retry_after_ms: 0,
                })));
                return;
            };
            if !open {
                match backend.begin() {
                    Ok(prompt) => {
                        let _ = sender.send(Answer::Began(prompt));
                    }
                    Err(verdict) => {
                        let _ = sender.send(Answer::Next(Next::Verdict(verdict)));
                        return;
                    }
                }
            }
            let _ = sender.send(Answer::Next(backend.respond(&secret)));
        });
    }

    fn answered(&mut self, answer: Answer, now: u64) {
        let verdict = match answer {
            // A new conversation's first prompt, which the answer already
            // on its way is for.
            Answer::Began(prompt) => {
                self.prompt_text(prompt, now);
                return;
            }
            // Another question in this conversation: the field takes the
            // next answer.
            Answer::Next(Next::Prompt(prompt)) => {
                self.prompt_text(prompt, now);
                self.session.prompted();
                self.mark_all();
                return;
            }
            Answer::Next(Next::Verdict(verdict)) => verdict,
        };
        self.open = false;
        match &verdict {
            Verdict::Accepted => crate::say("hyprlock: authenticated"),
            Verdict::Failed { text, .. } => crate::say(&format!("hyprlock: {text}")),
            Verdict::Unavailable(text) => crate::say(&format!("hyprlock: unavailable: {text}")),
        }
        let accepted = verdict == Verdict::Accepted;
        self.session.answered(verdict, now);
        if accepted {
            self.fade_out(now);
        }
        self.mark_all();
    }

    /// A prompt's text becomes `$PAMPROMPT`; the labels showing it are
    /// formatted again only when it changed, as upstream's conversation
    /// re-prompts only on a new text.
    fn prompt_text(&mut self, prompt: Prompt, now: u64) {
        if self.prompt.as_deref() != Some(prompt.text.as_str()) {
            self.prompt = Some(prompt.text);
            self.force_update(now);
        }
    }

    fn fade_out(&mut self, now: u64) {
        if self.fading_out {
            return;
        }
        self.fading_out = true;
        self.opacity = Tween::new(self.opacity.at(now), &self.config.animations, "fadeOut");
        self.opacity.set(0.0, now);
        self.mark_all();
    }

    fn force_update(&mut self, now: u64) {
        let context = self.context();
        let mut jobs = Vec::new();
        for (surface, state) in &mut self.surfaces {
            if let Some(scene) = &mut state.scene {
                jobs.extend(
                    scene
                        .force_update(&context, now)
                        .into_iter()
                        .map(|job| (*surface, job)),
                );
            }
        }
        self.start(jobs);
    }

    fn timers(&mut self, now: u64) {
        let context = self.context();
        let mut jobs = Vec::new();
        for (surface, state) in &mut self.surfaces {
            if let Some(scene) = &mut state.scene {
                let (due, changed) = scene.due(&context, now);
                state.dirty |= changed;
                jobs.extend(due.into_iter().map(|job| (*surface, job)));
            }
        }
        self.start(jobs);
    }

    fn start(&mut self, jobs: Vec<(SurfaceId, crate::scene::Job)>) {
        for (surface, job) in jobs {
            let command = Command {
                line: job.line.clone(),
                env: Vec::new(),
                output: ChildOutput::Whole,
            };
            match self.client.run(&command) {
                Ok(child) => {
                    let _ = self.children.insert(child, (surface, job.widget));
                }
                Err(error) => crate::say(&format!("Failed to run \"{}\": {error}", job.line)),
            }
        }
    }

    fn mark(&mut self, surface: SurfaceId) {
        if let Some(state) = self.surfaces.get_mut(&surface) {
            state.dirty = true;
        }
    }

    fn mark_all(&mut self) {
        for state in self.surfaces.values_mut() {
            state.dirty = true;
        }
    }

    fn draw_all(&mut self, now: u64) {
        let context = self.context();
        #[expect(clippy::cast_possible_truncation, reason = "a fraction")]
        let opacity = self.opacity.at(now) as f32;
        let fading = self.opacity.moving(now);
        let ids: Vec<SurfaceId> = self.surfaces.keys().copied().collect();
        for id in ids {
            let Some(state) = self.surfaces.get_mut(&id) else {
                continue;
            };
            if !(state.dirty || fading) || state.waiting_frame {
                continue;
            }
            let Some(scene) = state.scene.as_mut() else {
                continue;
            };
            let view = View {
                now,
                opacity,
                session: &self.session,
                context: &context,
                screenshot: self
                    .screenshots
                    .get(&state.output)
                    .map(tiny_skia::Pixmap::as_ref),
            };
            let assets = &mut self.assets;
            let mut moving = false;
            let drawn = self.client.draw(id, |pixmap| {
                moving = scene.draw(pixmap, &view, assets);
            });
            if let Err(error) = drawn {
                crate::say(&format!("hyprlock: drawing: {error}"));
                continue;
            }
            state.dirty = false;
            state.moving = moving || fading;
            state.waiting_frame = true;
            self.client.request_frame(id);
        }
    }
}

/// A screencopy buffer as a premultiplied pixmap: `ARGB8888` and
/// `XRGB8888`, which are what `wl_shm` guarantees, little-endian B, G, R,
/// A in memory.
fn to_pixmap(
    bytes: &[u8],
    format: u32,
    width: u32,
    height: u32,
    stride: u32,
    inverted: bool,
) -> Option<tiny_skia::Pixmap> {
    let opaque = match format {
        0 => false,
        1 => true,
        other => {
            crate::say(&format!(
                "screencopy: format {other:#x} is not one this reads"
            ));
            return None;
        }
    };
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let row_bytes = usize::try_from(stride).ok()?;
    let width_px = usize::try_from(width).ok()?;
    for (y, row) in pixmap.pixels_mut().chunks_mut(width_px).enumerate() {
        let source_row = if inverted {
            usize::try_from(height).ok()?.checked_sub(y + 1)?
        } else {
            y
        };
        let start = source_row.checked_mul(row_bytes)?;
        let source = bytes.get(start..start + width_px * 4)?;
        for (to, from) in row.iter_mut().zip(source.chunks_exact(4)) {
            let (b, g, r, a) = (
                from.first().copied().unwrap_or(0),
                from.get(1).copied().unwrap_or(0),
                from.get(2).copied().unwrap_or(0),
                if opaque {
                    255
                } else {
                    from.get(3).copied().unwrap_or(0)
                },
            );
            if let Some(pixel) =
                tiny_skia::PremultipliedColorU8::from_rgba(r.min(a), g.min(a), b.min(a), a)
            {
                *to = pixel;
            }
        }
    }
    Some(pixmap)
}
