//! The host's audio output and input for the sound card (audio.rs): `cpal`'s
//! default devices (CoreAudio, WASAPI, ALSA), the output's callback mixing
//! the card's streams into it, the input's capturing into them.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Opens the host's default output and plays the card's mix through it:
/// whether it could. The stream lives on a thread of its own (cpal's
/// streams needn't be `Send`).
pub fn start() -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("wasm-env audio out".into()).spawn(move || match open() {
        Ok(stream) => {
            let _ = tx.send(true);
            // (Playing until the process exits.)
            let _keep = stream;
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            if std::env::var_os("WASM_ENV_DEBUG_AUDIO").is_some() {
                eprintln!("wasm-env: no host audio output ({e}): sound goes nowhere");
            }
            let _ = tx.send(false);
        }
    });
    spawned.is_ok() && rx.recv().unwrap_or(false)
}

fn open() -> Result<cpal::Stream, String> {
    let device = cpal::default_host().default_output_device().ok_or("no output device")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let mut stereo: Vec<f32> = Vec::new();
    // The card's mix (float stereo) into the device's frames: left and
    // right to its first two channels (both to a mono one), silence to the
    // rest.
    let mut fill = move |out: &mut dyn FnMut(usize, f32), frames: usize| {
        stereo.resize(2 * frames, 0.0);
        crate::audio::mix(&mut stereo, rate);
        for f in 0..frames {
            let (l, r) = (stereo[2 * f], stereo[2 * f + 1]);
            for c in 0..channels {
                out(f * channels + c, match (channels, c) {
                    (1, _) => 0.5 * (l + r),
                    (_, 0) => l,
                    (_, 1) => r,
                    _ => 0.0,
                });
            }
        }
    };
    let error = |e: cpal::Error| eprintln!("wasm-env: host audio: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config.clone(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(&mut |k, v| data[k] = v, frames);
            },
            error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config.clone(),
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(&mut |k, v| data[k] = (v * 32767.0) as i16, frames);
            },
            error,
            None,
        ),
        f => return Err(format!("its sample format ({f:?})")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Opens the host's default input and captures through it into the card's
/// capture streams: whether it could. (A thread of its own, as start's.)
pub fn start_input() -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("wasm-env audio in".into()).spawn(move || match open_input() {
        Ok(stream) => {
            let _ = tx.send(true);
            let _keep = stream;
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            if std::env::var_os("WASM_ENV_DEBUG_AUDIO").is_some() {
                eprintln!("wasm-env: no host audio input ({e}): capture hears silence");
            }
            let _ = tx.send(false);
        }
    });
    spawned.is_ok() && rx.recv().unwrap_or(false)
}

fn open_input() -> Result<cpal::Stream, String> {
    let device = cpal::default_host().default_input_device().ok_or("no input device")?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let mut stereo: Vec<f32> = Vec::new();
    // The device's frames as float stereo: its first two channels (a mono
    // one to both).
    let mut take = move |sample: &dyn Fn(usize) -> f32, frames: usize| {
        stereo.clear();
        for f in 0..frames {
            let l = sample(f * channels);
            let r = if channels > 1 { sample(f * channels + 1) } else { l };
            stereo.extend([l, r]);
        }
        crate::audio::capture(&stereo, rate);
    };
    let error = |e: cpal::Error| eprintln!("wasm-env: host audio input: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.clone(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| take(&|k| data[k], data.len() / channels),
            error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.clone(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| take(&|k| data[k] as f32 / 32768.0, data.len() / channels),
            error,
            None,
        ),
        f => return Err(format!("its sample format ({f:?})")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}
