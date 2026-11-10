//! Microphone capture (desktop; on Android the page records), resampling to
//! whisper's rate, and WAV encoding for the server engine.

#[cfg(desktop)]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(desktop)]
use std::sync::mpsc;
#[cfg(desktop)]
use std::sync::{Arc, Mutex};
#[cfg(desktop)]
use std::thread::JoinHandle;

#[cfg(desktop)]
use anyhow::{anyhow, Context};
#[cfg(desktop)]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(desktop)]
use cpal::{FromSample, Sample, SampleFormat, SizedSample};

pub const WHISPER_RATE: u32 = 16_000;

#[cfg(desktop)]
/// A recording in progress. The cpal stream lives on its own thread (it isn't
/// `Send` on every backend); dropping the stop sender ends it.
pub struct Recording {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    /// Latest RMS level as f32 bits, for the pill's meter.
    pub level: Arc<AtomicU32>,
}

#[cfg(desktop)]
pub fn input_devices() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devs| devs.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

#[cfg(desktop)]
fn pick_device(name: &str) -> anyhow::Result<cpal::Device> {
    let host = cpal::default_host();
    if !name.is_empty() {
        if let Some(dev) = host
            .input_devices()?
            .find(|d| d.to_string() == name)
        {
            return Ok(dev);
        }
        log::warn!("microphone {name:?} not found, using default");
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no microphone found"))
}

#[cfg(desktop)]
impl Recording {
    pub fn start(device_name: &str) -> anyhow::Result<Self> {
        let device = pick_device(device_name)?;
        let supported = device
            .default_input_config()
            .context("microphone has no usable input format")?;
        let format = supported.sample_format();
        let config = supported.config();
        let channels = config.channels as usize;
        let sample_rate = config.sample_rate;

        let samples = Arc::new(Mutex::new(Vec::with_capacity(sample_rate as usize * 30)));
        let level = Arc::new(AtomicU32::new(0));
        let (stop, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();

        let thread = {
            let samples = samples.clone();
            let level = level.clone();
            std::thread::spawn(move || {
                let stream = match format {
                    SampleFormat::F32 => build::<f32>(&device, config, channels, samples, level),
                    SampleFormat::I16 => build::<i16>(&device, config, channels, samples, level),
                    SampleFormat::U16 => build::<u16>(&device, config, channels, samples, level),
                    SampleFormat::I32 => build::<i32>(&device, config, channels, samples, level),
                    other => Err(anyhow!("unsupported sample format {other}")),
                };
                let stream = match stream.and_then(|s| s.play().map(|_| s).map_err(Into::into)) {
                    Ok(s) => {
                        let _ = ready_tx.send(Ok(()));
                        s
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // Blocks until stop() sends or the sender is dropped.
                let _ = stop_rx.recv();
                drop(stream);
            })
        };

        ready_rx
            .recv()
            .map_err(|_| anyhow!("audio thread died"))??;

        Ok(Self { stop, thread, samples, sample_rate, level })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// Stops capture and returns mono 16 kHz samples for whisper.
    pub fn stop(self) -> Vec<f32> {
        let _ = self.stop.send(());
        let _ = self.thread.join();
        let captured = std::mem::take(&mut *self.samples.lock().unwrap());
        resample(&captured, self.sample_rate, WHISPER_RATE)
    }
}

#[cfg(desktop)]
fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    samples: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut sum_sq = 0.0f32;
            let mut out = samples.lock().unwrap();
            for frame in data.chunks(channels) {
                let mono = frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32;
                sum_sq += mono * mono;
                out.push(mono);
            }
            let frames = (data.len() / channels).max(1) as f32;
            level.store((sum_sq / frames).sqrt().to_bits(), Ordering::Relaxed);
        },
        |e| log::error!("audio stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Box-filter then linearly interpolate. Crude, but speech above 8 kHz carries
/// little that whisper needs, and it avoids pulling in a DSP crate.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let window = ratio.floor().max(1.0) as usize;
    let smoothed: Vec<f32> = if window > 1 {
        let mut acc = 0.0f32;
        let mut out = Vec::with_capacity(input.len());
        for i in 0..input.len() {
            acc += input[i];
            if i >= window {
                acc -= input[i - window];
            }
            out.push(acc / window.min(i + 1) as f32);
        }
        out
    } else {
        input.to_vec()
    };
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = smoothed[idx];
            let b = *smoothed.get(idx + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

pub fn to_wav(samples: &[f32]) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: WHISPER_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut buf, spec)?;
    for s in samples {
        writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_48k_to_16k_keeps_duration() {
        let one_second = vec![0.5f32; 48_000];
        let out = resample(&one_second, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!((out[8_000] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn wav_roundtrip_header() {
        let wav = to_wav(&[0.0; 160]).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 320);
    }
}
