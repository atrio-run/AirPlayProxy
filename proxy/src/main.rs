//! A Bluetooth speaker that plays on an AirPlay receiver.
//!
//! The audio a source sends over Bluetooth (A2DP) leaves over Wi-Fi for an AirPlay receiver
//! (RAOP). The time that takes is fixed, and the proxy reports it to the source when the stream
//! opens: a source that honors delay reports holds its video back by as much.

mod roaming;
mod settings;
mod volume;
mod web;

use core::ptr;
use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;
use std::thread;
use std::time::Duration;

use delay_line::{DelayLine, Pulled};
use esp_idf_svc::bt::a2dp::{A2dpEvent, AudioStatus, ConnectionStatus, EspA2dp, Sink};
use esp_idf_svc::bt::gap::{DiscoveryMode, EspGap};
use esp_idf_svc::bt::{reduce_bt_memory, BtClassic, BtDriver};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::thread::{MallocCap, ThreadSpawnConfiguration};
use esp_idf_svc::ipv4::{Configuration as IpConfiguration, RouterConfiguration};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::netif::{EspNetif, IpEvent, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{self, esp, EspError};
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi, WifiDriver, WifiEvent,
};
use log::{error, info, warn};
use raop::{Sender, FRAMES_PER_PACKET, FRAME_BYTES, SAMPLE_RATE};

/// How long the proxy stays on its own access point, with nobody asking for its page, before it
/// tries again the Wi-Fi network it has: the network may have been away when the proxy started.
const SETUP_PATIENCE: Duration = Duration::from_secs(5 * 60);
/// The service that AirPlay receivers offer on the local network.
const RAOP: &str = "_raop._tcp.local";

/// What ESP-IDF attributes to receiving and decoding the stream: its default delay report.
const STACK_DELAY: Duration = Duration::from_millis(120);
/// Audio held between Bluetooth and AirPlay, to cover the waits between the bursts in which
/// Bluetooth delivers it. With 80 ms the stream ran dry several times a minute.
const BUFFERED: Duration = Duration::from_millis(200);
/// Audio taken beyond that before any is discarded.
const SLACK: Duration = Duration::from_millis(200);
/// Frames the held audio is off when the delay line drops or repeats a frame about twice a
/// second. Nearer to the delay it does so more seldom, farther more often.
const TOLERANCE: usize = 480;
/// The latency asked of the receiver, in frames: a quarter of a second for the packets that
/// arrive late or have to be sent again.
const AIRPLAY_LATENCY: i32 = 11_025;
/// When a HomePod mini plays a frame sent with that latency: 547 ms as measured from a computer,
/// less the 75 ms by which the sound then came before the lips with this board.
const AIRPLAY_DELAY: Duration = Duration::from_millis(472);
/// The delay reported to the source: everything between its sending a frame and the sound.
const PLAYBACK_DELAY: Duration =
    Duration::from_millis((STACK_DELAY.as_millis() + BUFFERED.as_millis() + AIRPLAY_DELAY.as_millis()) as u64);
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// How long the proxy tries to join its Wi-Fi network at start before it opens its own.
const JOIN_PATIENCE: Duration = Duration::from_secs(30);
/// How long the local network is asked for the address of the receiver.
const LOOKUP_PATIENCE: Duration = Duration::from_secs(3);
/// Packets between two lines of the log on how the relay is doing: about ten seconds.
const REPORT_PACKETS: u32 = 10 * SAMPLE_RATE / FRAMES_PER_PACKET as u32;

struct Stream {
    line: Mutex<DelayLine>,
    /// Whether the source sends two channels. One channel plays on both.
    stereo: AtomicBool,
    /// Whether the source sends at the sample rate of AirPlay.
    playable: AtomicBool,
    discarded: AtomicU32,
    /// When audio last arrived, as `uptime_ms`. Zero while nothing plays.
    arrived: AtomicU32,
    /// The longest wait between two arrivals, in milliseconds, since it was last reported.
    longest_wait: AtomicU32,
}

impl Stream {
    fn new() -> Self {
        let mut line = DelayLine::new(FRAME_BYTES, frames(BUFFERED + SLACK));
        line.set_delay(frames(BUFFERED), TOLERANCE);
        Self {
            line: Mutex::new(line),
            stereo: AtomicBool::new(true),
            playable: AtomicBool::new(true),
            discarded: AtomicU32::new(0),
            arrived: AtomicU32::new(0),
            longest_wait: AtomicU32::new(0),
        }
    }

    fn clear(&self) {
        self.line.lock().unwrap().clear();
        self.arrived.store(0, Ordering::Relaxed);
    }
}

/// How strongly the access point is received, in dBm, or `None` if the Wi-Fi does not tell.
fn wifi_signal() -> Option<i32> {
    let mut signal = 0;
    esp!(unsafe { sys::esp_wifi_sta_get_rssi(&mut signal) })
        .ok()
        .map(|()| signal)
}

/// Joins the network again after a disconnection. The driver mends by itself only the one it
/// reports as roaming.
fn rejoin(reason: u16, signal: i8) {
    if u32::from(reason) == sys::wifi_err_reason_t_WIFI_REASON_ROAMING {
        return;
    }
    warn!("Wi-Fi disconnected (reason {reason}, signal {signal} dBm): joining again");
    if let Err(error) = esp!(unsafe { sys::esp_wifi_connect() }) {
        warn!("Wi-Fi did not start to join again: {error}");
    }
}

/// Gives the station the IPv6 link-local address that name questions leave from. ESP-IDF removes
/// it at every disconnection, and creates one only on an interface that is up.
fn ensure_link_local(station: *mut sys::esp_netif_t) {
    let mut address = sys::esp_ip6_addr_t::default();
    if unsafe { sys::esp_netif_get_ip6_linklocal(station, &mut address) } == sys::ESP_OK {
        return;
    }
    if let Err(error) = esp!(unsafe { sys::esp_netif_create_ip6_linklocal(station) }) {
        warn!("No IPv6 link-local address: {error}");
    }
}

/// The Wi-Fi networks around, strongest first, with how strongly each is received in dBm. Looking
/// for them takes the radio off the access point's channel for about a second, which would break
/// the sound: it is refused while the source plays.
fn networks() -> Result<Vec<(String, i8)>, String> {
    if roaming::source_plays() {
        return Err("The source plays, and looking for networks would break the sound.".to_owned());
    }
    // A plain C structure; zero looks on every channel, for as long as the driver sees fit.
    let search: sys::wifi_scan_config_t = unsafe { core::mem::zeroed() };
    esp!(unsafe { sys::esp_wifi_scan_start(&search, true) })
        .map_err(|error| format!("The Wi-Fi networks could not be looked for: {error}"))?;

    let mut found: Vec<(String, i8)> = Vec::new();
    // A plain C record that the driver fills in.
    let mut record: sys::wifi_ap_record_t = unsafe { core::mem::zeroed() };
    while unsafe { sys::esp_wifi_scan_get_ap_record(&mut record) } == sys::ESP_OK {
        let length = record.ssid.iter().position(|byte| *byte == 0).unwrap_or(record.ssid.len());
        let name = String::from_utf8_lossy(&record.ssid[..length]).into_owned();
        if name.is_empty() {
            continue;
        }
        match found.iter_mut().find(|(known, _)| *known == name) {
            Some((_, signal)) => *signal = (*signal).max(record.rssi),
            None => found.push((name, record.rssi)),
        }
    }
    unsafe { sys::esp_wifi_clear_ap_list() };
    found.sort_by_key(|(_, signal)| core::cmp::Reverse(*signal));
    Ok(found)
}

/// The internal memory that is free, and the largest piece of it, in bytes. Thread stacks come
/// from it.
fn internal_memory() -> (usize, usize) {
    unsafe {
        (
            sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL),
            sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_INTERNAL),
        )
    }
}

/// Makes the threads that this thread starts from now on take their stacks from external RAM,
/// or from internal RAM again. Only a thread that never writes to flash may have its stack in
/// external RAM.
fn stacks_in_external_ram(external: bool) -> Result<(), EspError> {
    let stacks = if external {
        MallocCap::Spiram | MallocCap::Cap8bit
    } else {
        MallocCap::Internal | MallocCap::Cap8bit
    };
    ThreadSpawnConfiguration {
        stack_alloc_caps: stacks,
        ..Default::default()
    }
    .set()
}

/// The IPv4 address of the station, while it has one.
fn station_address() -> Option<Ipv4Addr> {
    let station = unsafe { sys::esp_netif_get_handle_from_ifkey(c"WIFI_STA_DEF".as_ptr()) };
    let mut assigned = sys::esp_netif_ip_info_t::default();
    esp!(unsafe { sys::esp_netif_get_ip_info(station, &mut assigned) }).ok()?;
    let address = Ipv4Addr::from(assigned.ip.addr.to_ne_bytes());
    (!address.is_unspecified()).then_some(address)
}

/// Milliseconds since the firmware started, never zero.
fn uptime_ms() -> u32 {
    ((unsafe { sys::esp_timer_get_time() } / 1000) as u32).max(1)
}

fn frames(duration: Duration) -> usize {
    (duration.as_micros() * SAMPLE_RATE as u128 / 1_000_000) as usize
}

fn on_event(
    stream: &Stream,
    playing: &mpsc::Sender<bool>,
    converted: &mut Vec<u8>,
    event: A2dpEvent,
) {
    match event {
        A2dpEvent::SinkData(pcm) => {
            if !stream.playable.load(Ordering::Relaxed) {
                return;
            }
            let now = uptime_ms();
            let before = stream.arrived.swap(now, Ordering::Relaxed);
            if before != 0 {
                stream
                    .longest_wait
                    .fetch_max(now.wrapping_sub(before), Ordering::Relaxed);
            }
            let audio = if stream.stereo.load(Ordering::Relaxed) {
                pcm
            } else {
                converted.clear();
                converted.extend(
                    pcm.as_chunks::<2>()
                        .0
                        .iter()
                        .flat_map(|sample| [sample[0], sample[1], sample[0], sample[1]]),
                );
                converted.as_slice()
            };
            let discarded = stream.line.lock().unwrap().push(audio);
            if discarded > 0 {
                stream
                    .discarded
                    .fetch_add(discarded as u32, Ordering::Relaxed);
            }
        }
        A2dpEvent::SinkServiceCapabilitiesConfigured(capabilities) => {
            if capabilities & sys::ESP_A2D_PSC_DELAY_RPT as u16 != 0 {
                info!("The source enabled delay reports");
            } else {
                warn!("The source did not enable delay reports");
            }
        }
        A2dpEvent::AudioCodecConfigured { codec, .. } => {
            let playable = codec.bitrate() == Some(SAMPLE_RATE);
            if !playable {
                error!("Only streams at {SAMPLE_RATE} Hz play: {codec:?}");
            }
            stream.playable.store(playable, Ordering::Relaxed);
            stream
                .stereo
                .store(codec.stereo() != Some(false), Ordering::Relaxed);
        }
        A2dpEvent::AudioState { status, .. } => {
            let _ = playing.send(status == AudioStatus::Started);
        }
        A2dpEvent::ConnectionState {
            status: ConnectionStatus::Disconnected,
            ..
        } => {
            let _ = playing.send(false);
        }
        _ => {}
    }
}

/// Whether the source stopped playing, going by its latest word.
fn stopped(playing: &Receiver<bool>) -> bool {
    let mut stopped = false;
    loop {
        match playing.try_recv() {
            Ok(playing) => stopped = !playing,
            Err(TryRecvError::Empty) => return stopped,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

/// The address of the receiver: the one the user chose, which is asked of the local network by
/// the name of its service, or else the one built into the firmware, whose `.local` name is asked
/// of the local network. `interface` is the index of the network interface the questions leave
/// from.
fn receiver(interface: u32) -> Result<SocketAddr, Box<dyn Error>> {
    if let Some(chosen) = settings::receiver() {
        let found = mdns::find(RAOP, &chosen, interface, LOOKUP_PATIENCE)?;
        let address = match found.address {
            Some(address) => address,
            None => mdns::resolve(&found.host, interface, LOOKUP_PATIENCE)?,
        };
        return Ok(SocketAddr::from((address, found.port)));
    }
    let built_in = settings::built_in_receiver().ok_or("no receiver was chosen")?;
    match built_in.rsplit_once(':') {
        Some((host, port)) if host.ends_with(".local") => {
            let address = mdns::resolve(host, interface, LOOKUP_PATIENCE)?;
            Ok(SocketAddr::from((address, port.parse::<u16>()?)))
        }
        _ => built_in
            .to_socket_addrs()?
            .find(|address| address.is_ipv4())
            .ok_or_else(|| "the receiver has no IPv4 address".into()),
    }
}

/// Sends what the source plays to the receiver, until the source stops or the user chooses
/// another receiver. Returns whether the source still plays.
fn play(
    stream: &Stream,
    playing: &Receiver<bool>,
    interface: u32,
    log: &mpsc::Sender<String>,
) -> Result<bool, Box<dyn Error>> {
    // The console takes tens of milliseconds for a line, in which no packet would leave: while
    // the audio is relayed, the lines go to the thread that writes them.
    let tell = |line: String| {
        let _ = log.send(line);
    };
    let chosen = settings::receiver_changes();
    let receiver = receiver(interface)?;
    let mut sender = Sender::connect(receiver, AIRPLAY_LATENCY)?;
    let (free, largest) = internal_memory();
    info!(
        "Playing on {receiver}, which reports {} frames of latency. Internal memory: {free} \
         bytes free, the largest piece {largest}",
        sender.receiver_latency()
    );

    // The audio that arrived while the session opened is late.
    stream.clear();
    stream.longest_wait.store(0, Ordering::Relaxed);
    let mut discarded = stream.discarded.load(Ordering::Relaxed);
    let (mut dropped, mut repeated) = {
        let line = stream.line.lock().unwrap();
        (line.dropped(), line.repeated())
    };
    let mut fallen_behind = 0;
    let mut tone = tone::Tone::new(SAMPLE_RATE);
    let mut tone_set = None;
    // The time the equalizer and the loudness correction took since the last report.
    let mut shaped = Duration::ZERO;
    let mut packet = [0; FRAMES_PER_PACKET * FRAME_BYTES];
    let mut packets = 0;

    let mut still_plays = false;
    while !stopped(playing) {
        if settings::receiver_changes() != chosen {
            still_plays = true;
            break;
        }
        // The loudness correction follows the volume: a source that sets none plays at full.
        let asked_volume = volume::asked();
        let tone_now = (settings::tone_changes(), asked_volume);
        if tone_set != Some(tone_now) {
            let share = asked_volume.map_or(1.0, |volume| f32::from(volume) / f32::from(volume::LOUDEST));
            tone.set(settings::tone(), share);
            tone_set = Some(tone_now);
            // The correction lowers the whole to make room for the bass it raises: the receiver
            // plays louder by as much, so that only the bass changes.
            match asked_volume {
                Some(0) => sender.set_volume(volume::decibel(0))?,
                Some(volume) => sender.set_volume((volume::decibel(volume) + tone.room()).min(0.0))?,
                None => {}
            }
        }

        if stream.line.lock().unwrap().pull(&mut packet, 0) == Pulled::Stalled {
            tell("The stream stalled: playback restarts from silence".to_owned());
        }
        let shaping = Instant::now();
        tone.process(&mut packet);
        shaped += shaping.elapsed();
        sender.send(&packet)?;

        let total = stream.discarded.load(Ordering::Relaxed);
        if total != discarded {
            tell(format!(
                "Discarded {} frames that did not fit the delay line",
                total.wrapping_sub(discarded)
            ));
            discarded = total;
        }
        if sender.fallen_behind() != fallen_behind {
            tell("The relay fell behind: playback goes on after a gap".to_owned());
            fallen_behind = sender.fallen_behind();
        }

        packets += 1;
        if packets % REPORT_PACKETS == 0 {
            let (now_dropped, now_repeated) = {
                let line = stream.line.lock().unwrap();
                (line.dropped(), line.repeated())
            };
            let (timing_requests, timing_spread) = sender.timing_requests();
            tell(format!(
                "In ten seconds: longest wait for Bluetooth audio {} ms, latest packet {} ms \
                 late, {} frames dropped and {} repeated to follow the source's clock, {} \
                 timing requests of the receiver, the slowest {} ms slower to arrive than the \
                 quickest, Wi-Fi signal {} dBm, {} ms spent shaping the sound. In the session: {} \
                 packets sent again, {} lost to full network buffers, {} gaps",
                stream.longest_wait.swap(0, Ordering::Relaxed),
                sender.longest_delay().as_millis(),
                now_dropped.wrapping_sub(dropped),
                now_repeated.wrapping_sub(repeated),
                timing_requests,
                timing_spread.as_millis(),
                wifi_signal().map_or_else(|| "unknown".to_owned(), |signal| signal.to_string()),
                core::mem::take(&mut shaped).as_millis(),
                sender.retransmitted(),
                sender.dropped(),
                fallen_behind
            ));
            (dropped, repeated) = (now_dropped, now_repeated);
        }
    }

    if still_plays {
        info!("Another receiver was chosen");
    } else {
        info!("The source stopped");
    }
    stream.clear();
    sender.close()?;
    Ok(still_plays)
}

/// Opens an AirPlay session whenever the source plays.
fn relay(stream: &Stream, playing: &Receiver<bool>, interface: u32) {
    // An AirPlay session starts two threads, which answer the receiver over the network.
    if let Err(error) = stacks_in_external_ram(true) {
        warn!("The threads of a session take their stacks from internal memory: {error}");
    }
    let (log, lines) = mpsc::channel::<String>();
    thread::Builder::new()
        .name("log".into())
        .stack_size(4096)
        .spawn(move || {
            for line in lines {
                info!("{line}");
            }
        })
        .expect("Failed to start the log thread");

    while let Ok(started) = playing.recv() {
        if !started {
            continue;
        }
        roaming::playing(true);
        loop {
            match play(stream, playing, interface, &log) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    let (free, largest) = internal_memory();
                    error!(
                        "The AirPlay session failed: {error}. Internal memory: {free} bytes \
                         free, the largest piece {largest}"
                    );
                    thread::sleep(RETRY_DELAY);
                    if stopped(playing) {
                        break;
                    }
                }
            }
        }
        roaming::playing(false);
    }
}

/// Wakes who waits for the network whenever the Wi-Fi or its address changes.
static NETWORK_CHANGED: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

fn network_changed() {
    let _waiting = NETWORK_CHANGED.0.lock().unwrap();
    NETWORK_CHANGED.1.notify_all();
}

/// Joins `network`, or gives up after `JOIN_PATIENCE`: a network that keeps refusing the proxy,
/// as one does when the password is wrong, must not keep it trying for ever.
fn join(wifi: &mut BlockingWifi<EspWifi<'static>>, network: &settings::Network) -> Result<(), EspError> {
    let too_long = || EspError::from_infallible::<{ sys::ESP_ERR_INVALID_ARG }>();
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: network.name.as_str().try_into().map_err(|_| too_long())?,
        password: network.password.as_str().try_into().map_err(|_| too_long())?,
        auth_method: if network.password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.wifi_mut().connect()?;

    let give_up = Instant::now() + JOIN_PATIENCE;
    let mut waiting = NETWORK_CHANGED.0.lock().unwrap();
    while !wifi.is_up()? {
        let left = give_up.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(EspError::from_infallible::<{ sys::ESP_ERR_TIMEOUT }>());
        }
        waiting = NETWORK_CHANGED.1.wait_timeout(waiting, left).unwrap().0;
    }
    Ok(())
}

/// The name of the access point the proxy opens to be set up: its own, and the end of its
/// address, which tells two proxies apart.
fn setup_network(name: &str, address: [u8; 6]) -> String {
    let mut network = name.to_owned();
    while network.len() > settings::LONGEST_NETWORK - 5 {
        network.pop();
    }
    format!("{network} {:02X}{:02X}", address[4], address[5])
}

/// Opens an access point on which the page of the proxy sets its Wi-Fi network, and serves it
/// until the page restarts the proxy. With a network that was not joined, the proxy also
/// restarts once nobody has asked for the page for a while, to try that network again.
fn set_up(mut wifi: BlockingWifi<EspWifi<'static>>) -> Result<(), EspError> {
    let name = setup_network(&settings::name(), wifi.wifi().sta_netif().get_mac()?);
    wifi.set_configuration(&Configuration::Mixed(
        ClientConfiguration::default(),
        AccessPointConfiguration {
            ssid: name.as_str().try_into().expect("The name fits a network"),
            auth_method: AuthMethod::None,
            ..Default::default()
        },
    ))?;
    wifi.start()?;

    let address = wifi.wifi().ap_netif().get_ip_info()?.ip;
    info!("Join the Wi-Fi network {name} to give the proxy one: its page is at http://{address}");

    thread::Builder::new()
        .name("captive-dns".into())
        .stack_size(6144)
        .spawn(move || {
            let Err(error) = captive_dns::serve(address);
            error!("The access point no longer answers names: {error}");
        })
        .expect("Failed to start the name thread");
    if settings::network().is_some() {
        thread::Builder::new()
            .name("setup-patience".into())
            .stack_size(4096)
            .spawn(|| loop {
                let idle = uptime_ms().wrapping_sub(web::ASKED.load(Ordering::Relaxed));
                let left = SETUP_PATIENCE.saturating_sub(Duration::from_millis(idle.into()));
                if left.is_zero() {
                    info!("Nobody set a Wi-Fi network: restarting to try the one the proxy has");
                    unsafe { sys::esp_restart() };
                }
                thread::sleep(left);
            })
            .expect("Failed to start the patience thread");
    }

    let Err(error) = web::serve_setup(address);
    error!("The settings page stopped: {error}");
    Err(EspError::from_infallible::<{ sys::ESP_FAIL }>())
}

fn main() -> Result<(), EspError> {
    sys::link_patches();
    EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let events = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    settings::load(nvs.clone())?;
    let (wifi_modem, mut bluetooth_modem) = peripherals.modem.split();

    // The access point of the proxy names itself as the DNS server, which answers every name
    // with the proxy's address: a phone that joins it opens the page of the proxy.
    let router = RouterConfiguration::default();
    let access_point = EspNetif::new_with_conf(&NetifConfiguration {
        ip_configuration: Some(IpConfiguration::Router(RouterConfiguration {
            dns: Some(router.subnet.gateway),
            secondary_dns: None,
            ..router
        })),
        ..NetifConfiguration::wifi_default_router()
    })?;
    let mut wifi = BlockingWifi::wrap(
        EspWifi::wrap_all(
            WifiDriver::new(wifi_modem, events.clone(), Some(nvs.clone()))?,
            EspNetif::new(NetifStack::Sta)?,
            access_point,
        )?,
        events.clone(),
    )?;
    // The audio leaves at 1.4 Mbit/s over a radio it shares with Bluetooth: the slowest Wi-Fi
    // rates, those of 802.11b, cannot carry it.
    esp!(unsafe { sys::esp_wifi_config_11b_rate(sys::wifi_interface_t_WIFI_IF_STA, true) })?;

    let Some(network) = settings::network() else {
        info!("No Wi-Fi network was chosen");
        return set_up(wifi);
    };
    let wifi_events = events.subscribe::<WifiEvent, _>(|event| {
        roaming::handle(&event);
        network_changed();
        if let WifiEvent::StaDisconnected(disconnected) = event {
            rejoin(disconnected.reason(), disconnected.rssi());
        }
    })?;
    let _ip_events = events.subscribe::<IpEvent, _>(|event| {
        if let IpEvent::DhcpIpAssigned(assignment) = event {
            ensure_link_local(assignment.netif_handle());
        }
        network_changed();
    })?;
    if let Err(error) = join(&mut wifi, &network) {
        warn!("The Wi-Fi network {} was not joined: {error}", network.name);
        drop(wifi_events);
        wifi.stop()?;
        return set_up(wifi);
    }
    info!("Joined the Wi-Fi network {}", network.name);
    let _wifi_events = wifi_events;
    let interface = wifi.wifi().sta_netif().get_index();

    let host = format!("{}.local", mdns::host_name(&settings::name()));
    info!("The settings are at http://{host}");
    stacks_in_external_ram(true)?;
    thread::Builder::new()
        .name("names".into())
        .stack_size(4096)
        .spawn(move || {
            let Err(error) = mdns::answer_for(&host, station_address);
            error!("The proxy no longer answers to its name: {error}");
        })
        .expect("Failed to start the name thread");
    stacks_in_external_ram(false)?;

    let stream = Arc::new(Stream::new());
    let (playing, played) = mpsc::channel();

    let relayed = stream.clone();
    thread::Builder::new()
        .name("relay".into())
        .stack_size(12288)
        .spawn(move || relay(&relayed, &played, interface))
        .expect("Failed to start the relay thread");

    reduce_bt_memory(unsafe { bluetooth_modem.reborrow() })?;
    let bt = BtDriver::<BtClassic>::new(bluetooth_modem, Some(nvs))?;

    let gap = EspGap::<BtClassic, &BtDriver<'_, BtClassic>>::new(&bt)?;
    gap.set_device_name(&settings::name())?;
    // A speaker has neither a display nor keys: the source pairs without asking for a code.
    let io_capability = sys::ESP_BT_IO_CAP_NONE as sys::esp_bt_io_cap_t;
    esp!(unsafe {
        sys::esp_bt_gap_set_security_param(
            sys::esp_bt_sp_param_t_ESP_BT_SP_IOCAP_MODE,
            ptr::from_ref(&io_capability).cast_mut().cast(),
            1,
        )
    })?;

    volume::listen()?;
    let a2dp = EspA2dp::<BtClassic, &BtDriver<'_, BtClassic>, Sink>::new_sink(&bt)?;
    let mut converted = Vec::new();
    a2dp.subscribe(move |event| {
        on_event(&stream, &playing, &mut converted, event);
        0
    })?;
    // What the listener trims is far smaller than the delay.
    let trimmed = PLAYBACK_DELAY.as_millis() as i64 + i64::from(settings::sync());
    let delay = Duration::from_millis(trimmed as u64);
    info!("Reporting a delay of {delay:?}");
    a2dp.set_delay(delay)?;

    gap.set_scan_mode(true, DiscoveryMode::Discoverable)?;

    // The page is served here, on the stack that set everything up: internal memory is short,
    // and the threads of an AirPlay session need what a thread of its own would take.
    let Err(error) = web::serve(interface);
    error!("The settings page stopped: {error}");
    Err(EspError::from_infallible::<{ sys::ESP_FAIL }>())
}
