use std::{collections::VecDeque, sync::Mutex};

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

#[derive(Default)]
pub struct BannerState(Mutex<VecDeque<Notification>>);

impl BannerState {
    fn push(&self, notification: Notification) -> Result<(), NotifyError> {
        let mut banners = self
            .0
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        if let Some(existing) = banners
            .iter_mut()
            .find(|existing| existing.id == notification.id)
        {
            *existing = notification;
            return Ok(());
        }
        if banners.len() == MAX_VISIBLE_BANNERS {
            banners.pop_front();
        }
        banners.push_back(notification);
        Ok(())
    }

    fn dismiss(&self, id: i32) -> Result<bool, NotifyError> {
        let mut banners = self
            .0
            .lock()
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))?;
        banners.retain(|banner| banner.id != id);
        Ok(banners.is_empty())
    }

    fn snapshot(&self) -> Result<Vec<Notification>, NotifyError> {
        self.0
            .lock()
            .map(|banners| banners.iter().cloned().collect())
            .map_err(|_| NotifyError::Scheduling("Aizu banner state is unavailable".to_owned()))
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
    let app = app.clone();
    let main_thread_app = app.clone();
    let sound = notification.sound;
    app.run_on_main_thread(move || {
        let _ = present(&main_thread_app, sound);
    })
    .map_err(|error| NotifyError::Scheduling(error.to_string()))
}

fn present(app: &AppHandle<Wry>, sound: Option<NotificationSound>) -> Result<(), NotifyError> {
    let window = ensure_window(app)?;
    resize(app, MIN_BANNER_HEIGHT)?;
    window
        .emit("aizu://banners-changed", ())
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    window
        .show()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    if let Some(sound) = sound {
        play_sound(sound);
    }
    Ok(())
}

pub fn banners(app: &AppHandle<Wry>) -> Result<Vec<Notification>, NotifyError> {
    app.state::<BannerState>().snapshot()
}

pub fn dismiss(app: &AppHandle<Wry>, id: i32) -> Result<(), NotifyError> {
    let empty = app.state::<BannerState>().dismiss(id)?;
    let window = app
        .get_webview_window(BANNER_WINDOW)
        .ok_or_else(|| NotifyError::Scheduling("Aizu banner window is unavailable".to_owned()))?;
    window
        .emit("aizu://banners-changed", ())
        .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    if empty {
        window
            .hide()
            .map_err(|error| NotifyError::Scheduling(error.to_string()))?;
    }
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
    use super::{BannerState, MAX_VISIBLE_BANNERS};
    use crate::model::Notification;

    fn notification(id: i32, body: &str) -> Notification {
        Notification {
            id,
            title: format!("Notification {id}"),
            body: body.to_owned(),
            sound: None,
            delivery: crate::model::NotificationDelivery::AizuBanner,
            language: crate::model::LanguagePreference::English,
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
}
