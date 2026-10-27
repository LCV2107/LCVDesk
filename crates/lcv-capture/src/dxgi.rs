//! DXGI Desktop Duplication (Windows 8+), con selección de monitor.
//!
//! Flujo: `AcquireNextFrame` → copia de la textura del escritorio a una textura *staging* →
//! `ReleaseFrame` cuanto antes → `Map` y copia a un buffer compacto.

use crate::{CaptureError, DisplayInfo, Frame};
use std::time::Duration;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

/// Enumera todos los monitores conectados al escritorio, en todos los adaptadores (GPU) del equipo.
pub fn enumerate() -> Result<Vec<DisplayInfo>, CaptureError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut displays = Vec::new();
    let mut index = 0u32;
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        for j in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(j) }) else { break };
            let desc = unsafe { output.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let r = desc.DesktopCoordinates;
            displays.push(DisplayInfo {
                index,
                name: String::from_utf16_lossy(&desc.DeviceName)
                    .trim_end_matches('\0')
                    .to_string(),
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
                x: r.left,
                y: r.top,
                primary: r.left == 0 && r.top == 0,
            });
            index += 1;
        }
    }
    if displays.is_empty() {
        return Err(CaptureError::NoPrimaryOutput);
    }
    Ok(displays)
}

pub struct Dxgi {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
    buf: Vec<u8>,
    has_frame: bool,
}

impl Dxgi {
    /// Duplica el monitor cuyo índice corresponde al de [`enumerate`].
    pub fn new(display_index: u32) -> Result<Self, CaptureError> {
        let (adapter, output) = output_at(display_index)?;
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or(CaptureError::NoPrimaryOutput)?;
        let context = context.ok_or(CaptureError::NoPrimaryOutput)?;
        let duplication = unsafe { output.DuplicateOutput(&device)? };
        let desc = unsafe { duplication.GetDesc() };
        Ok(Self {
            device,
            context,
            duplication,
            staging: None,
            width: desc.ModeDesc.Width,
            height: desc.ModeDesc.Height,
            buf: Vec::new(),
            has_frame: false,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// `Ok(true)` si hay imagen nueva en el buffer; `Ok(false)` si no cambió (o solo se movió el ratón).
    pub fn acquire(&mut self, timeout: Duration) -> windows::core::Result<bool> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let res = unsafe {
            self.duplication.AcquireNextFrame(timeout.as_millis() as u32, &mut info, &mut resource)
        };
        match res {
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) => return Err(e),
            Ok(()) => {}
        }
        // LastPresentTime == 0 → solo cambió el puntero; la imagen sigue igual.
        let changed = info.LastPresentTime != 0 || !self.has_frame;
        let copied = match (changed, resource) {
            (true, Some(resource)) => self.copy_to_staging(&resource),
            _ => Ok(false),
        };
        unsafe { self.duplication.ReleaseFrame()? };
        if copied? {
            self.read_staging()?;
            self.has_frame = true;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn frame(&self) -> Option<Frame<'_>> {
        self.has_frame.then(|| Frame { width: self.width, height: self.height, data: &self.buf })
    }

    fn copy_to_staging(&mut self, resource: &IDXGIResource) -> windows::core::Result<bool> {
        let texture: ID3D11Texture2D = resource.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if self.staging.is_none() || desc.Width != self.width || desc.Height != self.height {
            self.width = desc.Width;
            self.height = desc.Height;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging))? };
            self.staging = staging;
        }
        let Some(staging) = self.staging.as_ref() else { return Ok(false) };
        unsafe { self.context.CopyResource(staging, &texture) };
        Ok(true)
    }

    fn read_staging(&mut self) -> windows::core::Result<()> {
        let Some(staging) = self.staging.as_ref() else { return Ok(()) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };
        let row = self.width as usize * 4;
        let pitch = mapped.RowPitch as usize;
        self.buf.resize(row * self.height as usize, 0);
        let src = mapped.pData as *const u8;
        for y in 0..self.height as usize {
            // SAFETY: Map garantiza `RowPitch * height` bytes legibles mientras el recurso está mapeado.
            let line = unsafe { std::slice::from_raw_parts(src.add(y * pitch), row) };
            self.buf[y * row..(y + 1) * row].copy_from_slice(line);
        }
        unsafe { self.context.Unmap(staging, 0) };
        Ok(())
    }
}

/// Localiza el adaptador y la salida del monitor con el índice de escritorio `target`.
fn output_at(target: u32) -> Result<(IDXGIAdapter1, IDXGIOutput1), CaptureError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut index = 0u32;
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        for j in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(j) }) else { break };
            let desc = unsafe { output.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            if index == target {
                return Ok((adapter, output.cast()?));
            }
            index += 1;
        }
    }
    Err(CaptureError::NoPrimaryOutput)
}
