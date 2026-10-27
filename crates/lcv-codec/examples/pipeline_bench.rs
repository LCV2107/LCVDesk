//! Mide la cadena completa en un solo PC: captura real → BGRA→I420 + H.264 → TCP local → decodificación → RGBA.
//!
//! Para simular movimiento sobre una pantalla quieta, se dibuja un rectángulo que se desplaza en cada fotograma.
//!
//! Uso: `cargo run --release -p lcv-codec --example pipeline_bench [fotogramas]`

use lcv_capture::Capturer;
use lcv_codec::{H264Decoder, H264Encoder};
use lcv_proto::{Message, VideoFrame, read_message, write_message};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn main() {
    let frames: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(300);
    // Monitor principal (si el índice no existe, Capturer recurre al primario).
    let mut capturer = Capturer::new(0).expect("captura");
    // Primer fotograma (DXGI siempre entrega la imagen completa al empezar).
    let mut base = None;
    for _ in 0..50 {
        if let Some(f) = capturer.next_frame(Duration::from_millis(100)).unwrap() {
            base = Some((f.width, f.height, f.data.to_vec()));
            break;
        }
    }
    let (w, h, base) = base.or_else(|| capturer.last_frame().map(|f| (f.width, f.height, f.data.to_vec()))).expect("sin fotograma");
    println!("pantalla {w}x{h}, captura {:?}, {frames} fotogramas", capturer.backend());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let receiver = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let mut decode = Vec::new();
        for _ in 0..frames {
            let Message::VideoFrame(f) = read_message(&mut s).unwrap() else { panic!() };
            let t = Instant::now();
            dec.decode(&f.data).unwrap().expect("imagen");
            decode.push(t.elapsed());
        }
        decode
    });

    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_nodelay(true).unwrap();
    let mut enc = H264Encoder::new(w, h, 60.0, 25_000_000).unwrap();
    let mut frame = base.clone();
    let (mut capture_t, mut encode_t, mut send_t, mut sizes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in 0..frames {
        let t = Instant::now();
        let _ = capturer.next_frame(Duration::ZERO);
        capture_t.push(t.elapsed());

        frame.copy_from_slice(&base);
        draw_box(&mut frame, w, h, i);
        let t = Instant::now();
        let out = enc.encode_bgra(&frame, w * 4).unwrap();
        encode_t.push(t.elapsed());
        sizes.push(out.data.len());

        let t = Instant::now();
        let msg = Message::VideoFrame(VideoFrame {
            seq: i as u64, width: w & !1, height: h & !1, keyframe: out.keyframe, host_us: 0, data: out.data,
        });
        write_message(&mut stream, &msg).unwrap();
        send_t.push(t.elapsed());
    }
    let decode_t = receiver.join().unwrap();

    report("captura (sin espera)", &capture_t);
    report("color + H.264", &encode_t);
    report("envío TCP local", &send_t);
    report("decodif. + RGBA", &decode_t);
    let avg = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    println!("tamaño medio {:.1} KB (IDR {:.1} KB) → {:.1} Mbps a 60 fps", avg / 1024.0, sizes[0] as f64 / 1024.0, avg * 8.0 * 60.0 / 1e6);
}

fn report(name: &str, samples: &[Duration]) {
    let mut ms: Vec<f64> = samples.iter().skip(1).map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let p = |q: f64| ms[((ms.len() - 1) as f64 * q) as usize];
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    println!("{name:<22} media {mean:6.2} ms   p50 {:6.2}   p95 {:6.2}   máx {:6.2}", p(0.5), p(0.95), p(1.0));
}

/// Rectángulo de 300×200 que recorre la pantalla (simula una ventana en movimiento).
fn draw_box(bgra: &mut [u8], w: u32, h: u32, i: usize) {
    let (bw, bh) = (300usize, 200usize);
    let x0 = (i * 13) % (w as usize - bw);
    let y0 = (i * 7) % (h as usize - bh);
    for y in y0..y0 + bh {
        let row = &mut bgra[(y * w as usize + x0) * 4..(y * w as usize + x0 + bw) * 4];
        for px in row.chunks_exact_mut(4) {
            px.copy_from_slice(&[(i * 3) as u8, 120, 255 - (i * 3) as u8, 255]);
        }
    }
}
