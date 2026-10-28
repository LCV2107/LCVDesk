//! Reproducción del sonido remoto por la salida por defecto del visor.

use crate::{Chunk, default_render_id};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

/// Colchón antes de empezar a sonar (y tras quedarse sin datos).
const PREBUFFER_MS: u32 = 40;
/// Cuánto sonido se mantiene en cola en el dispositivo.
const DEVICE_TARGET_MS: u32 = 40;
/// Si se acumula más que esto (la red entregó a golpes), se descarta lo más antiguo para no ir
/// cada vez más retrasado respecto al vídeo.
const MAX_QUEUE_MS: u32 = 200;

/// Reproductor en marcha. Al soltarlo se detiene.
pub struct Player {
    tx: SyncSender<Chunk>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn start() -> Self {
        // ~1 s de trozos de 20 ms: de sobra; si se llena, el hilo de reproducción no va al día.
        let (tx, rx) = sync_channel::<Chunk>(50);
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("audio-player".into()).spawn(move || run(&s, rx)).ok();
        Self { tx, stop, thread }
    }

    /// Encola un trozo para reproducir. Nunca bloquea: si el reproductor va atrasado, se descarta.
    pub fn push(&self, chunk: Chunk) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(chunk) {
            // El hilo de reproducción recorta la cola por su cuenta; aquí no hace falta más.
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(stop: &AtomicBool, rx: Receiver<Chunk>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    // Trozo que trajo un formato distinto y obligó a reabrir el dispositivo.
    let mut carry: Option<Chunk> = None;
    while !stop.load(Ordering::Relaxed) {
        // Esperar el primer trozo: de él sale el formato con el que abrir el dispositivo.
        let first = match carry.take() {
            Some(c) => c,
            None => match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(c) => c,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };
        match session(stop, &rx, first) {
            Ok(Some(next)) => carry = Some(next),
            Ok(None) => {}
            Err(e) => {
                warn!("audio: reproducción interrumpida ({e}); reintentando");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    unsafe { CoUninitialize() };
}

/// Una apertura del dispositivo con el formato de `first`. Devuelve `Ok(Some(trozo))` si llegó un
/// formato distinto (hay que reabrir con él) y `Ok(None)` al parar o si cambió la salida por defecto.
fn session(stop: &AtomicBool, rx: &Receiver<Chunk>, first: Chunk) -> windows::core::Result<Option<Chunk>> {
    let (rate, channels) = (first.sample_rate, first.channels.max(1));
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let device_id = default_render_id(&enumerator);
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let block = channels * 2;
        let wfx = WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // AUTOCONVERTPCM: Windows convierte a la frecuencia y canales del dispositivo si difieren.
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            1_000_000, // 100 ms
            0,
            &wfx,
            None,
        )?;
        let render: IAudioRenderClient = client.GetService()?;
        let buffer_frames = client.GetBufferSize()? as usize;
        info!("audio: reproduciendo el sonido remoto ({rate} Hz, {channels} canales)");

        let ms = |m: u32| (rate * m / 1000) as usize;
        let ch = channels as usize;
        let mut queue: VecDeque<i16> = first.samples.into_iter().collect();
        let mut playing = false;
        let mut started = false;
        let mut dropped_frames = 0usize;
        let mut last_device_check = Instant::now();
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(None);
            }
            // Recoger lo que haya llegado.
            loop {
                match rx.try_recv() {
                    Ok(c) if c.sample_rate != rate || c.channels.max(1) != channels => {
                        let _ = client.Stop();
                        return Ok(Some(c));
                    }
                    Ok(c) => queue.extend(c.samples),
                    Err(_) => break,
                }
            }
            // Acotar el retraso: descartar lo más antiguo si se acumuló demasiado.
            let max = ms(MAX_QUEUE_MS) * ch;
            if queue.len() > max {
                let extra = queue.len() - max;
                queue.drain(..extra);
                dropped_frames += extra / ch;
            }
            if !playing && queue.len() >= ms(PREBUFFER_MS) * ch {
                playing = true;
            }

            let padding = client.GetCurrentPadding()? as usize;
            let target = ms(DEVICE_TARGET_MS).min(buffer_frames);
            if playing && padding < target {
                let frames = (target - padding).min(queue.len() / ch);
                if frames > 0 {
                    let data = render.GetBuffer(frames as u32)?;
                    let out = std::slice::from_raw_parts_mut(data as *mut i16, frames * ch);
                    for (o, s) in out.iter_mut().zip(queue.drain(..frames * ch)) {
                        *o = s;
                    }
                    render.ReleaseBuffer(frames as u32, 0)?;
                    if !started {
                        client.Start()?;
                        started = true;
                    }
                }
                // Sin datos y el dispositivo vacío: volver a acumular colchón antes de sonar.
                if queue.is_empty() && padding == 0 {
                    playing = false;
                }
            }

            if last_device_check.elapsed() >= Duration::from_secs(2) {
                last_device_check = Instant::now();
                if dropped_frames > 0 {
                    info!("audio: descartados {} ms de sonido atrasado", dropped_frames * 1000 / rate as usize);
                    dropped_frames = 0;
                }
                if default_render_id(&enumerator) != device_id {
                    info!("audio: cambió la salida por defecto; reabriendo la reproducción");
                    break Ok(None);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let _ = client.Stop();
        result
    }
}
