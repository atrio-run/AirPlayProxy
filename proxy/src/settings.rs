//! What the user chose: the name of the proxy, the Wi-Fi network it joins, the receiver it plays
//! on and how the sound is shaped. Kept in flash. A network and a receiver may also be built into the firmware, and
//! hold until the user chooses others.

use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys::EspError;

/// Bytes of the longest name the proxy takes.
pub const LONGEST_NAME: usize = 32;
/// Bytes of the longest name of a Wi-Fi network, and of the longest password of one.
pub const LONGEST_NETWORK: usize = 32;
pub const LONGEST_PASSWORD: usize = 64;
/// Bytes of the longest name under which a receiver offers its service.
pub const LONGEST_RECEIVER: usize = 63;
const LONGEST_VALUE: usize = 64;

const DEFAULT_NAME: &str = "AirPlay Proxy";
const BUILT_IN_NETWORK: Option<&str> = option_env!("PROXY_WIFI_SSID");
const BUILT_IN_PASSWORD: Option<&str> = option_env!("PROXY_WIFI_PASSWORD");
/// Host and port of a receiver, such as `Kitchen.local:7000`.
const BUILT_IN_RECEIVER: Option<&str> = option_env!("PROXY_RECEIVER");

const NAMESPACE: &str = "proxy";
const NAME: &str = "name";
const NETWORK: &str = "network";
const PASSWORD: &str = "password";
const RECEIVER: &str = "receiver";
const TONE: &str = "tone";
const SYNC: &str = "sync";

/// The most the delay reported to the source is trimmed by, either way, in milliseconds.
pub const LARGEST_SYNC: i16 = 300;

/// A Wi-Fi network. One without a password is open.
#[derive(Clone, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    pub password: String,
}

struct Store {
    flash: EspNvs<NvsDefault>,
    name: String,
    network: Option<Network>,
    receiver: Option<String>,
    tone: tone::Settings,
    sync: i16,
}

/// Counts the changes of the tone settings, for who applies them to notice one.
static TONE_CHANGES: AtomicU32 = AtomicU32::new(0);

/// Counts the changes of the receiver, for a session on the one before to notice.
static RECEIVER_CHANGES: AtomicU32 = AtomicU32::new(0);

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn read(flash: &EspNvs<NvsDefault>, key: &str) -> Result<Option<String>, EspError> {
    let mut value = [0; LONGEST_VALUE + 1];
    Ok(flash.get_str(key, &mut value)?.map(str::to_owned))
}

/// Reads the settings from flash. To be called once, before anything asks for them.
pub fn load(partition: EspDefaultNvsPartition) -> Result<(), EspError> {
    let flash = EspNvs::new(partition, NAMESPACE, true)?;
    let name = read(&flash, NAME)?.unwrap_or_else(|| DEFAULT_NAME.to_owned());
    let network = match read(&flash, NETWORK)? {
        Some(name) => Some(Network {
            name,
            password: read(&flash, PASSWORD)?.unwrap_or_default(),
        }),
        None => None,
    };
    let receiver = read(&flash, RECEIVER)?;
    let tone = read(&flash, TONE)?
        .and_then(|kept| tone::Settings::from_text(&kept))
        .unwrap_or_default();
    let sync = read(&flash, SYNC)?
        .and_then(|kept| kept.parse::<i16>().ok())
        .filter(|sync| sync.abs() <= LARGEST_SYNC)
        .unwrap_or(0);
    let _ = STORE.set(Mutex::new(Store {
        flash,
        name,
        network,
        receiver,
        tone,
        sync,
    }));
    Ok(())
}

fn with<T>(act: impl FnOnce(&mut Store) -> T) -> T {
    let store = STORE.get().expect("The settings were not loaded");
    act(&mut store.lock().unwrap())
}

/// The name of the proxy: what a Bluetooth source shows, and what its `.local` name comes from.
pub fn name() -> String {
    with(|store| store.name.clone())
}

/// The Wi-Fi network to join: the one the user chose, or else the one built into the firmware.
pub fn network() -> Option<Network> {
    with(|store| store.network.clone()).or_else(|| {
        let name = BUILT_IN_NETWORK.filter(|name| !name.is_empty())?;
        Some(Network {
            name: name.to_owned(),
            password: BUILT_IN_PASSWORD.unwrap_or_default().to_owned(),
        })
    })
}

/// The name under which the chosen receiver offers its AirPlay service, once one was chosen.
pub fn receiver() -> Option<String> {
    with(|store| store.receiver.clone())
}

/// Host and port of the receiver built into the firmware, if one was.
pub fn built_in_receiver() -> Option<&'static str> {
    BUILT_IN_RECEIVER.filter(|receiver| !receiver.is_empty())
}

pub fn tone() -> tone::Settings {
    with(|store| store.tone)
}

/// A number that changes whenever the tone settings do.
pub fn tone_changes() -> u32 {
    TONE_CHANGES.load(Ordering::Relaxed)
}

pub fn set_tone(tone: tone::Settings) -> Result<(), EspError> {
    with(|store| {
        store.flash.set_str(TONE, &tone.to_text())?;
        store.tone = tone;
        TONE_CHANGES.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
}

/// What is added to the delay reported to the source, in milliseconds: more when the sound
/// comes after the lips, less when it comes before.
pub fn sync() -> i16 {
    with(|store| store.sync)
}

pub fn set_sync(sync: i16) -> Result<(), EspError> {
    with(|store| {
        store.flash.set_str(SYNC, &sync.to_string())?;
        store.sync = sync;
        Ok(())
    })
}

/// A number that changes whenever the receiver does.
pub fn receiver_changes() -> u32 {
    RECEIVER_CHANGES.load(Ordering::Relaxed)
}

pub fn set_name(name: &str) -> Result<(), EspError> {
    with(|store| {
        store.flash.set_str(NAME, name)?;
        store.name = name.to_owned();
        Ok(())
    })
}

pub fn set_network(network: Network) -> Result<(), EspError> {
    with(|store| {
        store.flash.set_str(NETWORK, &network.name)?;
        store.flash.set_str(PASSWORD, &network.password)?;
        store.network = Some(network);
        Ok(())
    })
}

pub fn set_receiver(receiver: &str) -> Result<(), EspError> {
    with(|store| {
        store.flash.set_str(RECEIVER, receiver)?;
        store.receiver = Some(receiver.to_owned());
        RECEIVER_CHANGES.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
}
