use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "daf")]
#[command(about = "Delayed Auditory Feedback")]
struct Args {
    /// Delay time in milliseconds (default 200)
    #[arg(short, long, default_value_t = 200.0)]
    delay: f32,
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

    let sample_rate = output_config.sample_rate().0 as f32;
    let channels = output_config.channels() as usize;
    let input_channels = input_config.channels() as usize;

    // FIX 1: Don't multiply by channels — think in mono frames
    let delay_samples = (args.delay / 1000.0 * sample_rate) as usize;

    let ring = HeapRb::new(delay_samples * 2);
    let (mut producer, mut consumer) = ring.split();

    // FIX 2: Pre-fill with silence so output reads zeros first,
    // creating the actual delay
    for _ in 0..delay_samples {
        let _ = producer.push(0.0);
    }

    let err_fn = |err| eprintln!("stream error: {}", err);

    let input_stream = match input_config.sample_format() {
        cpal::SampleFormat::F32 => input_device.build_input_stream(
            &input_config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // FIX 1: Push only one sample per frame (mono downmix)
                for frame in data.chunks(input_channels) {
                    let mono: f32 = frame.iter().sum::<f32>() / input_channels as f32;
                    let _ = producer.push(mono);
                }
            },
            err_fn,
            None,
        )?,
        other => {
            return Err(format!("unsupported input sample format: {:?}", other).into());
        }
    };

    let output_stream = match output_config.sample_format() {
        cpal::SampleFormat::F32 => output_device.build_output_stream(
            &output_config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    let sample = consumer.pop().unwrap_or(0.0);
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

    println!("DAF running — delay {:.0} ms", args.delay);
    println!("Press Ctrl+C to stop.");

    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
