use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{Interface, Result, BOOL, PCSTR};
use windows::Win32::Foundation::{HWND, HMODULE};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::sprite::FrameSet;

const SHADER: &str = r#"
cbuffer CB : register(b0) { uint g_slice; };

struct VSIn  { float2 pos : POSITION; float2 uv : TEXCOORD0; };
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

VSOut VSMain(VSIn i) {
    VSOut o;
    o.pos = float4(i.pos, 0.0, 1.0);
    o.uv  = i.uv;
    return o;
}

Texture2DArray g_tex  : register(t0);
SamplerState   g_samp : register(s0);

float4 PSMain(VSOut i) : SV_Target {
    return g_tex.Sample(g_samp, float3(i.uv, (float)g_slice));
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: Option<IDXGISwapChain1>,
    /// 后台缓冲的 RTV，按底层资源指针缓存。
    /// 本机驱动上只有索引 0 的缓冲能建 RTV，因此不能预建全量。
    rtv_cache: Vec<(usize, ID3D11RenderTargetView)>,

    dcomp: Option<IDCompositionDevice>,
    _target: Option<IDCompositionTarget>,
    visual: Option<IDCompositionVisual>,

    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    vbuf: ID3D11Buffer,
    cbuffer: ID3D11Buffer,

    /// 每个光标状态一套纹理数组：(SRV, 帧数, 宽, 高)
    srvs: HashMap<&'static str, (ID3D11ShaderResourceView, u32, u32, u32)>,
    active: Option<&'static str>,
    /// 绳子纹理（单帧，SRV + 宽 + 高）
    rope: Option<(ID3D11ShaderResourceView, u32, u32)>,

    pub width: u32,
    pub height: u32,
    cur_offset: (i32, i32),
    /// 上一帧是否处于遮挡状态（显示器休眠、锁屏、全屏应用）
    was_occluded: bool,
    /// 刚从遮挡恢复 —— 合成链路大概率已经失效，需要重建
    just_unoccluded: bool,
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let mut level = D3D_FEATURE_LEVEL_11_0;

            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )?;
            let device = device.unwrap();
            let context = context.unwrap();

            let dxgi_device: IDXGIDevice = device.cast()?;
            let adapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL::from(false),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let swap_chain = factory.CreateSwapChainForComposition(&device, &desc, None)?;

            let dcomp: IDCompositionDevice = DCompositionCreateDevice3(&dxgi_device)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap_chain)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let vs_blob = compile(SHADER, "VSMain", "vs_5_0")?;
            let ps_blob = compile(SHADER, "PSMain", "ps_5_0")?;
            let vs_code = blob_slice(&vs_blob);
            let ps_code = blob_slice(&ps_blob);

            let mut vs: Option<ID3D11VertexShader> = None;
            device.CreateVertexShader(vs_code, None, Some(&mut vs))?;
            let mut ps: Option<ID3D11PixelShader> = None;
            device.CreatePixelShader(ps_code, None, Some(&mut ps))?;

            let elems = [
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(b"POSITION\0".as_ptr()),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(b"TEXCOORD\0".as_ptr()),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 8,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];
            let mut layout: Option<ID3D11InputLayout> = None;
            device.CreateInputLayout(&elems, vs_code, Some(&mut layout))?;

            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            let mut sampler: Option<ID3D11SamplerState> = None;
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler))?;

            // 预乘 alpha 的合成公式：src + dst*(1-srcA)
            let rt_blend = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: BOOL::from(true),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let blend_desc = D3D11_BLEND_DESC {
                AlphaToCoverageEnable: BOOL::from(false),
                IndependentBlendEnable: BOOL::from(false),
                RenderTarget: [rt_blend; 8],
            };
            let mut blend: Option<ID3D11BlendState> = None;
            device.CreateBlendState(&blend_desc, Some(&mut blend))?;

            let raster_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                FrontCounterClockwise: BOOL::from(false),
                DepthBias: 0,
                DepthBiasClamp: 0.0,
                SlopeScaledDepthBias: 0.0,
                DepthClipEnable: BOOL::from(true),
                ScissorEnable: BOOL::from(false),
                MultisampleEnable: BOOL::from(false),
                AntialiasedLineEnable: BOOL::from(false),
            };
            let mut raster: Option<ID3D11RasterizerState> = None;
            device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;

            // D3D11 没有默认视口，不设的话 Draw 会被裁成 0x0
            let viewport = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            context.RSSetViewports(Some(&[viewport]));

            let vbuf_desc = D3D11_BUFFER_DESC {
                ByteWidth: (64 * size_of::<Vertex>()) as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut vbuf: Option<ID3D11Buffer> = None;
            device.CreateBuffer(&vbuf_desc, None, Some(&mut vbuf))?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut cbuffer: Option<ID3D11Buffer> = None;
            device.CreateBuffer(&cb_desc, None, Some(&mut cbuffer))?;

            Ok(Self {
                device,
                context,
                swap_chain: Some(swap_chain),
                rtv_cache: Vec::with_capacity(2),
                dcomp: Some(dcomp),
                _target: Some(target),
                visual: Some(visual),
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                sampler: sampler.unwrap(),
                blend: blend.unwrap(),
                raster: raster.unwrap(),
                vbuf: vbuf.unwrap(),
                cbuffer: cbuffer.unwrap(),
                srvs: HashMap::new(),
                active: None,
                rope: None,
                width,
                height,
                cur_offset: (i32::MIN, i32::MIN),
                was_occluded: false,
                just_unoccluded: false,
            })
        }
    }

    /// 把一套序列帧上传成纹理数组。同一状态重复上传会替换旧的。
    pub fn upload_state(&mut self, key: &'static str, frames: &FrameSet) -> Result<()> {
        let srv = create_texture_array(&self.device, frames)?;
        self.srvs
            .insert(key, (srv, frames.len() as u32, frames.width, frames.height));
        Ok(())
    }

    /// 切换当前显示的状态。该状态尚未加载时返回 false，画面保持原样。
    pub fn set_active(&mut self, key: &'static str) -> bool {
        if self.srvs.contains_key(&key) {
            self.active = Some(key);
            true
        } else {
            false
        }
    }

    /// 丢弃所有已上传的状态（切皮肤/改纹理尺寸时调用）。
    pub fn clear_states(&mut self) {
        self.srvs.clear();
        self.active = None;
    }

    /// 改变交换链尺寸。必须先释放所有后台缓冲引用，否则 ResizeBuffers 会失败。
    pub fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        if w == self.width && h == self.height {
            return Ok(());
        }
        self.rtv_cache.clear();
        unsafe {
            if let Some(sc) = self.swap_chain.as_ref() {
                sc.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            }
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.context.RSSetViewports(Some(&[vp]));
        }
        self.width = w;
        self.height = h;
        Ok(())
    }

    /// 移动合成图层。只在变化时 Commit，避免无意义的合成提交。
    pub fn set_offset(&mut self, x: i32, y: i32) -> Result<()> {
        if self.cur_offset == (x, y) {
            return Ok(());
        }
        self.cur_offset = (x, y);
        let (Some(visual), Some(dcomp)) = (self.visual.as_ref(), self.dcomp.as_ref()) else {
            return Ok(());
        };
        unsafe {
            visual.SetOffsetX2(x as f32)?;
            visual.SetOffsetY2(y as f32)?;
            dcomp.Commit()?;
        }
        Ok(())
    }

    fn back_buffer_rtv(&mut self) -> Result<ID3D11RenderTargetView> {
        unsafe {
            let Some(sc) = self.swap_chain.as_ref() else {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "交换链已释放",
                ));
            };
            let tex: ID3D11Texture2D = sc.GetBuffer(0)?;
            let key = tex.as_raw() as usize;
            if let Some((_, rtv)) = self.rtv_cache.iter().find(|(k, _)| *k == key) {
                return Ok(rtv.clone());
            }
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
            let rtv = rtv.unwrap();
            self.rtv_cache.push((key, rtv.clone()));
            Ok(rtv)
        }
    }

    /// 当前显示状态的纹理尺寸（像素）
    pub fn active_size(&self) -> (u32, u32) {
        self.active
            .and_then(|k| self.srvs.get(&k))
            .map(|(_, _, w, h)| (*w, *h))
            .unwrap_or((0, 0))
    }

    /// 上传绳子纹理（单帧）。
    pub fn upload_rope(&mut self, rope: &FrameSet) -> Result<()> {
        let srv = create_texture_array(&self.device, rope)?;
        self.rope = Some((srv, rope.width, rope.height));
        Ok(())
    }

    /// 开始一帧：清屏 + 设置所有与具体纹理无关的公共状态。
    pub fn begin_frame(&mut self) -> Result<()> {
        let rtv = self.back_buffer_rtv()?;
        unsafe {
            self.context
                .ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 0.0]);
            self.context.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            self.context.RSSetState(&self.raster);
            self.context
                .OMSetBlendState(&self.blend, Some(&[1.0, 1.0, 1.0, 1.0]), 0xffffffff);
            self.context.IASetInputLayout(&self.layout);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            self.context.VSSetShader(&self.vs, None);
            self.context.PSSetShader(&self.ps, None);
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.cbuffer.clone())]));
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.cbuffer.clone())]));
        }
        Ok(())
    }

    /// 画一个带纹理的四边形。`half` 是半宽半高（像素），`angle` 绕自身中心旋转。
    fn draw_quad(
        &mut self,
        srv: &ID3D11ShaderResourceView,
        center: (f32, f32),
        half: (f32, f32),
        angle: f32,
        slice: u32,
    ) -> Result<()> {
        unsafe {
            self.context
                .PSSetShaderResources(0, Some(&[Some(srv.clone())]));

            let (s, c) = (angle.sin(), angle.cos());
            let (hw, hh) = half;
            let corners = [(-hw, -hh), (hw, -hh), (-hw, hh), (hw, hh)];
            let uvs = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
            let mut verts = [Vertex::default(); 4];

            for i in 0..4 {
                let (lx, ly) = corners[i];
                let px = center.0 + lx * c - ly * s;
                let py = center.1 + lx * s + ly * c;
                verts[i] = Vertex {
                    pos: [
                        px / self.width as f32 * 2.0 - 1.0,
                        1.0 - py / self.height as f32 * 2.0,
                    ],
                    uv: [uvs[i].0, uvs[i].1],
                };
            }

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.vbuf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(
                verts.as_ptr() as *const u8,
                mapped.pData as *mut u8,
                size_of::<Vertex>() * 4,
            );
            self.context.Unmap(&self.vbuf, 0);

            let cb_data = [slice, 0, 0, 0];
            self.context
                .Map(&self.cbuffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(cb_data.as_ptr() as *const u8, mapped.pData as *mut u8, 16);
            self.context.Unmap(&self.cbuffer, 0);

            let stride = size_of::<Vertex>() as u32;
            let offset = 0u32;
            self.context.IASetVertexBuffers(
                0,
                1,
                Some(&Some(self.vbuf.clone())),
                Some(&stride),
                Some(&offset),
            );
            self.context.Draw(4, 0);
        }
        Ok(())
    }

    /// 画绳子：沿质点链生成三角带，UV.y 按累计弧长分配，所以纹理随弯曲走。
    pub fn draw_rope(&mut self, pts: &[[f32; 2]], base_w: f32) -> Result<()> {
        let Some((srv, _, _)) = self.rope.clone() else {
            return Ok(());
        };
        let n = pts.len();
        if n < 2 || n * 2 > 64 {
            return Ok(());
        }

        // 累计弧长 → UV.y
        let mut acc = [0f32; 64];
        for i in 1..n {
            let dx = pts[i][0] - pts[i - 1][0];
            let dy = pts[i][1] - pts[i - 1][1];
            acc[i] = acc[i - 1] + (dx * dx + dy * dy).sqrt();
        }
        let total = acc[n - 1].max(1.0);

        let (w2, h2) = (self.width as f32 * 0.5, self.height as f32 * 0.5);
        let mut verts = [Vertex::default(); 64];

        for i in 0..n {
            // 切线：端点用单侧，中间用中心差分
            let (tx, ty) = if i == 0 {
                (pts[1][0] - pts[0][0], pts[1][1] - pts[0][1])
            } else if i == n - 1 {
                (pts[n - 1][0] - pts[n - 2][0], pts[n - 1][1] - pts[n - 2][1])
            } else {
                (pts[i + 1][0] - pts[i - 1][0], pts[i + 1][1] - pts[i - 1][1])
            };
            let l = (tx * tx + ty * ty).sqrt().max(1e-4);
            let (nx, ny) = (-ty / l, tx / l);

            // 两端略收，看起来像打了结
            let t = i as f32 / (n - 1) as f32;
            let taper = 0.85 + 0.15 * (t * std::f32::consts::PI).sin();
            let hw = base_w * 0.5 * taper;
            let v = acc[i] / total;

            let (lx, ly) = (pts[i][0] + nx * hw, pts[i][1] + ny * hw);
            let (rx, ry) = (pts[i][0] - nx * hw, pts[i][1] - ny * hw);
            verts[i * 2] = Vertex {
                pos: [lx / w2 - 1.0, 1.0 - ly / h2],
                uv: [0.0, v],
            };
            verts[i * 2 + 1] = Vertex {
                pos: [rx / w2 - 1.0, 1.0 - ry / h2],
                uv: [1.0, v],
            };
        }

        unsafe {
            self.context
                .PSSetShaderResources(0, Some(&[Some(srv)]));

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.vbuf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(
                verts.as_ptr() as *const u8,
                mapped.pData as *mut u8,
                size_of::<Vertex>() * n * 2,
            );
            self.context.Unmap(&self.vbuf, 0);

            let cb_data = [0u32, 0, 0, 0];
            self.context
                .Map(&self.cbuffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(cb_data.as_ptr() as *const u8, mapped.pData as *mut u8, 16);
            self.context.Unmap(&self.cbuffer, 0);

            let stride = size_of::<Vertex>() as u32;
            let offset = 0u32;
            self.context.IASetVertexBuffers(
                0,
                1,
                Some(&Some(self.vbuf.clone())),
                Some(&stride),
                Some(&offset),
            );
            self.context.Draw((n * 2) as u32, 0);
        }
        Ok(())
    }

    /// 画当前光标状态的精灵。
    pub fn draw_sprite(&mut self, center: (f32, f32), angle: f32, slice: u32) -> Result<()> {
        let Some((srv, count, sw, sh)) = self.active.and_then(|k| self.srvs.get(&k).cloned()) else {
            return Ok(());
        };
        let slice = if count == 0 { 0 } else { slice % count };
        self.draw_quad(&srv, center, (sw as f32 * 0.5, sh as f32 * 0.5), angle, slice)
    }

    /// 结束一帧：提交到合成器。
    pub fn end_frame(&mut self) -> Result<()> {
        let Some(sc) = self.swap_chain.as_ref() else {
            return Ok(());
        };
        unsafe {
            let hr = sc.Present(1, DXGI_PRESENT(0));
            // DXGI_STATUS_OCCLUDED = 0x087A0001，是成功码不是错误
            let occluded = hr.0 == 0x087A0001;
            if self.was_occluded && !occluded {
                // 遮挡结束的那一帧：DWM 通常已经把视觉树丢了
                self.just_unoccluded = true;
            }
            self.was_occluded = occluded;
            hr.ok()?;
        }
        Ok(())
    }

    /// 当前是否处于遮挡状态
    pub fn is_occluded(&self) -> bool {
        self.was_occluded
    }

    /// 取走「刚从遮挡恢复」事件
    pub fn take_unoccluded(&mut self) -> bool {
        std::mem::take(&mut self.just_unoccluded)
    }

    /// 重建 DirectComposition 链路（交换链 + 设备 + 目标 + 视觉树）。
    ///
    /// 锁屏 / 显示器休眠唤醒 / 分辨率切换之后，DWM 可能已经丢掉了这个窗口的视觉树，
    /// 表现为窗口还在、进程还活着、但画面上什么都没有。设备本身没坏，所以只重建合成部分，
    /// 着色器和纹理都留着。
    pub fn reattach(&mut self, hwnd: HWND) -> Result<()> {
        // 先彻底释放旧链路，避免在同一个 HWND 上同时存在两个 target
        self.visual = None;
        self._target = None;
        self.dcomp = None;
        self.swap_chain = None;
        self.rtv_cache.clear();

        unsafe {
            let dxgi_device: IDXGIDevice = self.device.cast()?;
            let adapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: self.width,
                Height: self.height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL::from(false),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let sc = factory.CreateSwapChainForComposition(&self.device, &desc, None)?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice3(&dxgi_device)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&sc)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            self.swap_chain = Some(sc);
            self.dcomp = Some(dcomp);
            self._target = Some(target);
            self.visual = Some(visual);
        }

        // 强制下一次 set_offset 重新提交
        self.cur_offset = (i32::MIN, i32::MIN);
        Ok(())
    }

    /// 设备是否已经丢失（被驱动重置/移除）
    pub fn device_lost(&self) -> bool {
        unsafe { self.device.GetDeviceRemovedReason().is_err() }
    }

}

fn compile(src: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let entry = std::ffi::CString::new(entry).unwrap();
    let target = std::ffi::CString::new(target).unwrap();
    let mut blob: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;

    unsafe {
        D3DCompile(
            src.as_ptr() as *const c_void,
            src.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry.as_ptr() as *const u8),
            PCSTR(target.as_ptr() as *const u8),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut err),
        )?;
    }
    Ok(blob.unwrap())
}

fn blob_slice(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}

fn create_texture_array(device: &ID3D11Device, frames: &FrameSet) -> Result<ID3D11ShaderResourceView> {
    let n = frames.len() as u32;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: frames.width,
        Height: frames.height,
        MipLevels: 1,
        ArraySize: n,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };

    let pitch = frames.width * 4;
    let subresources: Vec<D3D11_SUBRESOURCE_DATA> = frames
        .frames
        .iter()
        .map(|f| D3D11_SUBRESOURCE_DATA {
            pSysMem: f.as_ptr() as *const c_void,
            SysMemPitch: pitch,
            SysMemSlicePitch: 0,
        })
        .collect();

    unsafe {
        let mut tex: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&desc, Some(subresources.as_ptr()), Some(&mut tex))?;

        let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    FirstArraySlice: 0,
                    ArraySize: n,
                },
            },
        };
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        device.CreateShaderResourceView(&tex.unwrap(), Some(&srv_desc), Some(&mut srv))?;
        Ok(srv.unwrap())
    }
}
