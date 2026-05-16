use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "yaper")]
#[command(about = "Audio threshold detector and pulse generator")]
struct Args {
    /// Input amplitude threshold, linear 0.0–1.0 (default 0.7)
    #[arg(short, long, default_value_t = 0.7)]
    threshold: f32,

    /// Pulse frequency in Hz (default 1200)
    #[arg(short, long, default_value_t = 1200.0)]
    frequency: f32,

    /// Pulse duration in milliseconds (default 100)
    #[arg(short = 'd', long, default_value_t = 100.0)]
    pulse_duration: f32,

    /// Cooldown / wait after pulse in milliseconds (default 5)
    #[arg(short, long, default_value_t = 5.0)]
    cooldown: f32,
}

struct SharedState {
    trigger: AtomicBool,
    pulse_samples: AtomicUsize,
    cooldown_samples: AtomicUsize,
    frequency: f32,
    phase: AtomicU32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();

    let input_device = host
        .default_input_device()
        .expect("no default input device available");
    let output_device = host
        .default_output_device()
        .expect("no default output device available");

    println!("Input:  {}", input_device.name()?);
    println!("Output: {}", output_device.name()?);

    let input_config = input_device.default_input_config()?;
    let output_config = output_device.default_output_config()?;

    println!("Input config:  {:?}", input_config);
    println!("Output config: {:?}", output_config);

    let state = Arc::new(SharedState {
        trigger: AtomicBool::new(false),
        pulse_samples: AtomicUsize::new(0),
        cooldown_samples: AtomicUsize::new(0),
        frequency: args.frequency,
        phase: AtomicU32::new(0f32.to_bits()),
    });

    let err_fn = |err| eprintln!("stream error: {}", err);

    // ---- Input stream ----
    let state_in = Arc::clone(&state);
    let threshold = args.threshold;
    let input_stream = match input_config.sample_format() {
        cpal::SampleFormat::F32 => input_device.build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // If still in cooldown, don't bother calculating.
                if state_in.cooldown_samples.load(Ordering::Relaxed) > 0 {
                    return;
                }
                let peak = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                if peak > threshold {
                    state_in.trigger.store(true, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?,
        other => {
            return Err(format!("unsupported input sample format: {:?}", other).into());
        }
    };

    // ---- Output stream ----
    let state_out = Arc::clone(&state);
    let out_sr = output_config.sample_rate().0 as f32;
    let pulse_dur = args.pulse_duration;
    let cooldown_dur = args.cooldown;
    let channels = output_config.channels() as usize;

    let output_stream = match output_config.sample_format() {
        cpal::SampleFormat::F32 => output_device.build_output_stream(
            &output_config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    let mut in_pulse = false;

                    // Already active?
                    let cd = state_out.cooldown_samples.load(Ordering::Relaxed);
                    if cd > 0 {
                        state_out.cooldown_samples.store(cd - 1, Ordering::Relaxed);
                        let ps = state_out.pulse_samples.load(Ordering::Relaxed);
                        if ps > 0 {
                            state_out.pulse_samples.store(ps - 1, Ordering::Relaxed);
                            in_pulse = true;
                        }
                    } else if state_out.trigger.swap(false, Ordering::Relaxed) {
                        // Start a new pulse + cooldown.
                        let pulse_samps = (pulse_dur / 1000.0 * out_sr) as usize;
                        let cooldown_samps = (cooldown_dur / 1000.0 * out_sr) as usize;
                        state_out
                            .pulse_samples
                            .store(pulse_samps, Ordering::Relaxed);
                        state_out
                            .cooldown_samples
                            .store(pulse_samps + cooldown_samps, Ordering::Relaxed);
                        in_pulse = true;
                    }

                    let sample = if in_pulse {
                        let phase = f32::from_bits(state_out.phase.load(Ordering::Relaxed));
                        let new_phase = phase + state_out.frequency / out_sr;
                        let wrapped = new_phase - new_phase.floor();
                        state_out.phase.store(wrapped.to_bits(), Ordering::Relaxed);
                        (wrapped * TAU).sin() * 0.8
                    } else {
                        0.0
                    };

                    for ch in frame.iter_mut() {
                        *ch = sample;
                    }
                }
            },
            err_fn,
            None,
        )?,
        other => {
            return Err(format!("unsupported output sample format: {:?}", other).into());
        }
    };

    input_stream.play()?;
    output_stream.play()?;

    println!(
        "Running — threshold {:.2}, freq {:.0} Hz, pulse {:.0} ms, cooldown {:.0} ms",
        args.threshold, args.frequency, args.pulse_duration, args.cooldown
    );
    println!("Press Ctrl+C to stop.");

    // Keep the process alive.
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
