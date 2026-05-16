use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "radio")]
#[command(about = "Generative Synthwave / Ambient Radio")]
struct Args {
    /// Master volume 0.0–1.0 (default 0.5)
    #[arg(short, long, default_value_t = 0.5)]
    volume: f32,

    /// Station change interval in seconds (default 15)
    #[arg(short, long, default_value_t = 15)]
    interval: u64,
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
        if self.amp < 0.001 {
            return 0.0;
        }
        self.phase += self.freq / sr;
        self.phase %= 1.0; // self.phase = self.phase % 1.0;
        let out = match self.wave {
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
        let val = out * self.amp;
        self.amp *= self.decay;
        val
    }
    fn trigger(&mut self, freq: f32, amp: f32, decay: f32, wave: u8) {
        self.phase = 0.0;
        self.freq = freq;
        self.amp = amp;
        self.decay = decay;
        self.wave = wave;
    }
}

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
        if self.amp < 0.001 {
            return 0.0;
        }
        self.m_phase += self.m_freq / sr;
        self.c_phase += self.c_freq / sr;
        self.m_phase %= 1.0; // self.m_phase = self.m_phase % 1.0;
        self.c_phase %= 1.0; // self.c_phase = self.c_phase % 1.0;

        let modulator = (self.m_phase * TAU).sin() * self.m_index;
        let val = (self.c_phase * TAU + modulator).sin() * self.amp;
        self.amp *= self.decay;
        val
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
        if self.amp < 0.001 {
            return 0.0;
        }
        self.freq += (self.target - self.freq) * 0.1; // Pitch drop
        self.phase += self.freq / sr;
        self.phase %= 1.0; // self.phase = self.phase % 1.0;
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
        // Smooth portamento and volume fading
        self.freq1 += (self.target_freq1 - self.freq1) * 0.00005;
        self.freq2 += (self.target_freq2 - self.freq2) * 0.00005;
        self.amp += (self.target_amp - self.amp) * 0.00005;

        self.phase1 += self.freq1 / sr;
        self.phase2 += self.freq2 / sr;
        self.phase1 %= 1.0; // self.phase1 = self.phase1 % 1.0;
        self.phase2 %= 1.0; // self.phase2 = self.phase2 % 1.0;

        ((self.phase1 * TAU).sin() + (self.phase2 * TAU).sin()) * 0.5 * self.amp
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();
    let output_device = host
        .default_output_device()
        .expect("no default output device available");

    println!("Output: {}", output_device.name()?);
    let output_config = output_device.default_output_config()?;
    println!("Config: {:?}", output_config);

    let out_sr = output_config.sample_rate().0 as f32;
    let channels = output_config.channels() as usize;

    let station = Arc::new(AtomicUsize::new(0));
    let retuning = Arc::new(AtomicBool::new(false));

    let station_out = Arc::clone(&station);
    let retuning_out = Arc::clone(&retuning);
    let volume = args.volume;

    // Musical scales (A minor pentatonic across octaves)
    let penta: [f32; 8] = [
        220.00, 261.63, 293.66, 329.63, 392.00, 440.00, 523.25, 587.33,
    ];
    let bass_notes: [f32; 4] = [110.00, 130.81, 164.81, 98.00];

    // Pre-calculate decays so they are consistent regardless of sample rate
    let pluck_decay = (-1.0 / (out_sr * 0.2)).exp();
    let kick_decay = (-1.0 / (out_sr * 0.3)).exp();
    let hat_decay = (-1.0 / (out_sr * 0.05)).exp();
    let bell_decay = (-1.0 / (out_sr * 3.0)).exp();
    let bass_decay = (-1.0 / (out_sr * 0.8)).exp();
    let dash_decay = (-1.0 / (out_sr * 0.5)).exp();

    // *** FIX: Audio engine state MUST be declared outside the closure to persist across callbacks! ***
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
    let step_samples = (out_sr * 0.125) as usize; // 120 BPM, 16th notes

    let mut tuning_phase = 0.0;
    let mut tuning_freq = 100.0;
    let mut tuning_dir = 1.0;

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

                        let cur_station = station_out.load(Ordering::Relaxed);

                        if cur_station == 0 {
                            // Station 1: Midnight Drive (Synthwave)
                            drone.target_amp = 0.0;
                            if step == 0 || step == 8 {
                                kick.trigger(150.0, 50.0, 0.8, kick_decay);
                            }
                            if step % 4 == 2 {
                                snare.trigger(600.0, 0.3, hat_decay, 1);
                            }
                            if step.is_multiple_of(2) {
                                // if step % 2 == 0 {
                                hat.trigger(800.0, 0.2, hat_decay, 2);
                            }
                            if rng.f32() > 0.6 {
                                let idx = (rng.next() % penta.len() as u32) as usize;
                                arp.trigger(penta[idx], 0.3, pluck_decay, 1);
                            }
                            if step == 0 {
                                let idx = (rng.next() % bass_notes.len() as u32) as usize;
                                bass.trigger(bass_notes[idx], 0.4, bass_decay, 1);
                            }
                        } else if cur_station == 1 {
                            // Station 2: Cosmic Lullaby (Ambient FM)
                            drone.target_amp = 0.3;
                            if step == 0 {
                                let idx = (rng.next() % bass_notes.len() as u32) as usize;
                                drone.target_freq1 = bass_notes[idx];
                                drone.target_freq2 = bass_notes[idx] * 1.005;
                            }
                            if step == 0 || (step == 8 && rng.f32() > 0.5) {
                                let idx = (rng.next() % penta.len() as u32) as usize;
                                fm1.trigger(penta[idx], 1.0 + rng.f32(), 1.5, 0.4, bell_decay);
                            }
                            if step == 4 || step == 12 {
                                let idx = (rng.next() % penta.len() as u32) as usize;
                                fm2.trigger(penta[idx] * 0.5, 2.0, 0.8, 0.3, bell_decay);
                            }
                        } else if cur_station == 2 {
                            // Station 3: The Bunker (Numbers Station)
                            drone.target_amp = 0.4;
                            drone.target_freq1 = 55.0;
                            drone.target_freq2 = 55.2;
                            if rng.f32() > 0.7 {
                                let freq = 600.0 + rng.f32() * 400.0;
                                arp.trigger(freq, 0.4, pluck_decay, 0);
                            }
                            if step == 0 {
                                bass.trigger(800.0, 0.5, dash_decay, 0);
                            }
                            if step == 6 {
                                bass.trigger(800.0, 0.5, dash_decay, 0);
                            }
                        }
                    }

                    // Synthesize current audio frame
                    let mut music_out = 0.0;
                    music_out += kick.play(out_sr);
                    music_out += hat.play(out_sr);
                    music_out += snare.play(out_sr);
                    music_out += arp.play(out_sr);
                    music_out += bass.play(out_sr);
                    music_out += fm1.play(out_sr);
                    music_out += fm2.play(out_sr);
                    music_out += drone.play(out_sr);

                    // Radio tuning simulation
                    let sample = if retuning_out.load(Ordering::Relaxed) {
                        tuning_freq += 0.02 * tuning_dir;
                        if tuning_freq > 2000.0 {
                            tuning_dir = -1.0;
                        }
                        if tuning_freq < 100.0 {
                            tuning_dir = 1.0;
                        }
                        tuning_phase += tuning_freq / out_sr;

                        let tone = (tuning_phase * TAU).sin() * 0.2;
                        let noise = (rng.f32() * 2.0 - 1.0) * 0.5;
                        music_out * 0.1 + tone + noise // Muffle music under static
                    } else {
                        music_out
                    };

                    // Warm soft-clipping (tanh) and master volume
                    let out_sample = sample.tanh() * volume;

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

    let names = [
        "Midnight Drive FM",
        "Cosmic Lullaby",
        "The Bunker - Numbers Station",
    ];
    println!(">> Now playing: {}", names[0]);
    println!(
        "Radio running — auto-tuning every {}s. Press Ctrl+C to stop.\n",
        args.interval
    );

    // Main thread: handles the "radio tuning" changes
    let mut main_rng = Lcg::new(1234);
    loop {
        thread::sleep(Duration::from_secs(args.interval));

        println!("[Tuning...]");
        retuning.store(true, Ordering::Relaxed);
        thread::sleep(Duration::from_secs(2)); // 2 seconds of static

        let new_station = (main_rng.next() % 3) as usize;
        station.store(new_station, Ordering::Relaxed);
        retuning.store(false, Ordering::Relaxed);

        println!(">> Now playing: {}", names[new_station]);
    }
}
