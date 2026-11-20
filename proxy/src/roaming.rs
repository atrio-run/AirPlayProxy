//! Moves the station to a stronger access point of its network while its signal is weak.
//!
//! The driver chooses the strongest access point when it connects, and keeps the one it got for
//! as long as the connection lasts: a station whose strongest access point refused it once stays
//! on a distant one. The audio is 1.4 Mbit/s over a radio that Bluetooth shares, and a distant
//! access point does not carry it. The driver reports once that the signal fell below
//! [`WEAK_RSSI`] (`WIFI_EVENT_STA_BSS_RSSI_LOW`). The station then scans for its network and, when
//! another access point is at least [`MARGIN_DB`] stronger, disconnects: the reconnection that
//! follows connects to the strongest one. No event reports an access point that comes back, so
//! while the signal stays weak the station scans again after intervals that double up to
//! [`MAX_INTERVAL_SECS`]. A scan takes the radio off the access point's channel for two seconds or
//! so, which would break the sound: no scan runs while the source plays.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use esp_idf_svc::sys::{
    esp_err_t, esp_timer_create, esp_timer_create_args_t, esp_timer_delete,
    esp_timer_dispatch_t_ESP_TIMER_TASK, esp_timer_handle_t, esp_timer_start_once, esp_timer_stop,
    esp_wifi_clear_ap_list, esp_wifi_disconnect, esp_wifi_scan_get_ap_record, esp_wifi_scan_start,
    esp_wifi_set_rssi_threshold, esp_wifi_sta_get_ap_info, wifi_ap_record_t, wifi_scan_config_t,
    wifi_scan_type_t_WIFI_SCAN_TYPE_ACTIVE, ESP_FAIL, ESP_OK,
};
use esp_idf_svc::wifi::WifiEvent;
use log::{info, warn};

/// The signal, in dBm, below which the station looks for a stronger access point.
const WEAK_RSSI: i8 = -60;
/// The signal, in dBm, at which a weak one counts as recovered.
const RECOVERED_RSSI: i8 = -55;
/// How much stronger, in dB, another access point must be for the station to move to it.
const MARGIN_DB: i16 = 10;
const FIRST_INTERVAL_SECS: u32 = 60;
const MAX_INTERVAL_SECS: u32 = 600;
/// The doublings after which the interval stays at [`MAX_INTERVAL_SECS`].
const MAX_DOUBLINGS: u32 = 4;

/// The roaming state, which the system event task, the timer task and the relay share: flags and
/// the number of doublings of the interval in one atomic, changed by compare and swap.
static STATE: AtomicU32 = AtomicU32::new(0);
const CONNECTED: u32 = 1;
const PLAYING: u32 = 1 << 1;
/// The signal is weak: the station scans until it moves or the signal recovers.
const WEAK: u32 = 1 << 2;
const SCANNING: u32 = 1 << 3;
/// The station disconnected to move to a stronger access point.
const MOVING: u32 = 1 << 4;
const DOUBLINGS_SHIFT: u32 = 8;
const DOUBLINGS: u32 = 0xff << DOUBLINGS_SHIFT;

/// The timer of the next scan, created when the signal first stays weak; zero until then.
static TIMER: AtomicUsize = AtomicUsize::new(0);

/// Changes the state and returns the one before.
fn update(change: impl Fn(u32) -> u32) -> u32 {
    STATE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
            Some(change(state))
        })
        .unwrap_or_else(|state| state)
}

/// Whether a scan may start in `state`: the station is connected with a weak signal, the source
/// does not play and no scan runs.
fn may_scan(state: u32) -> bool {
    state & (CONNECTED | WEAK | PLAYING | SCANNING) == CONNECTED | WEAK
}

/// Follows the driver's events, which the system event task delivers.
pub fn handle(event: &WifiEvent) {
    match event {
        WifiEvent::StaConnected(_) => connected(),
        WifiEvent::StaDisconnected(_) => disconnected(),
        WifiEvent::StaBssRssiLow => weak(),
        WifiEvent::ScanDone(_) => scan_done(),
        _ => {}
    }
}

/// Tells whether the source plays. While it does, no scan runs; once it stops, the driver is
/// asked again to report a weak signal.
pub fn playing(playing: bool) {
    if playing {
        update(|state| state | PLAYING);
        stop_timer();
        return;
    }
    let before = update(|state| state & !(PLAYING | WEAK | DOUBLINGS));
    if before & CONNECTED != 0 {
        watch();
    }
}

/// Whether the source plays, as it was last told.
pub fn source_plays() -> bool {
    STATE.load(Ordering::Relaxed) & PLAYING != 0
}

fn connected() {
    let still_weak = current_access_point().is_some_and(|(_, rssi)| rssi < WEAK_RSSI);
    let before = update(|state| (state | CONNECTED) & !MOVING);
    if before & MOVING != 0 && still_weak {
        // The move reached no stronger access point: the station looks again after the interval,
        // which keeps doubling.
        update(|state| state | WEAK);
        schedule();
        return;
    }
    update(|state| state & !(WEAK | DOUBLINGS));
    watch();
}

fn disconnected() {
    update(|state| state & !(CONNECTED | WEAK));
    stop_timer();
}

fn weak() {
    let before = update(|state| {
        if state & (CONNECTED | WEAK | PLAYING) == CONNECTED {
            (state | WEAK) & !DOUBLINGS
        } else {
            state
        }
    });
    let rssi = current_access_point().map_or(0, |(_, rssi)| rssi);
    match before & (CONNECTED | WEAK | PLAYING) {
        CONNECTED => {
            info!("Wi-Fi: weak signal ({rssi} dBm), looking for a stronger access point");
            scan();
        }
        state if state & PLAYING != 0 => {
            info!("Wi-Fi: weak signal ({rssi} dBm); a stronger access point is looked for once the source stops");
        }
        _ => {}
    }
}

/// Asks the driver to report a weak signal, as it does once.
fn watch() {
    let result = unsafe { esp_wifi_set_rssi_threshold(i32::from(WEAK_RSSI)) };
    if result != ESP_OK {
        warn!("Wi-Fi weak signal threshold could not be set: {result}");
    }
}

fn scan() {
    let before = update(|state| {
        if may_scan(state) {
            state | SCANNING
        } else {
            state
        }
    });
    if !may_scan(before) {
        return;
    }
    let result = start_scan();
    if result != ESP_OK {
        update(|state| state & !SCANNING);
        warn!("Wi-Fi scan for a stronger access point could not start: {result}");
        schedule();
    }
}

/// Scans for the network of the current access point, for the time on each channel that the
/// driver chooses: with Bluetooth running it accepts no other. The driver copies the
/// configuration when the scan starts.
#[inline(never)]
fn start_scan() -> esp_err_t {
    // Plain C structures; zero is the default of every field that is not set here.
    let mut current: wifi_ap_record_t = unsafe { core::mem::zeroed() };
    if unsafe { esp_wifi_sta_get_ap_info(&mut current) } != ESP_OK {
        return ESP_FAIL;
    }
    let mut config: wifi_scan_config_t = unsafe { core::mem::zeroed() };
    config.ssid = current.ssid.as_mut_ptr();
    config.scan_type = wifi_scan_type_t_WIFI_SCAN_TYPE_ACTIVE;
    unsafe { esp_wifi_scan_start(&config, false) }
}

fn scan_done() {
    let before = update(|state| state & !SCANNING);
    if before & SCANNING == 0 {
        return;
    }
    if before & (CONNECTED | WEAK | PLAYING) != CONNECTED | WEAK {
        unsafe { esp_wifi_clear_ap_list() };
        return;
    }
    let Some(survey) = survey() else {
        return;
    };
    let current = survey.current_rssi;
    if current >= RECOVERED_RSSI {
        info!("Wi-Fi: signal recovered ({current} dBm)");
        update(|state| state & !(WEAK | DOUBLINGS));
        watch();
        return;
    }
    match survey.strongest {
        Some(strongest) if i16::from(strongest.rssi) >= i16::from(current) + MARGIN_DB => {
            info!(
                "Wi-Fi: moving from {} ({current} dBm) to {} ({} dBm, channel {})",
                format_bssid(&survey.current_bssid),
                format_bssid(&strongest.bssid),
                strongest.rssi,
                strongest.channel
            );
            update(|state| state | MOVING);
            let result = unsafe { esp_wifi_disconnect() };
            if result != ESP_OK {
                update(|state| state & !MOVING);
                warn!("Wi-Fi disconnection for a stronger access point failed: {result}");
                schedule();
            }
        }
        strongest => {
            let interval_secs = schedule();
            info!(
                "Wi-Fi: no access point {MARGIN_DB} dB stronger than this one ({current} dBm, strongest other {}); looking again in {interval_secs} s",
                strongest.map_or_else(|| "none".to_string(), |other| format!("{} dBm", other.rssi))
            );
        }
    }
}

struct AccessPoint {
    bssid: [u8; 6],
    channel: u8,
    rssi: i8,
}

struct Survey {
    current_bssid: [u8; 6],
    current_rssi: i8,
    strongest: Option<AccessPoint>,
}

/// The current access point and the strongest other one of the network in the last scan, whose
/// list this frees. The scan records stay out of the caller's frame, which logs, on the 2.3 KB
/// stack of the system event task.
#[inline(never)]
fn survey() -> Option<Survey> {
    let Some((current_bssid, mut current_rssi)) = current_access_point() else {
        unsafe { esp_wifi_clear_ap_list() };
        return None;
    };
    let mut strongest: Option<AccessPoint> = None;
    // A plain C record that the driver fills in.
    let mut record: wifi_ap_record_t = unsafe { core::mem::zeroed() };
    while unsafe { esp_wifi_scan_get_ap_record(&mut record) } == ESP_OK {
        if record.bssid == current_bssid {
            current_rssi = record.rssi;
        } else if strongest
            .as_ref()
            .is_none_or(|strongest| record.rssi > strongest.rssi)
        {
            strongest = Some(AccessPoint {
                bssid: record.bssid,
                channel: record.primary,
                rssi: record.rssi,
            });
        }
    }
    unsafe { esp_wifi_clear_ap_list() };
    Some(Survey {
        current_bssid,
        current_rssi,
        strongest,
    })
}

#[inline(never)]
fn current_access_point() -> Option<([u8; 6], i8)> {
    // A plain C record that the driver fills in.
    let mut record: wifi_ap_record_t = unsafe { core::mem::zeroed() };
    (unsafe { esp_wifi_sta_get_ap_info(&mut record) } == ESP_OK)
        .then_some((record.bssid, record.rssi))
}

fn format_bssid(bssid: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bssid[0], bssid[1], bssid[2], bssid[3], bssid[4], bssid[5]
    )
}

/// Scans again after the interval, which doubles for the next scan, and returns the interval.
fn schedule() -> u32 {
    let before = update(|state| {
        let doublings = (((state & DOUBLINGS) >> DOUBLINGS_SHIFT) + 1).min(MAX_DOUBLINGS);
        (state & !DOUBLINGS) | (doublings << DOUBLINGS_SHIFT)
    });
    let doublings = (before & DOUBLINGS) >> DOUBLINGS_SHIFT;
    let interval_secs = (FIRST_INTERVAL_SECS << doublings).min(MAX_INTERVAL_SECS);
    if let Some(timer) = timer() {
        unsafe {
            esp_timer_stop(timer);
            esp_timer_start_once(timer, u64::from(interval_secs) * 1_000_000);
        }
    }
    interval_secs
}

fn stop_timer() {
    let timer = TIMER.load(Ordering::Acquire);
    if timer != 0 {
        unsafe { esp_timer_stop(timer as esp_timer_handle_t) };
    }
}

extern "C" fn scan_again(_argument: *mut core::ffi::c_void) {
    scan();
}

fn timer() -> Option<esp_timer_handle_t> {
    let existing = TIMER.load(Ordering::Acquire);
    if existing != 0 {
        return Some(existing as esp_timer_handle_t);
    }
    let arguments = esp_timer_create_args_t {
        callback: Some(scan_again),
        arg: core::ptr::null_mut(),
        dispatch_method: esp_timer_dispatch_t_ESP_TIMER_TASK,
        name: c"wifi-roaming".as_ptr(),
        skip_unhandled_events: true,
    };
    let mut timer: esp_timer_handle_t = core::ptr::null_mut();
    if unsafe { esp_timer_create(&arguments, &mut timer) } != ESP_OK {
        warn!("Wi-Fi roaming timer could not be created; no scan follows until the signal weakens again");
        return None;
    }
    // The system event task and the timer task may both create it.
    match TIMER.compare_exchange(0, timer as usize, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some(timer),
        Err(existing) => {
            unsafe { esp_timer_delete(timer) };
            Some(existing as esp_timer_handle_t)
        }
    }
}
