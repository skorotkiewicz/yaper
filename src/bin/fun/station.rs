use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "station")]
#[command(about = "Continuous Generative Music", version = "0.3.0")]
struct Args {
    /// Master volume 0.0–1.0 (default 0.5)
    #[arg(short, default_value_t = 0.5)]
    volume: f32,
}

// Simple deterministic random number generator for the audio thread
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
}

// --- Synthesis Voices ---

struct Voice {
    phase: f32,
    freq: f32,
    amp: f32,
    decay: f32,
    wave: u8, // 0=sine, 1=square, 2=saw
}

impl Voice {
    fn new() -> Self {
        Self {
            phase: 0.0,
            freq: 0.0,
            amp: 0.0,
            decay: 1.0,
            wave: 0,
        }
    }
    fn play(&mut self, sr: f32) -> f32 {
        if self.amp < 0.0005 {
            return 0.0;
        }
        self.phase += self.freq / sr;
        self.phase %= 1.0;
        let raw = match self.wave {
            1 => {
                if self.phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            2 => 2.0 * self.phase - 1.0,
            _ => (self.phase * TAU).sin(),
        };
        let out = raw * self.amp;
        self.amp *= self.decay;
        out
    }
    fn trigger(&mut self, freq: f32, amp: f32, decay: f32, wave: u8) {
        self.phase = 0.0;
        self.freq = freq;
        self.amp = amp;
        self.decay = decay;
        self.wave = wave;
    }
}

// ---------------- FM VOICE ----------------
struct FmVoice {
    c_phase: f32,
    m_phase: f32,
    c_freq: f32,
    m_freq: f32,
    m_index: f32,
    amp: f32,
    decay: f32,
}

impl FmVoice {
    fn new() -> Self {
        Self {
            c_phase: 0.0,
            m_phase: 0.0,
            c_freq: 0.0,
            m_freq: 0.0,
            m_index: 0.0,
            amp: 0.0,
            decay: 1.0,
        }
    }
    fn play(&mut self, sr: f32) -> f32 {
        if self.amp < 0.0005 {
            return 0.0;
        }
        self.m_phase += self.m_freq / sr;
        self.c_phase += self.c_freq / sr;
        self.m_phase %= 1.0;
        self.c_phase %= 1.0;
        let modulation = (self.m_phase * TAU).sin() * self.m_index;
        let out = (self.c_phase * TAU + modulation).sin() * self.amp;
        self.amp *= self.decay;
        out
    }
    fn trigger(&mut self, freq: f32, ratio: f32, m_index: f32, amp: f32, decay: f32) {
        self.c_phase = 0.0;
        self.m_phase = 0.0;
        self.c_freq = freq;
        self.m_freq = freq * ratio;
        self.m_index = m_index;
        self.amp = amp;
        self.decay = decay;
    }
}

// ---------------- KICK ----------------
struct Kick {
    phase: f32,
    freq: f32,
    target: f32,
    amp: f32,
    decay: f32,
}

impl Kick {
    fn new() -> Self {
        Self {
            phase: 0.0,
            freq: 0.0,
            target: 0.0,
            amp: 0.0,
            decay: 1.0,
        }
    }
    fn play(&mut self, sr: f32) -> f32 {
        if self.amp < 0.0005 {
            return 0.0;
        }
        self.freq += (self.target - self.freq) * 0.1; // Pitch drop
        self.phase += self.freq / sr;
        self.phase %= 1.0;
        let val = (self.phase * TAU).sin() * self.amp;
        self.amp *= self.decay;
        val
    }
    fn trigger(&mut self, start: f32, target: f32, amp: f32, decay: f32) {
        self.phase = 0.0;
        self.freq = start;
        self.target = target;
        self.amp = amp;
        self.decay = decay;
    }
}

// ---------------- DRONE ----------------
struct Drone {
    phase1: f32,
    phase2: f32,
    freq1: f32,
    freq2: f32,
    target_freq1: f32,
    target_freq2: f32,
    amp: f32,
    target_amp: f32,
}

impl Drone {
    fn new() -> Self {
        Self {
            phase1: 0.0,
            phase2: 0.0,
            freq1: 110.0,
            freq2: 110.5,
            target_freq1: 110.0,
            target_freq2: 110.5,
            amp: 0.0,
            target_amp: 0.0,
        }
    }
    fn play(&mut self, sr: f32) -> f32 {
        self.freq1 += (self.target_freq1 - self.freq1) * 0.00005;
        self.freq2 += (self.target_freq2 - self.freq2) * 0.00005;
        self.amp += (self.target_amp - self.amp) * 0.00005;
        self.phase1 += self.freq1 / sr;
        self.phase2 += self.freq2 / sr;
        self.phase1 %= 1.0;
        self.phase2 %= 1.0;
        ((self.phase1 * TAU).sin() + (self.phase2 * TAU).sin()) * 0.5 * self.amp
    }
}

// ---------------- SCALES ----------------
const SCALES: &[&[f32]] = &[
    &[220.0, 261.63, 293.66, 329.63, 392.0, 440.0, 523.25, 587.33], // minor pentatonic
    &[261.63, 293.66, 329.63, 349.23, 392.0, 440.0, 493.88, 523.25], // major
    &[220.0, 246.94, 261.63, 293.66, 329.63, 349.23, 392.0, 440.0], // natural minor
    &[220.0, 246.94, 261.63, 293.66, 329.63, 370.0, 392.0, 440.0],  // dorian
    &[220.0, 233.08, 261.63, 293.66, 311.13, 349.23, 392.0, 440.0], // phrygian
    &[220.0, 246.94, 277.18, 311.13, 349.23, 392.0],                // whole tone
    &[261.63, 311.13, 392.0, 466.16, 523.25, 622.25],               // major 7 feel
    &[196.0, 220.0, 261.63, 293.66, 329.63, 392.0],                 // low minor pentatonic
];

// ---------------- PATTERN ----------------
#[derive(Clone, Copy)]
struct Pattern16(u16);

impl Pattern16 {
    fn new(rng: &mut Lcg, density: f32) -> Self {
        let mut bits = 0u16;
        for i in 0..16 {
            if rng.f32() < density {
                bits |= 1 << i;
            }
        }
        Self(bits)
    }
    fn hit(&self, step: usize) -> bool {
        (self.0 >> (step & 15)) & 1 != 0
    }
}

// ---------------- SCENE ----------------
#[derive(Clone, Copy)]
struct Scene {
    scale: &'static [f32],
    octave: f32,
    bass_octave: f32,
    kick_pat: Pattern16,
    snare_pat: Pattern16,
    hat_pat: Pattern16,
    bass_pat: Pattern16,
    arp_prob: f32,
    fm_prob: f32,
    drone_amp: f32,
    drone_base: f32,
    kick_start: f32,
    kick_target: f32,
    kick_amp: f32,
    hat_freq: f32,
    hat_amp: f32,
    snare_freq: f32,
    snare_amp: f32,
    fm_ratio: f32,
    fm_index: f32,
    fm_amp: f32,
    bass_wave: u8,
    arp_wave: u8,
    hat_wave: u8,
    snare_wave: u8,
}

fn make_scene(rng: &mut Lcg) -> Scene {
    let scale = SCALES[(rng.next() % SCALES.len() as u32) as usize];
    let octave = [0.5f32, 1.0, 1.0, 2.0][(rng.next() % 4) as usize];
    let energy = rng.f32();
    let darkness = rng.f32();

    let kick_density = if energy > 0.7 {
        0.6
    } else if energy > 0.3 {
        0.35
    } else {
        0.08
    };
    let snare_density = energy * 0.35;
    let hat_density = if energy > 0.5 { 0.7 } else { 0.15 };
    let bass_density = if darkness > 0.5 { 0.4 } else { 0.15 };

    Scene {
        scale,
        octave,
        bass_octave: [0.25, 0.5, 0.5, 1.0][(rng.next() % 4) as usize],
        kick_pat: Pattern16::new(rng, kick_density),
        snare_pat: Pattern16::new(rng, snare_density),
        hat_pat: Pattern16::new(rng, hat_density),
        bass_pat: Pattern16::new(rng, bass_density),
        arp_prob: rng.f32() * energy,
        fm_prob: rng.f32() * (0.3 + darkness * 0.5),
        drone_amp: darkness * 0.5,
        drone_base: 30.0 + darkness * 90.0,
        kick_start: 80.0 + rng.f32() * 120.0,
        kick_target: 25.0 + rng.f32() * 40.0,
        kick_amp: (0.4 + energy * 0.5).min(1.0),
        hat_freq: if rng.f32() > 0.5 {
            5000.0 + rng.f32() * 6000.0
        } else {
            600.0 + rng.f32() * 1200.0
        },
        hat_amp: (0.08 + energy * 0.12).min(0.5),
        snare_freq: 150.0 + rng.f32() * 700.0,
        snare_amp: (0.2 + energy * 0.25).min(0.6),
        fm_ratio: 0.5 + rng.f32() * 3.5,
        fm_index: 0.3 + rng.f32() * 3.0,
        fm_amp: (0.2 + darkness * 0.3).min(0.6),
        bass_wave: (rng.next() % 3) as u8,
        arp_wave: (rng.next() % 3) as u8,
        hat_wave: (rng.next() % 3) as u8,
        snare_wave: (rng.next() % 3) as u8,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();
    let output_device = host.default_output_device().expect("No output device");

    println!("Output: {}", output_device.name()?);
    let output_config = output_device.default_output_config()?;
    println!("Config: {:?}", output_config);

    let out_sr = output_config.sample_rate().0 as f32;
    let channels = output_config.channels() as usize;

    let phrase_id = Arc::new(AtomicUsize::new(0));
    let phrase_id_out = Arc::clone(&phrase_id);
    let volume = args.volume;

    // Pre-calculate decays
    let kick_decay = (-1.0 / (out_sr * 0.3)).exp();
    let hat_decay = (-1.0 / (out_sr * 0.05)).exp();
    let snare_decay = (-1.0 / (out_sr * 0.15)).exp();
    let pluck_decay = (-1.0 / (out_sr * 0.2)).exp();
    let bass_decay = (-1.0 / (out_sr * 0.8)).exp();
    let bell_decay = (-1.0 / (out_sr * 3.0)).exp();

    let mut rng = Lcg::new(42);
    let mut kick = Kick::new();
    let mut hat = Voice::new();
    let mut snare = Voice::new();
    let mut arp = Voice::new();
    let mut bass = Voice::new();
    let mut fm1 = FmVoice::new();
    let mut fm2 = FmVoice::new();
    let mut drone = Drone::new();

    let mut step: usize = 0;
    let mut sample_counter: usize = 0;
    let mut step_samples = (out_sr * 0.125) as usize;
    let mut phrase_pos: usize = 0;
    let mut phrase_len: usize = 256;
    let mut scene = make_scene(&mut rng);
    let mut current_phrase_id: usize = 0;

    phrase_id_out.store(0, Ordering::Relaxed);

    let err_fn = |err| eprintln!("stream error: {}", err);

    let output_stream = match output_config.sample_format() {
        cpal::SampleFormat::F32 => output_device.build_output_stream(
            &output_config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    sample_counter += 1;
                    if sample_counter >= step_samples {
                        sample_counter = 0;
                        step += 1;
                        if step >= 16 {
                            step = 0;
                        }
                        phrase_pos += 1;
                        if phrase_pos >= phrase_len {
                            phrase_pos = 0;
                            step = 0;
                            current_phrase_id += 1;
                            phrase_id_out.store(current_phrase_id, Ordering::Relaxed);
                            scene = make_scene(&mut rng);
                            phrase_len = 128 + ((rng.next() % 256) as usize); // 128..384 steps
                            step_samples = (out_sr * (0.08 + rng.f32() * 0.14)) as usize; // 75..187 bpm
                        }

                        // Trigger voices based on current scene
                        if scene.kick_pat.hit(step) {
                            kick.trigger(
                                scene.kick_start,
                                scene.kick_target,
                                scene.kick_amp,
                                kick_decay,
                            );
                        }
                        if scene.snare_pat.hit(step) {
                            snare.trigger(
                                scene.snare_freq,
                                scene.snare_amp,
                                snare_decay,
                                scene.snare_wave,
                            );
                        }
                        if scene.hat_pat.hit(step) {
                            hat.trigger(scene.hat_freq, scene.hat_amp, hat_decay, scene.hat_wave);
                        }
                        if scene.bass_pat.hit(step) {
                            let idx = (rng.next() % scene.scale.len() as u32) as usize;
                            bass.trigger(
                                scene.scale[idx] * scene.bass_octave,
                                0.4,
                                bass_decay,
                                scene.bass_wave,
                            );
                        }
                        if rng.f32() < scene.arp_prob {
                            let idx = (rng.next() % scene.scale.len() as u32) as usize;
                            arp.trigger(
                                scene.scale[idx] * scene.octave,
                                0.25,
                                pluck_decay,
                                scene.arp_wave,
                            );
                        }
                        if rng.f32() < scene.fm_prob {
                            let idx = (rng.next() % scene.scale.len() as u32) as usize;
                            fm1.trigger(
                                scene.scale[idx] * scene.octave,
                                scene.fm_ratio,
                                scene.fm_index,
                                scene.fm_amp,
                                bell_decay,
                            );
                        }
                        if rng.f32() < scene.fm_prob * 0.6 {
                            let idx = (rng.next() % scene.scale.len() as u32) as usize;
                            fm2.trigger(
                                scene.scale[idx] * scene.octave * 0.5,
                                scene.fm_ratio * 0.5,
                                scene.fm_index * 0.7,
                                scene.fm_amp * 0.7,
                                bell_decay,
                            );
                        }
                        drone.target_amp = scene.drone_amp;
                        drone.target_freq1 = scene.drone_base.max(40.0);
                        drone.target_freq2 = (scene.drone_base * 1.005).max(40.5);
                    }

                    // Synthesize
                    let mut music_out = 0.0;
                    music_out += kick.play(out_sr);
                    music_out += hat.play(out_sr);
                    music_out += snare.play(out_sr);
                    music_out += arp.play(out_sr);
                    music_out += bass.play(out_sr);
                    music_out += fm1.play(out_sr);
                    music_out += fm2.play(out_sr);
                    music_out += drone.play(out_sr);

                    let out_sample = music_out.tanh() * volume;
                    for ch in frame.iter_mut() {
                        *ch = out_sample;
                    }
                }
            },
            err_fn,
            None,
        )?,
        other => {
            return Err(format!("unsupported sample format: {:?}", other).into());
        }
    };

    output_stream.play()?;

    println!(">> Generative Station v0.3.0 — continuous music");
    println!("Press Ctrl+C to stop.\n");

    const ADJS: &[&str] = &[
        "Neon",
        "Deep",
        "Crystal",
        "Void",
        "Solar",
        "Lunar",
        "Ghost",
        "Quantum",
        "Retro",
        "Warm",
        "Cold",
        "Liquid",
        "Cyber",
        "Analog",
        "Digital",
        "Prismatic",
    ];
    const NOUNS: &[&str] = &[
        "Pulse", "Drift", "Grid", "Echo", "Bloom", "Signal", "Wave", "Dust", "Light", "Shadow",
        "Flow", "Hum", "Rain", "Field", "Garden", "Stream",
    ];

    let mut seen = usize::MAX;
    loop {
        thread::sleep(Duration::from_millis(250));
        let id = phrase_id.load(Ordering::Relaxed);
        if id != seen {
            seen = id;
            let mut r = Lcg::new((id as u32).wrapping_add(777));
            let adj = ADJS[(r.next() as usize) % ADJS.len()];
            let noun = NOUNS[(r.next() as usize) % NOUNS.len()];
            println!(">> {} {}", adj, noun);
        }
    }
}
