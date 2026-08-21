use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tauri::{
    App, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Monitor, PhysicalPosition,
    PhysicalSize, UserAttentionType, WebviewUrl, WebviewWindowBuilder, Wry,
};

use crate::{
    model::{Notification, NotificationDisplay, NotificationSound, TextSize},
    notifier::NotifyError,
};

pub(crate) const BANNER_WINDOW: &str = "banner";
const MAX_VISIBLE_PASSIVE_BANNERS: usize = 3;
const BANNER_WIDTH: f64 = 420.0;
const MIN_BANNER_HEIGHT: f64 = 104.0;
const MAX_BANNER_HEIGHT: f64 = 720.0;
const APPROVAL_WIDTH: f64 = 680.0;
const MIN_APPROVAL_HEIGHT: f64 = 360.0;
const MAX_APPROVAL_HEIGHT: f64 = 640.0;
const SCREEN_MARGIN: f64 = 16.0;
const MAX_PRESENTATION_ATTEMPTS: u8 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PresentationMode {
    Passive,
    ApprovalCentered,
    ApprovalCorner,
}

impl PresentationMode {
    const fn is_approval(self) -> bool {
        matches!(self, Self::ApprovalCentered | Self::ApprovalCorner)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkAreaGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Default)]
struct PresentationRetry {
    attempts: u8,
    next_attempt_at: Option<Instant>,
}

struct PendingSound {
    generation: u64,
    notification_id: i32,
    sound: Option<NotificationSound>,
}

pub struct BannerState {
    banners: Mutex<VecDeque<Notification>>,
    activation_claims: Mutex<BTreeMap<i32, u64>>,
    pending_sound: Mutex<Option<PendingSound>>,
    retry: Mutex<PresentationRetry>,
    next_activation_claim: AtomicU64,
    generation: AtomicU64,
    center_approval_dialogs: AtomicBool,
    notification_display: AtomicU8,
    dirty: AtomicBool,
    presentation_scheduled: AtomicBool,
}

impl Default for BannerState {
    fn default() -> Self {
        Self {
            banners: Mutex::default(),
            activation_claims: Mutex::default(),
            pending_sound: Mutex::default(),
            retry: Mutex::default(),
            next_activation_claim: AtomicU64::default(),
            generation: AtomicU64::default(),
            center_approval_dialogs: AtomicBool::new(true),
            notification_display: AtomicU8::new(notification_display_code(
                NotificationDisplay::Primary,
            )),
            dirty: AtomicBool::default(),
            presentation_scheduled: AtomicBool::default(),
        }
    }
}

const fn notification_display_code(display: NotificationDisplay) -> u8 {
    match display {
        NotificationDisplay::Primary => 0,
        NotificationDisplay::FocusedWindow => 1,
        NotificationDisplay::Pointer => 2,
        NotificationDisplay::Secondary => 3,
    }
}

const fn notification_display_from_code(code: u8) -> NotificationDisplay {
    match code {
        1 => NotificationDisplay::FocusedWindow,
        2 => NotificationDisplay::Pointer,
        3 => NotificationDisplay::Secondary,
        _ => NotificationDisplay::Primary,
    }
}

impl BannerState {
    fn push(&self, notification: Notification) -> Result<(), NotifyError> {
        let mut banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        let mut activation_claims = self.activation_claims.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner activation state is unavailable".to_owned())
        })?;
        let mut pending_sound = self.pending_sound.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
        })?;
        let mut retry = self.retry.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner retry state is unavailable".to_owned())
        })?;

        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        *pending_sound = Some(PendingSound {
            generation,
            notification_id: notification.id,
            sound: notification.sound,
        });
        *retry = PresentationRetry::default();
        if let Some(existing) = banners
            .iter_mut()
            .find(|existing| existing.id == notification.id)
        {
            activation_claims.remove(&notification.id);
            *existing = notification;
            self.dirty.store(true, Ordering::Release);
            return Ok(());
        }
        if notification.approval.is_some() {
            if banners.iter().any(|banner| banner.approval.is_some()) {
                return Err(NotifyError::Scheduling(
                    "an Aizu command approval is already visible".to_owned(),
                ));
            }
        } else if banners
            .iter()
            .filter(|banner| banner.approval.is_none())
            .count()
            >= MAX_VISIBLE_PASSIVE_BANNERS
            && let Some(eviction) = banners.iter().position(|banner| banner.approval.is_none())
            && let Some(evicted) = banners.remove(eviction)
        {
            activation_claims.remove(&evicted.id);
        }
        banners.push_back(notification);
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    fn dismiss(&self, id: i32) -> Result<bool, NotifyError> {
        let mut banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        self.activation_claims
            .lock()
            .map_err(|_| {
                NotifyError::Scheduling("Aizu banner activation state is unavailable".to_owned())
            })?
            .remove(&id);
        self.remove_banner(&mut banners, id)
    }

    fn remove_banner(
        &self,
        banners: &mut VecDeque<Notification>,
        id: i32,
    ) -> Result<bool, NotifyError> {
        let mut pending_sound = self.pending_sound.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
        })?;
        let mut retry = self.retry.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner retry state is unavailable".to_owned())
        })?;
        let previous_len = banners.len();
        banners.retain(|banner| banner.id != id);
        if banners.len() != previous_len {
            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            if pending_sound
                .as_ref()
                .is_some_and(|pending| pending.notification_id == id)
            {
                *pending_sound = None;
            } else if let Some(pending) = pending_sound.as_mut() {
                pending.generation = generation;
            }
            *retry = PresentationRetry::default();
            self.dirty.store(true, Ordering::Release);
        }
        Ok(banners.is_empty())
    }

    fn snapshot(&self) -> Result<Vec<Notification>, NotifyError> {
        self.banners
            .lock()
            .map(|banners| banners.iter().cloned().collect())
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))
    }

    fn presentation_snapshot(&self) -> Result<Vec<Notification>, NotifyError> {
        self.snapshot()
            .map(|snapshot| visible_notifications(&snapshot))
    }

    fn claim_activation(
        &self,
        id: i32,
    ) -> Result<(BannerActivationClaim, aizu_core::TerminalActivation), NotifyError> {
        let banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        let target = banners
            .iter()
            .find(|banner| banner.id == id)
            .and_then(|banner| banner.activation.clone())
            .ok_or_else(|| {
                NotifyError::Scheduling("terminal activation is unavailable".to_owned())
            })?;
        let mut claims = self.activation_claims.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner activation state is unavailable".to_owned())
        })?;
        if claims.contains_key(&id) {
            return Err(NotifyError::Scheduling(
                "terminal activation is already in progress".to_owned(),
            ));
        }
        let token = self.next_activation_claim.fetch_add(1, Ordering::AcqRel) + 1;
        claims.insert(id, token);
        Ok((BannerActivationClaim { id, token }, target))
    }

    fn cancel_activation(&self, claim: &BannerActivationClaim) -> Result<(), NotifyError> {
        let mut claims = self.activation_claims.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner activation state is unavailable".to_owned())
        })?;
        if claims.get(&claim.id) == Some(&claim.token) {
            claims.remove(&claim.id);
        }
        Ok(())
    }

    fn complete_activation(&self, claim: &BannerActivationClaim) -> Result<bool, NotifyError> {
        let mut banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        let mut claims = self.activation_claims.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner activation state is unavailable".to_owned())
        })?;
        if claims.get(&claim.id) != Some(&claim.token) {
            return if banners.iter().any(|banner| banner.id == claim.id) {
                Err(NotifyError::Scheduling(
                    "terminal activation is no longer current".to_owned(),
                ))
            } else {
                Ok(banners.is_empty())
            };
        }
        claims.remove(&claim.id);
        drop(claims);
        self.remove_banner(&mut banners, claim.id)
    }

    fn update_text_size(&self, text_size: TextSize) -> Result<bool, NotifyError> {
        let mut banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        if banners.is_empty() || banners.iter().all(|banner| banner.text_size == text_size) {
            return Ok(false);
        }
        let mut pending_sound = self.pending_sound.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
        })?;
        let mut retry = self.retry.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner retry state is unavailable".to_owned())
        })?;

        for banner in &mut *banners {
            banner.text_size = text_size;
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(pending) = pending_sound.as_mut() {
            pending.generation = generation;
        }
        *retry = PresentationRetry::default();
        self.dirty.store(true, Ordering::Release);
        Ok(true)
    }

    fn update_approval_centering(&self, centered: bool) -> Result<bool, NotifyError> {
        let previous = self
            .center_approval_dialogs
            .swap(centered, Ordering::AcqRel);
        if previous == centered {
            return Ok(false);
        }
        let banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        if !banners.iter().any(|banner| banner.approval.is_some()) {
            return Ok(false);
        }
        let mut pending_sound = self.pending_sound.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
        })?;
        let mut retry = self.retry.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner retry state is unavailable".to_owned())
        })?;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(pending) = pending_sound.as_mut() {
            pending.generation = generation;
        }
        *retry = PresentationRetry::default();
        self.dirty.store(true, Ordering::Release);
        Ok(true)
    }

    fn notification_display(&self) -> NotificationDisplay {
        notification_display_from_code(self.notification_display.load(Ordering::Acquire))
    }

    fn update_notification_display(
        &self,
        display: NotificationDisplay,
    ) -> Result<bool, NotifyError> {
        let display_code = notification_display_code(display);
        if self
            .notification_display
            .swap(display_code, Ordering::AcqRel)
            == display_code
        {
            return Ok(false);
        }
        let banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        if banners.is_empty() {
            return Ok(false);
        }
        let mut pending_sound = self.pending_sound.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
        })?;
        let mut retry = self.retry.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner retry state is unavailable".to_owned())
        })?;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(pending) = pending_sound.as_mut() {
            pending.generation = generation;
        }
        *retry = PresentationRetry::default();
        self.dirty.store(true, Ordering::Release);
        Ok(true)
    }

    fn clear_passive(&self) -> Result<(), NotifyError> {
        let ids = self
            .snapshot()?
            .into_iter()
            .filter(|banner| banner.approval.is_none())
            .map(|banner| banner.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.dismiss(id)?;
        }
        Ok(())
    }

    fn begin_presentation(&self, now: Instant) -> Option<u64> {
        if self.presentation_scheduled.swap(true, Ordering::AcqRel) {
            return None;
        }
        let retry_due = self
            .retry
            .lock()
            .is_ok_and(|retry| retry.next_attempt_at.is_none_or(|deadline| now >= deadline));
        if !retry_due {
            self.presentation_scheduled.store(false, Ordering::Release);
            return None;
        }
        if self.dirty.swap(false, Ordering::AcqRel) {
            return Some(self.generation.load(Ordering::Acquire));
        }
        self.presentation_scheduled.store(false, Ordering::Release);
        None
    }

    fn finish_presentation(&self, generation: u64, succeeded: bool, now: Instant) {
        if self.generation.load(Ordering::Acquire) != generation {
            self.presentation_scheduled.store(false, Ordering::Release);
            return;
        }
        let mut discard_sound = false;
        if let Ok(mut retry) = self.retry.lock() {
            if succeeded {
                *retry = PresentationRetry::default();
            } else {
                retry.attempts = retry.attempts.saturating_add(1);
                if retry.attempts < MAX_PRESENTATION_ATTEMPTS {
                    self.dirty.store(true, Ordering::Release);
                    retry.next_attempt_at = now.checked_add(retry_delay(retry.attempts));
                } else {
                    retry.next_attempt_at = None;
                    discard_sound = true;
                }
            }
        }
        if discard_sound {
            let _ = self.take_pending_sound(generation);
        }
        self.presentation_scheduled.store(false, Ordering::Release);
    }

    fn take_pending_sound(
        &self,
        generation: u64,
    ) -> Result<Option<NotificationSound>, NotifyError> {
        self.pending_sound
            .lock()
            .map(|mut pending| {
                if pending
                    .as_ref()
                    .is_some_and(|pending| pending.generation == generation)
                {
                    return pending.take().and_then(|pending| pending.sound);
                }
                None
            })
            .map_err(|_| {
                NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
            })
    }

    #[cfg(test)]
    fn clear_pending(&self) -> Result<(), NotifyError> {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.dirty.store(false, Ordering::Release);
        *self.pending_sound.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner sound state is unavailable".to_owned())
        })? = None;
        *self.retry.lock().map_err(|_| {
            NotifyError::Scheduling("Aizu banner retry state is unavailable".to_owned())
        })? = PresentationRetry::default();
        Ok(())
    }
}

pub struct BannerActivationClaim {
    id: i32,
    token: u64,
}

fn retry_delay(attempts: u8) -> Duration {
    match attempts {
        0 | 1 => Duration::from_millis(250),
        2 => Duration::from_secs(1),
        3 => Duration::from_secs(4),
        _ => Duration::from_secs(15),
    }
}

fn visible_notifications(snapshot: &[Notification]) -> Vec<Notification> {
    snapshot
        .iter()
        .find(|banner| banner.approval.is_some())
        .map_or_else(|| snapshot.to_vec(), |approval| vec![approval.clone()])
}

fn presentation_mode(snapshot: &[Notification], center_approval_dialogs: bool) -> PresentationMode {
    if snapshot.iter().any(|banner| banner.approval.is_some()) {
        if center_approval_dialogs {
            PresentationMode::ApprovalCentered
        } else {
            PresentationMode::ApprovalCorner
        }
    } else {
        PresentationMode::Passive
    }
}

fn needs_user_attention(mode: PresentationMode, focused: Option<bool>) -> bool {
    mode.is_approval() && focused != Some(true)
}

fn window_geometry(
    mode: PresentationMode,
    requested_height: f64,
    work_area: WorkAreaGeometry,
) -> WindowGeometry {
    let available_width = (work_area.width - SCREEN_MARGIN * 2.0).max(1.0);
    let available_height = (work_area.height - SCREEN_MARGIN * 2.0).max(1.0);
    let (target_width, minimum_height, maximum_height) = match mode {
        PresentationMode::Passive | PresentationMode::ApprovalCorner => {
            (BANNER_WIDTH, MIN_BANNER_HEIGHT, MAX_BANNER_HEIGHT)
        }
        PresentationMode::ApprovalCentered => {
            (APPROVAL_WIDTH, MIN_APPROVAL_HEIGHT, MAX_APPROVAL_HEIGHT)
        }
    };
    let width = target_width.min(available_width);
    let minimum_height = minimum_height.min(available_height);
    let maximum_height = maximum_height.min(available_height).max(minimum_height);
    let height = requested_height.clamp(minimum_height, maximum_height);
    let (x, y) = match mode {
        PresentationMode::Passive | PresentationMode::ApprovalCorner => (
            work_area.x + work_area.width - width - SCREEN_MARGIN,
            work_area.y + SCREEN_MARGIN,
        ),
        PresentationMode::ApprovalCentered => (
            work_area.x + (work_area.width - width) / 2.0,
            work_area.y + (work_area.height - height) / 2.0,
        ),
    };
    WindowGeometry {
        x,
        y,
        width,
        height,
    }
}

pub fn setup(app: &App<Wry>) {
    app.manage(BannerState::default());
}

fn ensure_window(app: &AppHandle<Wry>) -> Result<tauri::WebviewWindow<Wry>, NotifyError> {
    if let Some(window) = app.get_webview_window(BANNER_WINDOW) {
        return Ok(window);
    }
    WebviewWindowBuilder::new(
        app,
        BANNER_WINDOW,
        WebviewUrl::App("index.html?banner=1".into()),
    )
    .title("Aizu Banner")
    .inner_size(BANNER_WIDTH, MIN_BANNER_HEIGHT)
    .resizable(false)
    .closable(false)
    .decorations(false)
    .transparent(true)
    .shadow(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .focused(false)
    .visible(false)
    .accept_first_mouse(true)
    .build()
    .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    app.get_webview_window(BANNER_WINDOW)
        .ok_or_else(|| NotifyError::Scheduling("Aizu banner window is unavailable".to_owned()))
}

pub fn show(app: &AppHandle<Wry>, notification: &Notification) -> Result<(), NotifyError> {
    app.state::<BannerState>().push(notification.clone())?;
    if let Err(error) = request_present(app) {
        let _ = app.state::<BannerState>().dismiss(notification.id);
        return Err(error);
    }
    Ok(())
}

pub fn request_present(app: &AppHandle<Wry>) -> Result<(), NotifyError> {
    let state = app.state::<BannerState>();
    let Some(generation) = state.begin_presentation(Instant::now()) else {
        return Ok(());
    };
    let app = app.clone();
    let main_thread_app = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        let result = present(&main_thread_app, generation);
        main_thread_app.state::<BannerState>().finish_presentation(
            generation,
            result.is_ok(),
            Instant::now(),
        );
        // A push or dismiss can supersede this generation while it is presenting.
        // Queue the current generation immediately instead of waiting for the worker tick.
        let _ = request_present(&main_thread_app);
    }) {
        state.finish_presentation(generation, false, Instant::now());
        return Err(NotifyError::Scheduling(error.to_string()));
    }
    Ok(())
}

fn present(app: &AppHandle<Wry>, generation: u64) -> Result<(), NotifyError> {
    let snapshot = app.state::<BannerState>().snapshot()?;
    if app
        .state::<BannerState>()
        .generation
        .load(Ordering::Acquire)
        != generation
    {
        return Ok(());
    }
    if snapshot.is_empty() {
        let _ = app.state::<BannerState>().take_pending_sound(generation)?;
        if let Some(window) = app.get_webview_window(BANNER_WINDOW) {
            window
                .hide()
                .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
        }
        return Ok(());
    }
    let window = ensure_window(app)?;
    let visible = visible_notifications(&snapshot);
    let mode = presentation_mode(
        &visible,
        app.state::<BannerState>()
            .center_approval_dialogs
            .load(Ordering::Acquire),
    );
    let monitor = configured_monitor(&window, app.state::<BannerState>().notification_display())?;
    resize_window_for_mode(&window, &monitor, MIN_BANNER_HEIGHT, mode)?;
    window
        .show()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    if let Some(broker) = app.try_state::<crate::approval_broker::ApprovalBroker>() {
        for banner in visible.iter().filter(|banner| banner.approval.is_some()) {
            let _ = broker.mark_window_shown(banner.id);
        }
    }
    app.emit_to(BANNER_WINDOW, "aizu://banners-changed", &visible)
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    // WebKit can throttle a non-key banner window before the Tauri event listener resumes.
    // This fixed, payload-free wake-up makes the frontend re-read the authorized backend state.
    let _ = window.eval("window.dispatchEvent(new Event('aizu-banner-refresh'))");
    if mode.is_approval() {
        let _ = window.set_focus();
        if needs_user_attention(mode, window.is_focused().ok()) {
            // Informational attention bounces the macOS Dock icon once. Unlike Critical,
            // it cannot outlive an approval that later times out or is disabled.
            let _ = window.request_user_attention(Some(UserAttentionType::Informational));
        }
    }
    if let Some(sound) = app.state::<BannerState>().take_pending_sound(generation)? {
        play_sound(sound);
    }
    Ok(())
}

pub fn banners(app: &AppHandle<Wry>) -> Result<Vec<Notification>, NotifyError> {
    app.state::<BannerState>().presentation_snapshot()
}

pub fn has_approval(app: &AppHandle<Wry>, id: i32) -> Result<bool, NotifyError> {
    Ok(app
        .state::<BannerState>()
        .snapshot()?
        .iter()
        .any(|banner| banner.id == id && banner.approval.is_some()))
}

pub fn update_text_size(app: &AppHandle<Wry>, text_size: TextSize) -> Result<(), NotifyError> {
    if app.state::<BannerState>().update_text_size(text_size)? {
        request_present(app)?;
    }
    Ok(())
}

pub fn update_approval_centering(
    app: &AppHandle<Wry>,
    center_approval_dialogs: bool,
) -> Result<(), NotifyError> {
    if app
        .state::<BannerState>()
        .update_approval_centering(center_approval_dialogs)?
    {
        request_present(app)?;
    }
    Ok(())
}

pub fn update_notification_display(
    app: &AppHandle<Wry>,
    display: NotificationDisplay,
) -> Result<(), NotifyError> {
    if app
        .state::<BannerState>()
        .update_notification_display(display)?
    {
        request_present(app)?;
    }
    Ok(())
}

pub fn dismiss(app: &AppHandle<Wry>, id: i32) -> Result<(), NotifyError> {
    app.state::<BannerState>().dismiss(id)?;
    // Queue removal is the command result. Window presentation is retried independently.
    let _ = request_present(app);
    Ok(())
}

pub fn claim_activation(
    app: &AppHandle<Wry>,
    id: i32,
) -> Result<(BannerActivationClaim, aizu_core::TerminalActivation), NotifyError> {
    app.state::<BannerState>().claim_activation(id)
}

pub fn cancel_activation(
    app: &AppHandle<Wry>,
    claim: &BannerActivationClaim,
) -> Result<(), NotifyError> {
    app.state::<BannerState>().cancel_activation(claim)
}

pub fn complete_activation(
    app: &AppHandle<Wry>,
    claim: &BannerActivationClaim,
) -> Result<(), NotifyError> {
    app.state::<BannerState>().complete_activation(claim)?;
    let _ = request_present(app);
    Ok(())
}

pub fn clear_passive(app: &AppHandle<Wry>) -> Result<(), NotifyError> {
    app.state::<BannerState>().clear_passive()?;
    let _ = request_present(app);
    Ok(())
}

pub fn resize(app: &AppHandle<Wry>, requested_height: f64) -> Result<(), NotifyError> {
    let state = app.state::<BannerState>();
    let mode = presentation_mode(
        &state.snapshot()?,
        state.center_approval_dialogs.load(Ordering::Acquire),
    );
    if !requested_height.is_finite() {
        return Err(NotifyError::Scheduling(
            "Aizu banner height is invalid".to_owned(),
        ));
    }
    let window = app
        .get_webview_window(BANNER_WINDOW)
        .ok_or_else(|| NotifyError::Scheduling("Aizu banner window is unavailable".to_owned()))?;
    let monitor = window
        .current_monitor()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| NotifyError::Scheduling("banner display is unavailable".to_owned()))?;
    resize_window_for_mode(&window, &monitor, requested_height, mode)
}

fn resize_window_for_mode(
    window: &tauri::WebviewWindow<Wry>,
    monitor: &Monitor,
    requested_height: f64,
    mode: PresentationMode,
) -> Result<(), NotifyError> {
    let scale = monitor.scale_factor();
    let work_area = monitor.work_area();
    let logical_position = work_area.position.to_logical::<f64>(scale);
    let logical_size = work_area.size.to_logical::<f64>(scale);
    let geometry = window_geometry(
        mode,
        requested_height,
        WorkAreaGeometry {
            x: logical_position.x,
            y: logical_position.y,
            width: logical_size.width,
            height: logical_size.height,
        },
    );
    window
        .set_size(LogicalSize::new(geometry.width, geometry.height))
        .and_then(|()| window.set_position(LogicalPosition::new(geometry.x, geometry.y)))
        .map_err(|error| NotifyError::Scheduling(error.to_string()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MonitorIdentity {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl From<&Monitor> for MonitorIdentity {
    fn from(monitor: &Monitor) -> Self {
        Self {
            x: monitor.position().x,
            y: monitor.position().y,
            width: monitor.size().width,
            height: monitor.size().height,
        }
    }
}

fn matching_monitor(monitors: &[Monitor], identity: MonitorIdentity) -> Option<Monitor> {
    monitors
        .iter()
        .find(|monitor| MonitorIdentity::from(*monitor) == identity)
        .cloned()
}

fn preferred_monitor_identity(
    monitors: &[MonitorIdentity],
    preferred: Option<MonitorIdentity>,
    primary: Option<MonitorIdentity>,
) -> Option<MonitorIdentity> {
    preferred
        .filter(|identity| monitors.contains(identity))
        .or_else(|| primary.filter(|identity| monitors.contains(identity)))
}

fn notification_display_identity(
    display: NotificationDisplay,
    primary: Option<MonitorIdentity>,
    secondary: Option<MonitorIdentity>,
    focused_window: Option<MonitorIdentity>,
    pointer: Option<MonitorIdentity>,
) -> Option<MonitorIdentity> {
    match display {
        NotificationDisplay::Primary => primary,
        NotificationDisplay::Secondary => secondary,
        NotificationDisplay::FocusedWindow => focused_window,
        NotificationDisplay::Pointer => pointer,
    }
}

fn scaled_monitor_identity(
    origin_x: f64,
    origin_y: f64,
    width: u32,
    height: u32,
    scale: f64,
) -> MonitorIdentity {
    let position: PhysicalPosition<i32> =
        PhysicalPosition::from_logical::<_, f64>((origin_x, origin_y), scale);
    let size: PhysicalSize<u32> =
        PhysicalSize::from_logical::<_, f64>((f64::from(width), f64::from(height)), scale);
    MonitorIdentity {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    }
}

fn configured_monitor(
    window: &tauri::WebviewWindow<Wry>,
    display: NotificationDisplay,
) -> Result<Monitor, NotifyError> {
    let monitors = window
        .available_monitors()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    #[cfg(target_os = "macos")]
    if let Some((preferred, primary)) = macos_monitor_candidates(display) {
        let identities = monitors
            .iter()
            .map(MonitorIdentity::from)
            .collect::<Vec<_>>();
        if let Some(monitor) = preferred_monitor_identity(&identities, preferred, primary)
            .and_then(|identity| matching_monitor(&monitors, identity))
        {
            return Ok(monitor);
        }
    }
    window
        .primary_monitor()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?
        .ok_or_else(|| NotifyError::Scheduling("primary display is unavailable".to_owned()))
}

#[cfg(target_os = "macos")]
fn macos_monitor_candidates(
    display: NotificationDisplay,
) -> Option<(Option<MonitorIdentity>, Option<MonitorIdentity>)> {
    use objc2_foundation::MainThreadMarker;

    let mtm = MainThreadMarker::new()?;
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    let primary = screens
        .iter()
        .next()
        .and_then(|screen| macos_monitor_identity(&screen));
    let secondary = screens
        .iter()
        .skip(1)
        .find_map(|screen| macos_monitor_identity(&screen));
    let focused_window = objc2_app_kit::NSScreen::mainScreen(mtm)
        .as_deref()
        .and_then(macos_monitor_identity);
    let pointer_location = objc2_app_kit::NSEvent::mouseLocation();
    let pointer = screens
        .iter()
        .find(|screen| {
            let frame = screen.frame();
            pointer_location.x >= frame.origin.x
                && pointer_location.x < frame.origin.x + frame.size.width
                && pointer_location.y >= frame.origin.y
                && pointer_location.y < frame.origin.y + frame.size.height
        })
        .and_then(|screen| macos_monitor_identity(&screen));
    let preferred =
        notification_display_identity(display, primary, secondary, focused_window, pointer);
    Some((preferred, primary))
}

#[cfg(target_os = "macos")]
fn macos_monitor_identity(screen: &objc2_app_kit::NSScreen) -> Option<MonitorIdentity> {
    use core_graphics::display::CGDisplay;
    use objc2_foundation::{NSDictionary, NSNumber, ns_string};

    let description = screen.deviceDescription();
    let display_number = NSDictionary::objectForKey(&description, ns_string!("NSScreenNumber"))?;
    let display_id = u32::try_from(display_number.downcast_ref::<NSNumber>()?.as_usize()).ok()?;
    let display = CGDisplay::new(display_id);
    let bounds = display.bounds();
    let scale = screen.backingScaleFactor();
    Some(scaled_monitor_identity(
        bounds.origin.x,
        bounds.origin.y,
        u32::try_from(display.pixels_wide()).ok()?,
        u32::try_from(display.pixels_high()).ok()?,
        scale,
    ))
}

#[cfg(target_os = "macos")]
fn play_sound(sound: NotificationSound) {
    use objc2::AnyThread;
    use std::cell::RefCell;

    const AIZU_POP: &[u8] = include_bytes!("../../../../assets/audio/aizu-pop.wav");
    const AIZU_CHIME: &[u8] = include_bytes!("../../../../assets/audio/aizu-chime.wav");
    const AIZU_PULSE: &[u8] = include_bytes!("../../../../assets/audio/aizu-pulse.wav");
    const AIZU_BLOOM: &[u8] = include_bytes!("../../../../assets/audio/aizu-bloom.wav");

    thread_local! {
        static AIZU_SOUNDS: RefCell<[
            Option<objc2::rc::Retained<objc2_app_kit::NSSound>>;
            4
        ]> = const { RefCell::new([None, None, None, None]) };
    }

    let (index, bytes) = match sound {
        NotificationSound::Default => (0, AIZU_POP),
        NotificationSound::Chime => (1, AIZU_CHIME),
        NotificationSound::Pulse => (2, AIZU_PULSE),
        NotificationSound::Bloom => (3, AIZU_BLOOM),
    };
    AIZU_SOUNDS.with_borrow_mut(|cached| {
        if cached[index].is_none() {
            let data = objc2_foundation::NSData::with_bytes(bytes);
            cached[index] =
                objc2_app_kit::NSSound::initWithData(objc2_app_kit::NSSound::alloc(), &data);
        }
        if let Some(sound) = cached[index].as_ref() {
            let _ = sound.stop();
            let _ = sound.play();
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn play_sound(_sound: NotificationSound) {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::{
        BannerState, MAX_PRESENTATION_ATTEMPTS, MAX_VISIBLE_PASSIVE_BANNERS, MonitorIdentity,
        PresentationMode, WindowGeometry, WorkAreaGeometry, needs_user_attention,
        notification_display_identity, preferred_monitor_identity, retry_delay,
        scaled_monitor_identity, window_geometry,
    };
    use crate::model::Notification;

    fn notification(id: i32, body: &str) -> Notification {
        Notification {
            id,
            title: format!("Notification {id}"),
            body: body.to_owned(),
            sound: None,
            delivery: crate::model::NotificationDelivery::AizuBanner,
            language: crate::model::LanguagePreference::English,
            text_size: crate::model::TextSize::Standard,
            can_activate_terminal: false,
            approval: None,
            activation: None,
        }
    }

    #[test]
    fn queue_is_bounded_and_replaces_matching_identifiers() {
        let state = BannerState::default();
        for id in 1..=i32::try_from(MAX_VISIBLE_PASSIVE_BANNERS).expect("small banner bound") + 1 {
            state
                .push(notification(id, "original"))
                .expect("queue banner");
        }
        state
            .push(notification(3, "replacement"))
            .expect("replace banner");

        let banners = state.snapshot().expect("banner snapshot");
        assert_eq!(banners.len(), MAX_VISIBLE_PASSIVE_BANNERS);
        assert_eq!(banners[0].id, 2);
        assert_eq!(banners[1].body, "replacement");
    }

    #[test]
    fn ordinary_notifications_do_not_evict_a_pending_command_approval() {
        let state = BannerState::default();
        state.push(notification(1, "first")).unwrap();
        state.push(notification(2, "second")).unwrap();
        state.push(notification(3, "third")).unwrap();
        let mut approval = notification(-1, "review command");
        approval.approval = Some(crate::model::ApprovalPresentation {
            agent: crate::model::AgentKind::Codex,
            tool_name: "Bash".to_owned(),
            command: "printf approved".to_owned(),
        });
        state.push(approval).unwrap();

        let snapshot = state.snapshot().unwrap();
        assert_eq!(snapshot.len(), MAX_VISIBLE_PASSIVE_BANNERS + 1);
        assert!(snapshot.iter().any(|banner| banner.id == -1));
        assert!(snapshot.iter().any(|banner| banner.id == 1));
        assert!(snapshot.iter().any(|banner| banner.id == 2));
        assert!(snapshot.iter().any(|banner| banner.id == 3));
        assert_eq!(state.presentation_snapshot().unwrap()[0].id, -1);

        state.dismiss(-1).unwrap();
        let restored = state.presentation_snapshot().unwrap();
        assert_eq!(
            restored.iter().map(|banner| banner.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn clearing_passive_notifications_preserves_command_approvals() {
        let state = BannerState::default();
        state.push(notification(1, "ordinary")).unwrap();
        let mut approval = notification(-1, "review command");
        approval.approval = Some(crate::model::ApprovalPresentation {
            agent: crate::model::AgentKind::Codex,
            tool_name: "Bash".to_owned(),
            command: "printf approved".to_owned(),
        });
        state.push(approval).unwrap();

        state.clear_passive().unwrap();

        let snapshot = state.snapshot().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].id, -1);
        assert!(snapshot[0].approval.is_some());
    }

    #[test]
    fn approval_presentation_temporarily_hides_but_preserves_passive_banners() {
        let state = BannerState::default();
        state.push(notification(1, "ordinary")).unwrap();
        let mut approval = notification(-1, "review command");
        approval.approval = Some(crate::model::ApprovalPresentation {
            agent: crate::model::AgentKind::Codex,
            tool_name: "Bash".to_owned(),
            command: "printf approved".to_owned(),
        });
        state.push(approval).unwrap();

        let queued = state.snapshot().unwrap();
        let presented = state.presentation_snapshot().unwrap();
        assert_eq!(queued.len(), 2);
        assert_eq!(presented.len(), 1);
        assert_eq!(presented[0].id, -1);

        state.dismiss(-1).unwrap();
        let presented = state.presentation_snapshot().unwrap();
        assert_eq!(presented.len(), 1);
        assert_eq!(presented[0].id, 1);
    }

    #[test]
    fn approval_window_is_large_and_centered_while_passive_banners_stay_top_right() {
        let work_area = WorkAreaGeometry {
            x: 100.0,
            y: 50.0,
            width: 1_440.0,
            height: 900.0,
        };

        assert_eq!(
            window_geometry(PresentationMode::Passive, 200.0, work_area),
            WindowGeometry {
                x: 1_104.0,
                y: 66.0,
                width: 420.0,
                height: 200.0,
            }
        );
        assert_eq!(
            window_geometry(PresentationMode::ApprovalCentered, 200.0, work_area),
            WindowGeometry {
                x: 480.0,
                y: 320.0,
                width: 680.0,
                height: 360.0,
            }
        );
        assert_eq!(
            window_geometry(PresentationMode::ApprovalCorner, 200.0, work_area),
            WindowGeometry {
                x: 1_104.0,
                y: 66.0,
                width: 420.0,
                height: 200.0,
            }
        );
    }

    #[test]
    fn configured_display_wins_then_primary_display_is_the_fallback() {
        let primary = MonitorIdentity {
            x: 0,
            y: 0,
            width: 1_920,
            height: 1_080,
        };
        let secondary = MonitorIdentity {
            x: 1_920,
            y: 0,
            width: 2_560,
            height: 1_440,
        };
        let monitors = [primary, secondary];

        assert_eq!(
            preferred_monitor_identity(&monitors, Some(secondary), Some(primary)),
            Some(secondary)
        );
        assert_eq!(
            preferred_monitor_identity(&monitors, None, Some(primary)),
            Some(primary)
        );
        assert_eq!(
            preferred_monitor_identity(
                &monitors,
                Some(MonitorIdentity {
                    x: -1,
                    y: -1,
                    width: 1,
                    height: 1,
                }),
                Some(primary),
            ),
            Some(primary)
        );
    }

    #[test]
    fn every_display_preference_selects_its_matching_monitor_candidate() {
        let primary = MonitorIdentity {
            x: 0,
            y: 0,
            width: 1_920,
            height: 1_080,
        };
        let secondary = MonitorIdentity {
            x: 1_920,
            y: 0,
            width: 2_560,
            height: 1_440,
        };
        let focused = MonitorIdentity {
            x: -1_280,
            y: 0,
            width: 1_280,
            height: 1_024,
        };
        let pointer = MonitorIdentity {
            x: 0,
            y: -900,
            width: 1_440,
            height: 900,
        };

        for (display, expected) in [
            (crate::model::NotificationDisplay::Primary, Some(primary)),
            (
                crate::model::NotificationDisplay::Secondary,
                Some(secondary),
            ),
            (
                crate::model::NotificationDisplay::FocusedWindow,
                Some(focused),
            ),
            (crate::model::NotificationDisplay::Pointer, Some(pointer)),
        ] {
            assert_eq!(
                notification_display_identity(
                    display,
                    Some(primary),
                    Some(secondary),
                    Some(focused),
                    Some(pointer),
                ),
                expected
            );
        }
        assert_eq!(
            notification_display_identity(
                crate::model::NotificationDisplay::Secondary,
                Some(primary),
                None,
                Some(focused),
                Some(pointer),
            ),
            None
        );
    }

    #[test]
    fn appkit_display_geometry_uses_the_same_backing_scale_as_tauri() {
        assert_eq!(
            scaled_monitor_identity(1_512.0, 0.0, 1_512, 982, 2.0),
            MonitorIdentity {
                x: 3_024,
                y: 0,
                width: 3_024,
                height: 1_964,
            }
        );
    }

    #[test]
    fn approval_window_remains_inside_a_small_work_area() {
        assert_eq!(
            window_geometry(
                PresentationMode::ApprovalCentered,
                900.0,
                WorkAreaGeometry {
                    x: 0.0,
                    y: 0.0,
                    width: 600.0,
                    height: 300.0,
                },
            ),
            WindowGeometry {
                x: 16.0,
                y: 16.0,
                width: 568.0,
                height: 268.0,
            }
        );
    }

    #[test]
    fn approval_requests_bounded_attention_when_native_focus_is_not_observed() {
        assert!(!needs_user_attention(
            PresentationMode::ApprovalCentered,
            Some(true)
        ));
        assert!(needs_user_attention(
            PresentationMode::ApprovalCorner,
            Some(false)
        ));
        assert!(needs_user_attention(
            PresentationMode::ApprovalCentered,
            None
        ));
        assert!(!needs_user_attention(
            PresentationMode::Passive,
            Some(false)
        ));
    }

    #[test]
    fn banners_remain_until_individually_dismissed() {
        let state = BannerState::default();
        state.push(notification(1, "first")).expect("first banner");
        state
            .push(notification(2, "second"))
            .expect("second banner");

        assert!(!state.dismiss(1).expect("dismiss first"));
        assert_eq!(state.snapshot().expect("remaining banners").len(), 1);
        assert!(state.dismiss(2).expect("dismiss second"));
    }

    #[test]
    fn frontend_receives_only_activation_availability_not_the_target() {
        let state = BannerState::default();
        let mut banner = notification(1, "ready");
        banner.can_activate_terminal = true;
        banner.activation = Some(aizu_core::TerminalActivation {
            application: aizu_core::TerminalApplication::Iterm2,
            application_session: Some("w0t0p0:ABCD".to_owned()),
            tmux: None,
        });
        state.push(banner).expect("queue actionable banner");

        let (claim, target) = state.claim_activation(1).expect("activation claim");
        assert_eq!(target.application, aizu_core::TerminalApplication::Iterm2);
        state.cancel_activation(&claim).expect("cancel claim");
        let serialized = serde_json::to_value(state.snapshot().expect("snapshot"))
            .expect("serialize frontend snapshot");
        assert_eq!(serialized[0]["canActivateTerminal"], true);
        assert!(serialized[0].get("activation").is_none());
        assert!(!serialized.to_string().contains("w0t0p0"));
    }

    #[test]
    fn activation_is_claimed_once_and_failure_keeps_the_banner() {
        let state = BannerState::default();
        let mut banner = notification(1, "ready");
        banner.activation = Some(aizu_core::TerminalActivation {
            application: aizu_core::TerminalApplication::Iterm2,
            application_session: Some("w0t0p0:ABCD".to_owned()),
            tmux: None,
        });
        state.push(banner).expect("queue actionable banner");

        let (claim, _) = state.claim_activation(1).expect("first claim");
        assert!(state.claim_activation(1).is_err());
        state
            .cancel_activation(&claim)
            .expect("cancel failed action");
        assert_eq!(state.snapshot().expect("snapshot").len(), 1);

        let (claim, _) = state.claim_activation(1).expect("retry claim");
        assert!(state.complete_activation(&claim).expect("complete action"));
        assert!(state.snapshot().expect("snapshot").is_empty());
    }

    #[test]
    fn dismiss_invalidates_an_in_flight_presentation() {
        let state = BannerState::default();
        let now = Instant::now();
        state.push(notification(1, "first")).expect("queue banner");
        let stale_generation = state.begin_presentation(now).expect("presentation");

        assert!(state.dismiss(1).expect("dismiss banner"));
        assert_ne!(state.generation.load(Ordering::Acquire), stale_generation);
        state.finish_presentation(stale_generation, true, now);

        let current_generation = state.begin_presentation(now).expect("empty presentation");
        assert_ne!(current_generation, stale_generation);
        assert!(state.snapshot().expect("banner snapshot").is_empty());
    }

    #[test]
    fn dismissing_an_older_banner_preserves_the_newer_pending_sound() {
        let state = BannerState::default();
        let now = Instant::now();
        state.push(notification(1, "first")).expect("first banner");
        let mut second = notification(2, "second");
        second.sound = Some(crate::model::NotificationSound::Default);
        state.push(second).expect("second banner");

        assert!(!state.dismiss(1).expect("dismiss first"));
        let generation = state.begin_presentation(now).expect("presentation");
        assert_eq!(
            state.take_pending_sound(generation).expect("pending sound"),
            Some(crate::model::NotificationSound::Default)
        );
    }

    #[test]
    fn failed_or_coalesced_presentation_remains_retryable() {
        let state = BannerState::default();
        let start = Instant::now();
        state.push(notification(1, "ready")).expect("queue banner");

        let generation = state
            .begin_presentation(start)
            .expect("initial presentation");
        assert!(state.begin_presentation(start).is_none());
        state.finish_presentation(generation, false, start);
        assert!(state.dirty.load(Ordering::Acquire));
        assert!(state.begin_presentation(start).is_none());
        let retry_at = start + retry_delay(1);
        let generation = state
            .begin_presentation(retry_at)
            .expect("retry presentation");
        state.finish_presentation(generation, true, retry_at);
        assert!(!state.dirty.load(Ordering::Acquire));
    }

    #[test]
    fn persistent_presentation_failure_stops_after_a_bounded_attempt_count() {
        let state = BannerState::default();
        let mut now = Instant::now();
        let mut audible = notification(1, "ready");
        audible.sound = Some(crate::model::NotificationSound::Default);
        state.push(audible).expect("queue banner");

        for attempt in 1..=MAX_PRESENTATION_ATTEMPTS {
            let generation = state.begin_presentation(now).expect("presentation attempt");
            state.finish_presentation(generation, false, now);
            if attempt < MAX_PRESENTATION_ATTEMPTS {
                now += retry_delay(attempt);
            }
        }
        assert!(!state.dirty.load(Ordering::Acquire));
        assert!(
            state
                .begin_presentation(now + Duration::from_mins(1))
                .is_none()
        );

        state
            .push(notification(2, "new event"))
            .expect("new banner");
        let generation = state
            .begin_presentation(now + Duration::from_mins(1))
            .expect("new notification presentation");
        assert_eq!(
            state
                .take_pending_sound(generation)
                .expect("silent notification"),
            None
        );
    }

    #[test]
    fn stale_presentation_cannot_consume_a_new_notification_sound() {
        let state = BannerState::default();
        let now = Instant::now();
        let mut first = notification(1, "first");
        first.sound = Some(crate::model::NotificationSound::Default);
        state.push(first).expect("first notification");
        let first_generation = state.begin_presentation(now).expect("first presentation");

        state
            .push(notification(2, "silent replacement"))
            .expect("second notification");
        state.finish_presentation(first_generation, true, now);

        assert!(state.dirty.load(Ordering::Acquire));
        let second_generation = state.begin_presentation(now).expect("second presentation");
        assert_ne!(first_generation, second_generation);
        assert_eq!(
            state
                .take_pending_sound(second_generation)
                .expect("second sound"),
            None
        );
    }

    #[test]
    fn clearing_banners_invalidates_in_flight_sound_and_retry() {
        let state = BannerState::default();
        let now = Instant::now();
        let mut audible = notification(1, "audible");
        audible.sound = Some(crate::model::NotificationSound::Default);
        state.push(audible).expect("notification");
        let generation = state.begin_presentation(now).expect("presentation");

        state.clear_pending().expect("clear pending state");
        state.finish_presentation(generation, false, now);

        assert!(!state.dirty.load(Ordering::Acquire));
        assert_eq!(
            state.take_pending_sound(generation).expect("cleared sound"),
            None
        );
        assert!(
            state
                .begin_presentation(now + Duration::from_mins(1))
                .is_none()
        );
    }

    #[test]
    fn text_size_refreshes_visible_banners_without_replaying_sound() {
        let state = BannerState::default();
        let now = Instant::now();
        let mut audible = notification(1, "visible");
        audible.sound = Some(crate::model::NotificationSound::Default);
        state.push(audible).expect("notification");
        let visible_generation = state.begin_presentation(now).expect("presentation");
        assert_eq!(
            state
                .take_pending_sound(visible_generation)
                .expect("initial sound"),
            Some(crate::model::NotificationSound::Default)
        );
        state.finish_presentation(visible_generation, true, now);

        assert!(
            state
                .update_text_size(crate::model::TextSize::Large)
                .expect("update text size")
        );
        let refreshed_generation = state.begin_presentation(now).expect("refresh presentation");
        assert_ne!(visible_generation, refreshed_generation);
        assert_eq!(
            state.snapshot().expect("banner snapshot")[0].text_size,
            crate::model::TextSize::Large
        );
        assert_eq!(
            state
                .take_pending_sound(refreshed_generation)
                .expect("refresh sound"),
            None
        );
    }

    #[test]
    fn text_size_invalidates_an_in_flight_banner_presentation() {
        let state = BannerState::default();
        let now = Instant::now();
        state
            .push(notification(1, "visible"))
            .expect("notification");
        let stale_generation = state.begin_presentation(now).expect("presentation");

        assert!(
            state
                .update_text_size(crate::model::TextSize::Large)
                .expect("update text size")
        );
        state.finish_presentation(stale_generation, true, now);

        let refreshed_generation = state.begin_presentation(now).expect("refresh presentation");
        assert_ne!(stale_generation, refreshed_generation);
    }

    #[test]
    fn approval_centering_refreshes_an_existing_dialog_without_replaying_sound() {
        let state = BannerState::default();
        let now = Instant::now();
        let mut approval = notification(-1, "review command");
        approval.approval = Some(crate::model::ApprovalPresentation {
            agent: crate::model::AgentKind::Codex,
            tool_name: "Bash".to_owned(),
            command: "printf approved".to_owned(),
        });
        approval.sound = Some(crate::model::NotificationSound::Default);
        state.push(approval).expect("approval");
        let visible_generation = state.begin_presentation(now).expect("presentation");
        assert_eq!(
            state
                .take_pending_sound(visible_generation)
                .expect("initial sound"),
            Some(crate::model::NotificationSound::Default)
        );
        state.finish_presentation(visible_generation, true, now);

        assert!(
            state
                .update_approval_centering(false)
                .expect("disable centering")
        );
        let refreshed_generation = state.begin_presentation(now).expect("refresh presentation");
        assert_ne!(visible_generation, refreshed_generation);
        assert_eq!(
            state
                .take_pending_sound(refreshed_generation)
                .expect("refresh sound"),
            None
        );
        assert_eq!(
            super::presentation_mode(
                &state.snapshot().expect("snapshot"),
                state.center_approval_dialogs.load(Ordering::Acquire),
            ),
            PresentationMode::ApprovalCorner
        );
    }

    #[test]
    fn display_setting_refreshes_existing_banners_without_replaying_sound() {
        let state = BannerState::default();
        let mut banner = notification(42, "move this banner");
        banner.sound = Some(crate::model::NotificationSound::Default);
        state.push(banner).expect("queue banner");
        let now = Instant::now();
        let visible_generation = state.begin_presentation(now).expect("initial presentation");
        assert_eq!(
            state
                .take_pending_sound(visible_generation)
                .expect("initial sound"),
            Some(crate::model::NotificationSound::Default)
        );
        state.finish_presentation(visible_generation, true, now);

        assert!(
            state
                .update_notification_display(crate::model::NotificationDisplay::Secondary)
                .expect("change display")
        );
        assert_eq!(
            state.notification_display(),
            crate::model::NotificationDisplay::Secondary
        );
        let refreshed_generation = state.begin_presentation(now).expect("refresh presentation");
        assert_ne!(visible_generation, refreshed_generation);
        assert_eq!(
            state
                .take_pending_sound(refreshed_generation)
                .expect("refresh sound"),
            None
        );
    }
}
