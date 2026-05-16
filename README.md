# yaper

Minimal audio threshold detector and tone-pulse generator.

## Binaries

### yaper (threshold detector)

| Flag | Default | Description |
|------|---------|-------------|
| `-t, --threshold` | `0.7` | Amplitude trigger threshold (0.0 – 1.0) |
| `-f, --frequency` | `1200` | Pulse tone frequency in Hz |
| `-d, --pulse-duration` | `100` | Pulse length in milliseconds |
| `-c, --cooldown` | `5` | Wait time after pulse in milliseconds |

### daf (Delayed Auditory Feedback)

Applies a configurable delay to microphone input, useful for speech therapy.

| Flag | Default | Description |
|------|---------|-------------|
| `-t, --threshold` | `0.05` | Input amplitude threshold (0.0–1.0) |
| `-r, --release` | `100` | Release time in ms after signal drops |
| `-d, --delay` | `200` | Delay time in milliseconds |

## Install / Build

```bash
cargo build --release
```

Binaries end up in `target/release/`.

## Usage

### yaper

```bash
yaper [OPTIONS]
```

### daf

```bash
daf --delay 200
```

## Requirements

- ALSA / PulseAudio / PipeWire on Linux (for `cpal`).
- A working default input and output device.

## License

MIT