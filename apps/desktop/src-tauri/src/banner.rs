use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tauri::{
    App, AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl,
    WebviewWindowBuilder, Wry,
};

use crate::{
    model::{Notification, NotificationSound},
    notifier::NotifyError,
};

const BANNER_WINDOW: &str = "banner";
const MAX_VISIBLE_BANNERS: usize = 3;
const BANNER_WIDTH: f64 = 420.0;
const MIN_BANNER_HEIGHT: f64 = 104.0;
const MAX_BANNER_HEIGHT: f64 = 720.0;
const SCREEN_MARGIN: f64 = 16.0;
const MAX_PRESENTATION_ATTEMPTS: u8 = 5;

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

#[derive(Default)]
pub struct BannerState {
    banners: Mutex<VecDeque<Notification>>,
    pending_sound: Mutex<Option<PendingSound>>,
    retry: Mutex<PresentationRetry>,
    generation: AtomicU64,
    dirty: AtomicBool,
    presentation_scheduled: AtomicBool,
}

impl BannerState {
    fn push(&self, notification: Notification) -> Result<(), NotifyError> {
        let mut banners = self
            .banners
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
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
            *existing = notification;
            self.dirty.store(true, Ordering::Release);
            return Ok(());
        }
        if banners.len() == MAX_VISIBLE_BANNERS {
            banners.pop_front();
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

fn retry_delay(attempts: u8) -> Duration {
    match attempts {
        0 | 1 => Duration::from_millis(250),
        2 => Duration::from_secs(1),
        3 => Duration::from_secs(4),
        _ => Duration::from_secs(15),
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
    request_present(app)
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
    resize(app, MIN_BANNER_HEIGHT)?;
    window
        .emit("aizu://banners-changed", ())
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    window
        .show()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    if let Some(sound) = app.state::<BannerState>().take_pending_sound(generation)? {
        play_sound(sound);
    }
    Ok(())
}

pub fn banners(app: &AppHandle<Wry>) -> Result<Vec<Notification>, NotifyError> {
    app.state::<BannerState>().snapshot()
}

pub fn dismiss(app: &AppHandle<Wry>, id: i32) -> Result<(), NotifyError> {
    app.state::<BannerState>().dismiss(id)?;
    // Queue removal is the command result. Window presentation is retried independently.
    let _ = request_present(app);
    Ok(())
}

pub fn clear(app: &AppHandle<Wry>) -> Result<(), NotifyError> {
    let ids = app
        .state::<BannerState>()
        .snapshot()?
        .into_iter()
        .map(|banner| banner.id)
        .collect::<Vec<_>>();
    for id in ids {
        app.state::<BannerState>().dismiss(id)?;
    }
    app.state::<BannerState>().clear_pending()?;
    if let Some(window) = app.get_webview_window(BANNER_WINDOW) {
        window
            .hide()
            .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    }
    Ok(())
}

pub fn resize(app: &AppHandle<Wry>, requested_height: f64) -> Result<(), NotifyError> {
    if !requested_height.is_finite() {
        return Err(NotifyError::Scheduling(
            "Aizu banner height is invalid".to_owned(),
        ));
    }
    let window = app
        .get_webview_window(BANNER_WINDOW)
        .ok_or_else(|| NotifyError::Scheduling("Aizu banner window is unavailable".to_owned()))?;
    let monitor = window
        .primary_monitor()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?
        .ok_or_else(|| NotifyError::Scheduling("primary display is unavailable".to_owned()))?;
    let scale = monitor.scale_factor();
    let work_area = monitor.work_area();
    let logical_position = work_area.position.to_logical::<f64>(scale);
    let logical_size = work_area.size.to_logical::<f64>(scale);
    let max_height =
        (logical_size.height - SCREEN_MARGIN * 2.0).clamp(MIN_BANNER_HEIGHT, MAX_BANNER_HEIGHT);
    let height = requested_height.clamp(MIN_BANNER_HEIGHT, max_height);
    let x = logical_position.x + logical_size.width - BANNER_WIDTH - SCREEN_MARGIN;
    let y = logical_position.y + SCREEN_MARGIN;
    window
        .set_size(LogicalSize::new(BANNER_WIDTH, height))
        .and_then(|()| window.set_position(LogicalPosition::new(x, y)))
        .map_err(|error| NotifyError::Scheduling(error.to_string()))
}

#[cfg(target_os = "macos")]
fn play_sound(sound: NotificationSound) {
    use objc2::AnyThread;
    use std::cell::RefCell;

    const AIZU_POP: &[u8] = include_bytes!("../../../../assets/audio/aizu-pop.wav");

    thread_local! {
        static AIZU_POP_SOUND: RefCell<Option<objc2::rc::Retained<objc2_app_kit::NSSound>>> =
            const { RefCell::new(None) };
    }

    if sound == NotificationSound::Default {
        AIZU_POP_SOUND.with_borrow_mut(|cached| {
            if cached.is_none() {
                let data = objc2_foundation::NSData::with_bytes(AIZU_POP);
                *cached =
                    objc2_app_kit::NSSound::initWithData(objc2_app_kit::NSSound::alloc(), &data);
            }
            if let Some(sound) = cached.as_ref() {
                let _ = sound.stop();
                let _ = sound.play();
            }
        });
        return;
    }

    let name = match sound {
        NotificationSound::Default => unreachable!("Aizu Pop handled above"),
        NotificationSound::Glass => "Glass",
        NotificationSound::Ping => "Ping",
        NotificationSound::Pop => "Pop",
        NotificationSound::Hero => "Hero",
    }
    .to_owned();
    let name = objc2_foundation::NSString::from_str(&name);
    if let Some(sound) = objc2_app_kit::NSSound::soundNamed(&name) {
        let _ = sound.play();
    }
}

#[cfg(not(target_os = "macos"))]
fn play_sound(_sound: NotificationSound) {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::{BannerState, MAX_PRESENTATION_ATTEMPTS, MAX_VISIBLE_BANNERS, retry_delay};
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
        }
    }

    #[test]
    fn queue_is_bounded_and_replaces_matching_identifiers() {
        let state = BannerState::default();
        for id in 1..=i32::try_from(MAX_VISIBLE_BANNERS).expect("small banner bound") + 1 {
            state
                .push(notification(id, "original"))
                .expect("queue banner");
        }
        state
            .push(notification(3, "replacement"))
            .expect("replace banner");

        let banners = state.snapshot().expect("banner snapshot");
        assert_eq!(banners.len(), MAX_VISIBLE_BANNERS);
        assert_eq!(banners[0].id, 2);
        assert_eq!(banners[1].body, "replacement");
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
}
