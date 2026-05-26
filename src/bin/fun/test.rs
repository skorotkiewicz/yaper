use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host.default_input_device().unwrap();
    let config = device.default_input_config()?;

    println!(
        "🎤 Testing: {} ({:?})",
        device.name()?,
        config.sample_format()
    );

    let err_fn = |e| eprintln!("Error: {}", e);
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            |data: &[f32], _: &cpal::InputCallbackInfo| {
                let peak = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                if peak > 0.5 {
                    let bar_len = (peak * 40.0) as usize;
                    let bar = "█".repeat(bar_len.min(40));
                    println!("\r[{}] {:.3}", bar, peak);
                    // eprintln!("🔊 Peak: {:.3}", peak);
                }
            },
            err_fn,
            None,
        )?,
        _ => return Err("Only F32 supported".into()),
    };

    stream.play()?;
    println!("Listening... (Ctrl+C to stop)");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
