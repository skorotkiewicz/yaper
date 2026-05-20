use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
enum AsmrMode {
    Rain,
    Brush,
    Tingles,
    Mixed,
}

impl From<&str> for AsmrMode {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "rain" => Self::Rain,
            "brush" => Self::Brush,
            "tingles" => Self::Tingles,
            _ => Self::Mixed,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "asmr",
    about = "Continuous Adaptive Generative ASMR",
    version = "1.0.0"
)]
struct Args {
    /// Master volume 0.0–1.0 (default 0.7)
    #[arg(short, default_value_t = 0.7)]
    volume: f32,

    /// ASMR mode: rain, brush, tingles, mixed (default mixed)
    #[arg(short, long, default_value = "mixed")]
    mode: String,

    /// Mic reactivity 0.0–1.0 (how much surroundings affect the ASMR, default 0.6)
    #[arg(short, long, default_value_t = 0.6)]
    reactivity: f32,

    /// Binaural beat frequency in Hz (e.g., 4.0 for Theta, 10.0 for Alpha, 0.0 to disable)
    #[arg(short, long, default_value_t = 10.0)]
    binaural: f32,
}

// Deterministic random number generator for the audio thread
struct Lcg {
    state: u32,
}

impl Lcg {
    fn new(seed: u32) -> Self {
        Self { state: seed }
    }
    fn next(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(1103515245).wrapping_add(12345);
        self.state
    }
    fn f32(&mut self) -> f32 {
        self.next() as f32 / u32::MAX as f32
    }
    fn white(&mut self) -> f32 {
        self.f32() * 2.0 - 1.0
    }
}

// --- Noise Generators ---

struct NoiseGen {
    state: u32,
    brown: f32,
    pink: [f32; 7],
}

impl NoiseGen {
    fn new(seed: u32) -> Self {
        Self {
            state: seed,
            brown: 0.0,
            pink: [0.0; 7],
        }
    }

    fn next(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(1103515245).wrapping_add(12345);
        self.state
    }

    fn white(&mut self) -> f32 {
        (self.next() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn brown(&mut self) -> f32 {
        let w = self.white();
        self.brown += 0.02 * w;
        self.brown *= 0.998; // Prevent DC drift
        self.brown * 6.0 // Scale up
    }

    fn pink(&mut self) -> f32 {
        let white = self.white();
        self.pink[0] = 0.99886 * self.pink[0] + white * 0.0555179;
        self.pink[1] = 0.99332 * self.pink[1] + white * 0.0750759;
        self.pink[2] = 0.96900 * self.pink[2] + white * 0.153_852;
        self.pink[3] = 0.86650 * self.pink[3] + white * 0.3104856;
        self.pink[4] = 0.55000 * self.pink[4] + white * 0.5329522;
        self.pink[5] = -0.7616 * self.pink[5] - white * 0.0168980;
        self.pink[6] = white * 0.115926;
        (self.pink[0]
            + self.pink[1]
            + self.pink[2]
            + self.pink[3]
            + self.pink[4]
            + self.pink[5]
            + self.pink[6]
            + self.pink[6]
            + white * 0.5362)
            * 0.11
    }
}

// --- ASMR Voices ---

struct Trigger {
    amp: f32,
    decay: f32,
    noise_burst: f32,
    pitch: f32,
    phase: f32,
}

impl Trigger {
    fn new() -> Self {
        Self {
            amp: 0.0,
            decay: 0.0,
            noise_burst: 0.0,
            pitch: 0.0,
            phase: 0.0,
        }
    }

    fn play(&mut self, sr: f32) -> f32 {
        if self.amp < 0.001 {
            return 0.0;
        }
        self.phase += self.pitch / sr;
        self.phase %= 1.0;

        let click = (self.phase * TAU).sin();
        self.noise_burst *= self.decay.powf(2.0); // Noise fades even faster
        let out = (click * 0.4 + self.noise_burst * 0.6) * self.amp;
        self.amp *= self.decay;
        out
    }

    fn trigger_tap(&mut self, rng: &mut Lcg) {
        self.amp = 0.3 + rng.f32() * 0.4;
        self.decay = (-1.0 / (44100.0 * (0.008 + rng.f32() * 0.015))).exp(); // 8-23ms
        self.pitch = 1500.0 + rng.f32() * 3500.0;
        self.noise_burst = rng.white();
    }

    fn trigger_crinkle(&mut self, rng: &mut Lcg) {
        self.amp = 0.2 + rng.f32() * 0.3;
        self.decay = (-1.0 / (44100.0 * (0.002 + rng.f32() * 0.008))).exp(); // 2-10ms
        self.pitch = 3000.0 + rng.f32() * 5000.0;
        self.noise_burst = rng.white() * 1.5;
    }

    fn trigger_heartbeat(&mut self, rng: &mut Lcg) {
        self.amp = 0.4 + rng.f32() * 0.2;
        self.decay = (-1.0_f32 / (44100.0 * 0.2)).exp(); // 200ms thud
        self.pitch = 40.0 + rng.f32() * 30.0;
        self.noise_burst = rng.white() * 0.5;
    }
}

struct Chime {
    phase: f32,
    freq: f32,
    amp: f32,
    target_amp: f32,
    decay: f32,
    age: usize,
}

impl Chime {
    fn new() -> Self {
        Self {
            phase: 0.0,
            freq: 0.0,
            amp: 0.0,
            target_amp: 0.0,
            decay: 0.0,
            age: 0,
        }
    }

    fn play(&mut self, sr: f32) -> f32 {
        if self.target_amp < 0.001 {
            return 0.0;
        }
        self.phase += self.freq / sr;
        self.phase %= 1.0;

        self.age += 1;
        let attack_samples = (sr * 0.005) as usize; // 5ms soft attack to avoid clicking
        let attack_env = if self.age < attack_samples {
            self.age as f32 / attack_samples as f32
        } else {
            1.0
        };

        self.amp += (self.target_amp * attack_env - self.amp) * 0.1;
        let out = (self.phase * TAU).sin() * self.amp;
        self.target_amp *= self.decay;
        out
    }

    fn trigger(&mut self, freq: f32, amp: f32, decay: f32) {
        self.phase = 0.0;
        self.freq = freq;
        self.target_amp = amp;
        self.amp = 0.0;
        self.decay = decay;
        self.age = 0;
    }
}

struct Brush {
    phase: f32,
    speed: f32,
    filter_state: f32,
}

impl Brush {
    fn new() -> Self {
        Self {
            phase: 0.0,
            speed: 3.0,
            filter_state: 0.0,
        }
    }

    fn play(&mut self, sr: f32, noise: f32) -> f32 {
        self.phase += self.speed / sr;
        self.phase %= 1.0;
        let lfo = (self.phase * TAU).sin() * 0.5 + 0.5; // 0.0 to 1.0

        // Modulate filter cutoff to simulate physical brushing texture
        let coeff = 0.005 + lfo * 0.05;
        self.filter_state += coeff * (noise - self.filter_state);
        self.filter_state * 2.0
    }
}

#[inline(always)]
fn cubic_clip(x: f32) -> f32 {
    if x <= -1.0 {
        -2.0 / 3.0
    } else if x >= 1.0 {
        2.0 / 3.0
    } else {
        x - (x * x * x) / 3.0
    }
}

struct FastBinaural {
    // Current sample states
    sin_l: f32,
    cos_l: f32,
    sin_r: f32,
    cos_r: f32,

    // Step coefficients (rotation matrix constants)
    k_sin_l: f32,
    k_cos_l: f32,
    k_sin_r: f32,
    k_cos_r: f32,

    freq_base: f32,
    freq_diff: f32,
    amp: f32,
    target_amp: f32,
}

impl FastBinaural {
    fn new(freq_diff: f32, sr: f32) -> Self {
        let mut s = Self {
            sin_l: 0.0,
            cos_l: 1.0, // Cosine starts at 1.0, Sine at 0.0
            sin_r: 0.0,
            cos_r: 1.0,
            k_sin_l: 0.0,
            k_cos_l: 0.0,
            k_sin_r: 0.0,
            k_cos_r: 0.0,
            freq_base: 60.0,
            freq_diff,
            amp: 0.0,
            target_amp: 0.12,
        };
        s.update_coefficients(sr);
        s
    }

    // Call this ONLY when freq_base or freq_diff changes (Control Rate)
    fn update_coefficients(&mut self, sr: f32) {
        use std::f32::consts::TAU;
        let omega_l = (self.freq_base * TAU) / sr;
        let omega_r = ((self.freq_base + self.freq_diff) * TAU) / sr;

        self.k_sin_l = omega_l.sin();
        self.k_cos_l = omega_l.cos();
        self.k_sin_r = omega_r.sin();
        self.k_cos_r = omega_r.cos();
    }

    #[inline(always)]
    fn play(&mut self, sr: f32) -> (f32, f32) {
        let smooth_coeff = 1.0 / (sr * 3.0);
        self.amp += (self.target_amp - self.amp) * smooth_coeff;

        // Vector rotation for Left channel (No transcendental math!)
        let next_sin_l = self.sin_l * self.k_cos_l + self.cos_l * self.k_sin_l;
        self.cos_l = self.cos_l * self.k_cos_l - self.sin_l * self.k_sin_l;
        self.sin_l = next_sin_l;

        // Vector rotation for Right channel
        let next_sin_r = self.sin_r * self.k_cos_r + self.cos_r * self.k_sin_r;
        self.cos_r = self.cos_r * self.k_cos_r - self.sin_r * self.k_sin_r;
        self.sin_r = next_sin_r;

        (self.sin_l * self.amp, self.sin_r * self.amp)
    }
}

// struct Binaural {
//     phase_l: f32,
//     phase_r: f32,
//     freq_base: f32,
//     freq_diff: f32,
//     amp: f32,
//     target_amp: f32,
// }

// impl Binaural {
//     fn new(freq_diff: f32) -> Self {
//         Self {
//             phase_l: 0.0,
//             phase_r: 0.0,
//             freq_base: 60.0,
//             freq_diff,
//             amp: 0.0,
//             target_amp: 0.12,
//         }
//     }

//     fn play(&mut self, sr: f32) -> (f32, f32) {
//         let smooth_coeff = 1.0 / (sr * 3.0); // 3 seconds smoothing
//         self.amp += (self.target_amp - self.amp) * smooth_coeff;

//         self.phase_l += self.freq_base / sr;
//         self.phase_r += (self.freq_base + self.freq_diff) / sr;
//         self.phase_l %= 1.0;
//         self.phase_r %= 1.0;

//         (
//             (self.phase_l * TAU).sin() * self.amp,
//             (self.phase_r * TAU).sin() * self.amp,
//         )
//     }
// }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mode = AsmrMode::from(args.mode.as_str());
    let reactivity = args.reactivity;
    let binaural_freq = args.binaural;
    let volume = args.volume;

    let host = cpal::default_host();

    // --- Input Stream (Microphone) ---
    let ambient_level = Arc::new(AtomicU32::new(0));
    let ambient_level_in = ambient_level.clone();

    let mut mic_active = false;
    if let Some(input_device) = host.default_input_device()
        && let Ok(input_config) = input_device.default_input_config()
    {
        // let in_channels = input_config.channels() as usize;
        let err_fn = |err: cpal::StreamError| eprintln!("Input error: {}", err);

        println!("🎤 Input: {}", input_device.name()?);
        println!("⚙️ Input Config: {:?}", input_config);

        let input_stream = input_device.build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if !data.is_empty() {
                    let mut sum = 0.0f32;

                    for &sample in data {
                        sum += sample * sample;
                    }

                    let rms = (sum / data.len() as f32).sqrt();

                    // Smooth the RMS to avoid jitter, map to u32
                    let prev = ambient_level_in.load(Ordering::Relaxed) as f32 / 10000.0;
                    let smoothed = prev + (rms - prev) * 0.2;

                    ambient_level_in.store((smoothed * 10000.0) as u32, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?;

        // let input_stream = input_device.build_input_stream(
        //     &input_config.into(),
        //     move |data: &[f32], _: &cpal::InputCallbackInfo| {
        //         if !data.is_empty() && in_channels > 0 {
        //             let frames = data.len() / in_channels;
        //             if frames > 0 {
        //                 let mut sum = 0.0f32;
        //                 for frame in data.chunks(in_channels) {
        //                     let sample = frame[0];
        //                     sum += sample * sample;
        //                 }
        //                 let rms = (sum / frames as f32).sqrt();
        //                 // Smooth the RMS to avoid jitter, map to u32
        //                 let prev = ambient_level_in.load(Ordering::Relaxed) as f32 / 10000.0;
        //                 let smoothed = prev + (rms - prev) * 0.2;
        //                 ambient_level_in.store((smoothed * 10000.0) as u32, Ordering::Relaxed);
        //             }
        //         }
        //     },
        //     err_fn,
        //     None,
        // )?;

        input_stream.play()?;
        std::mem::forget(input_stream); // Prevent drop, keep stream alive
        mic_active = true;
        println!("🎤 Microphone active: Listening to surroundings...");
    }

    if !mic_active {
        println!("⚠️ No microphone found. Running in autonomous mode (no reactivity).");
    }

    // --- Output Stream (Speakers/Headphones) ---
    let output_device = host
        .default_output_device()
        .expect("No output device found");
    println!("🎧 Output: {}", output_device.name()?);
    let output_config = output_device.default_output_config()?;
    println!("⚙️ Config: {:?}", output_config);

    let out_sr = output_config.sample_rate().0 as f32;
    let channels = output_config.channels() as usize;

    let mut rng = Lcg::new(42);
    let mut noise = NoiseGen::new(777);
    let mut tap = Trigger::new();
    let mut crinkle = Trigger::new();
    let mut heartbeat = Trigger::new();
    let mut chime = Chime::new();
    let mut brush = Brush::new();
    let mut drone = FastBinaural::new(binaural_freq, out_sr);

    let mut sample_counter: usize = 0;
    let mut next_trigger_time: usize = (out_sr * 0.2) as usize;
    let mut current_mask_gain: f32 = 0.2;
    let mut scene_counter: usize = 0;

    let chime_scale: &[f32] = &[523.25, 587.33, 659.25, 783.99, 880.00]; // C5 Pentatonic

    let err_fn = |err: cpal::StreamError| eprintln!("Output error: {}", err);

    // --- Scene Cycling for Mixed Mode ---
    let mut mixed_scene: usize = 0; // 0: Rain, 1: Brush, 2: Tingles
    let mut mixed_scene_counter: usize = 0;
    let mixed_scene_duration = (out_sr * 15.0) as usize; // 15s per focus

    let output_stream = output_device.build_output_stream(
        &output_config.into(),
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(channels) {
                sample_counter += 1;

                // Read ambient noise level (0.0 to ~1.0)
                let mic_rms = (ambient_level.load(Ordering::Relaxed) as f32) / 10000.0;

                // Scene cycling for Mixed mode
                if matches!(mode, AsmrMode::Mixed) {
                    mixed_scene_counter += 1;
                    if mixed_scene_counter >= mixed_scene_duration {
                        mixed_scene_counter = 0;
                        mixed_scene = (mixed_scene + 1) % 3;
                    }
                }

                // Scene weight: 1.0 if active, 0.15 bleed for smooth crossfade
                let scene_weight = |target: usize| -> f32 {
                    if matches!(mode, AsmrMode::Mixed) {
                        if mixed_scene == target { 1.0 } else { 0.15 }
                    } else {
                        1.0
                    }
                };

                // --- Organic Trigger Timing ---
                if sample_counter >= next_trigger_time {
                    sample_counter = 0;
                    // Randomize next trigger time (100ms to 500ms) to prevent habituation
                    next_trigger_time = (out_sr * (0.1 + rng.f32() * 0.4)) as usize;

                    // Slow scene evolution (kept for drone/brush params)
                    scene_counter += 1;
                    if scene_counter >= 20 {
                        scene_counter = 0;
                        drone.target_amp = 0.05 + rng.f32() * 0.1;
                        drone.freq_base = 40.0 + rng.f32() * 60.0;
                        brush.speed = 2.0 + rng.f32() * 4.0;
                    }

                    // --- Adaptive Probability Math (Scene-Aware) ---
                    let t_prob = if scene_weight(2) > 0.1 {
                        (0.5 - mic_rms * reactivity * 1.0).max(0.0) * scene_weight(2)
                    } else {
                        0.0
                    };

                    let c_prob = if scene_weight(1) > 0.1 {
                        (0.3 - mic_rms * reactivity * 0.8).max(0.0) * scene_weight(1)
                    } else {
                        0.0
                    };

                    let ch_prob = if scene_weight(2) > 0.1 {
                        (0.25 - mic_rms * reactivity * 0.5).max(0.0) * scene_weight(2)
                    } else {
                        0.0
                    };

                    let hb_prob = if scene_weight(0) > 0.1 { 0.04 } else { 0.0 };

                    if rng.f32() < t_prob {
                        tap.trigger_tap(&mut rng);
                    }
                    if rng.f32() < c_prob {
                        crinkle.trigger_crinkle(&mut rng);
                    }
                    if rng.f32() < hb_prob {
                        heartbeat.trigger_heartbeat(&mut rng);
                    }

                    if rng.f32() < ch_prob {
                        let freq = chime_scale[(rng.next() as usize) % chime_scale.len()];
                        let decay = (-1.0 / (out_sr * (1.0 + rng.f32() * 3.0))).exp();
                        chime.trigger(freq, 0.15 + rng.f32() * 0.1, decay);
                    }
                }

                // --- Synthesis ---
                let _w = noise.white();
                let br = noise.brown();
                let pk = noise.pink();

                // Dynamic Masking: Scales with current Mixed scene focus
                let mask_target = match mode {
                    AsmrMode::Rain => 0.25 + mic_rms * reactivity * 2.5,
                    AsmrMode::Brush => 0.15 + mic_rms * reactivity * 1.5,
                    AsmrMode::Tingles => 0.05 + mic_rms * reactivity * 0.5,
                    AsmrMode::Mixed => match mixed_scene {
                        0 => 0.25 + mic_rms * reactivity * 2.5, // Rain focus
                        1 => 0.15 + mic_rms * reactivity * 1.5, // Brush focus
                        _ => 0.05 + mic_rms * reactivity * 0.5, // Tingles focus
                    },
                };

                let smooth_coeff = 1.0 / (out_sr * 2.0); // 2 seconds smoothing
                current_mask_gain += (mask_target - current_mask_gain) * smooth_coeff;

                let mut out_l = 0.0f32;
                let mut out_r = 0.0f32;

                // Rain / Masking foundation (Brown + Pink mix)
                let rain_out = (br * 0.8 + pk * 0.2) * current_mask_gain;
                out_l += rain_out;
                out_r += rain_out;

                // Whisper/Breathe (Pink noise with slow LFO, fades if room is loud)
                let breathe_lfo = ((sample_counter as f32 / out_sr) * 0.15 * TAU).sin() * 0.5 + 0.5;
                let whisper_amp =
                    0.1 * breathe_lfo.powf(2.0) * (1.0 - (mic_rms * reactivity * 3.0).min(1.0));
                out_l += pk * whisper_amp * 0.8;
                out_r += pk * whisper_amp * 0.6; // Slight off-center

                // Brushing only active during Brush scene in Mixed mode
                if matches!(mode, AsmrMode::Brush)
                    || (matches!(mode, AsmrMode::Mixed) && mixed_scene == 1)
                {
                    let brush_out = brush.play(out_sr, pk);
                    let brush_gain = 0.25 + mic_rms * reactivity * 0.5;
                    out_l += brush_out * brush_gain * 0.7;
                    out_r += brush_out * brush_gain * 0.5;
                }

                // Triggers
                let tap_out = tap.play(out_sr);
                out_l += tap_out * 0.6; // Stereo field placement
                out_r += tap_out * 0.4;

                let crinkle_out = crinkle.play(out_sr);
                out_l += crinkle_out * 0.5;
                out_r += crinkle_out * 0.5;

                let hb_out = heartbeat.play(out_sr);
                out_l += hb_out * 0.5;
                out_r += hb_out * 0.5;

                let chime_out = chime.play(out_sr);
                out_l += chime_out * 0.3;
                out_r += chime_out * 0.7; // Shimmering panned chimes

                // Binaural Drone
                let (drone_l, drone_r) = drone.play(out_sr);
                let drone_lfo = ((sample_counter as f32 / out_sr) * 0.05 * TAU).sin() * 0.5 + 0.5;
                out_l += drone_l * drone_lfo;
                out_r += drone_r * drone_lfo;

                // Soft clip & Master Volume
                // out_l = (out_l * volume).tanh();
                // out_r = (out_r * volume).tanh();
                out_l = cubic_clip(out_l * volume);
                out_r = cubic_clip(out_r * volume);

                // Channel Assignment
                if channels == 1 {
                    frame[0] = (out_l + out_r) * 0.5;
                } else if channels >= 2 {
                    frame[0] = out_l;
                    frame[1] = out_r;
                    for item in frame.iter_mut().take(channels).skip(2) {
                        *item = (out_l + out_r) * 0.5;
                    }
                }
            }
        },
        err_fn,
        None,
    )?;

    output_stream.play()?;

    println!("✨ Autonomous ASMR Generator v1.0.0 — Continuous & Adaptive");
    println!(
        "Mode: {:?} | Reactivity: {} | Binaural: {}Hz",
        mode, reactivity, binaural_freq
    );
    println!("Press Ctrl+C to stop.\n");

    loop {
        thread::sleep(Duration::from_millis(1000));
    }
}
