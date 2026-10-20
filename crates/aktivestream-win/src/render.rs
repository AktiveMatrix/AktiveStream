//! Direct3D 11 presentation: the framebuffer lives in a GPU texture, dirty rectangles are
//! uploaded as they arrive, and a pixel shader scales it into the view with an exact area
//! filter (each output pixel is the coverage-weighted average of the source pixels under
//! it). Downscaled text stays as sharp as GDI's HALFTONE mode at a fraction of the cost,
//! and enlarging keeps hard pixel edges, which suits magnification.

use aktivestream_rfb::{Framebuffer, Rect};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer,
    ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11ShaderResourceView,
    ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGISwapChain1,
};
use windows::core::{PCSTR, s};

use crate::view::View;

const SHADER: &str = r#"
cbuffer ViewConstants : register(b0) {
    float2 origin;    // top-left of the view in render-target pixels
    float2 scale;     // source texels per destination pixel
    int2 src_size;    // framebuffer size in texels
    int2 pad;
};
Texture2D<float4> fb : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}

float4 ps_area(float4 pos : SV_Position) : SV_Target {
    float2 s0 = (pos.xy - 0.5 - origin) * scale;
    float2 s1 = s0 + scale;
    int2 t0 = (int2)floor(s0);
    int2 t1 = min((int2)ceil(s1) - 1, t0 + 7);
    float3 acc = 0;
    float total = 0;
    [loop] for (int y = t0.y; y <= t1.y; y++) {
        float wy = min(s1.y, y + 1) - max(s0.y, y);
        [loop] for (int x = t0.x; x <= t1.x; x++) {
            float w = (min(s1.x, x + 1) - max(s0.x, x)) * wy;
            acc += fb.Load(int3(clamp(int2(x, y), 0, src_size - 1), 0)).rgb * w;
            total += w;
        }
    }
    return float4(acc / max(total, 1e-6), 1);
}
"#;

#[repr(C)]
struct Constants {
    origin: [f32; 2],
    scale: [f32; 2],
    src_size: [i32; 2],
    pad: [i32; 2],
}

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    target: Option<ID3D11RenderTargetView>,
    texture: Option<(ID3D11Texture2D, ID3D11ShaderResourceView, u32, u32)>,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    constants: ID3D11Buffer,
    size: (u32, u32),
}

fn compile(entry: PCSTR, target: PCSTR) -> windows::core::Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("aktivestream.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let detail = errors
            .map(|blob: ID3DBlob| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    blob.GetBufferPointer() as *const u8,
                    blob.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        return Err(windows::core::Error::new(e.code(), detail));
    }
    Ok(code.expect("D3DCompile returned no bytecode"))
}

fn bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}

impl Renderer {
    pub fn new(hwnd: HWND) -> windows::core::Result<Renderer> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.expect("D3D11CreateDevice returned no device");
            let context = context.expect("D3D11CreateDevice returned no context");

            let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swap_chain = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;

            let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
            let ps_code = compile(s!("ps_area"), s!("ps_5_0"))?;
            let mut vertex_shader = None;
            device.CreateVertexShader(bytes(&vs_code), None, Some(&mut vertex_shader))?;
            let mut pixel_shader = None;
            device.CreatePixelShader(bytes(&ps_code), None, Some(&mut pixel_shader))?;
            let mut constants = None;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: size_of::<Constants>() as u32,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut constants),
            )?;
            let mut renderer = Renderer {
                device,
                context,
                swap_chain,
                target: None,
                texture: None,
                vertex_shader: vertex_shader.expect("vertex shader"),
                pixel_shader: pixel_shader.expect("pixel shader"),
                constants: constants.expect("constant buffer"),
                size: (0, 0),
            };
            renderer.create_target()?;
            Ok(renderer)
        }
    }

    fn create_target(&mut self) -> windows::core::Result<()> {
        unsafe {
            let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            buffer.GetDesc(&mut desc);
            self.size = (desc.Width, desc.Height);
            let mut target = None;
            self.device.CreateRenderTargetView(&buffer, None, Some(&mut target))?;
            self.target = target;
        }
        Ok(())
    }

    /// Matches the swap chain to the window's client size.
    pub fn resize(&mut self, width: u32, height: u32) -> windows::core::Result<()> {
        if width == 0 || height == 0 || (width, height) == self.size {
            return Ok(());
        }
        self.target = None;
        unsafe {
            self.context.OMSetRenderTargets(None, None);
            self.swap_chain.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
        }
        self.create_target()
    }

    /// Uploads `area` of the framebuffer (everything if the texture is new or resized).
    pub fn upload(&mut self, fb: &Framebuffer, area: Option<Rect>) -> windows::core::Result<()> {
        if fb.width == 0 || fb.height == 0 {
            return Ok(());
        }
        let fresh = !matches!(&self.texture, Some((_, _, w, h)) if (*w, *h) == (fb.width, fb.height));
        if fresh {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: fb.width,
                Height: fb.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            unsafe {
                let mut texture = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut texture))?;
                let texture = texture.expect("texture");
                let mut view = None;
                self.device.CreateShaderResourceView(&texture, None, Some(&mut view))?;
                self.texture = Some((texture, view.expect("shader resource view"), fb.width, fb.height));
            }
        }
        let full = Rect { x: 0, y: 0, w: fb.width as u16, h: fb.height as u16 };
        let r = if fresh { full } else { area.unwrap_or(full) };
        let x1 = (u32::from(r.x) + u32::from(r.w)).min(fb.width);
        let y1 = (u32::from(r.y) + u32::from(r.h)).min(fb.height);
        if u32::from(r.x) >= x1 || u32::from(r.y) >= y1 {
            return Ok(());
        }
        let Some((texture, ..)) = &self.texture else { return Ok(()) };
        let region = D3D11_BOX { left: r.x.into(), top: r.y.into(), front: 0, right: x1, bottom: y1, back: 1 };
        let start = usize::from(r.y) * fb.width as usize + usize::from(r.x);
        unsafe {
            self.context.UpdateSubresource(
                texture,
                0,
                Some(&region),
                fb.pixels[start..].as_ptr().cast(),
                fb.width * 4,
                0,
            );
        }
        Ok(())
    }

    /// Draws the framebuffer into `view` on a black background.
    pub fn draw(&mut self, view: View) {
        let (Some(target), Some((_, srv, w, h))) = (&self.target, &self.texture) else { return };
        unsafe {
            self.context.ClearRenderTargetView(target, &[0.0, 0.0, 0.0, 1.0]);
            if view.w <= 0 || view.h <= 0 {
                return;
            }
            let constants = Constants {
                origin: [view.x as f32, view.y as f32],
                scale: [*w as f32 / view.w as f32, *h as f32 / view.h as f32],
                src_size: [*w as i32, *h as i32],
                pad: [0, 0],
            };
            self.context.UpdateSubresource(&self.constants, 0, None, (&raw const constants).cast(), 0, 0);
            self.context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: view.x as f32,
                TopLeftY: view.y as f32,
                Width: view.w as f32,
                Height: view.h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            self.context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetInputLayout(None);
            self.context.VSSetShader(&self.vertex_shader, None);
            self.context.PSSetShader(&self.pixel_shader, None);
            self.context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            self.context.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
            self.context.Draw(3, 0);
        }
    }

    pub fn present(&self) -> windows::core::Result<()> {
        unsafe { self.swap_chain.Present(0, DXGI_PRESENT(0)).ok() }
    }

    /// Reads back the current back buffer as RGBA (call after `draw`, before `present`).
    pub fn capture(&self) -> windows::core::Result<(u32, u32, Vec<u8>)> {
        unsafe {
            let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            buffer.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.expect("staging texture");
            self.context.CopyResource(&staging, &buffer);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let mut rgba = Vec::with_capacity(desc.Width as usize * desc.Height as usize * 4);
            for y in 0..desc.Height as usize {
                let row = std::slice::from_raw_parts(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    desc.Width as usize * 4,
                );
                for px in row.chunks_exact(4) {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
            }
            self.context.Unmap(&staging, 0);
            Ok((desc.Width, desc.Height, rgba))
        }
    }
}
