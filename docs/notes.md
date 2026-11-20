# Engineering notes

What the firmware does and why, with the measurements each choice rests on. The dates are those of the measurements. For what the proxy is and how to build it, see the [README](../README.md).

## Settings

The proxy serves a page at `http://<name>.local`, where the name is the proxy's own in lower case with hyphens: `http://airplay-proxy.local` until it is changed. The log gives the address at start, and the page shows the IP address, for a computer that does not find `.local` names. Windows 10 and 11 are documented to find them; not tried.

- **Sound**: an equalizer of ten bands, each lowered by up to 24 dB from the top, and a loudness correction. They take effect as they are moved, while the source plays, and are kept. See Sound below.
- **Lip sync**: moves the sound against the picture by up to 300 ms either way, by reporting a longer or a shorter delay to the source. A source reads the delay when the stream opens, so saving another restarts the proxy. The 547 ms of the AirPlay leg were measured from a computer, not from this board, and this trims what the board adds or takes.
- **Wi-Fi network**: the name of the network and its password. A saved password shows as dots that stand for it without telling it. The page tells the proxy whether the field was touched: until it is, the page does not send the field at all, the proxy keeps the password it has, and a browser has no password to offer to keep. Scan lists the networks around, strongest first, and picking one fills in its name. Looking for networks takes the radio off the access point's channel for about a second, so it is refused while the source plays. Saving another network restarts the proxy.
- **Name**: what a Bluetooth source shows and what the `.local` name comes from, 32 bytes at most. Saving a new one restarts the proxy. A source that was paired may show the old name until it is paired again.
- **Plays on**: the AirPlay receivers that answer on the network (`_raop._tcp`, asked through `crates/mdns`), which the page asks for once it shows. Until one is chosen, the receiver built into the firmware shows as chosen. The one chosen is kept by the name of its service and looked up each time the source plays, so it may change address. Choosing another while the source plays moves the sound to it at once.

### Without a network

While no network was chosen, or the one it has is not joined within 30 seconds of the start, the proxy opens an access point without a password, named after itself and the end of its address: `AirPlay Proxy A1B2`. A phone that joins it opens the page by itself, as it does for the sign-in page of a public network: the access point answers every name with its own address (`crates/captive-dns`) and sends every request to the page. There the page looks for the networks around as it opens, and saving restarts the proxy. Bluetooth stays off meanwhile: the one radio does not carry an access point and the audio together.

On 20 November 2026 the proxy held a password its network refused: every access point let it in and dropped it three seconds later (reason 15, the keys were not agreed), and the wait for the network, whose time started over at every such event, never ended, so the access point never opened. The wait now has one deadline. The wrong password came from the page, after a few saves: it then stood for the saved password with a value made anew at every start and took any other value for a new password.

With a network that was not joined, the proxy restarts after five minutes in which nobody asked for the page, and tries that network again: it may have been away when the proxy started.

### The `.local` name

The proxy answers who asks for an address of its name with its IPv4 address and with the word that the name has no other record. A Mac asks for the IPv4 and the IPv6 address together, and gives up on a name that is silent about one of them: tried on 20 November 2026 with a computer answering for a name of its own, where the address alone left the name unfound and the answer above opened the page in 0.2 s. The proxy sends the answer to every host, as multicast DNS asks, and to the asker alone: that night the computer heard no multicast DNS from any other host in 20 seconds, and took an answer sent to it alone.

The settings are kept in flash (NVS, namespace `proxy`). The page and the answers to the `.local` name are served by `crates/httpd` and `crates/mdns`. From a computer, `crates/mdns` listed the HomePod mini, an Apple TV and a Mac on 19 November 2026; neither the page nor the access point was tried on the board yet.

## What happens

1. The proxy joins the Wi-Fi network, then becomes discoverable over Bluetooth.
2. When a source connects, the log tells whether it enabled delay reports.
3. When the source plays, the proxy finds the receiver, opens an AirPlay session with it and relays the audio. The session closes when the source stops.
4. When the source sets the volume, the proxy sets it on the receiver.

The log reports each session that fails, audio discarded because it arrived faster than it left, playback that restarted from silence because the stream stalled, and a gap left because the relay fell behind. Every ten seconds it reports:

- the longest wait for Bluetooth audio;
- how late the latest packet left for the receiver;
- the frames dropped and repeated to follow the clock of the source;
- the timing requests of the receiver, and how much longer the slowest took to arrive than the quickest;
- how strongly the access point is received;
- the time spent shaping the sound;
- the packets the receiver asked for again, those that found the network buffers full, and the gaps, since the session opened.

The receiver follows the clock of the proxy through the answers to its timing requests. A request that waits on the way makes it read that clock wrong by half the wait.

## Finding the receiver

A receiver given by a `.local` name is asked for on the local network each time the source plays, over IPv6 and over IPv4 (`crates/mdns`). A receiver given by its address is used as it is.

Both ways are needed. On 18 November 2026 the HomePod mini answered the question over IPv6 and not over IPv4, and the lookup of ESP-IDF, which asks over IPv4 only, failed at every attempt. From a computer on the same network `crates/mdns` then got the address in 140 to 240 ms. On the board, the session opened 2.8 s after the source started to play, lookup included: as long as with the lookup of ESP-IDF when that worked.

## The Wi-Fi link

The audio is not compressed: 1.4 Mbit/s, 125 packets a second, over a radio that Bluetooth shares. The proxy does not use the 802.11b rates, from 1 to 11 Mbit/s: the slowest of them cannot carry that.

On the evening of 19 November 2026 the access point was received at -68 dBm; that morning, when no packet was lost, at -50 dBm. With a firmware that still used the 802.11b rates, stretches of 20 to 40 seconds in which six packets in ten found the network buffers full alternated with stretches in which none did, and nothing played well. In the bad stretches 46 to 50 packets a second left, about what 1 Mbit/s carries in the share of the radio that Bluetooth leaves. That the Wi-Fi had fallen back to that rate is a guess, not a measurement. Not yet tried without those rates.

### Roaming

The Wi-Fi driver joins the strongest access point of the network and keeps the one it got. On that evening the strongest one, on channel 11, refused the proxy at start, and the proxy stayed on the one at -68 dBm.

`proxy/src/roaming.rs` moves the proxy while its signal is weak:

- at every connection it asks the driver to report a signal below -60 dBm, which the driver does once;
- it then scans for its network, about a second in all, and when another access point is at least 10 dB stronger it disconnects: the reconnection joins the strongest;
- while the signal stays weak it scans again after 1, 2, 4 and 8 minutes and then every 10 minutes; a signal of -55 dBm or more ends the search;
- a scan takes the radio off the access point's channel and would break the sound, so none runs while the source plays. A weak signal reported meanwhile is looked into once the source stops.

A network that goes away is joined again when it comes back. Not yet tried on the board.

## Volume

The proxy presents itself as a speaker with absolute volume (AVRCP). A source that uses it sends the audio at full scale and tells the proxy the volume, from 0 to 127, which the proxy passes to the receiver as an AirPlay volume from -30 to 0 dB, or -144 dB for silence. The log reports each volume the source sets.

A source that does not use absolute volume scales the audio itself, and the volume of the receiver stays as it is. The receiver's own volume cannot be read: until the source sets one, the proxy tells it the volume is at half. A volume changed on the receiver is not passed back to the source.

A Magcubic HY320Pro sends a volume at every step of its own.

## The delay it reports

The proxy reports 792 ms, the sum of three parts:

- 120 ms that ESP-IDF attributes to receiving and decoding the Bluetooth stream. Not measured.
- 200 ms of audio held between Bluetooth and AirPlay, to cover the waits between the bursts in which Bluetooth delivers it. With 80 ms the stream ran dry several times a minute; the log reports the longest wait every ten seconds, to size this figure.
- 472 ms between sending a frame over AirPlay and hearing it: the 250 ms of latency asked of the receiver, for the packets that arrive late or have to be sent again, and what a HomePod mini adds. From a computer it measured 547 ms, see below; with this board and 867 ms reported, the sound came before the lips, and on 20 November 2026 the lips matched by ear with 70 to 75 ms less.

### What it rests on

Two experiments of 18 November 2026, made with programs that are no longer part of the project.

A source that honors delay reports: a Magcubic HY320Pro (Android 14) playing YouTube, with an ESP32 that played the stream 700 ms late. The projector enables delay reports when it connects. With 700 ms reported when the stream opens, the speech matched the lips; with 120.1 ms, the smallest delay ESP-IDF accepts, it trailed them. A report that changes while the stream is open had no visible effect: a source reads it when the stream opens. Not tried: video that reaches the projector over HDMI, and apps other than YouTube.

When a HomePod mini plays: a computer streamed a beep every second through `crates/raop` and timed it on its microphone. Each row is a session of six beeps; the delay is the median.

| Latency asked | Beep heard after |
|---------------|------------------|
| 500 ms        | 788 ms           |
| 250 ms        | 547 ms           |
| 150 ms        | 439 ms           |
| 100 ms        | 386 ms           |
| 50 ms         | 332 ms           |
| 0 ms          | 293 ms           |
| -50 ms        | 240 ms           |
| -100 ms       | 187 ms           |
| -150 ms       | 135 ms           |
| -200 ms       | not heard        |

The HomePod plays about 290 ms later than asked, whatever is asked; 85 ms, which -200 ms asks for, is less than it can do, and it stays silent.

## Following the clock of the source

The source and the proxy count the 44.1 kHz on different clocks, so the audio held between Bluetooth and AirPlay slowly grows or shrinks. The proxy brings it back by dropping or repeating single frames (`crates/delay-line`): one every 64 packets, about twice a second, when the average over two seconds is 480 frames (11 ms) off, four times as often at twice that distance, and more seldom nearer.

## Measurements

On 18 November 2026, with a Magcubic HY320Pro as the source and a HomePod mini as the receiver, a session of 223 seconds played, until the source stopped, without the stream running dry and without a failed session. Over that time the log reported, every ten seconds:

- a longest wait for Bluetooth audio between 89 and 145 ms, which is what the 200 ms held between Bluetooth and AirPlay have to cover;
- one packet sent again at the receiver's request;
- no packet lost to full network buffers.

On the evening of the same day the receiver asked for 78 packets again in two minutes, in bursts, and the sound broke often.

On 19 November 2026 a session was logged for 430 seconds, with the latency asked of the receiver at -50 ms, which plays a frame 240 ms after it is sent:

- the longest wait for Bluetooth audio was between 79 and 184 ms;
- no packet left more than 10 ms late, none was sent again, none was lost, and there was no gap;
- the delay line, which then steered a frame per packet whenever the average was 480 frames off, dropped 889 frames and repeated 409, in bursts of more than 200 within ten seconds, where the clocks had drifted by a few hundred frames at most. The burst of 211 repeated frames followed the wait of 184 ms.

The sound still broke now and then, also during a minute in which the log showed none of the above: the breaks come from something that firmware did not measure. The latency of 250 ms, the count of the timing requests and the steering described above came after this session, and have not been tried yet.

## Sound

Between Bluetooth and AirPlay the audio is plain 16-bit stereo, and `crates/tone` shapes it there:

- an equalizer with ten bands an octave apart, from 31 Hz to 16 kHz, which only takes away: a band at the top passes as it came in, and lower down it is lowered by up to 24 dB. With every band at the top the audio is the same as with the equalizer bypassed, bit for bit;
- a loudness correction with the contour of the loudness button of a hi-fi amplifier of the 1980s: the bass raised by 10 dB, about 7 dB of it at 100 Hz, and the treble by 8 dB, about 6 dB of it at 10 kHz. It is whole up to 58 % of the volume that the source sets and fades to nothing at the top, as with the tapped volume knobs of those amplifiers, which had no fixed figures: these are in the range their owners and service notes report.

A sound that fills the 16 bits has no room above it. An equalizer that raised bands had to lower everything else to make that room, and played quieter than the bypass even with every band raised: tried and dropped on 20 November 2026. The loudness correction does lower the whole by what it raises the bass, and the proxy raises the volume of the receiver by as much. The correction fades towards full volume fast enough for the receiver always to have that room: what is heard is more bass and treble at the same level.

On 20 November 2026, with the processor at 160 MHz, the ten bands took 3.5 s of every ten, a third of one of the two processors. At 240 MHz they took 2.3 s. A band at the top needs no filter, and since then only the bands that were moved are filtered: what that takes was not measured yet. The filters add no delay worth reporting. A HomePod shapes the sound on its own as well, which cannot be turned off: this comes on top. The loudness correction was computed on a computer and not yet heard on the board.

## Memory

Internal memory is short once Wi-Fi and Bluetooth run; the board has 4 MB of external RAM mapped. External RAM takes the allocations above 16 KB, the Wi-Fi and network buffers, the Bluetooth host, and the stacks of the threads that only talk to the network: the two that an AirPlay session starts and the one that answers for the `.local` name. A thread that writes to flash keeps its stack in internal memory, because external RAM is out of reach while flash is written: the page, which saves the settings, is served on the main thread.

On 20 November 2026, with the page and the name each on a thread of its own with a stack in internal memory (16 KB and 6 KB), the two threads of a session could not be created, and every session failed with `Not enough space`. The log tells the free internal memory, and its largest piece, when a session opens and when one fails. The arrangement above was not tried on the board yet.
