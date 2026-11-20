# History

How the proxy came to be, in the order things were found. The figures are in [the engineering notes](docs/notes.md).

## 18 November 2026: can it work at all?

The idea was a projector whose only wireless audio is Bluetooth, and a HomePod that only takes AirPlay. AirPlay plays late by design, so the question that decided everything came first: does the projector keep its picture in step with a speaker that plays late and says so?

- **A probe for the delay.** An ESP32 played the Bluetooth stream 700 ms late and reported that delay to the source. With the report, speech matched the lips on the projector; without it, speech trailed them. The projector reads the report when the stream opens, and not again.
- **A sender for AirPlay.** A HomePod mini turned out to accept a plain AirPlay 1 session: no pairing, no password, no encryption, uncompressed audio. It plays about 290 ms later than the latency it is asked for, whatever that is, down to a floor between 85 and 135 ms.
- **The first proxy.** Bluetooth in, AirPlay out, on one ESP32. Three defects in the first hour: sessions that died after half a minute, until the proxy learnt to send a sign of life every ten seconds; a single full network buffer that ended a session, now counted as a lost packet; and audio that ran dry several times a minute, until the delay line held 200 ms instead of 80.
- **A name that would not resolve.** After a restart the HomePod's name was no longer found. The HomePod was answering name questions over IPv6 and not over IPv4, and ESP-IDF asks over IPv4 only. The proxy got a resolver of its own, which asks both ways.
- **Volume.** With absolute volume (AVRCP) the volume keys of the projector set the volume of the HomePod.

## 19 November 2026: from working to pleasant

- **Following the clock without being heard.** The delay line corrected the drift between the two clocks one frame per packet, in bursts of two hundred that followed any hiccup of Bluetooth. It now corrects a frame now and then, the more often the farther it is from its mark.
- **More time for the network.** Asking the HomePod to play as early as it could left no time for a packet to be sent again. The latency went from -50 to 250 ms, which the projector makes up for.
- **A weak access point.** On an evening at -68 dBm, six packets in ten never left the board for stretches of half a minute. The slowest Wi-Fi rates, those of 802.11b, cannot carry the stream, and the proxy stopped using them.
- **Roaming.** The driver keeps the access point it got at start, even when the strongest one refused it once. The proxy now moves to a stronger one while its signal is weak and nothing plays.
- **A page.** The name, the receiver and then the Wi-Fi network moved from the firmware to a web page, with an access point of its own for the first setup.

## 20 November 2026: the details

- **The `.local` name on a Mac.** The page answered by address and not by name. A Mac asks for the IPv4 and the IPv6 address together and gives up on a name that is silent about one of them; and that night the network did not carry multicast to it. The proxy now says that it has no other address, and answers the asker directly as well as everyone.
- **Out of memory.** With the page and the name each on a thread of its own, the two threads of an AirPlay session could no longer be created. The threads that only talk to the network moved their stacks to the external RAM, and the page to the main thread, which writes the settings to flash and has to stay in internal memory.
- **A password that locked it out.** The page stood for the saved Wi-Fi password with a random value, and after a few saves took it for a new password. The network refused the proxy for ever, because the wait for it started over at every refusal and the setup network never opened. The wait got one deadline, and the page now tells plainly whether the password field was touched.
- **An equalizer, three times.** The first raised and lowered five bands, and lowered everything to make room for what it raised: touching a band made the sound quieter. The second kept the level fixed and clipped where neighbouring bands added up. The third only takes away: ten bands from the top down, the untouched sound with every band at the top, and nothing to clip.
- **Loudness, twice.** The first was a modern correction, bass only and at low volume only. The second is the button of a 1980s amplifier: bass and treble, over most of the volume, with the HomePod turned up by what the correction takes.
- **Lip sync by ear.** The delay measured from a computer was 75 ms too long for the board. The page got a trim, and the figure found with it became the default.
- **Processor time.** Ten bands took a third of a processor at 160 MHz. The processor went to 240 MHz, and a band at the top is no longer filtered at all.
- **The probes retired.** The programs of the first day, the delay probe and the tone that timed the HomePod, left the repository. Their two results are in the notes.
