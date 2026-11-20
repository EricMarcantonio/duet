//! Dioxus components. Logic decisions are delegated to [`super::logic`].

use super::icons;
use super::logic::*;
use super::{Backends, WindowMode};
use crate::audio::{BluetoothDevice, DeviceId, HeadphoneInfo, ListeningMode, OutputDevice};
use crate::config::Config;
use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const TOAST_TTL: Duration = Duration::from_secs(6);

/// Launch-time options, provided as context by `main`.
#[derive(Clone, Debug)]
pub struct AppOptions {
    pub mode: WindowMode,
    pub config_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub message: String,
    /// Offer an "Open Bluetooth Settings" button.
    pub bluetooth_settings: bool,
}

/// All reactive app state (signals are `Copy`, so this is too).
#[derive(Clone, Copy)]
pub struct AppState {
    pub devices: Signal<Vec<OutputDevice>>,
    pub paired: Signal<Vec<BluetoothDevice>>,
    /// Volumes as displayed (local value wins while the user is dragging).
    pub volumes: Signal<HashMap<String, f32>>,
    pub guard: Signal<InteractionGuard>,
    pub writes: Signal<Coalescer<(DeviceId, f32)>>,
    pub config: Signal<Config>,
    pub config_path: Signal<PathBuf>,
    pub toast: Signal<Option<Toast>>,
    pub last_audio_error: Signal<Option<String>>,
    pub last_bt_error: Signal<Option<String>>,
    /// Bluetooth access is blocked by privacy settings (shown as a banner, not toasts).
    pub bt_permission: Signal<bool>,
    /// Bluetooth addresses with a connect in flight.
    pub connecting: Signal<HashSet<String>>,
    /// Volume before muting, per UID.
    pub premute: Signal<HashMap<String, f32>>,
    pub loaded: Signal<bool>,
    /// The user asked to listen together; Duet keeps it that way until they pick
    /// another output (macOS otherwise jumps to whichever AirPods connected last).
    pub together_wanted: Signal<bool>,
    pub flips: Signal<FlipGuard>,
    /// The chosen headphones were all connected at the last poll.
    pub was_ready: Signal<bool>,
    /// "Choose headphones" list is open.
    pub choosing: Signal<bool>,
    pub busy: Signal<bool>,
    /// The user dragged the panel somewhere; stop snapping it back under the tray icon.
    pub panel_moved: Signal<bool>,
    /// Noise control / battery per Bluetooth address (connected devices only).
    pub headphones: Signal<HashMap<String, HeadphoneInfo>>,
    pub pending_modes: Signal<PendingModes>,
}

impl AppState {
    fn new(opts: &AppOptions) -> Self {
        Self {
            devices: Signal::new(Vec::new()),
            paired: Signal::new(Vec::new()),
            volumes: Signal::new(HashMap::new()),
            guard: Signal::new(InteractionGuard::default()),
            writes: Signal::new(Coalescer::default()),
            config: Signal::new(Config::load_from(&opts.config_path)),
            config_path: Signal::new(opts.config_path.clone()),
            toast: Signal::new(None),
            last_audio_error: Signal::new(None),
            last_bt_error: Signal::new(None),
            bt_permission: Signal::new(false),
            connecting: Signal::new(HashSet::new()),
            premute: Signal::new(HashMap::new()),
            loaded: Signal::new(false),
            together_wanted: Signal::new(false),
            flips: Signal::new(FlipGuard::default()),
            was_ready: Signal::new(false),
            choosing: Signal::new(false),
            busy: Signal::new(false),
            panel_moved: Signal::new(false),
            headphones: Signal::new(HashMap::new()),
            pending_modes: Signal::new(PendingModes::default()),
        }
    }

    pub fn error(self, message: impl Into<String>) {
        self.show_toast(message.into(), false);
    }

    fn show_toast(mut self, message: String, bluetooth_settings: bool) {
        crate::dlog!("toast: {message}");
        let id = self.toast.peek().as_ref().map_or(1, |t| t.id + 1);
        self.toast.set(Some(Toast {
            id,
            message,
            bluetooth_settings,
        }));
        spawn_forever(async move {
            tokio::time::sleep(TOAST_TTL).await;
            if self.toast.peek().as_ref().is_some_and(|t| t.id == id) {
                self.toast.set(None);
            }
        });
    }

    /// What the "Listen together" card shows right now.
    pub fn together(&self) -> Together {
        plan_together(
            &self.devices.read(),
            &self.paired.read(),
            &self.config.read().listeners,
        )
    }

    pub fn save_nickname(mut self, uid: &str, nickname: &str) {
        self.config.write().set_nickname(uid, nickname);
        let cfg = self.config.peek().clone();
        let path = self.config_path.peek().clone();
        if let Err(e) = cfg.save_to(&path) {
            self.error(format!("Couldn't save nickname: {e}"));
        }
    }
}

/// Run a blocking backend call off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("Background task failed: {e}"))
}

/// Poll both backends once and merge into state.
/// Poll audio devices once and merge into state.
pub async fn refresh_audio(mut state: AppState, b: Backends) {
    let b_keep = b.clone();
    let audio = b.audio.clone();
    let problem = match blocking(move || audio.output_devices()).await {
        Ok(Ok(devs)) => {
            let now = Instant::now();
            let merged = merge_volumes(&state.volumes.peek(), &devs, &state.guard.peek(), now);
            state.guard.write().prune(now);
            if *state.volumes.peek() != merged {
                state.volumes.set(merged);
            }
            let learned = state
                .config
                .write()
                .remember_names(devs.iter().map(|d| (d.uid.as_str(), d.name.as_str())));
            if learned {
                let cfg = state.config.peek().clone();
                let path = state.config_path.peek().clone();
                let _ = cfg.save_to(&path);
            }
            if *state.devices.peek() != devs {
                log_device_changes(&state.devices.peek(), &devs);
                state.devices.set(devs);
            }
            None
        }
        Ok(Err(e)) => Some(format!("Couldn't read audio devices: {e}")),
        Err(e) => Some(e),
    };
    if *state.last_audio_error.peek() != problem {
        if let Some(p) = &problem {
            state.error(p.clone());
        }
        state.last_audio_error.set(problem);
    }
    if !*state.loaded.peek() {
        let duet_playing = state.devices.peek().iter().any(|d| {
            d.is_default && d.is_multi_output() && crate::audio::is_duet_group_uid(&d.uid)
        });
        state.together_wanted.set(duet_playing);
        state.loaded.set(true);
    } else {
        keep_together_now(state, b_keep);
    }
}

fn log_device_changes(old: &[OutputDevice], new: &[OutputDevice]) {
    let default_name =
        |ds: &[OutputDevice]| ds.iter().find(|d| d.is_default).map(|d| d.name.clone());
    let (a, b) = (default_name(old), default_name(new));
    if a != b && !old.is_empty() {
        crate::dlog!(
            "output: {} -> {}",
            a.as_deref().unwrap_or("none"),
            b.as_deref().unwrap_or("none")
        );
    }
    for d in new.iter().filter(|d| !old.iter().any(|o| o.uid == d.uid)) {
        crate::dlog!("device appeared: {} ({:?}, {})", d.name, d.transport, d.uid);
    }
    for d in old.iter().filter(|d| !new.iter().any(|n| n.uid == d.uid)) {
        crate::dlog!("device gone: {} ({})", d.name, d.uid);
    }
}

/// Move the default input off AirPods (their mic forces call mode, which keeps
/// knocking them out of the Duet output).
fn keep_mic_off_headphones(b: &Backends) {
    match b.audio.avoid_bluetooth_input() {
        Ok(Some(old)) => {
            crate::dlog!("microphone: {old} -> built-in (keeps AirPods in high-quality mode)")
        }
        Ok(None) => {}
        Err(e) => crate::dlog!("microphone switch failed: {e}"),
    }
}

/// While the user wants to listen together, undo macOS switching the output to a
/// pair that just connected, and fold newly connected chosen headphones in.
fn keep_together_now(mut state: AppState, b: Backends) {
    if !*state.together_wanted.peek() || *state.busy.peek() || !state.connecting.peek().is_empty() {
        return;
    }
    keep_mic_off_headphones(&b);
    let plan = plan_together(
        &state.devices.peek(),
        &state.paired.peek(),
        &state.config.peek().listeners,
    );
    let ready = plan.is_ready();
    let became_ready = ready && !*state.was_ready.peek();
    if *state.was_ready.peek() != ready {
        state.was_ready.set(ready);
    }
    let action = keep_together(&state.devices.peek(), &plan, became_ready);
    if action == Keep::Fine {
        return;
    }
    if !state.flips.write().allow(Instant::now()) {
        crate::dlog!("keep-together: macOS keeps moving the output; giving up ({action:?})");
        state.together_wanted.set(false);
        state.show_toast(
            "macOS keeps switching the output away from Duet. Choose “Duet” in the Sound menu, \
             and set each pair's “Connect to This Mac” to “When Last Connected to This Mac”."
                .into(),
            false,
        );
        return;
    }
    crate::dlog!("keep-together: {action:?} (became_ready={became_ready})");
    match action {
        Keep::Fine => {}
        Keep::Reselect(id) => set_default(state, b, id),
        Keep::Rebuild(uids) => apply_duet_group(state, b, uids),
    }
}

/// Poll paired Bluetooth devices once (may block for seconds; runs off the UI
/// thread and independently of the audio poll).
pub async fn refresh_bluetooth(mut state: AppState, b: Backends) {
    let bt = b.bluetooth.clone();
    let polled = blocking(move || {
        let paired = bt.paired_devices()?;
        // Extras are best-effort: a device that can't report them just has none.
        let extras: HashMap<String, HeadphoneInfo> = paired
            .iter()
            .filter(|d| d.connected && d.is_audio)
            .filter_map(|d| {
                let info = bt.headphone_info(&d.address).ok().flatten()?;
                Some((crate::audio::normalize_address(&d.address), info))
            })
            .collect();
        Ok::<_, crate::audio::AudioError>((paired, extras))
    })
    .await;
    let problem = match polled {
        Ok(Ok((p, extras))) => {
            if *state.paired.peek() != p {
                state.paired.set(p);
            }
            if *state.headphones.peek() != extras {
                state.headphones.set(extras);
            }
            state.pending_modes.write().prune(Instant::now());
            None
        }
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) => Some(e),
    };
    let permission = problem
        .as_deref()
        .is_some_and(is_bluetooth_permission_error);
    if *state.bt_permission.peek() != permission {
        state.bt_permission.set(permission);
    }
    if *state.last_bt_error.peek() != problem {
        if let Some(p) = problem.as_ref().filter(|_| !permission) {
            state.error(format!("Couldn't read Bluetooth devices: {p}"));
        }
        state.last_bt_error.set(problem);
    }
}

/// Refresh everything (after user actions).
pub async fn refresh(state: AppState, b: Backends) {
    refresh_audio(state, b.clone()).await;
    refresh_bluetooth(state, b).await;
}

pub fn set_default(mut state: AppState, b: Backends, id: DeviceId) {
    let name = state
        .devices
        .peek()
        .iter()
        .find(|d| d.id == id)
        .map(|d| d.name.clone());
    crate::dlog!("select output: {}", name.as_deref().unwrap_or("?"));
    state.busy.set(true);
    spawn_forever(async move {
        let audio = b.audio.clone();
        match blocking(move || audio.set_default_output(id)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => state.error(format!("Couldn't switch output: {e}")),
            Err(e) => state.error(e),
        }
        refresh_audio(state, b).await;
        state.busy.set(false);
    });
}

pub fn set_volume(mut state: AppState, b: Backends, id: DeviceId, uid: String, v: f32) {
    let v = v.clamp(0.0, 1.0);
    state.volumes.write().insert(uid.clone(), v);
    state.guard.write().input(&uid, Instant::now());
    if !state.writes.write().push(&uid, (id, v)) {
        return; // a writer is running; it will pick up the newest value
    }
    spawn_forever(async move {
        loop {
            let next = state.writes.write().next(&uid);
            let Some((id, v)) = next else { break };
            let audio = b.audio.clone();
            match blocking(move || audio.set_volume(id, v)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => state.error(format!("Couldn't set volume: {e}")),
                Err(e) => state.error(e),
            }
        }
    });
}

pub fn connect(mut state: AppState, b: Backends, address: String) {
    if !state.connecting.write().insert(address.clone()) {
        return;
    }
    spawn_forever(async move {
        let (bt, addr) = (b.bluetooth.clone(), address.clone());
        match blocking(move || bt.connect(&addr)).await {
            Ok(Ok(())) => {}
            Ok(Err(crate::audio::AudioError::Bluetooth(msg))) => {
                state.show_toast(format!("Couldn't connect: {msg}"), true)
            }
            Ok(Err(e)) => state.error(format!("Couldn't connect: {e}")),
            Err(e) => state.error(e),
        }
        refresh(state.to_owned(), b.clone()).await;
        state.connecting.write().remove(&address);
        sync_if_playing(state, b);
    });
}

pub fn set_listening_mode(mut state: AppState, b: Backends, address: String, mode: ListeningMode) {
    state
        .pending_modes
        .write()
        .pick(&address, mode, Instant::now());
    spawn_forever(async move {
        let bt = b.bluetooth.clone();
        let addr = address.clone();
        match blocking(move || bt.set_listening_mode(&addr, mode)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => state.error(format!("Couldn't switch to {}: {e}", mode.label())),
            Err(e) => state.error(e),
        }
        refresh_bluetooth(state, b).await;
    });
}

/// Point the single "Duet" output at `uids` and play through it: reuse it if it
/// already holds exactly those, otherwise build a fresh one and remove every older
/// Duet-made output (never the user's own Multi-Output devices).
pub fn apply_duet_group(mut state: AppState, b: Backends, uids: Vec<String>) {
    let devices = state.devices.peek().clone();
    let reusable = find_group_with_members(&devices, &uids)
        .filter(|g| crate::audio::is_duet_group_uid(&g.uid))
        .map(|g| g.id);
    keep_mic_off_headphones(&b);
    if let Some(id) = reusable {
        crate::dlog!("duet output: reuse for {uids:?}");
        set_default(state, b, id);
        return;
    }
    let stale: Vec<DeviceId> = devices
        .iter()
        .filter(|d| d.is_multi_output() && crate::audio::is_duet_group_uid(&d.uid))
        .map(|d| d.id)
        .collect();
    crate::dlog!(
        "duet output: build for {uids:?} (removing {} old)",
        stale.len()
    );
    state.busy.set(true);
    spawn_forever(async move {
        let audio = b.audio.clone();
        let res = blocking(move || {
            let id = audio.create_multi_output(DUET_GROUP_NAME, &uids)?;
            audio.set_default_output(id)?;
            for old in stale {
                let _ = audio.remove_multi_output(old);
            }
            Ok::<_, crate::audio::AudioError>(())
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => state.error(format!("Couldn't set up the Duet output: {e}")),
            Err(e) => state.error(e),
        }
        refresh_audio(state, b).await;
        state.busy.set(false);
    });
}

/// "Listen together": connect any chosen headphones that aren't connected, then
/// play through the Duet output with all of them.
pub fn listen_together(mut state: AppState, b: Backends, plan: &Together) {
    if !plan.can_start() {
        return;
    }
    state.together_wanted.set(true);
    state.flips.write().reset();
    keep_mic_off_headphones(&b);
    let missing = plan.to_connect();
    crate::dlog!(
        "listen together: {} connected, connecting {:?}",
        plan.live_uids().len(),
        missing
    );
    if missing.is_empty() {
        match plan {
            Together::Group { group, .. } if !group.is_default => set_default(state, b, group.id),
            Together::Proposed { .. } => apply_duet_group(state, b, plan.live_uids()),
            _ => {}
        }
        return;
    }
    // Connect one at a time (Bluetooth controllers dislike parallel pages), then
    // keep_together_now picks the result up and builds the Duet output.
    state.connecting.write().extend(missing.iter().cloned());
    spawn_forever(async move {
        for addr in missing {
            let (bt, a) = (b.bluetooth.clone(), addr.clone());
            crate::dlog!("connect {addr}…");
            match blocking(move || bt.connect(&a)).await {
                Ok(Ok(())) => crate::dlog!("connected {addr}"),
                Ok(Err(crate::audio::AudioError::Bluetooth(msg))) => {
                    state.show_toast(format!("Couldn't connect: {msg}"), true)
                }
                Ok(Err(e)) => state.error(format!("Couldn't connect: {e}")),
                Err(e) => state.error(e),
            }
            state.connecting.write().remove(&addr);
        }
        // The new outputs can take a moment to appear in CoreAudio.
        for _ in 0..8 {
            refresh(state, b.clone()).await;
            let plan = state.together();
            if plan.is_ready() && plan.to_connect().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
        let plan = state.together();
        if plan.is_ready() {
            listen_together(state, b, &plan);
        }
    });
}

/// While Duet is the output, keep it in step with the chosen headphones (after the
/// user changes the choice or a chosen pair finishes connecting).
pub fn sync_if_playing(state: AppState, b: Backends) {
    let devices = state.devices.peek().clone();
    let playing = devices
        .iter()
        .any(|d| d.is_default && d.is_multi_output() && crate::audio::is_duet_group_uid(&d.uid));
    if !playing {
        return;
    }
    let plan = plan_together(
        &devices,
        &state.paired.peek(),
        &state.config.peek().listeners,
    );
    if matches!(plan, Together::Proposed { .. }) && plan.is_ready() {
        apply_duet_group(state, b, plan.live_uids());
    }
}

/// Add/remove a pair of headphones from Duet; connects it when added.
pub fn toggle_listener(mut state: AppState, b: Backends, address: String, connected: bool) {
    // Start from what's chosen now (before any explicit choice: every connected pair,
    // even if that's just one).
    let current = chosen_listeners(
        &state.devices.peek(),
        &state.paired.peek(),
        &state.config.peek().listeners,
    );
    let picks = toggle_pick(&current, &state.config.peek().listeners, &address);
    let added = picks.contains(&crate::audio::normalize_address(&address));
    state.config.write().listeners = picks;
    let cfg = state.config.peek().clone();
    let path = state.config_path.peek().clone();
    if let Err(e) = cfg.save_to(&path) {
        state.error(format!("Couldn't save your choice: {e}"));
    }
    if added && !connected {
        connect(state, b, address);
    } else {
        sync_if_playing(state, b);
    }
}

fn quit_app() {
    std::process::exit(0)
}

fn hide_window() {
    dioxus::desktop::window().window.set_visible(false);
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[component]
pub fn App() -> Element {
    let backends = use_context::<Backends>();
    let opts = use_context::<AppOptions>();
    let state = use_context_provider(|| AppState::new(&opts));
    let mode = opts.mode;

    {
        let b = backends.clone();
        let b2 = b.clone();
        use_future(move || {
            let b = b.clone();
            async move {
                loop {
                    refresh_audio(state, b.clone()).await;
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        });
        use_future(move || {
            let b = b2.clone();
            async move {
                loop {
                    refresh_bluetooth(state, b.clone()).await;
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        });
    }

    #[cfg(debug_assertions)]
    use_future(dev_hooks);

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    tray_hooks::use_tray(state, mode);

    let shell_class = match mode {
        WindowMode::Popover => "shell popover",
        WindowMode::Windowed => "shell windowed",
    };

    rsx! {
        div {
            class: "{shell_class}",
            tabindex: "-1",
            onkeydown: move |e: KeyboardEvent| {
                if e.key() == Key::Escape && mode == WindowMode::Popover {
                    hide_window();
                }
            },
            Header { mode }
            div { class: "content",
                if *state.bt_permission.read() {
                    div { class: "banner",
                        span { class: "banner-icon", dangerous_inner_html: icons::BLUETOOTH }
                        span { "Allow Bluetooth in System Settings › Privacy & Security › Bluetooth to connect AirPods." }
                    }
                }
                if *state.loaded.read() {
                    TogetherSection {}
                    OtherOutputs {}
                    footer { class: "footer",
                        span { class: "version", "Duet {env!(\"CARGO_PKG_VERSION\")}" }
                        button { class: "link quiet", onclick: move |_| quit_app(), "Quit Duet" }
                    }
                } else {
                    div { class: "loading", div { class: "spinner" } }
                }
            }
            ToastView {}
        }
    }
}

/// Development aids (debug builds only):
/// * `DUET_DEV_JS=<js>` runs the snippet once, ~2.5s after launch (e.g. click a button);
/// * `DUET_DUMP_HTML=<path>` writes the rendered DOM (with inline CSS) to `path`
///   every second, so the UI can be snapshotted without screen-recording rights.
#[cfg(debug_assertions)]
async fn dev_hooks() {
    let js = std::env::var("DUET_DEV_JS").ok();
    let dump = std::env::var_os("DUET_DUMP_HTML");
    if js.is_none() && dump.is_none() {
        return;
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;
    if let Some(js) = js {
        let _ = document::eval(&js).await;
    }
    let Some(path) = dump else { return };
    loop {
        if let Ok(v) = document::eval(
            "document.querySelectorAll('input').forEach(i => i.setAttribute('value', i.value)); \
             return document.documentElement.outerHTML",
        )
        .await
            && let Some(html) = v.as_str()
        {
            let _ = std::fs::write(&path, format!("<!doctype html>\n{html}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[component]
fn Header(mode: WindowMode) -> Element {
    let state = use_context::<AppState>();
    let devices = state.devices.read();
    let current = devices.iter().find(|d| d.is_default);
    let (label, icon) = match current {
        Some(d) => (d.name.clone(), icons::for_device(d.transport, &d.name)),
        None => ("No output".to_string(), icons::SPEAKER_MUTE),
    };
    let together = current.is_some_and(|d| d.is_multi_output());
    let mut moved = state.panel_moved;
    rsx! {
        header {
            class: "header",
            title: "Drag to move",
            onmousedown: move |e: MouseEvent| {
                if e.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) {
                    if mode == WindowMode::Popover {
                        moved.set(true);
                    }
                    dioxus::desktop::window().drag();
                }
            },
            div { class: "brand",
                span { class: "brand-mark", dangerous_inner_html: icons::AIRPODS }
                span { class: "brand-name", "Duet" }
            }
            div { class: if together { "pill together" } else { "pill" }, title: "Current output",
                span { class: "pill-icon", dangerous_inner_html: icon }
                span { class: "pill-label", "{label}" }
            }
            if mode == WindowMode::Popover {
                button {
                    class: "close-btn",
                    title: "Hide (Esc)",
                    onmousedown: move |e: MouseEvent| e.stop_propagation(),
                    onclick: move |_| hide_window(),
                    span { dangerous_inner_html: icons::CLOSE }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Listen together
// ---------------------------------------------------------------------------

#[component]
fn TogetherSection() -> Element {
    let state = use_context::<AppState>();
    let backends = use_context::<Backends>();
    let plan = state.together();
    let mut choosing = state.choosing;
    let picking = *choosing.read();
    let busy = *state.busy.read();
    let members = plan.listeners();
    let active = plan.is_active();
    let ready = plan.is_ready();
    let names: Vec<String> = {
        let cfg = state.config.read();
        members
            .iter()
            .map(|l| display_name(&cfg, l.uid(), &l.device_name_with(&cfg)))
            .collect()
    };
    let (eyebrow, title, sub) = match (&plan, active, ready) {
        (Together::Empty, _, _) => (
            "Listen together",
            "Choose two headphones".to_string(),
            "Pick any two Bluetooth headphones. Duet plays your audio on both, each with its own volume.".to_string(),
        ),
        (_, true, _) => ("Now playing on", names.join(" & "), DUET_GROUP_NAME.to_string()),
        (_, false, true) => ("Ready to share", names.join(" & "), "Plays through the “Duet” output".to_string()),
        (_, false, false) if plan.can_start() => (
            "Ready to connect",
            names.join(" & "),
            "Duet connects them and plays on both".to_string(),
        ),
        (_, false, false) => (
            "Choose another pair",
            names.join(" & "),
            "Pick at least two headphones to listen together".to_string(),
        ),
    };

    rsx! {
        section { class: if active { "card hero active" } else { "card hero" },
            div { class: "hero-head",
                div { class: "hero-icon", dangerous_inner_html: icons::TOGETHER }
                div { class: "hero-text",
                    div { class: "eyebrow", "{eyebrow}" }
                    div { class: "hero-title", "{title}" }
                    div { class: "hero-sub", "{sub}" }
                }
            }
            if plan == Together::Empty {
                button {
                    class: "primary",
                    onclick: move |_| choosing.set(true),
                    span { class: "btn-icon", dangerous_inner_html: icons::BLUETOOTH }
                    "Choose headphones"
                }
            } else {
                button {
                    class: if active { "primary active" } else { "primary" },
                    disabled: active || busy || !plan.can_start() || !state.connecting.read().is_empty(),
                    onclick: move |_| listen_together(state, backends.clone(), &plan),
                    if active {
                        span { class: "btn-icon", dangerous_inner_html: icons::CHECK }
                        "Listening together"
                    } else if !state.connecting.read().is_empty() {
                        span { class: "spinner small" }
                        "Connecting…"
                    } else if !plan.to_connect().is_empty() {
                        span { class: "btn-icon", dangerous_inner_html: icons::BLUETOOTH }
                        "Connect & listen together"
                    } else {
                        span { class: "btn-icon", dangerous_inner_html: icons::TOGETHER }
                        "Listen together"
                    }
                }
            }
        }
        if picking {
            HeadphonePicker {}
        }
        if !members.is_empty() {
            div { class: "section-row",
                div { class: "section-label", "Listeners" }
                if !picking {
                    button { class: "link", onclick: move |_| choosing.set(true), "Choose headphones…" }
                }
            }
            div { class: "stack",
                for (i, l) in members.into_iter().enumerate() {
                    ListenerCard { key: "{l.uid()}", listener: l, index: i }
                }
            }
        }
    }
}

/// Checklist of every paired Bluetooth audio device; ticking one connects it and
/// adds it to Duet.
#[component]
fn HeadphonePicker() -> Element {
    let state = use_context::<AppState>();
    let backends = use_context::<Backends>();
    let mut choosing = state.choosing;
    let chosen: Vec<String> = chosen_listeners(
        &state.devices.read(),
        &state.paired.read(),
        &state.config.read().listeners,
    )
    .iter()
    .filter_map(|l| crate::audio::address_from_uid(l.uid()))
    .collect();
    let choices = headphone_choices(&state.devices.read(), &state.paired.read());
    let connecting = state.connecting.read().clone();
    rsx! {
        section { class: "card picker",
            div { class: "picker-head",
                div {
                    div { class: "picker-title", "Choose headphones" }
                    div { class: "picker-sub", "Tick the ones to listen on. Duet connects them for you." }
                }
                button { class: "link", onclick: move |_| choosing.set(false), "Done" }
            }
            if choices.is_empty() {
                div { class: "picker-empty",
                    "No Bluetooth headphones are paired yet. Pair them in Bluetooth settings first."
                }
            }
            div { class: "pick-list",
                for (addr, name, connected) in choices {
                    {
                        let on = chosen.contains(&addr);
                        let busy = connecting.contains(&addr);
                        let b = backends.clone();
                        let a = addr.clone();
                        let status = if busy { "Connecting…" } else if connected { "Connected" } else { "Not connected" };
                        rsx! {
                            button {
                                key: "{addr}",
                                class: if on { "pick on" } else { "pick" },
                                role: "checkbox",
                                "aria-checked": "{on}",
                                onclick: move |_| toggle_listener(state, b.clone(), a.clone(), connected),
                                span { class: "check", dangerous_inner_html: if on { icons::CHECK } else { "" } }
                                span { class: "row-icon", dangerous_inner_html: icons::for_device(crate::audio::Transport::Bluetooth, &name) }
                                span { class: "row-name", "{name}" }
                                span { class: if connected { "pick-status on" } else { "pick-status" },
                                    if busy { span { class: "spinner small" } }
                                    "{status}"
                                }
                            }
                        }
                    }
                }
            }
            if cfg!(target_os = "windows") {
                div { class: "picker-note", "On Windows, connect headphones in Settings › Bluetooth & devices; they appear here once connected." }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Listener card
// ---------------------------------------------------------------------------

#[component]
fn ListenerCard(listener: Listener, index: usize) -> Element {
    let state = use_context::<AppState>();
    let backends = use_context::<Backends>();
    let mut editing = use_signal(|| false);
    let mut draft = use_signal(String::new);

    let uid = listener.uid().to_string();
    let device_name = listener.device_name_with(&state.config.read());
    let name = display_name(&state.config.read(), &uid, &device_name);
    let tone = ["tone-0", "tone-1", "tone-2", "tone-3"][index % 4];

    let (status_class, sub) = match &listener {
        Listener::Connected(_) => ("listener", device_name.clone()),
        Listener::Disconnected { .. } => ("listener offline", "Not connected".to_string()),
        Listener::Unknown { .. } => ("listener offline", "Unavailable".to_string()),
    };

    let commit = {
        let uid = uid.clone();
        move || {
            if *editing.peek() {
                editing.set(false);
                state.save_nickname(&uid, &draft.peek());
            }
        }
    };

    let name_view = if *editing.read() {
        let mut commit_key = commit.clone();
        let mut commit_blur = commit.clone();
        rsx! {
            input {
                class: "name-input",
                value: "{draft}",
                maxlength: "40",
                spellcheck: "false",
                oninput: move |e| draft.set(e.value()),
                onkeydown: move |e: KeyboardEvent| match e.key() {
                    Key::Enter => commit_key(),
                    Key::Escape => { e.stop_propagation(); editing.set(false) }
                    _ => {}
                },
                onblur: move |_| commit_blur(),
                onmounted: move |e| async move {
                    let _ = e.set_focus(true).await;
                },
            }
        }
    } else {
        let current = name.clone();
        rsx! {
            button {
                class: "name",
                title: "Rename",
                onclick: move |_| {
                    draft.set(current.clone());
                    editing.set(true);
                },
                "{name}"
            }
        }
    };

    let trailing = match &listener {
        Listener::Connected(d) => {
            let v = state.volumes.read().get(&d.uid).copied().or(d.volume);
            match v {
                Some(v) => rsx! {
                    div { class: "readout", span { class: "num", "{to_percent(v)}" } span { class: "unit", "%" } }
                },
                None => rsx! { span { class: "muted-note", "Fixed volume" } },
            }
        }
        Listener::Disconnected { bluetooth, .. } => {
            let addr = bluetooth.address.clone();
            let is_connecting = state.connecting.read().contains(&addr);
            let b = backends.clone();
            rsx! {
                button {
                    class: if is_connecting { "connect busy" } else { "connect" },
                    disabled: is_connecting,
                    onclick: move |_| connect(state, b.clone(), addr.clone()),
                    if is_connecting {
                        span { class: "spinner small" }
                        "Connecting…"
                    } else {
                        span { class: "btn-icon", dangerous_inner_html: icons::BLUETOOTH }
                        "Connect"
                    }
                }
            }
        }
        Listener::Unknown { .. } => rsx! {},
    };

    let extras: Option<(String, HeadphoneInfo)> = match &listener {
        Listener::Connected(d) => crate::audio::bluetooth_address_of(d).and_then(|a| {
            let a = crate::audio::normalize_address(&a);
            state.headphones.read().get(&a).cloned().map(|i| (a, i))
        }),
        _ => None,
    };
    let battery = extras
        .as_ref()
        .map(|(_, i)| battery_parts(&i.battery))
        .unwrap_or_default();
    let noise = match &extras {
        Some((addr, info)) if !info.supported_modes.is_empty() => {
            let shown = state
                .pending_modes
                .read()
                .shown(addr, info.listening_mode, Instant::now());
            rsx! { NoiseControl { address: addr.clone(), modes: info.supported_modes.clone(), current: shown } }
        }
        _ => rsx! {},
    };

    let controls = match &listener {
        Listener::Connected(d) if d.volume.is_some() => {
            let v = state
                .volumes
                .read()
                .get(&d.uid)
                .copied()
                .or(d.volume)
                .unwrap_or(0.0);
            rsx! { VolumeControls { device: d.clone(), volume: v, big: true } }
        }
        _ => rsx! {},
    };

    rsx! {
        div { class: "card {status_class} {tone}",
            div { class: "listener-head",
                div { class: "avatar", dangerous_inner_html: icons::for_device(crate::audio::Transport::Bluetooth, &device_name) }
                div { class: "who",
                    {name_view}
                    div { class: "device", title: "{device_name}",
                        span { class: "device-name", "{sub}" }
                        if !battery.is_empty() {
                            span { class: "battery",
                                for (label, pct) in battery {
                                    span {
                                        key: "{label}",
                                        class: if pct <= 20 { "batt low" } else { "batt" },
                                        title: "Battery",
                                        span { class: "batt-icon", style: "--level: {pct}" }
                                        if !label.is_empty() { span { class: "batt-label", "{label}" } }
                                        "{pct}%"
                                    }
                                }
                            }
                        }
                    }
                }
                {trailing}
            }
            {controls}
            {noise}
        }
    }
}

/// Off / Transparency / Adaptive / Noise Cancellation switcher.
#[component]
fn NoiseControl(
    address: String,
    modes: Vec<ListeningMode>,
    current: Option<ListeningMode>,
) -> Element {
    let state = use_context::<AppState>();
    let backends = use_context::<Backends>();
    rsx! {
        div { class: "noise", role: "radiogroup", "aria-label": "Noise control",
            for m in modes {
                button {
                    key: "{m.label()}",
                    class: if current == Some(m) { "nc on" } else { "nc" },
                    role: "radio",
                    "aria-checked": "{current == Some(m)}",
                    title: "{m.label()}",
                    onclick: {
                        let (b, addr) = (backends.clone(), address.clone());
                        move |_| {
                            if current != Some(m) {
                                set_listening_mode(state, b.clone(), addr.clone(), m);
                            }
                        }
                    },
                    span { class: "nc-icon", dangerous_inner_html: icons::for_mode(m) }
                    span { class: "nc-label",
                        if m == ListeningMode::NoiseCancellation { "Noise Cancel" } else { "{m.label()}" }
                    }
                }
            }
        }
    }
}

#[component]
fn VolumeControls(device: OutputDevice, volume: f32, big: bool) -> Element {
    let state = use_context::<AppState>();
    let backends = use_context::<Backends>();
    let pct = to_percent(volume);
    let id = device.id;
    let uid = device.uid.clone();

    let on_input = {
        let (b, uid) = (backends.clone(), uid.clone());
        move |e: FormEvent| {
            if let Some(v) = parse_slider(&e.value()) {
                set_volume(state, b.clone(), id, uid.clone(), v);
            }
        }
    };
    let on_down = {
        let uid = uid.clone();
        move |_| {
            let mut g = state.guard;
            g.write().press(&uid, Instant::now());
        }
    };
    let on_up = {
        let uid = uid.clone();
        move |_| {
            let mut g = state.guard;
            g.write().release(&uid, Instant::now());
        }
    };

    let chips = big.then(|| {
        let muted = pct == 0;
        let mute_click = {
            let (b, uid) = (backends.clone(), uid.clone());
            move |_| {
                let mut premute = state.premute;
                let target = if muted {
                    premute.peek().get(&uid).copied().filter(|v| *v > 0.0).unwrap_or(0.5)
                } else {
                    premute.write().insert(uid.clone(), volume);
                    0.0
                };
                set_volume(state, b.clone(), id, uid.clone(), target);
            }
        };
        rsx! {
            div { class: "chips",
                button {
                    class: if muted { "chip mute on" } else { "chip mute" },
                    onclick: mute_click,
                    span { class: "chip-icon", dangerous_inner_html: if muted { icons::SPEAKER_MUTE } else { icons::SPEAKER_LOW } }
                    if muted { "Unmute" } else { "Mute" }
                }
                for preset in [25u8, 50, 75, 100] {
                    button {
                        key: "{preset}",
                        class: if pct == preset { "chip on" } else { "chip" },
                        onclick: {
                            let (b, uid) = (backends.clone(), uid.clone());
                            move |_| set_volume(state, b.clone(), id, uid.clone(), from_percent(preset as f64))
                        },
                        "{preset}"
                    }
                }
            }
        }
    });

    rsx! {
        div { class: if big { "slider-row big" } else { "slider-row" },
            span { class: "vol-icon", dangerous_inner_html: if pct == 0 { icons::SPEAKER_MUTE } else { icons::SPEAKER_LOW } }
            input {
                r#type: "range",
                class: "slider",
                min: "0",
                max: "100",
                step: "1",
                value: "{pct}",
                style: "--p: {volume.clamp(0.0, 1.0)}",
                "aria-label": "Volume for {device.name}",
                oninput: on_input,
                onpointerdown: on_down,
                onpointerup: on_up,
            }
            span { class: "vol-icon", dangerous_inner_html: icons::SPEAKER_HIGH }
            if !big {
                span { class: "mini-readout", "{pct}%" }
            }
        }
        {chips}
    }
}

// ---------------------------------------------------------------------------
// Other outputs
// ---------------------------------------------------------------------------

#[component]
fn OtherOutputs() -> Element {
    let state = use_context::<AppState>();
    let mut open = use_signal(|| false);
    let shown = state.together().shown_uids();
    let devices = state.devices.read();
    let others: Vec<OutputDevice> = other_outputs(&devices, &shown)
        .into_iter()
        .cloned()
        .collect();
    if others.is_empty() {
        return rsx! {};
    }
    let n = others.len();
    let is_open = *open.read();
    rsx! {
        button {
            class: if is_open { "disclosure open" } else { "disclosure" },
            onclick: move |_| open.toggle(),
            span { class: "chev", dangerous_inner_html: icons::CHEVRON }
            span { "Other outputs" }
            span { class: "count", "{n}" }
        }
        if is_open {
            div { class: "card list",
                for d in others {
                    OutputRow { key: "{d.uid}", device: d }
                }

            }
        }
    }
}

#[component]
fn OutputRow(device: OutputDevice) -> Element {
    let state = use_context::<AppState>();
    let backends = use_context::<Backends>();
    let vol = state
        .volumes
        .read()
        .get(&device.uid)
        .copied()
        .or(device.volume);
    let id = device.id;
    let busy = *state.busy.read();
    rsx! {
        div { class: if device.is_default { "row current" } else { "row" },
            div { class: "row-main",
                span { class: "row-icon", dangerous_inner_html: icons::for_device(device.transport, &device.name) }
                span { class: "row-name", title: "{device.name}", "{device.name}" }
                if device.is_default {
                    span { class: "in-use", span { class: "btn-icon", dangerous_inner_html: icons::CHECK } "In use" }
                } else {
                    button {
                        class: "use",
                        disabled: busy,
                        onclick: move |_| {
                            let mut wanted = state.together_wanted;
                            wanted.set(false);
                            set_default(state, backends.clone(), id);
                        },
                        "Use"
                    }
                }
            }
            if let Some(v) = vol {
                VolumeControls { device: device.clone(), volume: v, big: false }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

#[component]
fn ToastView() -> Element {
    let mut state = use_context::<AppState>();
    let toast = state.toast.read().clone();
    match toast {
        Some(t) => rsx! {
            div { key: "{t.id}", class: "toast", role: "alert",
                span { class: "toast-icon", dangerous_inner_html: icons::WARNING }
                span { class: "toast-msg", "{t.message}" }
                if t.bluetooth_settings {
                    button {
                        class: "toast-action",
                        onclick: move |_| {
                            state.toast.set(None);
                            if let Err(e) = crate::ui::platform::open_bluetooth_settings() {
                                state.error(format!("Couldn't open Bluetooth settings: {e}"));
                            }
                        },
                        "Open Bluetooth Settings"
                    }
                }
                button { class: "toast-close", title: "Dismiss", onclick: move |_| state.toast.set(None),
                    span { dangerous_inner_html: icons::CLOSE }
                }
            }
        },
        None => rsx! {},
    }
}

// ---------------------------------------------------------------------------
// Tray integration
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
mod tray_hooks {
    use super::*;
    use crate::ui::tray::{create_tray, place_popover, show_popover, show_window, to_screen_rect};
    use dioxus::desktop::tao::event::Event;
    use dioxus::desktop::trayicon::{MouseButton, MouseButtonState, TrayIcon, TrayIconEvent};
    use dioxus::desktop::{
        use_tray_icon_event_handler, use_tray_menu_event_handler, use_window, use_wry_event_handler,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    /// Tray icon click: hide the panel if it's up front, otherwise show it (under the
    /// icon unless the user has moved it).
    fn toggle_panel(
        w: &dioxus::desktop::tao::window::Window,
        state: AppState,
        mode: WindowMode,
        icon: Option<crate::ui::logic::ScreenRect>,
    ) {
        #[cfg(target_os = "macos")]
        let here = crate::ui::tray::on_active_space(w);
        #[cfg(not(target_os = "macos"))]
        let here = true;
        if w.is_visible() && w.is_focused() && here {
            w.set_visible(false);
        } else if mode == WindowMode::Popover && !*state.panel_moved.peek() {
            show_popover(w, icon);
        } else {
            show_window(w);
        }
    }

    pub fn use_tray(state: AppState, mode: WindowMode) {
        let desktop = use_window();
        let tray: Rc<Option<TrayIcon>> = use_hook(|| {
            Rc::new(match create_tray() {
                Ok(t) => Some(t),
                Err(e) => {
                    state.error(format!("Couldn't create the menu bar icon: {e}"));
                    None
                }
            })
        });

        // First show: Dioxus applies the builder's visibility once the webview
        // initializes, so position (under the tray icon) and show just after that.
        {
            let (desktop, tray) = (desktop.clone(), tray.clone());
            use_hook(move || {
                spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    if mode == WindowMode::Popover {
                        let rect = tray
                            .as_ref()
                            .as_ref()
                            .and_then(|t| t.rect())
                            .map(|r| to_screen_rect(&r));
                        place_popover(&desktop.window, rect);
                    }
                    show_window(&desktop.window);
                });
            });
        }

        let show = {
            let (desktop, tray) = (desktop.clone(), tray.clone());
            move || match mode {
                WindowMode::Popover if *state.panel_moved.peek() => show_window(&desktop.window),
                WindowMode::Popover => {
                    let rect = tray
                        .as_ref()
                        .as_ref()
                        .and_then(|t| t.rect())
                        .map(|r| to_screen_rect(&r));
                    show_popover(&desktop.window, rect);
                }
                WindowMode::Windowed => show_window(&desktop.window),
            }
        };

        {
            let show = show.clone();
            use_tray_menu_event_handler(move |ev| {
                match parse_tray_id(ev.id.as_ref()) {
                    Some(TrayAction::Open) => {
                        // Let the native menu finish closing first, or macOS hands focus
                        // straight back to the previous app.
                        let show = show.clone();
                        spawn(async move {
                            tokio::time::sleep(Duration::from_millis(120)).await;
                            show();
                        });
                    }
                    Some(TrayAction::Quit) => std::process::exit(0),
                    None => {}
                }
            });
        }

        {
            let desktop = desktop.clone();
            use_tray_icon_event_handler(move |ev| {
                // Left click (released) or right click (pressed; macOS reports no release).
                if let TrayIconEvent::Click {
                    button,
                    button_state,
                    rect,
                    ..
                } = ev
                    && matches!(
                        (button, button_state),
                        (MouseButton::Left, MouseButtonState::Up)
                            | (MouseButton::Right, MouseButtonState::Down)
                    )
                {
                    toggle_panel(&desktop.window, state, mode, Some(to_screen_rect(rect)));
                }
            });
        }

        // Debug builds: `DUET_DEV_CMD=<file>`; writing "toggle"/"show"/"hide" to it runs the
        // same code as clicking the tray icon (lets the panel logic be tested headlessly).
        #[cfg(debug_assertions)]
        {
            let (desktop, tray) = (desktop.clone(), tray.clone());
            use_future(move || {
                let (desktop, tray) = (desktop.clone(), tray.clone());
                async move {
                    let Some(path) = std::env::var_os("DUET_DEV_CMD") else {
                        return;
                    };
                    loop {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let Ok(cmd) = std::fs::read_to_string(&path) else {
                            continue;
                        };
                        if cmd.trim().is_empty() {
                            continue;
                        }
                        let _ = std::fs::write(&path, "");
                        let rect = tray
                            .as_ref()
                            .as_ref()
                            .and_then(|t| t.rect())
                            .map(|r| to_screen_rect(&r));
                        let w = &desktop.window;
                        match cmd.trim() {
                            "toggle" => toggle_panel(w, state, mode, rect),
                            "show" => show_window(w),
                            "hide" => w.set_visible(false),
                            _ => {}
                        }
                    }
                }
            });
        }

        {
            let reopen = show.clone();
            let policy_set = Cell::new(false);
            use_wry_event_handler(move |event, _target| {
                if !policy_set.replace(true) {
                    #[cfg(target_os = "macos")]
                    {
                        use dioxus::desktop::tao::platform::macos::{
                            ActivationPolicy, EventLoopWindowTargetExtMacOS,
                        };
                        _target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
                    }
                }
                // Opening Duet again (Finder, Spotlight, Login Items) brings the panel back.
                if let Event::Reopen { .. } = event {
                    reopen();
                }
            });
        }
    }
}
