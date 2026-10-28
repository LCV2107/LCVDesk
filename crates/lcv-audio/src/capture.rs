//! Captura *loopback*: lo que suena por la salida por defecto del equipo.

use crate::{Chunk, default_render_id};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

/// Captura en marcha. Al soltarla se detiene.
pub struct Loopback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Loopback {
    /// Empieza a capturar en un hilo propio. `sink` recibe trozos de 20 ms (estéreo, 16 bits) y
    /// devuelve `false` para parar (p. ej. si la conexión se cerró).
    pub fn start(sink: impl FnMut(Chunk) -> bool + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("audio-loopback".into()).spawn(move || run(&s, sink)).ok();
        Self { stop, thread }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(stop: &AtomicBool, mut sink: impl FnMut(Chunk) -> bool) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    while !stop.load(Ordering::Relaxed) {
        match session(stop, &mut sink) {
            Ok(true) => break,
            // Cambió el dispositivo por defecto: reabrir enseguida en el nuevo.
            Ok(false) => info!("audio: cambió la salida por defecto; reabriendo la captura"),
            Err(e) => {
                warn!("audio: captura interrumpida ({e}); reintentando");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    unsafe { CoUninitialize() };
}

/// Formato de mezcla del dispositivo (lo que entrega el loopback).
struct MixFormat {
    rate: u32,
    channels: usize,
    bits: u16,
    float: bool,
    block_align: usize,
}

impl MixFormat {
    unsafe fn read(p: *const WAVEFORMATEX) -> Self {
        let f = unsafe { std::ptr::read_unaligned(p) };
        let tag = f.wFormatTag;
        // WAVE_FORMAT_IEEE_FLOAT = 3; WAVE_FORMAT_EXTENSIBLE = 0xFFFE, con el subformato en el GUID
        // (su primer campo es 3 para float y 1 para PCM entero).
        let float = match tag {
            3 => true,
            0xFFFE => {
                let ext = unsafe { std::ptr::read_unaligned(p as *const WAVEFORMATEXTENSIBLE) };
                let sub = ext.SubFormat;
                sub.data1 == 3
            }
            _ => false,
        };
        Self {
            rate: f.nSamplesPerSec,
            channels: f.nChannels as usize,
            bits: f.wBitsPerSample,
            float,
            block_align: f.nBlockAlign as usize,
        }
    }

    /// Muestra `ch` del fotograma que empieza en `frame` → i16.
    fn sample(&self, frame: &[u8], ch: usize) -> i16 {
        let bytes = (self.bits / 8) as usize;
        let at = ch * bytes;
        let Some(b) = frame.get(at..at + bytes) else { return 0 };
        match (self.float, self.bits) {
            (true, 32) => {
                let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
            }
            (false, 16) => i16::from_le_bytes([b[0], b[1]]),
            (false, 24) => i16::from_le_bytes([b[1], b[2]]),
            (false, 32) => i16::from_le_bytes([b[2], b[3]]),
            _ => 0,
        }
    }
}

/// Una apertura del dispositivo. `Ok(true)` = se pidió parar; `Ok(false)` = cambió el dispositivo.
fn session(stop: &AtomicBool, sink: &mut impl FnMut(Chunk) -> bool) -> windows::core::Result<bool> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let device_id = default_render_id(&enumerator);
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let fmt_ptr = client.GetMixFormat()?;
        let fmt = MixFormat::read(fmt_ptr);
        // 100 ms de buffer en el dispositivo; se vacía cada 10 ms.
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 1_000_000, 0, fmt_ptr, None);
        CoTaskMemFree(Some(fmt_ptr as *const _));
        init?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        info!(
            "audio: capturando la salida del equipo ({} Hz, {} canales, {} bits{})",
            fmt.rate,
            fmt.channels,
            fmt.bits,
            if fmt.float { " float" } else { "" }
        );

        let chunk_frames = (fmt.rate / 50).max(1) as usize; // 20 ms
        let mut pending: Vec<i16> = Vec::with_capacity(chunk_frames * 4);
        let mut last_device_check = Instant::now();
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(true);
            }
            std::thread::sleep(Duration::from_millis(10));
            while capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if silent || data.is_null() {
                    pending.resize(pending.len() + frames as usize * 2, 0);
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * fmt.block_align);
                    for frame in bytes.chunks_exact(fmt.block_align) {
                        let left = fmt.sample(frame, 0);
                        let right = if fmt.channels > 1 { fmt.sample(frame, 1) } else { left };
                        pending.push(left);
                        pending.push(right);
                    }
                }
                capture.ReleaseBuffer(frames)?;
            }
            let mut closed = false;
            while !closed && pending.len() >= chunk_frames * 2 {
                let rest = pending.split_off(chunk_frames * 2);
                let samples = std::mem::replace(&mut pending, rest);
                closed = !sink(Chunk { sample_rate: fmt.rate, channels: 2, samples });
            }
            if closed {
                break Ok(true);
            }
            if last_device_check.elapsed() >= Duration::from_secs(2) {
                last_device_check = Instant::now();
                if default_render_id(&enumerator) != device_id {
                    break Ok(false);
                }
            }
        };
        let _ = client.Stop();
        result
    }
}
