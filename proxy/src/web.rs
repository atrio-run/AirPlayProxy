//! The page on which the user names the proxy and chooses the Wi-Fi network it joins and the
//! receiver it plays on. It is served on the network the proxy joined, or else on the access
//! point that the proxy opens to be set up.

use std::convert::Infallible;
use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use esp_idf_svc::sys;
use httpd::{escape, Request, Response};
use log::{info, warn};

use crate::settings::{self, Network};

const PORT: u16 = 80;
/// How long the receivers of the network are given to answer when the page asks for them.
const BROWSE_PATIENCE: Duration = Duration::from_millis(1500);
/// The browser gets the answer to the form before the proxy restarts.
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// A dark card with an orange accent.
const STYLE: &str = "\
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}\
:root{color-scheme:dark;--accent:#f97316;--accent-hover:#fb923c;--on-accent:#1a0b02;\
--glass:rgba(255,255,255,.045);--glass-hover:rgba(255,255,255,.08);--border:rgba(255,255,255,.09);\
--text:#fff;--secondary:#a0a0a0;--muted:#808080}\
body{font:15px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;\
background:radial-gradient(60% 50% at 50% 0,rgba(249,115,22,.15),transparent) fixed,#1a1a1a;\
color:var(--text);min-height:100vh;display:flex;align-items:center;justify-content:center;\
padding:1rem;-webkit-font-smoothing:antialiased}\
.card{width:100%;max-width:420px;background:var(--glass);border:1px solid var(--border);\
border-radius:14px;padding:2rem;box-shadow:0 12px 40px rgba(0,0,0,.4),inset 0 1px 0 rgba(255,255,255,.06);\
-webkit-backdrop-filter:blur(18px) saturate(150%);backdrop-filter:blur(18px) saturate(150%)}\
.logo{text-align:center;margin-bottom:2rem}\
h1{font-size:1.75rem;font-weight:600;letter-spacing:-.025em;line-height:1.25}\
.hint,.state{color:var(--muted);font-size:.875rem;margin:.5rem 0}\
.accent{color:var(--accent)}\
.group{margin-bottom:1.5rem}\
.group>label:not([class]),.title{display:block;color:var(--secondary);font-size:.8125rem;font-weight:500;margin-bottom:.5rem}\
.input{width:100%;background:rgba(0,0,0,.22);border:1px solid var(--border);color:var(--text);\
padding:.625rem .875rem;border-radius:10px;font:inherit;transition:border-color .15s,box-shadow .15s}\
.input+.input{margin-top:.5rem}\
.input:hover{border-color:rgba(255,255,255,.16)}\
.input:focus{outline:none;border-color:var(--accent);box-shadow:0 0 0 3px rgba(249,115,22,.18)}\
.input::placeholder{color:var(--muted)}\
.choice{width:100%;font:inherit;color:var(--text);text-align:left;display:flex;align-items:center;gap:.75rem;padding:.625rem .875rem;margin-bottom:.5rem;\
background:rgba(0,0,0,.22);border:1px solid var(--border);border-radius:10px;cursor:pointer;\
transition:border-color .15s,background-color .15s}\
.choice:hover{background:var(--glass-hover)}\
.choice:has(input:checked){border-color:var(--accent);background:rgba(249,115,22,.12)}\
.choice input{accent-color:var(--accent)}\
.choice small{display:block;color:var(--muted);font-size:.8125rem}\
.choice.found{justify-content:space-between}\
#networks:not(:empty){margin-top:.75rem}\
.band{display:flex;align-items:center;gap:.75rem;margin-bottom:.5rem;font-size:.8125rem;color:var(--secondary)}\
.band span{width:3.5rem}.band output{min-width:3.5rem;text-align:right}\
.band input{flex:1;min-width:0;accent-color:var(--accent)}.band input:disabled{opacity:.35}\
.bands{display:flex;justify-content:space-between;margin:.75rem 0}\
.column{display:flex;flex-direction:column;align-items:center;gap:.375rem;flex:1;min-width:0;\
font-size:.6875rem;color:var(--secondary)}\
.column input{writing-mode:vertical-lr;direction:rtl;width:1.25rem;height:9rem;accent-color:var(--accent)}\
.column input:disabled{opacity:.35}\
hr{border:0;border-top:1px solid var(--border);margin:0 0 1.5rem}\
.btn{display:inline-flex;align-items:center;justify-content:center;padding:.625rem 1.125rem;\
border:1px solid transparent;border-radius:10px;font:inherit;font-size:.9375rem;font-weight:500;\
cursor:pointer;transition:background-color .15s}\
.primary{width:100%;background:var(--accent);color:var(--on-accent);font-weight:600}\
.primary:hover{background:var(--accent-hover)}\
.secondary{background:var(--glass);border-color:var(--border);color:var(--text);\
padding:.4375rem .875rem;font-size:.8125rem;margin-top:.5rem}\
.secondary:hover{background:var(--glass-hover)}\
.alert{padding:.75rem 1rem;border-radius:10px;font-size:.875rem;margin-bottom:1rem;\
background:rgba(249,115,22,.12);border:1px solid rgba(249,115,22,.35);color:#fdba74}\
.loading{display:flex;align-items:center;gap:.75rem;color:var(--secondary);padding:.625rem 0}\
.spinner{width:1.125rem;height:1.125rem;border:2px solid var(--border);border-top-color:var(--accent);\
border-radius:50%;animation:spin .8s linear infinite}\
@keyframes spin{to{transform:rotate(360deg)}}\
.state{text-align:center;margin-top:1.5rem}";

/// Looks for Wi-Fi networks when asked to, and takes the one that is picked.
const NETWORKS_SCRIPT: &str = "\
const networks=document.getElementById('networks'),network=document.getElementById('network'),\
password=document.getElementById('password'),touched=document.getElementById('touched');\
password.addEventListener('focus',()=>{if(touched.value=='0')password.value='';});\
password.addEventListener('input',()=>{touched.value='1';});\
password.addEventListener('blur',()=>{if(touched.value=='0')password.value=password.defaultValue;});\
function take(into,from,what){\
into.innerHTML='<div class=loading><span class=spinner></span>Looking for '+what+'…</div>';\
fetch(from,{cache:'no-store'}).then(answer=>answer.text()).then(found=>into.innerHTML=found)\
.catch(()=>into.innerHTML='<p class=hint>The proxy does not answer. Load the page again.</p>');}\
function scan(){take(networks,'/networks','Wi-Fi networks');}\
document.getElementById('scan').addEventListener('click',scan);\
networks.addEventListener('click',event=>{const found=event.target.closest('[data-network]');\
if(found){network.value=found.dataset.network;touched.value='1';password.value='';password.focus();}});";

/// Gives the form to the proxy without leaving the page, and tells what the proxy answers. A
/// password that was not touched is not sent. When the proxy restarts, the page loads again
/// once it is back.
const SAVE_SCRIPT: &str = "\
const form=document.querySelector('form'),note=document.getElementById('note');\
form.addEventListener('submit',event=>{event.preventDefault();\
password.disabled=touched.value=='0';const body=new URLSearchParams(new FormData(form));\
password.disabled=false;\
fetch('/save',{method:'POST',body}).then(answer=>answer.text()).then(told=>{\
note.textContent=told;note.hidden=false;\
if(told.includes('restarts'))setTimeout(()=>location.reload(),12000);})\
.catch(()=>{note.textContent='The proxy does not answer.';note.hidden=false;});});";

/// Tells beside the lip sync which way it moves the sound.
const SYNC_SCRIPT: &str = "\
const sync=document.querySelector('#lips input');\
function told(){const by=Math.abs(sync.value)+' ms ';\
sync.nextElementSibling.textContent=sync.value>0?by+'sooner':sync.value<0?by+'later':'as it is';}\
sync.addEventListener('input',told);told();";

/// Asks the proxy for the receivers once the page shows, and again when asked to.
const RECEIVERS_SCRIPT: &str = "\
const receivers=document.getElementById('receivers');\
function look(){take(receivers,'/receivers','AirPlay receivers');}\
document.getElementById('again').addEventListener('click',look);look();";

/// Shows the gain beside a band as it moves, and gives the sound to the proxy when it is let go.
const SOUND_SCRIPT: &str = "\
const sound=document.getElementById('sound'),loudness=document.getElementById('loudness'),\
bypass=document.getElementById('bypass'),bands=sound.querySelectorAll('input[type=range]');\
function shown(band){band.previousElementSibling.textContent=band.value;}\
function dim(){bands.forEach(band=>band.disabled=bypass.checked);}\
function give(){dim();const body=new URLSearchParams();\
bands.forEach((band,index)=>body.set('b'+index,band.value));\
body.set('equalizer',bypass.checked?0:1);body.set('loudness',loudness.checked?1:0);\
fetch('/tone',{method:'POST',body});}\
sound.addEventListener('input',event=>{if(event.target.type=='range')shown(event.target);});\
sound.addEventListener('change',give);dim();";

/// Where the page is served.
#[derive(Clone, Copy)]
pub enum Place {
    /// On the network the proxy joined. The receivers are asked for from the network interface
    /// of this index.
    Network { interface: u32 },
    /// On the proxy's own access point.
    Setup,
}

/// When the page was last asked for, as `uptime_ms`.
pub static ASKED: AtomicU32 = AtomicU32::new(0);

/// The receivers as the page lists them, for the page to take in once it shows: asking for
/// them takes longer than a page should take to show. Until the user chooses one, the one built
/// into the firmware shows as chosen.
fn receivers_list(interface: u32) -> String {
    let found = mdns::browse(crate::RAOP, interface, BROWSE_PATIENCE).unwrap_or_else(|error| {
        warn!("The receivers of the network could not be asked for: {error}");
        Vec::new()
    });
    let chosen = settings::receiver();
    let built_in = settings::built_in_receiver();
    let mut list = String::new();
    for receiver in &found {
        let playing = match &chosen {
            Some(chosen) => *chosen == receiver.name,
            None => built_in == Some(format!("{}:{}", receiver.host, receiver.port).as_str()),
        };
        list.push_str(&choice(&receiver.name, &receiver.host, playing));
    }
    if let Some(chosen) = chosen.filter(|chosen| !found.iter().any(|found| found.name == *chosen)) {
        list.push_str(&choice(&chosen, "does not answer now", true));
    }
    if list.is_empty() {
        list.push_str("<p class=hint>No AirPlay receiver answered.</p>");
    }
    list
}

/// A receiver to choose: the name under which it offers its service, where the name people
/// gave it follows an `@`, and where it is.
fn choice(name: &str, place: &str, chosen: bool) -> String {
    let shown = name.split_once('@').map_or(name, |(_, shown)| shown);
    let checked = if chosen { " checked" } else { "" };
    format!(
        "<label class=choice><input type=radio name=receiver value=\"{}\"{checked}>\
         <span>{}<small>{}</small></span></label>",
        escape(name),
        escape(shown),
        escape(place)
    )
}

/// The Wi-Fi networks around as the page lists them, for the page to take in when asked.
fn networks_list() -> String {
    match crate::networks() {
        Ok(found) if found.is_empty() => "<p class=hint>No Wi-Fi network was found.</p>".to_owned(),
        Ok(found) => found
            .iter()
            .map(|(name, signal)| {
                format!(
                    "<button type=button class=\"choice found\" data-network=\"{}\">{}\
                     <small>{signal} dBm</small></button>",
                    escape(name),
                    escape(name)
                )
            })
            .collect(),
        Err(problem) => format!("<p class=hint>{}</p>", escape(&problem)),
    }
}

/// The equalizer and the loudness correction, which take effect as they are moved.
fn sound_group() -> String {
    let settings = settings::tone();
    let mut bands = String::new();
    for (hertz, gain) in tone::BANDS.into_iter().zip(settings.gains) {
        let named = if hertz < 1000.0 {
            format!("{hertz}")
        } else {
            format!("{}k", hertz / 1000.0)
        };
        bands.push_str(&format!(
            "<label class=column><output>{gain}</output><input type=range \
             min={deepest} max=0 value={gain}><span>{named}</span></label>",
            deepest = tone::DEEPEST_CUT
        ));
    }
    let checked = |on: bool| if on { " checked" } else { "" };
    format!(
        "<div class=group id=sound><span class=title>Sound</span>\
         <label class=choice><input type=checkbox id=bypass{}>\
         <span>Bypass equalizer</span></label><div class=bands>{bands}</div>\
         <label class=choice><input type=checkbox id=loudness{}>\
         <span>Loudness<small>More bass and treble</small></span></label></div><hr>",
        checked(!settings.equalizer),
        checked(settings.loudness)
    )
}

/// Takes the sound that the page gives when a band or the loudness is moved.
fn save_sound(request: &Request) -> Option<()> {
    let form = request.form();
    let field = |name: &str| form.iter().find(|(field, _)| field == name).map(|(_, value)| value.as_str());
    let mut gains = [0; tone::BANDS.len()];
    for (index, gain) in gains.iter_mut().enumerate() {
        *gain = field(&format!("b{index}"))?.parse().ok()?;
    }
    let chosen = tone::Settings {
        equalizer: field("equalizer")? == "1",
        gains,
        loudness: field("loudness")? == "1",
    };
    let chosen = tone::Settings::from_text(&chosen.to_text())?;
    if let Err(error) = settings::set_tone(chosen) {
        warn!("The sound could not be saved: {error}");
        return None;
    }
    Some(())
}

fn network_group() -> String {
    let joined = settings::network();
    // Dots that stand for a saved password without telling it. The page tells whether the
    // field was touched: until it is, the proxy keeps the password it has, whatever the field
    // holds.
    let password = match &joined {
        Some(joined) if !joined.password.is_empty() => "••••••••",
        _ => "",
    };
    format!(
        "<div class=group><label for=network>Wi-Fi network</label>\
         <input class=input id=network name=network value=\"{}\" maxlength={} required \
         autocomplete=off autocapitalize=none spellcheck=false>\
         <input class=input type=password id=password name=password value=\"{password}\" \
         maxlength={} autocomplete=new-password placeholder=Password>\
         <input type=hidden id=touched name=touched value=0>\
         <div id=networks></div>\
         <button type=button class=\"btn secondary\" id=scan>Scan</button></div>",
        escape(&joined.map(|joined| joined.name).unwrap_or_default()),
        settings::LONGEST_NETWORK,
        settings::LONGEST_PASSWORD
    )
}

fn page(place: Place) -> String {
    let (sound, sound_script) = match place {
        Place::Network { .. } => (sound_group(), SOUND_SCRIPT),
        Place::Setup => (String::new(), ""),
    };
    let (receivers, state, script) = match place {
        Place::Network { .. } => {
            let signal = crate::wifi_signal().map_or(String::new(), |signal| format!(", {signal} dBm"));
            let address = crate::station_address().map_or(String::new(), |address| format!(" at {address}"));
            (
                "<div class=group><span class=title>Plays on</span><div id=receivers></div>\
                 <button type=button class=\"btn secondary\" id=again>Look again</button></div>",
                format!("On its Wi-Fi network{address}{signal}."),
                RECEIVERS_SCRIPT,
            )
        }
        Place::Setup => (
            "",
            "The proxy is on no Wi-Fi network and opened this one to be given one.".to_owned(),
            "scan();",
        ),
    };
    let name = settings::name();
    let (first, rest) = name.split_once(' ').unwrap_or((&name, ""));

    format!(
        "<!doctype html><html lang=en><meta charset=utf-8>\
         <meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>{name}</title><style>{STYLE}</style>\
         <div class=card><div class=logo><h1><span class=accent>{first}</span> {rest}</h1></div>\
         <div class=alert id=note hidden></div>{sound}\
         <form method=post action=/save>\
         <div class=group><label for=name>Name</label>\
         <input class=input id=name name=name value=\"{name}\" maxlength={longest} required autocomplete=off></div>\
         {network}{receivers}\
         <div class=group id=lips><span class=title>Lip sync</span>\
         <label class=band><span>Sound</span><input type=range name=sync min=-{trim} max={trim} \
         step=5 value={sync}><output></output></label></div>\
         <button class=\"btn primary\">Save</button></form>\
         <p class=state>{state}</p></div><script>{NETWORKS_SCRIPT}{SAVE_SCRIPT}{SYNC_SCRIPT}{script}{sound_script}</script>",
        name = escape(&name),
        first = escape(first),
        rest = escape(rest),
        longest = settings::LONGEST_NAME,
        network = network_group(),
        trim = settings::LARGEST_SYNC,
        sync = settings::sync(),
    )
}

/// Saves what the form posts. Returns the note to show, and whether the proxy has to restart.
fn save(request: &Request, place: Place) -> Result<(&'static str, bool), String> {
    let form = request.form();
    let field = |name: &str| {
        form.iter()
            .find(|(field, _)| field == name)
            .map_or("", |(_, value)| value.trim())
    };
    let failed = |error| format!("The settings could not be saved: {error}");

    let name = field("name");
    if name.is_empty() || name.len() > settings::LONGEST_NAME || mdns::host_name(name).is_empty() {
        return Err(format!(
            "The name needs a letter or a digit and takes {} bytes at most.",
            settings::LONGEST_NAME
        ));
    }
    let network = field("network");
    let password = field("password");
    if network.is_empty() || network.len() > settings::LONGEST_NETWORK {
        return Err(format!(
            "The Wi-Fi network needs a name of {} bytes at most.",
            settings::LONGEST_NETWORK
        ));
    }
    if password.len() > settings::LONGEST_PASSWORD {
        return Err(format!(
            "A Wi-Fi password takes {} bytes at most.",
            settings::LONGEST_PASSWORD
        ));
    }
    let receiver = field("receiver");
    if receiver.len() > settings::LONGEST_RECEIVER {
        return Err("That is not the name of a receiver.".to_owned());
    }

    let mut restart = matches!(place, Place::Setup);
    let joined = settings::network();
    let chosen = Network {
        name: network.to_owned(),
        password: if field("touched") == "1" {
            password.to_owned()
        } else {
            joined.as_ref().map(|joined| joined.password.clone()).unwrap_or_default()
        },
    };
    if joined.as_ref() != Some(&chosen) {
        settings::set_network(chosen).map_err(failed)?;
        info!("The proxy now joins the Wi-Fi network {network}");
        restart = true;
    }
    if !receiver.is_empty() && settings::receiver().as_deref() != Some(receiver) {
        settings::set_receiver(receiver).map_err(failed)?;
        info!("The proxy now plays on {receiver}");
    }
    if let Ok(sync) = field("sync").parse::<i16>() {
        if sync.abs() <= settings::LARGEST_SYNC && sync != settings::sync() {
            settings::set_sync(sync).map_err(failed)?;
            info!("The delay reported to the source is now trimmed by {sync} ms");
            restart = true;
        }
    }
    if name != settings::name() {
        settings::set_name(name).map_err(failed)?;
        info!("The proxy is now called {name}");
        restart = true;
    }

    Ok(if restart {
        ("Saved. The proxy restarts: give it ten seconds.", true)
    } else {
        ("Saved.", false)
    })
}

fn answer(request: &Request, place: Place) -> Option<Response> {
    match (request.method.as_str(), request.path.as_str(), place) {
        ("GET", "/", _) => Some(Response::page(page(place))),
        ("GET", "/networks", _) => Some(Response::page(networks_list())),
        ("GET", "/receivers", Place::Network { interface }) => {
            Some(Response::page(receivers_list(interface)))
        }
        ("POST", "/tone", Place::Network { .. }) => Some(match save_sound(request) {
            Some(()) => Response::page(String::new()),
            None => Response::refused(400, "Bad Request"),
        }),
        ("POST", "/save", _) => Some(match save(request, place) {
            Ok((note, restart)) => {
                if restart {
                    thread::spawn(|| {
                        thread::sleep(RESTART_DELAY);
                        unsafe { sys::esp_restart() };
                    });
                }
                Response::page(note.to_owned())
            }
            Err(problem) => Response::page(problem),
        }),
        _ => None,
    }
}

/// Serves the page on the network the proxy joined, for as long as the proxy runs.
pub fn serve(interface: u32) -> io::Result<Infallible> {
    let place = Place::Network { interface };
    httpd::serve(PORT, |request| {
        answer(request, place).unwrap_or_else(|| Response::refused(404, "Not Found"))
    })
}

/// Serves the page on the proxy's access point, whose address is `address`. A phone that joins
/// the access point asks for a page of its maker to see whether the network is open: whatever
/// is asked for leads to the page.
pub fn serve_setup(address: Ipv4Addr) -> io::Result<Infallible> {
    let page = format!("http://{address}/");
    httpd::serve(PORT, |request| {
        ASKED.store(crate::uptime_ms(), Ordering::Relaxed);
        answer(request, Place::Setup).unwrap_or_else(|| Response::found(&page))
    })
}
