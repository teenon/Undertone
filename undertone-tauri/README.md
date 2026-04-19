# Undertone Tauri Desktop App

Linux desktop mixer for Elgato Wave devices, modelled after Wave Link.
Talks to `undertone-daemon` over the existing Unix-socket IPC; the
frontend is React + TypeScript + Tailwind, the Tauri shell is Rust.

## Build

```sh
cargo tauri build --no-bundle           # release binary, no installer
cargo tauri dev                         # dev with HMR
```

System packages (Ubuntu 24.04 / Mint 22.3):

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev
```

The `src-tauri/` Cargo workspace is intentionally standalone (empty
`[workspace]` table) to keep Tauri's large dep tree out of the main
workspace lockfile.

## Known Wave XLR caveats on Linux

These are operational gotchas discovered while bringing up the Wave XLR
on Mint 22.3 / kernel 6.14. They are **not** specific to Tauri — they
apply to any client of `undertone-daemon` on Linux.

### 1. Start order: daemon must come up after `snd-usb-audio` finishes init

If the daemon claims USB interface 3 (the vendor-specific control
interface) **while** `snd-usb-audio` is still issuing its init mixer
queries on EP0, the kernel transfers time out (`-110 ETIMEDOUT`) and
`snd-usb-audio` fails to register the device's PCM streams. The Wave
XLR then shows up as a USB mixer-only card with no recordable/playable
device, and it won't appear in PipeWire / sound settings.

**Workflow:**

- Plug the Wave XLR in **first**, wait a couple of seconds, **then**
  start the daemon.
- If the device gets into the stuck state (visible because
  `arecord -l` doesn't list the Wave XLR while `/proc/asound/card1/`
  exists with only `usbmixer`), the recovery is:
  1. `pkill undertone-daemon` (and the Tauri app)
  2. Unplug the Wave XLR USB cable, wait 3 s, plug it back in
  3. `systemctl --user restart wireplumber`
  4. Restart the daemon

A defensive fix in the daemon (refuse to claim until PCM streams are
visible in `/proc/asound`) is a known follow-up.

### 2. Headphone monitor mix is firmware-controlled

The Wave XLR's rotary knob has multiple modes (volume / gain /
crossfader). When set to "mic monitor only" — or with the crossfader
dialled fully to one side — USB-stream audio reaches the device but
**isn't routed to the headphone jack**. PipeWire shows the link as
active and the device's level LEDs respond, but no sound comes out of
the headphones.

**Workaround:** press the rotary knob on the Wave XLR to cycle modes
or rebalance. The byte that controls this lives in the 34-byte state
blob but hasn't been decoded — it sits in one of the "opaque" regions
(offsets 5–8 or 11–15) that didn't change across the Wireshark
captures we have. A follow-up Wireshark capture of toggling the
crossfader in Wave Link on Windows is needed to identify it.

### 3. Mic mute is dual-state on Linux

The Wave XLR has two independent mute states:

- **Firmware mute** — byte 4 of the vendor state blob; toggled by the
  device's tag button and by the daemon's `SetMicMute` command. This
  is what physically silences the mic at the device level.
- **ALSA capture switch** — `numid=3` on the ALSA card; settable via
  `amixer -c 1 sset Mic cap/nocap`. This is what PipeWire and other
  audio tools see.

Our daemon writes only the firmware mute (via interface 3) because
that's what the tag button drives, and reading it back lets the UI
reflect physical button presses. Other tools may set the ALSA mute
independently, and the two can disagree silently. If your mic is
audibly muted but our UI shows unmuted, run
`amixer -c 1 sget Mic | grep Mono` — if it shows `[off]`, that's the
ALSA mute set externally; `amixer -c 1 sset Mic cap` resets it.

### 4. Mic gain is also dual-state

Same as mute. The daemon's `SetMicGain` writes the firmware gain
(0..0x3FFF u16) via the vendor protocol; ALSA's `Mic Capture Volume`
(0..150) is independent. Wave Link on Windows uses only the firmware
gain; PipeWire's mixer can reach the ALSA gain. The two are
multiplicative if both are non-trivial.
