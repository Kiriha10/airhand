//! GPU 上明确平均每个 2×2 块的色度，避免视频处理器的单列采样放大文字彩边。

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{PCSTR, s};

pub(super) struct Target {
    pub(super) texture: ID3D11Texture2D,
    luma: ID3D11RenderTargetView,
    chroma: ID3D11RenderTargetView,
}

impl Target {
    pub(super) fn new(device: &ID3D11Device, w: u32, h: u32) -> Result<Self> {
        let texture = super::nv12_texture(device, w, h)?;
        let view = |format| -> Result<ID3D11RenderTargetView> {
            let desc = D3D11_RENDER_TARGET_VIEW_DESC {
                Format: format,
                ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
                },
            };
            let mut result = None;
            unsafe { device.CreateRenderTargetView(&texture, Some(&desc), Some(&mut result)) }
                .context("NV12 plane render target (requires D3D11.1 video formats)")?;
            result.context("null NV12 plane view")
        };
        Ok(Self {
            luma: view(DXGI_FORMAT_R8_UNORM)?,
            chroma: view(DXGI_FORMAT_R8G8_UNORM)?,
            texture,
        })
    }
}

pub(super) struct RgbToNv12 {
    // 每个会话独立录制，再用一条命令提交，避免共享立即上下文的状态互相干扰。
    commands: ID3D11DeviceContext,
    vertex: ID3D11VertexShader,
    luma: ID3D11PixelShader,
    chroma: ID3D11PixelShader,
    width: u32,
    height: u32,
}

fn compile(entry: PCSTR, profile: PCSTR) -> Result<Vec<u8>> {
    let source = include_bytes!("rgb_to_nv12.hlsl");
    let mut code = None;
    let mut errors = None;
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("rgb_to_nv12.hlsl"),
            None,
            None,
            entry,
            profile,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let details = errors.map(|blob| unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(
                blob.GetBufferPointer().cast(),
                blob.GetBufferSize(),
            ))
            .into_owned()
        });
        return Err(error).context(details.unwrap_or_else(|| "compile NV12 shader".into()));
    }
    let code = code.context("empty NV12 shader bytecode")?;
    Ok(unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize()).to_vec()
    })
}

impl RgbToNv12 {
    pub(super) fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self> {
        let (mut commands, mut vertex, mut luma, mut chroma) = (None, None, None, None);
        unsafe {
            device.CreateDeferredContext(0, Some(&mut commands))?;
            device.CreateVertexShader(
                &compile(s!("vertex"), s!("vs_4_0"))?,
                None,
                Some(&mut vertex),
            )?;
            device.CreatePixelShader(&compile(s!("luma"), s!("ps_4_0"))?, None, Some(&mut luma))?;
            device.CreatePixelShader(
                &compile(s!("chroma"), s!("ps_4_0"))?,
                None,
                Some(&mut chroma),
            )?;
        }
        Ok(Self {
            commands: commands.context("null NV12 deferred context")?,
            vertex: vertex.context("null NV12 vertex shader")?,
            luma: luma.context("null NV12 luma shader")?,
            chroma: chroma.context("null NV12 chroma shader")?,
            width,
            height,
        })
    }

    pub(super) fn convert(
        &self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        rgb: &ID3D11Texture2D,
        target: &Target,
    ) -> Result<()> {
        unsafe {
            let mut input = None;
            device
                .CreateShaderResourceView(rgb, None, Some(&mut input))
                .context("NV12 RGB input view")?;
            let cmd = &self.commands;
            cmd.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            cmd.VSSetShader(&self.vertex, None);
            cmd.PSSetShaderResources(0, Some(&[input]));
            for (plane, shader, divisor) in [
                (&target.luma, &self.luma, 1),
                (&target.chroma, &self.chroma, 2),
            ] {
                cmd.RSSetViewports(Some(&[D3D11_VIEWPORT {
                    Width: (self.width / divisor) as f32,
                    Height: (self.height / divisor) as f32,
                    MaxDepth: 1.0,
                    ..Default::default()
                }]));
                cmd.PSSetShader(shader, None);
                cmd.OMSetRenderTargets(Some(&[Some(plane.clone())]), None);
                cmd.Draw(3, 0);
            }
            cmd.PSSetShaderResources(0, Some(&[None]));
            cmd.OMSetRenderTargets(None, None);
            let mut list = None;
            cmd.FinishCommandList(false, Some(&mut list))
                .context("record RGB to NV12 conversion")?;
            context.ExecuteCommandList(&list.context("null NV12 command list")?, true);
        }
        Ok(())
    }
}
