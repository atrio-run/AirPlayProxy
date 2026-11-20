//! The volume the Bluetooth source asks for.
//!
//! A source that finds a speaker with absolute volume (AVRCP) leaves the audio at full scale and
//! tells the speaker how loud to play it. The proxy passes that on to the AirPlay receiver.

use core::sync::atomic::{AtomicU8, Ordering};

use esp_idf_svc::sys::{self, esp, EspError};
use log::{info, warn};

/// The loudest volume a source asks for.
pub const LOUDEST: u8 = 127;
const UNSET: u8 = u8::MAX;
/// The volume the source is told the speaker has until it sets one: the receiver's own is not
/// known.
const ASSUMED: u8 = LOUDEST / 2;

static ASKED: AtomicU8 = AtomicU8::new(UNSET);

/// Starts answering the volume commands of the source. To be called before A2DP starts.
pub fn listen() -> Result<(), EspError> {
    esp!(unsafe { sys::esp_avrc_tg_init() })?;
    esp!(unsafe { sys::esp_avrc_tg_register_callback(Some(on_event)) })?;

    let mut notified = sys::esp_avrc_rn_evt_cap_mask_t { bits: 0 };
    unsafe {
        sys::esp_avrc_rn_evt_bit_mask_operation(
            sys::esp_avrc_bit_mask_op_t_ESP_AVRC_BIT_MASK_OP_SET,
            &mut notified,
            sys::esp_avrc_rn_event_ids_t_ESP_AVRC_RN_VOLUME_CHANGE,
        );
    }
    esp!(unsafe { sys::esp_avrc_tg_set_rn_evt_cap(&notified) })
}

/// The volume the source asked for, from 0 to `LOUDEST`, once it has asked for one.
pub fn asked() -> Option<u8> {
    match ASKED.load(Ordering::Relaxed) {
        UNSET => None,
        volume => Some(volume),
    }
}

/// The AirPlay volume for a volume of the source: from -30 to 0 dB, or -144 dB for silence.
pub fn decibel(volume: u8) -> f32 {
    if volume == 0 {
        -144.0
    } else {
        30.0 * f32::from(volume.min(LOUDEST)) / f32::from(LOUDEST) - 30.0
    }
}

unsafe extern "C" fn on_event(
    event: sys::esp_avrc_tg_cb_event_t,
    parameters: *mut sys::esp_avrc_tg_cb_param_t,
) {
    match event {
        sys::esp_avrc_tg_cb_event_t_ESP_AVRC_TG_SET_ABSOLUTE_VOLUME_CMD_EVT => {
            let volume = unsafe { (*parameters).set_abs_vol.volume }.min(LOUDEST);
            ASKED.store(volume, Ordering::Relaxed);
            info!(
                "The source sets the volume to {}%",
                u32::from(volume) * 100 / u32::from(LOUDEST)
            );
        }
        // The source asks to be told when the volume changes on the speaker, and is answered at
        // once with the volume the speaker has.
        sys::esp_avrc_tg_cb_event_t_ESP_AVRC_TG_REGISTER_NOTIFICATION_EVT => {
            let about = u32::from(unsafe { (*parameters).reg_ntf.event_id });
            if about != sys::esp_avrc_rn_event_ids_t_ESP_AVRC_RN_VOLUME_CHANGE {
                return;
            }
            let mut current = sys::esp_avrc_rn_param_t {
                volume: asked().unwrap_or(ASSUMED),
            };
            let answered = esp!(unsafe {
                sys::esp_avrc_tg_send_rn_rsp(
                    sys::esp_avrc_rn_event_ids_t_ESP_AVRC_RN_VOLUME_CHANGE,
                    sys::esp_avrc_rn_rsp_t_ESP_AVRC_RN_RSP_INTERIM,
                    &mut current,
                )
            });
            if let Err(error) = answered {
                warn!("The source was not told the volume: {error}");
            }
        }
        _ => {}
    }
}
