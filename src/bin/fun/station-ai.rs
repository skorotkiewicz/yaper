use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════
//  CLI
// ═══════════════════════════════════════════════════════════════

#[derive(Parser, Debug)]
#[command(name = "station")]
#[command(
    about = "🎵 Generative Music with Self-Improving AI",
    version = "0.4.0"
)]
struct Args {
    /// Master volume 0.0–1.0
    #[arg(short, default_value_t = 0.5)]
    volume: f32,
    /// Dataset file (one melody per line, MIDI notes or note names like C4 E4 G4)
    #[arg(short, long)]
    dataset: Option<String>,
    /// Learning rate 0.0–1.0
    #[arg(long, default_value_t = 0.1)]
    learning_rate: f32,
    /// Max model influence 0.0–1.0
    #[arg(long, default_value_t = 0.85)]
    max_influence: f32,
}

// ═══════════════════════════════════════════════════════════════
//  RNG
// ═══════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════
//  VOICES
// ═══════════════════════════════════════════════════════════════

struct Voice {
    phase: f32,
    freq: f32,
    amp: f32,
    decay: f32,
    wave: u8,
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
        self.freq += (self.target - self.freq) * 0.1;
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

// ═══════════════════════════════════════════════════════════════
//  SCALES & PATTERNS
// ═══════════════════════════════════════════════════════════════

const SCALES: &[&[f32]] = &[
    &[220.0, 261.63, 293.66, 329.63, 392.0, 440.0, 523.25, 587.33], // minor pentatonic
    &[261.63, 293.66, 329.63, 349.23, 392.0, 440.0, 493.88, 523.25], // major
    &[220.0, 246.94, 261.63, 293.66, 329.63, 349.23, 392.0, 440.0], // natural minor
    &[220.0, 246.94, 261.63, 293.66, 329.63, 370.0, 392.0, 440.0],  // dorian
    &[220.0, 233.08, 261.63, 293.66, 311.13, 349.23, 392.0, 440.0], // phrygian
    &[220.0, 246.94, 277.18, 311.13, 349.23, 392.0],                // whole tone
    &[261.63, 311.13, 392.0, 466.16, 523.25, 622.25],               // major 7
    &[196.0, 220.0, 261.63, 293.66, 329.63, 392.0],                 // low minor pent
];

const SCALE_NAMES: &[&str] = &[
    "Minor Pentatonic",
    "Major",
    "Natural Minor",
    "Dorian",
    "Phrygian",
    "Whole Tone",
    "Major 7th",
    "Low Minor Pent",
];

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

// ═══════════════════════════════════════════════════════════════
//  SCENE
// ═══════════════════════════════════════════════════════════════

#[derive(Clone, Copy)]
struct Scene {
    scale: &'static [f32],
    scale_idx: usize,
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
    bpm_hint: f32,
}

fn make_scene(rng: &mut Lcg, brain: &MusicBrain) -> Scene {
    let scale_idx = brain.choose_scale_idx(SCALES, rng);
    let scale = SCALES[scale_idx];
    let octave = [0.5f32, 1.0, 1.0, 2.0][(rng.next() % 4) as usize];
    let energy = rng.f32();
    let darkness = rng.f32();
    let inf = brain.influence;

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

    let mut arp_prob = rng.f32() * energy;
    let mut fm_prob = rng.f32() * (0.3 + darkness * 0.5);
    // Model with high influence plays more notes (more expression)
    if inf > 0.3 {
        arp_prob = (arp_prob + inf * 0.35).min(0.95);
    }
    if inf > 0.3 {
        fm_prob = (fm_prob + inf * 0.2).min(0.8);
    }

    let bpm_hint = 80.0 + rng.f32() * 100.0;
    Scene {
        scale,
        scale_idx,
        octave,
        bass_octave: [0.25, 0.5, 0.5, 1.0][(rng.next() % 4) as usize],
        kick_pat: Pattern16::new(rng, kick_density),
        snare_pat: Pattern16::new(rng, snare_density),
        hat_pat: Pattern16::new(rng, hat_density),
        bass_pat: Pattern16::new(rng, bass_density),
        arp_prob,
        fm_prob,
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
        bpm_hint,
    }
}

// ═══════════════════════════════════════════════════════════════
//  NOTE UTILITIES
// ═══════════════════════════════════════════════════════════════

const PC_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn freq_to_pc(freq: f32) -> u8 {
    if freq <= 0.0 {
        return 0;
    }
    let midi = 12.0 * (freq / 440.0).log2() + 69.0;
    ((midi.round() as i32 % 12 + 12) % 12) as u8
}

fn parse_note(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u8>() {
        if n <= 127 {
            return Some(n);
        }
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let base = match bytes[0] {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return None,
    };
    let mut idx = 1usize;
    let mut offset: i8 = 0;
    if idx < bytes.len() {
        match bytes[idx] {
            b'#' => {
                offset = 1;
                idx += 1;
            }
            b'b' => {
                offset = -1;
                idx += 1;
            }
            _ => {}
        }
    }
    if idx >= bytes.len() {
        return Some(((base as i8 + offset + 60).clamp(0, 127)) as u8);
    }
    let octave: i8 = s[idx..].parse().ok()?;
    let midi = base as i8 + offset + (octave + 1) * 12;
    if midi >= 0 && midi <= 127 {
        Some(midi as u8)
    } else {
        None
    }
}

// ═══════════════════════════════════════════════════════════════
//  BUILT-IN DATASET
// ═══════════════════════════════════════════════════════════════

const BUILTIN_DATASET: &[&[u8]] = &[
    &[60, 62, 64, 65, 67, 69, 71, 72],                 // C major up
    &[72, 71, 69, 67, 65, 64, 62, 60],                 // C major down
    &[60, 64, 67, 72, 67, 64, 60],                     // C major arpeggio
    &[57, 60, 64, 69, 64, 60, 57],                     // A minor arpeggio
    &[60, 63, 65, 66, 67, 70, 72],                     // Blues scale
    &[60, 62, 64, 67, 69, 72],                         // Pentatonic up
    &[72, 69, 67, 64, 62, 60],                         // Pentatonic down
    &[60, 67, 62, 69, 64, 71, 66, 61, 68, 63, 70, 65], // Circle of 5ths
    &[60, 60, 63, 60, 65, 60, 63, 60],                 // Ostinato
    &[48, 55, 60, 55, 48, 52, 57, 52],                 // Bass pattern
    &[60, 64, 67, 71, 72, 71, 67, 64],                 // Maj7 arpeggio
    &[57, 60, 63, 67, 69, 67, 63, 60],                 // Min7 arpeggio
    &[65, 64, 60, 62, 60, 57, 55, 57],                 // Descending melody
    &[67, 69, 71, 72, 74, 72, 71, 69],                 // Climbing and falling
    &[60, 62, 64, 60, 64, 62, 60, 64],                 // Alternating pattern
    &[48, 60, 55, 67, 52, 64, 57, 69],                 // Bass + chord alternation
    &[60, 63, 67, 63, 60, 63, 67, 72],                 // Minor triad pattern
    &[72, 68, 65, 60, 63, 67, 72, 67],                 // Jazz fragment
    &[60, 62, 64, 65, 64, 62, 60, 59],                 // Wave pattern
    &[55, 57, 59, 60, 62, 64, 65, 67],                 // Ascending from G
];

// ═══════════════════════════════════════════════════════════════
//  MUSIC BRAIN — Markov model with reinforcement learning
// ═══════════════════════════════════════════════════════════════

const MAX_PHRASE_NOTES: usize = 512;

struct MusicBrain {
    // Bigram transitions: transitions[from_pc][to_pc] = weight
    transitions: [[f32; 12]; 12],
    // Pitch class preference (for scale selection)
    pc_pref: [f32; 12],
    // Learning params
    lr: f32,
    influence: f32,
    max_influence: f32,
    // Generation / phrase tracking
    generation: usize,
    prev_pc: u8,
    prev_scale_idx: usize,
    current_scale_idx: usize, // FIX: track which scale is active
    phrase_pcs: [u8; MAX_PHRASE_NOTES],
    phrase_pc_count: usize,
    // Real-time consonance accumulator
    consonance_acc: f32,
    consonance_count: usize,
    // Fitness tracking
    best_fitness: f32,
    avg_fitness: f32,
    // Stats
    dataset_notes: usize,
    total_trans: usize,
    // Milestones
    last_milestone: usize,
}

impl MusicBrain {
    fn new(lr: f32, max_influence: f32) -> Self {
        Self {
            transitions: [[0.1f32; 12]; 12],
            pc_pref: [0.0; 12],
            lr,
            influence: 0.05,
            max_influence,
            generation: 0,
            prev_pc: 0,
            prev_scale_idx: 0,
            current_scale_idx: 0, // FIX
            phrase_pcs: [0u8; MAX_PHRASE_NOTES],
            phrase_pc_count: 0,
            consonance_acc: 0.0,
            consonance_count: 0,
            best_fitness: 0.0,
            avg_fitness: 0.5,
            dataset_notes: 0,
            total_trans: 0,
            last_milestone: 0,
        }
    }

    fn feed_seq(&mut self, notes: &[u8]) {
        for i in 1..notes.len() {
            let from = (notes[i - 1] % 12) as usize;
            let to = (notes[i] % 12) as usize;
            self.transitions[from][to] += 2.0;
            self.pc_pref[to] += 0.5;
            self.total_trans += 1;
        }
        self.dataset_notes += notes.len();
    }

    fn choose_scale_idx(&self, scales: &[&[f32]], rng: &mut Lcg) -> usize {
        let n = scales.len();
        if self.influence < 0.1 || self.dataset_notes == 0 {
            return (rng.next() % n as u32) as usize;
        }
        let mut scores = [0.0f32; 16];
        let cnt = n.min(16);
        for i in 0..cnt {
            let mut s = 1.0f32;
            for &freq in scales[i] {
                let pc = freq_to_pc(freq);
                s += self.pc_pref[pc as usize].max(0.0) * 0.5;
            }
            scores[i] = s + rng.f32() * 2.0;
        }
        let total: f32 = scores[..cnt].iter().sum();
        if total < 0.001 {
            return (rng.next() % n as u32) as usize;
        }
        let mut r = rng.f32() * total;
        for i in 0..cnt {
            r -= scores[i];
            if r <= 0.0 {
                return i;
            }
        }
        0
    }

    fn choose_note(&mut self, scale: &[f32], octave: f32, rng: &mut Lcg) -> (usize, f32) {
        let n = scale.len().min(8);
        let mut scores = [0.0f32; 8];

        for i in 0..n {
            let pc = freq_to_pc(scale[i] * octave);
            // Model prediction score
            if self.phrase_pc_count > 0 {
                scores[i] += self.transitions[self.prev_pc as usize][pc as usize].max(0.01)
                    * self.influence
                    * 3.0;
            }
            // Stepwise motion bonus — prefer adjacent scale degrees
            if self.influence > 0.15 && self.phrase_pc_count > 0 {
                let dist = (i as i32 - self.prev_scale_idx as i32).abs();
                if dist == 1 {
                    scores[i] += 1.5 * self.influence;
                } else if dist == 2 {
                    scores[i] += 0.5 * self.influence;
                }
            }
            // Base randomness
            scores[i] += 1.0 + rng.f32() * 0.8;
        }

        let total: f32 = scores[..n].iter().sum();
        if total < 0.001 {
            let idx = (rng.next() % n as u32) as usize;
            return (idx, scale[idx] * octave);
        }
        let mut r = rng.f32() * total;
        for i in 0..n {
            r -= scores[i];
            if r <= 0.0 {
                self.prev_scale_idx = i;
                return (i, scale[i] * octave);
            }
        }
        self.prev_scale_idx = 0;
        (0, scale[0] * octave)
    }

    fn record_note(&mut self, freq: f32) {
        if self.phrase_pc_count >= MAX_PHRASE_NOTES {
            return;
        }
        let pc = freq_to_pc(freq);
        self.phrase_pcs[self.phrase_pc_count] = pc;
        self.phrase_pc_count += 1;
        self.prev_pc = pc;
    }

    fn score_consonance_rt(&mut self, f1: f32, f2: f32) {
        if f1 < 20.0 || f2 < 20.0 {
            return;
        }
        let ratio = f1.max(f2) / f1.min(f2).max(1.0);
        let consonances = [
            1.0,
            6.0 / 5.0,
            5.0 / 4.0,
            4.0 / 3.0,
            3.0 / 2.0,
            5.0 / 3.0,
            2.0,
        ];
        let mut best = 0.0f32;
        for &c in &consonances {
            let diff = (ratio - c).abs();
            if diff < 0.1 {
                best = best.max(1.0 - diff / 0.1);
            }
        }
        self.consonance_acc += best;
        self.consonance_count += 1;
    }

    fn end_phrase(&mut self) -> f32 {
        // --- Interval quality (melodic consonance) ---
        let mut interval_score = 0.0f32;
        let mut interval_count = 0usize;
        for i in 1..self.phrase_pc_count {
            let diff = (self.phrase_pcs[i] as i32 - self.phrase_pcs[i - 1] as i32).abs();
            let q = match diff {
                0 | 7 => 1.0,
                3 | 4 => 0.9,
                5 | 9 => 0.8,
                2 | 10 => 0.5,
                1 | 11 => 0.2,
                6 => 0.15,
                _ => 0.3,
            };
            interval_score += q;
            interval_count += 1;
        }
        let intervals = if interval_count > 0 {
            interval_score / interval_count as f32
        } else {
            0.5
        };

        // --- Real-time consonance ---
        let consonance = if self.consonance_count > 0 {
            self.consonance_acc / self.consonance_count as f32
        } else {
            0.5
        };

        // --- Variety ---
        let mut used = [false; 12];
        for i in 0..self.phrase_pc_count {
            used[self.phrase_pcs[i] as usize] = true;
        }
        let variety = used.iter().filter(|&&x| x).count() as f32;
        let variety_score = if variety < 3.0 {
            0.3
        } else if variety > 8.0 {
            0.5
        } else {
            0.7 + 0.3 * (1.0 - (variety - 5.5).abs() / 2.5)
        };

        // --- Resolution ---
        let resolution = if self.phrase_pc_count > 0 {
            let last = self.phrase_pcs[self.phrase_pc_count - 1];
            match last {
                0 | 7 => 1.0,
                4 | 5 | 9 => 0.7,
                _ => 0.3,
            }
        } else {
            0.5
        };

        // --- Contour balance ---
        let mut ups = 0usize;
        let mut downs = 0usize;
        for i in 1..self.phrase_pc_count {
            if self.phrase_pcs[i] > self.phrase_pcs[i - 1] {
                ups += 1;
            } else if self.phrase_pcs[i] < self.phrase_pcs[i - 1] {
                downs += 1;
            }
        }
        let total_dirs = ups + downs;
        let contour = if total_dirs > 0 {
            0.3 + 0.7 * (ups.min(downs) as f32 / total_dirs as f32)
        } else {
            0.5
        };

        // --- Density ---
        let density = if self.phrase_pc_count < 4 {
            0.3
        } else if self.phrase_pc_count > 64 {
            0.4
        } else {
            0.7 + 0.3 * (1.0 - (self.phrase_pc_count as f32 - 20.0).abs() / 44.0)
        };

        let fitness = intervals * 0.25
            + consonance * 0.2
            + variety_score * 0.15
            + resolution * 0.15
            + contour * 0.15
            + density * 0.1;

        // --- Reinforcement ---
        let reward = (fitness - self.avg_fitness) * self.lr;
        if self.phrase_pc_count > 1 {
            for i in 1..self.phrase_pc_count {
                let from = self.phrase_pcs[i - 1] as usize;
                let to = self.phrase_pcs[i] as usize;
                self.transitions[from][to] = (self.transitions[from][to] + reward).max(0.01);
                self.pc_pref[to] += reward * 0.3;
            }
        }

        // Slight decay to prevent stagnation
        for row in &mut self.transitions {
            for w in row.iter_mut() {
                *w = (*w * 0.9995).max(0.01);
            }
        }

        self.best_fitness = self.best_fitness.max(fitness);
        self.avg_fitness = self.avg_fitness * 0.85 + fitness * 0.15;

        // Grow influence
        self.influence = self
            .max_influence
            .min(self.influence + 0.004 * fitness * (1.0 + self.dataset_notes as f32 * 0.001));

        self.generation += 1;

        // Reset phrase state
        self.phrase_pc_count = 0;
        self.consonance_acc = 0.0;
        self.consonance_count = 0;

        fitness
    }

    fn top_prefs(&self, n: usize) -> [(usize, f32); 4] {
        let mut prefs: [(usize, f32); 4] = [(0, f32::MIN); 4];
        for (i, &v) in self.pc_pref.iter().enumerate() {
            if v > prefs[3].1 {
                prefs[3] = (i, v);
                prefs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            }
        }
        prefs
    }
}

// ═══════════════════════════════════════════════════════════════
//  SHARED STATE (audio → display)
// ═══════════════════════════════════════════════════════════════

struct Shared {
    generation: AtomicUsize,
    fitness_x10k: AtomicUsize,
    best_x10k: AtomicUsize,
    influence_x10k: AtomicUsize,
    scale_idx: AtomicUsize,
    dataset_notes: AtomicUsize,
    total_trans: AtomicUsize,
    milestone: AtomicUsize,
}

impl Shared {
    fn new() -> Self {
        Self {
            generation: AtomicUsize::new(0),
            fitness_x10k: AtomicUsize::new(0),
            best_x10k: AtomicUsize::new(0),
            influence_x10k: AtomicUsize::new(0),
            scale_idx: AtomicUsize::new(0),
            dataset_notes: AtomicUsize::new(0),
            total_trans: AtomicUsize::new(0),
            milestone: AtomicUsize::new(0),
        }
    }
    fn store(&self, brain: &MusicBrain, phrase_fitness: f32) {
        self.generation.store(brain.generation, Ordering::Relaxed);
        self.fitness_x10k
            .store((phrase_fitness * 10000.0) as usize, Ordering::Relaxed); // FIX: store actual phrase fitness
        self.best_x10k
            .store((brain.best_fitness * 10000.0) as usize, Ordering::Relaxed);
        self.influence_x10k
            .store((brain.influence * 10000.0) as usize, Ordering::Relaxed);
        self.scale_idx
            .store(brain.current_scale_idx, Ordering::Relaxed); // FIX: store the actual scale index
        self.dataset_notes
            .store(brain.dataset_notes, Ordering::Relaxed);
        self.total_trans.store(brain.total_trans, Ordering::Relaxed);
        self.milestone
            .store(brain.last_milestone, Ordering::Relaxed);
    }
}

// ═══════════════════════════════════════════════════════════════
//  DATASET LOADING
// ═══════════════════════════════════════════════════════════════

fn load_dataset(path: &str) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let mut sequences = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let mut seq = Vec::new();
        for token in line.split(|c: char| c.is_whitespace() || c == ',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some(note) = parse_note(token) {
                seq.push(note);
            }
        }
        if seq.len() >= 2 {
            sequences.push(seq);
        }
    }
    Ok(sequences)
}

// ═══════════════════════════════════════════════════════════════
//  MAIN
// ═══════════════════════════════════════════════════════════════

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();
    let output_device = host
        .default_output_device()
        .expect("No output device found");
    println!("🔊  Output: {}", output_device.name()?);
    let output_config = output_device.default_output_config()?;
    println!("⚙️   Config: {:?}", output_config);

    let out_sr = output_config.sample_rate().0 as f32;
    let channels = output_config.channels() as usize;

    // ── Initialize brain ──
    let mut brain = MusicBrain::new(args.learning_rate, args.max_influence);

    // Load dataset
    if let Some(path) = &args.dataset {
        match load_dataset(path) {
            Ok(seqs) => {
                for seq in &seqs {
                    brain.feed_seq(seq);
                }
                println!(
                    "📚  Loaded {} sequences ({} notes) from {}",
                    seqs.len(),
                    brain.dataset_notes,
                    path
                );
            }
            Err(e) => eprintln!("⚠️   Failed to load dataset: {}", e),
        }
    }

    // Always load built-in dataset too
    let builtin_count_before = brain.dataset_notes;
    for seq in BUILTIN_DATASET {
        brain.feed_seq(seq);
    }
    let builtin_notes = brain.dataset_notes - builtin_count_before;
    println!("🎵  Built-in dataset: {} notes loaded", builtin_notes);
    println!("🧠  Total transitions learned: {}", brain.total_trans);

    let shared = Arc::new(Shared::new());
    let shared_audio = Arc::clone(&shared);
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
    let mut bass_v = Voice::new();
    let mut fm1 = FmVoice::new();
    let mut fm2 = FmVoice::new();
    let mut drone = Drone::new();

    let mut step: usize = 0;
    let mut sample_counter: usize = 0;
    let mut scene = make_scene(&mut rng, &brain);
    let mut step_samples = (out_sr * (60.0 / scene.bpm_hint / 4.0)) as usize; // FIX: /4.0 for 16th notes
    let mut phrase_pos: usize = 0;
    let mut phrase_len: usize = 256;
    let mut last_freq: f32 = 0.0;

    // FIX: store the initial scene's scale index
    brain.current_scale_idx = scene.scale_idx;

    let err_fn = |err: cpal::StreamError| eprintln!("stream error: {}", err);

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
                            // ── End of phrase: train & evaluate ──
                            let fitness = brain.end_phrase();
                            brain.last_milestone = 0;

                            // Check milestones
                            if brain.influence >= 0.25 && brain.last_milestone < 1 {
                                brain.last_milestone = 1;
                            }
                            if brain.influence >= 0.50 && brain.last_milestone < 2 {
                                brain.last_milestone = 2;
                            }
                            if brain.influence >= 0.75 && brain.last_milestone < 3 {
                                brain.last_milestone = 3;
                            }
                            if fitness >= 0.75 && brain.last_milestone < 4 {
                                brain.last_milestone = 4;
                            }

                            // New scene
                            phrase_pos = 0;
                            step = 0;
                            scene = make_scene(&mut rng, &brain);
                            brain.current_scale_idx = scene.scale_idx; // FIX: track which scale is active
                            phrase_len = 128 + ((rng.next() % 256) as usize);
                            step_samples = (out_sr * (60.0 / scene.bpm_hint / 4.0)) as usize; // FIX: /4.0 for 16th notes

                            // Relight drone for new scene
                            drone.target_amp = scene.drone_amp;
                            drone.target_freq1 = scene.drone_base.max(40.0);
                            drone.target_freq2 = (scene.drone_base * 1.005).max(40.5);

                            shared_audio.store(&brain, fitness); // FIX: pass phrase fitness
                        }

                        // ── Trigger voices ──
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
                            let (_, freq) =
                                brain.choose_note(scene.scale, scene.bass_octave, &mut rng);
                            bass_v.trigger(freq, 0.4, bass_decay, scene.bass_wave);
                            brain.record_note(freq);
                            if last_freq > 0.0 {
                                brain.score_consonance_rt(freq, last_freq);
                            }
                            last_freq = freq;
                        }
                        if rng.f32() < scene.arp_prob {
                            let (_, freq) = brain.choose_note(scene.scale, scene.octave, &mut rng);
                            arp.trigger(freq, 0.25, pluck_decay, scene.arp_wave);
                            brain.record_note(freq);
                            if last_freq > 0.0 {
                                brain.score_consonance_rt(freq, last_freq);
                            }
                            last_freq = freq;
                        }
                        if rng.f32() < scene.fm_prob {
                            let (_, freq) = brain.choose_note(scene.scale, scene.octave, &mut rng);
                            fm1.trigger(
                                freq,
                                scene.fm_ratio,
                                scene.fm_index,
                                scene.fm_amp,
                                bell_decay,
                            );
                            brain.record_note(freq);
                            if last_freq > 0.0 {
                                brain.score_consonance_rt(freq, last_freq);
                            }
                            last_freq = freq;
                        }
                        if rng.f32() < scene.fm_prob * 0.6 {
                            let (_, freq) =
                                brain.choose_note(scene.scale, scene.octave * 0.5, &mut rng);
                            fm2.trigger(
                                freq,
                                scene.fm_ratio * 0.5,
                                scene.fm_index * 0.7,
                                scene.fm_amp * 0.7,
                                bell_decay,
                            );
                            brain.record_note(freq);
                            if last_freq > 0.0 {
                                brain.score_consonance_rt(freq, last_freq);
                            }
                            last_freq = freq;
                        }

                        drone.target_amp = scene.drone_amp;
                        drone.target_freq1 = scene.drone_base.max(40.0);
                        drone.target_freq2 = (scene.drone_base * 1.005).max(40.5);
                    }

                    // ── Synthesize ──
                    let mut out = 0.0f32;
                    out += kick.play(out_sr);
                    out += hat.play(out_sr);
                    out += snare.play(out_sr);
                    out += arp.play(out_sr);
                    out += bass_v.play(out_sr);
                    out += fm1.play(out_sr);
                    out += fm2.play(out_sr);
                    out += drone.play(out_sr);

                    let sample = out.tanh() * volume;
                    for ch in frame.iter_mut() {
                        *ch = sample;
                    }
                }
            },
            err_fn,
            None,
        )?,
        other => return Err(format!("unsupported sample format: {:?}", other).into()),
    };

    output_stream.play()?;

    // ═══════════════════════════════════════════════════════════
    //  DISPLAY LOOP
    // ═══════════════════════════════════════════════════════════

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
        "Ethereal",
        "Velvet",
    ];
    const NOUNS: &[&str] = &[
        "Pulse", "Drift", "Grid", "Echo", "Bloom", "Signal", "Wave", "Dust", "Light", "Shadow",
        "Flow", "Hum", "Rain", "Field", "Garden", "Stream", "Aether", "Prism",
    ];

    let mut last_gen = 0usize;

    // Print header
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║        🎵  GENERATIVE STATION v0.4.0 — AI Music  🎵        ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  The AI listens to itself play and learns what sounds good  ║");
    println!("║  It builds melodies from transitions & reinforces harmony   ║");
    println!("║  Load a dataset with --dataset <file> to teach it more!     ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
    println!("Press Ctrl+C to stop.\n");

    loop {
        thread::sleep(Duration::from_millis(500));

        let generation = shared.generation.load(Ordering::Relaxed);
        if generation != last_gen {
            last_gen = generation;

            let fitness = shared.fitness_x10k.load(Ordering::Relaxed) as f32 / 10000.0;
            let best = shared.best_x10k.load(Ordering::Relaxed) as f32 / 10000.0;
            let influence = shared.influence_x10k.load(Ordering::Relaxed) as f32 / 10000.0;
            let scale_idx = shared.scale_idx.load(Ordering::Relaxed);
            let ds_notes = shared.dataset_notes.load(Ordering::Relaxed);
            let total_tr = shared.total_trans.load(Ordering::Relaxed);
            let milestone = shared.milestone.load(Ordering::Relaxed);

            let mut r = Lcg::new((generation as u32).wrapping_add(777));
            let adj = ADJS[(r.next() as usize) % ADJS.len()];
            let noun = NOUNS[(r.next() as usize) % NOUNS.len()];
            let scale_name = SCALE_NAMES.get(scale_idx).unwrap_or(&"Unknown");

            // Fitness bar
            let bar_len = 20;
            let filled = (fitness * bar_len as f32).round() as usize;
            let filled = filled.min(bar_len); // clamp
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_len - filled);

            // Influence bar
            let inf_filled = (influence * bar_len as f32).round() as usize;
            let inf_filled = inf_filled.min(bar_len); // clamp
            let inf_bar: String = "▓".repeat(inf_filled) + &"░".repeat(bar_len - inf_filled);

            // Brain emoji based on influence
            let brain_emoji = if influence < 0.2 {
                "🧒"
            } else if influence < 0.4 {
                "🧠"
            } else if influence < 0.6 {
                "🧠✨"
            } else if influence < 0.8 {
                "🧠🔥"
            } else {
                "🧠💎"
            };

            // Milestone message
            let milestone_msg = match milestone {
                1 => " — 🌱 AI is finding its voice...",
                2 => " — 🎶 AI is singing along!",
                3 => " — 🔥 AI is leading the jam!",
                4 => " — 💎 Peak performance!",
                _ => "",
            };

            println!(
                "│ Gen {:>4} │ {} {} │ Fit [{}] {:.0}% (best {:.0}%) │ Inf [{}] {:.0}% {}{}",
                generation,
                adj,
                noun,
                bar,
                fitness * 100.0,
                best * 100.0,
                inf_bar,
                influence * 100.0,
                brain_emoji,
                milestone_msg
            );
            println!(
                "│          │ Scale: {} │ Notes: {} │ Transitions: {}",
                scale_name, ds_notes, total_tr
            );
        }
    }
}
