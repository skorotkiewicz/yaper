use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream};
use std::error::Error;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
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
        match s.trim().to_lowercase().as_str() {
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
    version = "1.1.0"
)]
struct Args {
    /// Master volume 0.0-1.0.
    #[arg(short, default_value_t = 0.7)]
    volume: f32,

    /// ASMR mode: rain, brush, tingles, mixed.
    #[arg(short, long, default_value = "mixed")]
    mode: String,

    /// Mic reactivity 0.0-1.0.
    #[arg(short, long, default_value_t = 0.6)]
    reactivity: f32,

    /// Binaural beat frequency in Hz, or 0.0 to disable.
    #[arg(short, long, default_value_t = 10.0)]
    binaural: f32,
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn clamp01(value: f32) -> f32 {
    finite_or(value, 0.0).clamp(0.0, 1.0)
}

fn store_f32(atom: &AtomicU32, value: f32) {
    atom.store(clamp01(value).to_bits(), Ordering::Relaxed);
}

fn load_f32(atom: &AtomicU32) -> f32 {
    clamp01(f32::from_bits(atom.load(Ordering::Relaxed)))
}

fn follow_sample(current: f32, target: f32, sample_rate: f32, seconds: f32) -> f32 {
    let denom = (sample_rate * seconds.max(0.001)).max(1.0);
    current + (target - current) * (1.0 / denom).min(1.0)
}

fn follow_block(current: f32, target: f32, block_seconds: f32, seconds: f32) -> f32 {
    let tau = seconds.max(0.001);
    let coeff = 1.0 - (-block_seconds.max(0.0) / tau).exp();
    current + (target - current) * coeff.clamp(0.0, 1.0)
}

fn decay_for(sample_rate: f32, seconds: f32) -> f32 {
    (-1.0 / (sample_rate * seconds.max(0.001))).exp()
}

fn smoothstep(value: f32) -> f32 {
    let t = clamp01(value);
    t * t * (3.0 - 2.0 * t)
}

fn advance_phase(phase: &mut f32, hz: f32, sample_rate: f32) -> f32 {
    *phase += hz.max(0.0) / sample_rate.max(1.0);
    *phase %= 1.0; // Clean, standard wrapping that eliminates rounding stutter
    *phase
}

// fn advance_phase(phase: &mut f32, hz: f32, sample_rate: f32) -> f32 {
//     *phase += hz.max(0.0) / sample_rate.max(1.0);
//     if *phase >= 1.0 {
//         *phase -= phase.floor();
//     }
//     *phase
// }

struct AmbientState {
    fast_rms: AtomicU32,
    slow_rms: AtomicU32,
    motion: AtomicU32,
    peak: AtomicU32,
}

impl AmbientState {
    fn new() -> Self {
        Self {
            fast_rms: AtomicU32::new(0.0f32.to_bits()),
            slow_rms: AtomicU32::new(0.0f32.to_bits()),
            motion: AtomicU32::new(0.0f32.to_bits()),
            peak: AtomicU32::new(0.0f32.to_bits()),
        }
    }
}

struct AmbientAnalyzer {
    sample_rate: f32,
    fast_rms: f32,
    slow_rms: f32,
    motion: f32,
    peak: f32,
    dc: f32,
}

impl AmbientAnalyzer {
    fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            fast_rms: 0.0,
            slow_rms: 0.0,
            motion: 0.0,
            peak: 0.0,
            dc: 0.0,
        }
    }

    fn process<T>(&mut self, data: &[T], channels: usize, shared: &AmbientState)
    where
        T: Sample,
        f32: FromSample<T>,
    {
        let channels = channels.max(1);
        let frames = data.len() / channels;
        if frames == 0 {
            return;
        }

        let dc_coeff = (1.0 / (self.sample_rate * 0.25)).min(1.0);
        let mut sum = 0.0f32;
        let mut hp_sum = 0.0f32;
        let mut peak = 0.0f32;

        for frame in data.chunks_exact(channels) {
            let mut mono = 0.0f32;
            for &sample in frame {
                mono += finite_or(f32::from_sample(sample), 0.0).clamp(-1.5, 1.5);
            }
            mono /= channels as f32;

            self.dc += (mono - self.dc) * dc_coeff;
            let hp = mono - self.dc;
            let abs = mono.abs();

            sum += mono * mono;
            hp_sum += hp * hp;
            peak = peak.max(abs);
        }

        let block_seconds = frames as f32 / self.sample_rate;
        let rms = (sum / frames as f32).sqrt();
        let hp_rms = (hp_sum / frames as f32).sqrt();

        self.fast_rms = follow_block(self.fast_rms, rms, block_seconds, 0.06);
        self.slow_rms = follow_block(self.slow_rms, rms, block_seconds, 3.5);

        let room_lift = (self.fast_rms - self.slow_rms).max(0.0);
        let transient = (peak - self.fast_rms * 2.8).max(0.0);
        let target_motion = clamp01(room_lift * 4.0 + hp_rms * 0.7 + transient * 0.35);
        let motion_tau = if target_motion > self.motion {
            0.03
        } else {
            0.70
        };
        self.motion = follow_block(self.motion, target_motion, block_seconds, motion_tau);

        let peak_tau = if peak > self.peak { 0.01 } else { 0.40 };
        self.peak = follow_block(self.peak, peak, block_seconds, peak_tau);

        store_f32(&shared.fast_rms, self.fast_rms);
        store_f32(&shared.slow_rms, self.slow_rms);
        store_f32(&shared.motion, self.motion);
        store_f32(&shared.peak, self.peak);
    }
}

struct FastRng {
    state: u32,
}

impl FastRng {
    fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0xDEADBEEF } else { seed },
        }
    }

    fn next(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    fn f32(&mut self) -> f32 {
        self.next() as f32 / u32::MAX as f32
    }

    fn white(&mut self) -> f32 {
        self.f32() * 2.0 - 1.0
    }

    fn range(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.f32()
    }
}

struct NoiseGen {
    rng: FastRng,
    brown: f32,
    pink: [f32; 7],
}

impl NoiseGen {
    fn new(seed: u32) -> Self {
        Self {
            rng: FastRng::new(seed),
            brown: 0.0,
            pink: [0.0; 7],
        }
    }

    fn white(&mut self) -> f32 {
        self.rng.white()
    }

    fn brown(&mut self) -> f32 {
        let w = self.white();
        self.brown = (self.brown + 0.02 * w) * 0.998;
        self.brown * 6.0
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

        for p in self.pink.iter_mut().take(5) {
            *p = p.clamp(-15.0, 15.0);
        }

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

struct Trigger {
    amp: f32,
    decay: f32,
    pitch: f32,
    phase: f32,
    noise_amp: f32,
    noise_decay: f32,
    noise_filter: f32,
    noise_rng: FastRng,
}

impl Trigger {
    fn new(seed: u32) -> Self {
        Self {
            amp: 0.0,
            decay: 0.0,
            pitch: 0.0,
            phase: 0.0,
            noise_amp: 0.0,
            noise_decay: 0.0,
            noise_filter: 0.0,
            noise_rng: FastRng::new(seed),
        }
    }

    fn active(&self) -> bool {
        self.amp >= 0.0001 || self.noise_amp >= 0.0001
    }

    fn play(&mut self, sample_rate: f32) -> f32 {
        if !self.active() {
            return 0.0;
        }

        self.phase += self.pitch / sample_rate;
        self.phase %= 1.0;

        let tone = (self.phase * TAU).sin();

        let raw = self.noise_rng.white();
        self.noise_filter += 0.15 * (raw - self.noise_filter);
        self.noise_amp *= self.noise_decay;

        let noise = self.noise_filter * self.noise_amp * 3.5;
        let out = (tone * 0.4 + noise * 0.6) * self.amp;
        self.amp *= self.decay;
        out
    }

    fn trigger_tap(&mut self, rng: &mut FastRng, sample_rate: f32) {
        self.phase = 0.0;
        self.amp = rng.range(0.3, 0.7);
        self.decay = decay_for(sample_rate, rng.range(0.008, 0.023));
        self.pitch = rng.range(1500.0, 5000.0);
        self.noise_amp = 1.0;
        self.noise_decay = decay_for(sample_rate, rng.range(0.006, 0.018));
    }

    fn trigger_crinkle(&mut self, rng: &mut FastRng, sample_rate: f32) {
        self.phase = 0.0;
        self.amp = rng.range(0.2, 0.5);
        self.decay = decay_for(sample_rate, rng.range(0.002, 0.010));
        self.pitch = rng.range(3000.0, 8000.0);
        self.noise_amp = 1.5;
        self.noise_decay = decay_for(sample_rate, rng.range(0.002, 0.008));
    }

    fn trigger_heartbeat(&mut self, rng: &mut FastRng, sample_rate: f32) {
        self.phase = 0.0;
        self.amp = rng.range(0.4, 0.6);
        self.decay = decay_for(sample_rate, 0.20);
        self.pitch = rng.range(40.0, 70.0);
        self.noise_amp = 0.5;
        self.noise_decay = decay_for(sample_rate, 0.05);
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

    fn play(&mut self, sample_rate: f32) -> f32 {
        if self.target_amp < 0.0001 && self.amp < 0.0001 {
            return 0.0;
        }

        self.phase += self.freq / sample_rate;
        self.phase %= 1.0;
        self.age += 1;

        let attack_samples = ((sample_rate * 0.005) as usize).max(1);
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

struct BrushComponent {
    phase: f32,
    speed: f32,
    target_speed: f32,
    filter_state: f32,
}

impl BrushComponent {
    fn new() -> Self {
        Self {
            phase: 0.0,
            speed: 3.0,
            target_speed: 3.0,
            filter_state: 0.0,
        }
    }

    fn set_target_speed(&mut self, speed: f32) {
        self.target_speed = speed.clamp(0.6, 9.0);
    }

    fn play(&mut self, sample_rate: f32, noise: f32) -> f32 {
        self.speed = follow_sample(self.speed, self.target_speed, sample_rate, 0.45);
        self.phase += self.speed / sample_rate;
        self.phase %= 1.0;

        let lfo = (self.phase * TAU).sin() * 0.5 + 0.5;
        let coeff = 0.005 + lfo * 0.05;
        self.filter_state += coeff * (noise - self.filter_state);
        self.filter_state * 2.0
    }
}

struct Binaural {
    phase_l: f32,
    phase_r: f32,
    freq_base: f32,
    target_freq_base: f32,
    freq_diff: f32,
    amp: f32,
    target_amp: f32,
    enabled: bool,
}

impl Binaural {
    fn new(freq_diff: f32) -> Self {
        let enabled = freq_diff.is_finite() && freq_diff > 0.0;
        Self {
            phase_l: 0.0,
            phase_r: 0.0,
            freq_base: 60.0,
            target_freq_base: 60.0,
            freq_diff: freq_diff.clamp(0.0, 40.0),
            amp: 0.0,
            target_amp: if enabled { 0.10 } else { 0.0 },
            enabled,
        }
    }

    fn play(&mut self, sample_rate: f32) -> (f32, f32) {
        if !self.enabled {
            return (0.0, 0.0);
        }

        self.freq_base = follow_sample(self.freq_base, self.target_freq_base, sample_rate, 2.0);
        self.amp = follow_sample(self.amp, self.target_amp, sample_rate, 3.0);
        self.phase_l += self.freq_base / sample_rate;
        self.phase_r += (self.freq_base + self.freq_diff) / sample_rate;
        self.phase_l %= 1.0;
        self.phase_r %= 1.0;

        (
            (self.phase_l * TAU).sin() * self.amp,
            (self.phase_r * TAU).sin() * self.amp,
        )
    }
}

#[derive(Clone)]
struct EngineConfig {
    mode: AsmrMode,
    reactivity: f32,
    volume: f32,
    binaural_freq: f32,
    sample_rate: f32,
    ambient: Arc<AmbientState>,
}

#[derive(Clone, Copy)]
struct ReactiveState {
    fast: f32,
    slow: f32,
    motion: f32,
    peak: f32,
    activity: f32,
    quiet: f32,
    duck: f32,
}

struct AsmrEngine {
    mode: AsmrMode,
    reactivity: f32,
    volume: f32,
    sample_rate: f32,
    ambient: Arc<AmbientState>,
    rng: FastRng,
    noise: NoiseGen,
    tap: Trigger,
    crinkle: Trigger,
    heartbeat: Trigger,
    chime: Chime,
    brush: BrushComponent,
    drone: Binaural,
    trigger_countdown: usize,
    scene_event_countdown: usize,
    mask_gain: f32,
    duck_gain: f32,
    room_fast: f32,
    room_slow: f32,
    room_motion: f32,
    room_peak: f32,
    breathe_phase: f32,
    drone_lfo_phase: f32,
    mixed_scene: usize,
    mixed_next_scene: usize,
    mixed_pos: usize,
    mixed_scene_samples: usize,
    mixed_fade_samples: usize,
    brush_base_speed: f32,
    drone_base_amp: f32,
    chime_scale: [f32; 5],
}

impl AsmrEngine {
    fn new(config: EngineConfig) -> Self {
        let sample_rate = config.sample_rate.max(1.0);
        let mixed_scene_samples = ((sample_rate * 18.0) as usize).max(1);
        let mixed_fade_samples = ((sample_rate * 5.0) as usize).clamp(1, mixed_scene_samples);

        Self {
            mode: config.mode,
            reactivity: config.reactivity,
            volume: config.volume,
            sample_rate,
            ambient: config.ambient,
            rng: FastRng::new(42),
            noise: NoiseGen::new(777),
            tap: Trigger::new(101),
            crinkle: Trigger::new(202),
            heartbeat: Trigger::new(303),
            chime: Chime::new(),
            brush: BrushComponent::new(),
            drone: Binaural::new(config.binaural_freq),
            trigger_countdown: (sample_rate * 0.2) as usize,
            scene_event_countdown: (sample_rate * 2.0) as usize,
            mask_gain: 0.15,
            duck_gain: 1.0,
            room_fast: 0.0,
            room_slow: 0.0,
            room_motion: 0.0,
            room_peak: 0.0,
            breathe_phase: 0.0,
            drone_lfo_phase: 0.0,
            mixed_scene: 0,
            mixed_next_scene: 1,
            mixed_pos: 0,
            mixed_scene_samples,
            mixed_fade_samples,
            brush_base_speed: 3.0,
            drone_base_amp: 0.08,
            chime_scale: [523.25, 587.33, 659.25, 783.99, 880.00],
        }
    }

    fn next_frame(&mut self) -> (f32, f32) {
        let weights = self.scene_weights();
        let [rain_weight, brush_weight, tingle_weight] = weights;
        let env = self.reactive_state();
        self.tick_schedulers(&env, weights);
        self.advance_mixed_scene();

        // Generate independent noise samples for Left and Right channels
        let br_l = self.noise.brown();
        let br_r = self.noise.brown();
        let pk_l = self.noise.pink();
        let pk_r = self.noise.pink();

        // Scale down the target gain to keep the mix safely under digital ceiling (1.0)
        let mask_target = (0.015
            + rain_weight * 0.08  // Reduced from 0.19
            + brush_weight * 0.03
            + tingle_weight * 0.01
            + self.reactivity * (env.slow * 0.08 + env.fast * 0.06))
            .clamp(0.015, 0.35);

        let mask_tau = if mask_target > self.mask_gain {
            0.35
        } else {
            2.00
        };
        self.mask_gain = follow_sample(self.mask_gain, mask_target, self.sample_rate, mask_tau);

        let mut out_l = 0.0f32;
        let mut out_r = 0.0f32;

        // Apply stereo-separated rain noise
        out_l += (br_l * 0.75 + pk_l * 0.25) * self.mask_gain;
        out_r += (br_r * 0.75 + pk_r * 0.25) * self.mask_gain;

        // Whispers
        let breathe_rate = 0.12 + env.quiet * 0.05;
        let breathe =
            (advance_phase(&mut self.breathe_phase, breathe_rate, self.sample_rate) * TAU).sin()
                * 0.5
                + 0.5;
        let whisper_amp = (0.01 + tingle_weight * 0.03 + brush_weight * 0.02)
            * breathe
            * breathe
            * (0.25 + env.quiet * 0.75)
            * env.duck;

        out_l += pk_l * whisper_amp * 0.60;
        out_r += pk_r * whisper_amp * 0.60;

        if brush_weight > 0.01 {
            let speed_target =
                self.brush_base_speed + self.reactivity * (env.activity * 5.0 + env.motion * 2.0);
            self.brush.set_target_speed(speed_target);
            let brush_out = self.brush.play(self.sample_rate, pk_l);
            let brush_gain = (0.04 + brush_weight * 0.10) * (0.65 + env.duck * 0.35);
            out_l += brush_out * brush_gain * 0.72;
            out_r += brush_out * brush_gain * 0.52;
        }

        let delicate_gain = (0.35 + env.quiet * 0.65) * env.duck;

        // Triggers
        out_l += self.tap.play(self.sample_rate) * delicate_gain * 0.62;
        out_r += self.tap.play(self.sample_rate) * delicate_gain * 0.38;

        out_l += self.crinkle.play(self.sample_rate) * delicate_gain * 0.48;
        out_r += self.crinkle.play(self.sample_rate) * delicate_gain * 0.54;

        let hb_out = self.heartbeat.play(self.sample_rate) * (0.15 + rain_weight * 0.45);
        out_l += hb_out * 0.50;
        out_r += hb_out * 0.50;

        let chime_out = self.chime.play(self.sample_rate) * delicate_gain * (0.20 + tingle_weight);
        out_l += chime_out * 0.40;
        out_r += chime_out * 0.60;

        // Binaural Drone
        self.drone.target_amp = self.drone_base_amp * (0.15 + tingle_weight * 0.45) * env.duck;
        let (drone_l, drone_r) = self.drone.play(self.sample_rate);
        let drone_lfo =
            (advance_phase(&mut self.drone_lfo_phase, 0.045, self.sample_rate) * TAU).sin() * 0.5
                + 0.5;
        let drone_mod = 0.40 + drone_lfo * 0.60;

        out_l += drone_l * drone_mod;
        out_r += drone_r * drone_mod;

        // Final soft-clip clamp
        (
            (finite_or(out_l, 0.0) * self.volume).tanh(),
            (finite_or(out_r, 0.0) * self.volume).tanh(),
        )
    }

    // fn next_frame(&mut self) -> (f32, f32) {
    //     let weights = self.scene_weights();
    //     let [rain_weight, brush_weight, tingle_weight] = weights;
    //     let env = self.reactive_state();
    //     self.tick_schedulers(&env, weights);
    //     self.advance_mixed_scene();

    //     let br = self.noise.brown().tanh() * 2.0;
    //     let pk = self.noise.pink().tanh() * 1.5;

    //     let mask_target = (0.025
    //         + rain_weight * 0.19
    //         + brush_weight * 0.06
    //         + tingle_weight * 0.025
    //         + self.reactivity * (env.slow * 0.24 + env.fast * 0.18 + env.motion * 0.10))
    //         .clamp(0.025, 0.70);
    //     let mask_tau = if mask_target > self.mask_gain {
    //         0.35
    //     } else {
    //         2.00
    //     };
    //     self.mask_gain = follow_sample(self.mask_gain, mask_target, self.sample_rate, mask_tau);

    //     let mut out_l = 0.0f32;
    //     let mut out_r = 0.0f32;

    //     let rain_out = (br * 0.82 + pk * 0.18) * self.mask_gain;
    //     out_l += rain_out;
    //     out_r += rain_out;

    //     let breathe_rate = 0.12 + env.quiet * 0.05;
    //     let breathe =
    //         (advance_phase(&mut self.breathe_phase, breathe_rate, self.sample_rate) * TAU).sin()
    //             * 0.5
    //             + 0.5;
    //     let breathe_curve = breathe * breathe;
    //     let whisper_amp = (0.025 + tingle_weight * 0.065 + brush_weight * 0.04)
    //         * breathe_curve
    //         * (0.25 + env.quiet * 0.75)
    //         * env.duck;
    //     out_l += pk * whisper_amp * 0.80;
    //     out_r += pk * whisper_amp * 0.62;

    //     if brush_weight > 0.01 {
    //         let speed_target =
    //             self.brush_base_speed + self.reactivity * (env.activity * 5.0 + env.motion * 2.0);
    //         self.brush.set_target_speed(speed_target);

    //         let brush_out = self.brush.play(self.sample_rate, pk);
    //         let brush_gain = (0.06 + brush_weight * 0.22 + self.reactivity * env.slow * 0.16)
    //             * (0.65 + env.duck * 0.35);
    //         out_l += brush_out * brush_gain * 0.72;
    //         out_r += brush_out * brush_gain * 0.52;
    //     }

    //     let delicate_gain = (0.35 + env.quiet * 0.65) * env.duck;

    //     let tap_out = self.tap.play(self.sample_rate) * delicate_gain;
    //     out_l += tap_out * 0.62;
    //     out_r += tap_out * 0.38;

    //     let crinkle_out = self.crinkle.play(self.sample_rate) * delicate_gain;
    //     out_l += crinkle_out * 0.48;
    //     out_r += crinkle_out * 0.54;

    //     let hb_out = self.heartbeat.play(self.sample_rate) * (0.20 + rain_weight * 0.80);
    //     out_l += hb_out * 0.45;
    //     out_r += hb_out * 0.45;

    //     let chime_out = self.chime.play(self.sample_rate) * delicate_gain * (0.35 + tingle_weight);
    //     out_l += chime_out * 0.30;
    //     out_r += chime_out * 0.72;

    //     self.drone.target_amp = self.drone_base_amp
    //         * (0.20 + rain_weight * 0.20 + tingle_weight * 0.70)
    //         * (0.40 + env.quiet * 0.60)
    //         * env.duck;
    //     let (drone_l, drone_r) = self.drone.play(self.sample_rate);
    //     let drone_lfo =
    //         (advance_phase(&mut self.drone_lfo_phase, 0.045, self.sample_rate) * TAU).sin() * 0.5
    //             + 0.5;
    //     let drone_mod = 0.35 + drone_lfo * 0.65;
    //     out_l += drone_l * drone_mod;
    //     out_r += drone_r * drone_mod;

    //     (
    //         (finite_or(out_l, 0.0) * self.volume).tanh(),
    //         (finite_or(out_r, 0.0) * self.volume).tanh(),
    //     )
    // }

    fn reactive_state(&mut self) -> ReactiveState {
        let fast_target = clamp01(load_f32(&self.ambient.fast_rms) * 8.0);
        let slow_target = clamp01(load_f32(&self.ambient.slow_rms) * 8.0);
        let motion_target = clamp01(load_f32(&self.ambient.motion) * 10.0);
        let peak_target = clamp01(load_f32(&self.ambient.peak) * 3.5);

        self.room_fast = follow_sample(self.room_fast, fast_target, self.sample_rate, 0.03);
        self.room_slow = follow_sample(self.room_slow, slow_target, self.sample_rate, 0.60);
        self.room_motion = follow_sample(
            self.room_motion,
            motion_target,
            self.sample_rate,
            if motion_target > self.room_motion {
                0.025
            } else {
                0.35
            },
        );
        self.room_peak = follow_sample(
            self.room_peak,
            peak_target,
            self.sample_rate,
            if peak_target > self.room_peak {
                0.012
            } else {
                0.50
            },
        );

        let activity = clamp01(
            self.room_fast * 0.50
                + self.room_slow * 0.25
                + self.room_motion * 0.45
                + self.room_peak * 0.15,
        );
        let quiet = (1.0 - self.reactivity * activity).clamp(0.0, 1.0);
        let duck_target = (1.0
            - self.reactivity * clamp01(self.room_motion * 0.55 + self.room_peak * 0.25) * 0.55)
            .clamp(0.45, 1.0);
        let duck_tau = if duck_target < self.duck_gain {
            0.025
        } else {
            0.80
        };
        self.duck_gain = follow_sample(self.duck_gain, duck_target, self.sample_rate, duck_tau);

        ReactiveState {
            fast: self.room_fast,
            slow: self.room_slow,
            motion: self.room_motion,
            peak: self.room_peak,
            activity,
            quiet,
            duck: self.duck_gain,
        }
    }

    fn scene_weights(&self) -> [f32; 3] {
        match self.mode {
            AsmrMode::Rain => [1.0, 0.00, 0.06],
            AsmrMode::Brush => [0.35, 1.0, 0.10],
            AsmrMode::Tingles => [0.12, 0.00, 1.0],
            AsmrMode::Mixed => {
                let mut focus = [0.0f32; 3];
                let fade_start = self
                    .mixed_scene_samples
                    .saturating_sub(self.mixed_fade_samples);
                if self.mixed_pos >= fade_start {
                    let fade_len = self.mixed_fade_samples.max(1) as f32;
                    let t = smoothstep((self.mixed_pos - fade_start) as f32 / fade_len);
                    focus[self.mixed_scene] = 1.0 - t;
                    focus[self.mixed_next_scene] = t;
                } else {
                    focus[self.mixed_scene] = 1.0;
                }

                let bleed = 0.10;
                [
                    bleed + focus[0] * (1.0 - bleed),
                    bleed + focus[1] * (1.0 - bleed),
                    bleed + focus[2] * (1.0 - bleed),
                ]
            }
        }
    }

    fn advance_mixed_scene(&mut self) {
        if !matches!(self.mode, AsmrMode::Mixed) {
            return;
        }

        self.mixed_pos += 1;
        if self.mixed_pos >= self.mixed_scene_samples {
            self.mixed_pos = 0;
            self.mixed_scene = self.mixed_next_scene;
            self.mixed_next_scene = (self.mixed_next_scene + 1) % 3;
        }
    }

    fn tick_schedulers(&mut self, env: &ReactiveState, weights: [f32; 3]) {
        if self.trigger_countdown == 0 {
            self.fire_triggers(env, weights);
        } else {
            self.trigger_countdown -= 1;
        }

        if self.scene_event_countdown == 0 {
            self.evolve_scene(env);
        } else {
            self.scene_event_countdown -= 1;
        }
    }

    fn evolve_scene(&mut self, env: &ReactiveState) {
        let seconds = self.rng.range(4.0, 13.0) + env.activity * self.reactivity * 4.0;
        self.scene_event_countdown = (self.sample_rate * seconds).max(1.0) as usize;

        self.brush_base_speed = self.rng.range(1.4, 4.8);
        self.drone_base_amp = self.rng.range(0.035, 0.11);
        self.drone.target_freq_base = self.rng.range(42.0, 88.0) + env.slow * 18.0;
    }

    fn fire_triggers(&mut self, env: &ReactiveState, weights: [f32; 3]) {
        let [rain_weight, brush_weight, tingle_weight] = weights;
        let busy = clamp01(env.activity * self.reactivity);
        let interval = self.rng.range(0.08, 0.50) + busy * 0.45;
        self.trigger_countdown = (self.sample_rate * interval).max(1.0) as usize;

        let delicate = clamp01(env.quiet * env.duck);
        let stable =
            (1.0 - self.reactivity * (env.motion * 0.75 + env.peak * 0.25)).clamp(0.25, 1.0);

        let tap_prob = ((0.06 + 0.36 * delicate) * tingle_weight * stable).min(0.65);
        let crinkle_prob =
            ((0.04 + 0.26 * delicate) * (brush_weight * 0.85 + tingle_weight * 0.25)).min(0.50);
        let chime_prob = ((0.025 + 0.16 * delicate) * tingle_weight * stable).min(0.35);
        let heartbeat_prob = ((0.010 + 0.055 * busy) * rain_weight).min(0.18);

        if self.rng.f32() < tap_prob {
            self.tap.trigger_tap(&mut self.rng, self.sample_rate);
        }
        if self.rng.f32() < crinkle_prob {
            self.crinkle
                .trigger_crinkle(&mut self.rng, self.sample_rate);
        }
        if self.rng.f32() < heartbeat_prob {
            self.heartbeat
                .trigger_heartbeat(&mut self.rng, self.sample_rate);
        }
        if self.rng.f32() < chime_prob {
            let idx = (self.rng.next() as usize) % self.chime_scale.len();
            let freq = self.chime_scale[idx];
            let decay = decay_for(self.sample_rate, self.rng.range(1.0, 4.5));
            let amp = self.rng.range(0.10, 0.20) * (0.50 + delicate * 0.50);
            self.chime.trigger(freq, amp, decay);
        }
    }
}

macro_rules! dispatch_sample_format {
    ($format:expr, $builder:ident, $label:expr, $($arg:expr),+ $(,)?) => {
        match $format {
            SampleFormat::I8 => Ok($builder::<i8>($($arg),+)?),
            SampleFormat::I16 => Ok($builder::<i16>($($arg),+)?),
            SampleFormat::I32 => Ok($builder::<i32>($($arg),+)?),
            SampleFormat::I64 => Ok($builder::<i64>($($arg),+)?),
            SampleFormat::U8 => Ok($builder::<u8>($($arg),+)?),
            SampleFormat::U16 => Ok($builder::<u16>($($arg),+)?),
            SampleFormat::U32 => Ok($builder::<u32>($($arg),+)?),
            SampleFormat::U64 => Ok($builder::<u64>($($arg),+)?),
            SampleFormat::F32 => Ok($builder::<f32>($($arg),+)?),
            SampleFormat::F64 => Ok($builder::<f64>($($arg),+)?),
            sf => Err(format!("unsupported {} sample format: {sf}", $label).into()),
        }
    };
}

fn setup_input_stream(
    host: &cpal::Host,
    ambient: Arc<AmbientState>,
    err_tx: mpsc::Sender<cpal::StreamError>,
) -> Option<Stream> {
    let input_device = host.default_input_device()?;
    let input_config = match input_device.default_input_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("input config error: {err}");
            return None;
        }
    };

    let name = input_device
        .name()
        .unwrap_or_else(|_| "unknown input".to_string());
    println!("Input:  {name}");
    println!("Input config:  {input_config:?}");

    let stream = match build_input_stream(&input_device, &input_config, ambient, err_tx) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("input stream unavailable: {err}");
            return None;
        }
    };

    match stream.play() {
        Ok(()) => {
            println!("Microphone active: adapting to surroundings.");
            Some(stream)
        }
        Err(err) => {
            eprintln!("input stream could not start: {err}");
            None
        }
    }
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ambient: Arc<AmbientState>,
    err_tx: mpsc::Sender<cpal::StreamError>,
) -> Result<Stream, Box<dyn Error>> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let channels = stream_config.channels as usize;
    let sample_rate = stream_config.sample_rate.0 as f32;

    dispatch_sample_format!(
        config.sample_format(),
        build_input_stream_typed,
        "input",
        device,
        &stream_config,
        channels,
        sample_rate,
        ambient,
        err_tx
    )
}

fn build_input_stream_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    sample_rate: f32,
    ambient: Arc<AmbientState>,
    err_tx: mpsc::Sender<cpal::StreamError>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let mut analyzer = AmbientAnalyzer::new(sample_rate);
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            analyzer.process(data, channels, &ambient);
        },
        move |err| {
            let _ = err_tx.send(err);
        },
        None,
    )
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    engine_config: EngineConfig,
    err_tx: mpsc::Sender<cpal::StreamError>,
) -> Result<Stream, Box<dyn Error>> {
    let stream_config: cpal::StreamConfig = config.clone().into();
    let channels = (stream_config.channels as usize).max(1);

    dispatch_sample_format!(
        config.sample_format(),
        build_output_stream_typed,
        "output",
        device,
        &stream_config,
        channels,
        engine_config,
        err_tx
    )
}

fn build_output_stream_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    engine_config: EngineConfig,
    err_tx: mpsc::Sender<cpal::StreamError>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let mut engine = AsmrEngine::new(engine_config);
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            write_output(data, channels, &mut engine);
        },
        move |err| {
            let _ = err_tx.send(err);
        },
        None,
    )
}

fn write_output<T>(output: &mut [T], channels: usize, engine: &mut AsmrEngine)
where
    T: Sample + FromSample<f32>,
{
    for frame in output.chunks_mut(channels) {
        let (out_l, out_r) = engine.next_frame();
        if channels == 1 {
            frame[0] = T::from_sample((out_l + out_r) * 0.5);
        } else {
            frame[0] = T::from_sample(out_l);
            frame[1] = T::from_sample(out_r);
            // Surround protection: write zero out to supplementary multi-channels
            // instead of a bleeding downmix, maintaining clean binaural parsing.
            for sample in frame.iter_mut().skip(2) {
                *sample = T::from_sample(0.0f32);
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let mode = AsmrMode::from(args.mode.as_str());
    let reactivity = finite_or(args.reactivity, 0.6).clamp(0.0, 1.0);
    let volume = finite_or(args.volume, 0.7).clamp(0.0, 1.0);
    let binaural_freq = finite_or(args.binaural, 0.0).clamp(0.0, 40.0);

    let host = cpal::default_host();
    let ambient = Arc::new(AmbientState::new());

    // Setup communication channel for processing errors across runtime bounds safely
    let (err_tx, err_rx) = mpsc::channel();

    let _input_stream = if reactivity > 0.0 {
        let stream = setup_input_stream(&host, Arc::clone(&ambient), err_tx.clone());
        if stream.is_none() {
            println!("Failed to start microphone. Running in autonomous mode.");
        }
        stream
    } else {
        println!("Reactivity is 0. Running in autonomous mode (microphone disabled).");
        None
    };

    let output_device = host.default_output_device().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no default output device available",
        )
    })?;
    let output_config = output_device.default_output_config()?;
    let output_name = output_device
        .name()
        .unwrap_or_else(|_| "unknown output".to_string());

    println!("Output: {output_name}");
    println!("Output config: {output_config:?}");

    let output_stream = build_output_stream(
        &output_device,
        &output_config,
        EngineConfig {
            mode,
            reactivity,
            volume,
            binaural_freq,
            sample_rate: output_config.sample_rate().0 as f32,
            ambient,
        },
        err_tx,
    )?;

    output_stream.play()?;

    println!("Continuous adaptive ASMR generator is running.");
    println!("Mode: {mode:?} | Reactivity: {reactivity:.2} | Binaural: {binaural_freq:.2} Hz");
    println!("Press Ctrl+C to stop.");

    loop {
        if let Ok(err) = err_rx.try_recv() {
            eprintln!("Fatal stream exception captured: {err}. Closing audio pipelines.");
            break;
        }
        thread::sleep(Duration::from_millis(1000));
    }

    Ok(())
}
