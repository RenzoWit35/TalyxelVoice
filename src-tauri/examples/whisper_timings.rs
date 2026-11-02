//! Prints whisper.cpp's per-stage timings for a WAV: `cargo run --release --example whisper_timings -- model.bin clip.wav [threads]`
use std::ffi::CString;
use whisper_rs_sys as sys;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let threads: i32 = args.get(3).and_then(|t| t.parse().ok()).unwrap_or(8);
    let samples: Vec<f32> = hound::WavReader::open(&args[2]).unwrap().into_samples::<i16>()
        .map(|s| s.unwrap() as f32 / i16::MAX as f32).collect();
    unsafe {
        let mut cp = sys::whisper_context_default_params();
        cp.flash_attn = true;
        let path = CString::new(args[1].as_str()).unwrap();
        let ctx = sys::whisper_init_from_file_with_params(path.as_ptr(), cp);
        for run in 0..2 {
            let mut p = sys::whisper_full_default_params(sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY);
            p.n_threads = threads;
            p.no_timestamps = true;
            p.audio_ctx = ((samples.len() as f32 / 16000.0 * 50.0) as i32 + 50 + 63) / 64 * 64;
            let lang = CString::new(args.get(4).map_or("en", |s| s.as_str())).unwrap();
            p.language = lang.as_ptr();
            let prompt = CString::new(args.get(5).map_or("", |s| s.as_str())).unwrap();
            if args.len() > 5 { p.initial_prompt = prompt.as_ptr(); }
            sys::whisper_reset_timings(ctx);
            let t = std::time::Instant::now();
            sys::whisper_full(ctx, p, samples.as_ptr(), samples.len() as i32);
            eprintln!("run {run}: {:?}", t.elapsed());
            sys::whisper_print_timings(ctx);
        }
    }
}
