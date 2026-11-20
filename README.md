# AirPlay Proxy

A Bluetooth speaker that plays on an AirPlay speaker, on an ESP32 board that costs a few euros.

Anything that sends audio over Bluetooth, such as a projector, a TV or a turntable, sees a speaker called `AirPlay Proxy`. What it plays comes out of an AirPlay receiver on the Wi-Fi network: a HomePod, for one. The proxy tells the source how late the sound is, so that a source which honors that keeps its video in step with it.

<img src="docs/settings-page.jpg" alt="The settings page of the proxy" width="360">

The settings page, with made-up names.

## What it does

- **Bluetooth in, AirPlay out.** An A2DP sink (SBC, 44.1 kHz) on one side, an AirPlay 1 (RAOP) sender on the other. The audio crosses uncompressed.
- **Lip sync.** It reports its delay to the source (A2DP delay reporting). A source that honors the report holds its video back by as much. The page trims it in steps of 5 ms.
- **Volume.** It presents itself as a speaker with absolute volume (AVRCP): the volume keys of the source set the volume of the receiver.
- **A page instead of a display.** At `http://airplay-proxy.local` you name the proxy, choose its Wi-Fi network and the receiver it plays on, and shape the sound. Choosing another receiver moves the sound to it at once.
- **Setup without a cable.** With no network, or one it cannot join, it opens its own Wi-Fi network, and a phone that joins it lands on the page.
- **Equalizer and loudness.** Ten bands that only take away, so that nothing ever clips and every band at the top is the untouched sound; and the loudness button of a 1980s amplifier, bass and treble.
- **Roaming.** With several access points on one network it moves to a stronger one while nothing plays.

## What it was tried with

| | |
|---|---|
| Source | Magcubic HY320Pro projector (Android 14), which honors delay reports |
| Receiver | HomePod mini, software 18.6 |
| Board | ESP32-WROVER-E, 16 MB of flash, 8 MB of PSRAM |

A receiver has to accept an AirPlay 1 session without pairing, password or encryption, as a HomePod mini does. Others were found on the network and never played on.

A source that ignores delay reports plays its video about 0.8 s ahead of the sound: fine for music, not for films.

## Hardware

It needs an **original ESP32 with PSRAM** and 4 MB of flash or more: the module is called **ESP32-WROVER-E**, or ESP32-WROVER-IE with a connector for an external antenna.

- The original ESP32 is the one with Bluetooth Classic, which carries audio. An ESP32-S2, S3, C3, C6 or H2 has none and cannot do this.
- An ESP32-WROOM has no PSRAM, and the firmware does not start without it.

Boards with that module, of which only the first kind was tried:

- a development board with an ESP32-WROVER-E, such as those of Waveshare;
- Espressif ESP32-DevKitC-VE, or ESP32-DevKitC-VIE for an external antenna;
- LILYGO T7 V1.5;
- Freenove ESP32-WROVER.

One radio and one printed antenna serve Bluetooth and Wi-Fi in turns, and the audio is 1.4 Mbit/s. Keep the board a few metres from an access point and leave the end of the module with the antenna free: a hand closed around it stops the sound. In a case, PETG stands the warmth better than PLA, and neither stops the radio. Where the signal is weak, take the module with the external antenna.

## Installing

### A firmware that is already built

Every push builds the firmware (see [the workflow](.github/workflows/firmware.yml)) and keeps `airplay-proxy.bin`, one image for the whole flash. With [espflash](https://github.com/esp-rs/espflash):

```bash
espflash write-bin 0x0 airplay-proxy.bin
```

### Building it

With the Rust toolchain for Espressif chips ([espup](https://github.com/esp-rs/espup)), `ldproxy` and `espflash`:

```bash
source ~/export-esp.sh
make -C proxy flash
```

The first build downloads ESP-IDF v5.4.1 and its toolchain into `proxy/.embuild`, a few gigabytes. `make -C proxy build` only builds.

A Wi-Fi network and a receiver can be built in, to hold until others are chosen on the page: copy `proxy/local.mk.example` to `proxy/local.mk`, which git ignores, and fill it in.

## First start

1. Power the board. After half a minute without a network it opens the Wi-Fi network `AirPlay Proxy` followed by four characters, without a password.
2. Join it with a phone. The page opens by itself; if it does not, go to `http://192.168.71.1`.
3. Pick your network from the list, type its password and save. The proxy restarts and joins it.
4. Open `http://airplay-proxy.local` and choose the receiver under *Plays on*.
5. On the source, pair the Bluetooth speaker `AirPlay Proxy` and play.

A computer that does not find `.local` names reaches the page by the address that the page itself and the serial log show.

## How it works

```
source ──Bluetooth A2DP──▶ delay line ──▶ equalizer, loudness ──▶ AirPlay 1 ──Wi-Fi──▶ receiver
        ◀──delay report──                                        ──volume──▶
        ──volume (AVRCP)──▶
```

- The delay line holds 200 ms of audio, to cover the bursts in which Bluetooth delivers it, and follows the clock of the source by dropping or repeating a single frame now and then.
- The receiver is asked to play a quarter of a second after each packet is sent, which leaves time for a packet that is late or has to be sent again.
- The proxy reports 792 ms to the source: what Bluetooth, the delay line and AirPlay take together.

[The engineering notes](docs/notes.md) give the reasons and the measurements behind each of these, and [the history](HISTORY.md) tells how it got there.

## The code

| | |
|---|---|
| `proxy/` | the firmware: Bluetooth, Wi-Fi and roaming, settings, the page |
| `crates/raop` | the AirPlay 1 sender |
| `crates/delay-line` | the delay line that follows the clock of the source |
| `crates/tone` | the equalizer and the loudness correction |
| `crates/mdns` | finding hosts and services by name, and answering for the proxy's own |
| `crates/httpd` | the web server of the page |
| `crates/captive-dns` | the DNS server that leads a phone to the page |

The crates are plain Rust and have their tests, which run on a computer:

```bash
cd crates/raop && cargo test
```

## Limits

- Sources at 44.1 kHz only, the sample rate of AirPlay 1.
- One receiver at a time.
- Wi-Fi on 2.4 GHz only, WPA2 with a password or open.
- The page has no password: whoever is on the network can change the settings, and whoever is near can join the setup network.
- A volume changed on the receiver is not passed back to the source.

## License

[MIT](LICENSE).
