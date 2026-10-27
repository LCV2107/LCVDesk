//! Códec de video H.264 por software (OpenH264), configurado para baja latencia.
//!
//! La conversión de color usa el crate `yuv` (SIMD). Ambos extremos usan BT.601 de rango limitado.

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig, Flush};
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode,
    UsageType,
};
use openh264::formats::{YUVSlices, YUVSource};
use yuv::{
    BufferStoreMut, YuvConversionMode, YuvPlanarImage, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
};

const RANGE: YuvRange = YuvRange::Limited;
const MATRIX: YuvStandardMatrix = YuvStandardMatrix::Bt601;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("OpenH264: {0}")]
    OpenH264(#[from] openh264::Error),
    #[error("conversión de color: {0}")]
    Yuv(String),
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub struct H264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
    /// I420 contiguo: Y (w×h), U (w/2×h/2), V (w/2×h/2).
    yuv: Vec<u8>,
}

impl H264Encoder {
    /// `width`/`height` de la fuente; se recortan a pares (requisito de I420).
    pub fn new(width: u32, height: u32, fps: f32, bitrate_bps: u32) -> Result<Self, CodecError> {
        let (w, h) = (width & !1, height & !1);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4) as u16;
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .max_frame_rate(FrameRate::from_hz(fps))
            .bitrate(BitRate::from_bps(bitrate_bps))
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(false)
            .complexity(Complexity::Low)
            // Valores que OpenH264 exige en modo "screen content" (los otros los ignora con aviso).
            .scene_change_detect(true)
            .adaptive_quantization(false)
            .background_detection(false)
            // Sin IDR periódicos: se piden bajo demanda (conexión nueva o pérdida de sincronía).
            .intra_frame_period(IntraFramePeriod::from_num_frames(0))
            .num_threads(threads);
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)?;
        Ok(Self { encoder, width: w, height: h, yuv: vec![0; (w * h * 3 / 2) as usize] })
    }

    /// Dimensiones de la fuente que acepta este codificador (antes del recorte a pares).
    pub fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width & !1 && self.height == height & !1
    }

    pub fn request_keyframe(&mut self) {
        self.encoder.force_intra_frame();
    }

    /// Codifica un fotograma BGRA compacto (`stride` = ancho de la fuente × 4).
    pub fn encode_bgra(&mut self, bgra: &[u8], stride: u32) -> Result<EncodedFrame, CodecError> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (y, uv) = self.yuv.split_at_mut(w * h);
        let (u, v) = uv.split_at_mut(w * h / 4);
        let mut planar = YuvPlanarImageMut {
            y_plane: BufferStoreMut::Borrowed(y),
            y_stride: self.width,
            u_plane: BufferStoreMut::Borrowed(u),
            u_stride: self.width / 2,
            v_plane: BufferStoreMut::Borrowed(v),
            v_stride: self.width / 2,
            width: self.width,
            height: self.height,
        };
        yuv::bgra_to_yuv420(&mut planar, bgra, stride, RANGE, MATRIX, YuvConversionMode::Balanced)
            .map_err(|e| CodecError::Yuv(e.to_string()))?;

        let (y, uv) = self.yuv.split_at(w * h);
        let (u, v) = uv.split_at(w * h / 4);
        let source = YUVSlices::new((y, u, v), (w, h), (w, w / 2, w / 2));
        let bitstream = self.encoder.encode(&source)?;
        let keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        Ok(EncodedFrame { data: bitstream.to_vec(), keyframe })
    }
}

pub struct DecodedFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// RGBA compacto (stride = ancho × 4).
    pub rgba: &'a [u8],
}

pub struct H264Decoder {
    decoder: Decoder,
    rgba: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        // Flush tras cada paquete: el fotograma sale en la misma llamada, sin retener ninguno.
        let config = DecoderConfig::new().flush_after_decode(Flush::Flush);
        Ok(Self { decoder: Decoder::with_api_config(OpenH264API::from_source(), config)?, rgba: Vec::new() })
    }

    /// `Ok(None)` si el paquete no produjo imagen (p. ej. solo SPS/PPS).
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame<'_>>, CodecError> {
        let Some(yuv) = self.decoder.decode(data)? else { return Ok(None) };
        let (w, h) = yuv.dimensions();
        let (ys, us, vs) = yuv.strides();
        let planar = YuvPlanarImage {
            y_plane: yuv.y(),
            y_stride: ys as u32,
            u_plane: yuv.u(),
            u_stride: us as u32,
            v_plane: yuv.v(),
            v_stride: vs as u32,
            width: w as u32,
            height: h as u32,
        };
        self.rgba.resize(w * h * 4, 0);
        yuv::yuv420_to_rgba(&planar, &mut self.rgba, (w * 4) as u32, RANGE, MATRIX)
            .map_err(|e| CodecError::Yuv(e.to_string()))?;
        Ok(Some(DecodedFrame { width: w as u32, height: h as u32, rgba: &self.rgba }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codifica y decodifica una imagen sintética y comprueba que el color se conserva.
    #[test]
    fn encode_decode_roundtrip() {
        let (w, h) = (322u32, 180u32); // ancho par, alto par; 322 no es múltiplo de 16
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for px in bgra.chunks_exact_mut(4) {
            px.copy_from_slice(&[200, 100, 50, 255]); // B, G, R, A
        }
        let mut enc = H264Encoder::new(w, h, 60.0, 5_000_000).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let encoded = enc.encode_bgra(&bgra, w * 4).unwrap();
        assert!(encoded.keyframe, "el primer fotograma debe ser IDR");
        let frame = dec.decode(&encoded.data).unwrap().expect("debe producir imagen");
        assert_eq!((frame.width, frame.height), (w, h));
        let [r, g, b, _] = frame.rgba[..4] else { unreachable!() };
        assert!(r.abs_diff(50) < 8 && g.abs_diff(100) < 8 && b.abs_diff(200) < 8, "rgb={r},{g},{b}");
    }
}
