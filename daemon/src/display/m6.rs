/// Display module for M6, light LEDs on the front of the device.
/// DisplayState::Recording => Signal Green LED is solid.
/// DisplayState::Paused => Signal Green and Red LED is off.
/// DisplayState::WarningDetected => Signal LED is solid red.
use log::{error, info};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use std::time::Duration;

use crate::config::{self, UiLevel};
use crate::display::DisplayState;

macro_rules! led {
    ($l:expr) => {{ format!("/sys/class/leds/{}/brightness", $l) }};
}

async fn led_on(path: String) {
    tokio::fs::write(&path, "1").await.ok();
}

async fn led_off(path: String) {
    tokio::fs::write(&path, "0").await.ok();
}

pub fn update_ui(
    task_tracker: &TaskTracker,
    config: &config::Config,
    shutdown_token: CancellationToken,
    mut ui_update_rx: mpsc::Receiver<DisplayState>,
) {
    let mut invisible: bool = false;
    if config.ui_level == UiLevel::Invisible {
        info!("Invisible mode, not spawning UI.");
        invisible = true;
    }
    task_tracker.spawn(async move {
        let mut state = DisplayState::Recording;
        let mut last_state = DisplayState::Paused;
        let mut last_update = std::time::Instant::now();

        loop {
            if shutdown_token.is_cancelled() {
                info!("received UI shutdown");
                break;
            }
            match ui_update_rx.try_recv() {
                Ok(new_state) => state = new_state,
                Err(mpsc::error::TryRecvError::Empty) => {}
                Err(e) => error!("error receiving ui update message: {e}"),
            };

            // Update LEDs if state changed or if 5 seconds have passed since last update
            let now = std::time::Instant::now();
            let should_update = !invisible
                && (state != last_state
                    || now.duration_since(last_update) >= Duration::from_secs(5));

            if should_update {
                match state {
                    DisplayState::Paused => {
                        led_off(led!("4g_1")).await;
                        led_off(led!("4g_type")).await;
                    }
                    DisplayState::Recording => {
                        led_off(led!("4g_type")).await;
                        led_on(led!("4g_1")).await;
                    }
                    DisplayState::WarningDetected { .. } => {
                        led_off(led!("4g_1")).await;
                        led_on(led!("4g_type")).await;
                    }
                }
                last_state = state;
                last_update = now;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
