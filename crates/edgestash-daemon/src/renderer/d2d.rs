use std::mem::size_of;

use edgestash_ipc::types::{ChurnZoneId, DockMode, EdgePillStyle, ScreenEdge};
use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2F, D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_ROUNDED_RECT,
    D2D1CreateDevice, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, FindWindowExW, GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, GetClassNameW,
    GetIconInfo, GetWindowThreadProcessId, HICON, ICON_BIG, ICON_SMALL, ICON_SMALL2, ICONINFO,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETICON,
};
use windows::core::{Interface, PCWSTR, Result, w};

use crate::geometry::{HeaderButtonKind, header_button_rects};

/// Tests whether a Direct2D / DXGI error represents GPU device removal or target recreation necessity.
pub fn is_device_lost_error(err: &windows::core::Error) -> bool {
    let code = err.code().0;
    code == 0x8899000C_u32 as i32 // D2DERR_RECREATE_TARGET
        || code == 0x887A0005_u32 as i32 // DXGI_ERROR_DEVICE_REMOVED
        || code == 0x887A0007_u32 as i32 // DXGI_ERROR_DEVICE_RESET
        || code == 0x887A0006_u32 as i32 // DXGI_ERROR_DEVICE_HUNG
}

/// Premultiplied BGRA bitmap extracted from a Win32 `HICON`.
#[derive(Debug, Clone)]
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Frame parameters for the pill's closed-window notification animation.
#[derive(Debug, Clone)]
pub struct CloseAnimFrame<'a> {
    pub progress: f32,
    pub icon: Option<&'a IconPixels>,
}

/// Per-slot visual data rendered inside an `AppIconsStack` edge pill.
#[derive(Debug, Clone)]
pub struct PillSlotVisual<'a> {
    pub icon: Option<&'a IconPixels>,
    pub title: &'a str,
    pub needs_attention: bool,
}

/// Visual state rendered inside the `28px` docked drawer header strip.
#[derive(Debug, Clone)]
pub struct DrawerHeaderVisual<'a> {
    pub title: &'a str,
    pub slot_index: usize,
    pub slot_count: usize,
    pub dock_mode: DockMode,
    pub hovered_button: Option<HeaderButtonKind>,
}

/// Shared Direct3D11 + DXGI + Direct2D + DirectComposition + DirectWrite device pipeline.
#[derive(Clone)]
pub struct D2DGraphicsContext {
    pub d3d_device: ID3D11Device,
    pub dxgi_factory: IDXGIFactory2,
    pub d2d_device: ID2D1Device,
    pub d2d_context: ID2D1DeviceContext,
    pub dcomp_device: IDCompositionDevice,
    pub dwrite_factory: Option<IDWriteFactory>,
}

impl D2DGraphicsContext {
    /// Initializes hardware Direct2D + DirectComposition (with automatic WARP software fallback).
    pub fn new() -> Result<Self> {
        let mut d3d_device: Option<ID3D11Device> = None;
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

        let hw_res = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                None,
            )
        };
        if hw_res.is_err() || d3d_device.is_none() {
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    None,
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut d3d_device),
                    None,
                    None,
                )?;
            }
        }

        let d3d_device = d3d_device.expect("D3D11 device created");
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let dxgi_factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        let d2d_device = unsafe { D2D1CreateDevice(&dxgi_device, None)? };
        let d2d_context =
            unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
        let dcomp_device: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi_device)? };
        let dwrite_factory: Option<IDWriteFactory> =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok() };

        Ok(Self {
            d3d_device,
            dxgi_factory,
            d2d_device,
            d2d_context,
            dcomp_device,
            dwrite_factory,
        })
    }

    /// Binds a composition swap chain + DirectComposition visual target to an overlay `HWND`.
    pub fn create_window_surface(
        &self,
        hwnd: HWND,
        width: u32,
        height: u32,
    ) -> Result<D2DWindowSurface> {
        let w = width.max(1);
        let h = height.max(1);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
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

        let swap_chain = unsafe {
            self.dxgi_factory
                .CreateSwapChainForComposition(&self.d3d_device, &desc, None)?
        };
        let target = unsafe { self.dcomp_device.CreateTargetForHwnd(hwnd, true)? };
        let visual = unsafe { self.dcomp_device.CreateVisual()? };
        unsafe {
            visual.SetContent(&swap_chain)?;
            target.SetRoot(&visual)?;
            self.dcomp_device.Commit()?;
        }

        Ok(D2DWindowSurface {
            d2d_context: self.d2d_context.clone(),
            dcomp_device: self.dcomp_device.clone(),
            dwrite_factory: self.dwrite_factory.clone(),
            _target: target,
            _visual: visual,
            swap_chain,
            width: w,
            height: h,
        })
    }
}

/// Per-window DirectComposition + Direct2D swapchain surface.
pub struct D2DWindowSurface {
    d2d_context: ID2D1DeviceContext,
    dcomp_device: IDCompositionDevice,
    dwrite_factory: Option<IDWriteFactory>,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    swap_chain: IDXGISwapChain1,
    width: u32,
    height: u32,
}

impl D2DWindowSurface {
    fn ensure_size(&mut self, width: u32, height: u32) -> Result<()> {
        let w = width.max(1);
        let h = height.max(1);
        if self.width != w || self.height != h {
            unsafe {
                self.d2d_context.SetTarget(None);
                self.swap_chain.ResizeBuffers(
                    2,
                    w,
                    h,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG(0),
                )?;
            }
            self.width = w;
            self.height = h;
        }
        Ok(())
    }

    fn begin_target(&mut self, width: u32, height: u32) -> Result<ID2D1Bitmap1> {
        self.ensure_size(width, height)?;
        let surface: IDXGISurface = unsafe { self.swap_chain.GetBuffer(0)? };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let target_bitmap = unsafe {
            self.d2d_context
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))?
        };
        unsafe {
            self.d2d_context.SetTarget(&target_bitmap);
            self.d2d_context.BeginDraw();
            self.d2d_context.Clear(Some(&D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }));
        }
        Ok(target_bitmap)
    }

    fn end_and_commit(&self) -> Result<()> {
        unsafe {
            self.d2d_context.EndDraw(None, None)?;
            self.d2d_context.SetTarget(None);
            self.swap_chain.Present(0, DXGI_PRESENT(0)).ok()?;
            self.dcomp_device.Commit()?;
        }
        Ok(())
    }

    fn create_brush(&self, r: f32, g: f32, b: f32, a: f32) -> Result<ID2D1SolidColorBrush> {
        unsafe {
            self.d2d_context
                .CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
        }
    }

    fn create_bitmap_from_icon(&self, icon: &IconPixels) -> Option<ID2D1Bitmap1> {
        let bmp_props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        unsafe {
            self.d2d_context
                .CreateBitmap(
                    windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                        width: icon.width,
                        height: icon.height,
                    },
                    Some(icon.bgra.as_ptr() as *const _),
                    icon.width * 4,
                    &bmp_props,
                )
                .ok()
        }
    }

    fn create_text_format(
        &self,
        font_size: f32,
        bold: bool,
        centered: bool,
    ) -> Option<IDWriteTextFormat> {
        let factory = self.dwrite_factory.as_ref()?;
        let weight = if bold {
            DWRITE_FONT_WEIGHT_BOLD
        } else {
            DWRITE_FONT_WEIGHT_SEMI_BOLD
        };
        let fmt = unsafe {
            factory
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    font_size,
                    w!(""),
                )
                .ok()?
        };
        unsafe {
            let _ = fmt.SetTextAlignment(if centered {
                DWRITE_TEXT_ALIGNMENT_CENTER
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            });
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            let sign = factory.CreateEllipsisTrimmingSign(&fmt).ok();
            let _ = fmt.SetTrimming(&trimming, sign.as_ref());
        }
        Some(fmt)
    }

    fn draw_text_utf8(
        &self,
        text: &str,
        rect: D2D_RECT_F,
        fmt: &IDWriteTextFormat,
        brush: &ID2D1SolidColorBrush,
    ) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.d2d_context.DrawText(
                &wide,
                fmt,
                &rect,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// Renders the translucent edge indicator pill (`AppIconsStack` icons, `MinimalLine` dots, or close anim).
    #[allow(clippy::too_many_arguments)]
    pub fn render_pill(
        &mut self,
        width: u32,
        height: u32,
        style: EdgePillStyle,
        slots: &[PillSlotVisual<'_>],
        slot_count: usize,
        active_index: usize,
        close_anim: Option<CloseAnimFrame<'_>>,
        attention_count: u32,
        now_ms: u64,
    ) -> Result<()> {
        let _target = self.begin_target(width, height)?;
        let wf = width.max(1) as f32;
        let hf = height.max(1) as f32;
        let radius = (wf.min(hf) / 2.0).min(10.0);

        let bg_brush = self.create_brush(0.08, 0.10, 0.14, 0.88)?;
        let border_brush = self.create_brush(1.0, 1.0, 1.0, 0.24)?;
        let accent_brush = self.create_brush(0.22, 0.74, 0.97, 0.96)?;
        let active_bg_brush = self.create_brush(0.22, 0.74, 0.97, 0.26)?;
        let inactive_bg_brush = self.create_brush(1.0, 1.0, 1.0, 0.08)?;
        let muted_brush = self.create_brush(0.75, 0.80, 0.88, 0.45)?;
        let text_brush = self.create_brush(0.95, 0.97, 1.0, 0.95)?;

        let pulse = if attention_count > 0 || slots.iter().any(|s| s.needs_attention) {
            ((now_ms as f32 / 240.0).sin() * 0.5 + 0.5).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let amber_halo_brush = self.create_brush(0.98, 0.65, 0.15, 0.35 + 0.55 * pulse)?;
        let amber_dot_brush = self.create_brush(1.0, 0.75, 0.18, 0.95)?;
        let amber_bg_brush = self.create_brush(0.98, 0.65, 0.15, 0.16 + 0.20 * pulse)?;

        let outer = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 1.0,
                top: 1.0,
                right: (wf - 1.0).max(2.0),
                bottom: (hf - 1.0).max(2.0),
            },
            radiusX: radius,
            radiusY: radius,
        };

        unsafe {
            self.d2d_context.FillRoundedRectangle(&outer, &bg_brush);
            if attention_count > 0 && slots.len() <= 1 {
                self.d2d_context
                    .FillRoundedRectangle(&outer, &amber_bg_brush);
                self.d2d_context
                    .DrawRoundedRectangle(&outer, &amber_halo_brush, 2.0, None);
            } else {
                self.d2d_context
                    .DrawRoundedRectangle(&outer, &border_brush, 1.0, None);
            }
        }

        if let Some(anim) = close_anim {
            let fade = (1.0 - anim.progress).clamp(0.0, 1.0);
            let close_brush = self.create_brush(0.97, 0.35, 0.35, 0.92 * fade)?;
            let cx = wf / 2.0;
            let cy = hf / 2.0;
            let size = (wf.min(hf) * 0.55) * (1.0 - 0.35 * anim.progress);

            if let Some(icon) = anim.icon
                && let Some(bmp) = self.create_bitmap_from_icon(icon)
            {
                let half = size / 2.0;
                let dest = D2D_RECT_F {
                    left: cx - half,
                    top: cy - half,
                    right: cx + half,
                    bottom: cy + half,
                };
                unsafe {
                    self.d2d_context.DrawBitmap(
                        &bmp,
                        Some(&dest),
                        fade,
                        windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR,
                        None,
                        None,
                    );
                }
            }

            let arm = (size * 0.32).max(3.0);
            unsafe {
                self.d2d_context.DrawLine(
                    D2D_POINT_2F {
                        x: cx - arm,
                        y: cy - arm,
                    },
                    D2D_POINT_2F {
                        x: cx + arm,
                        y: cy + arm,
                    },
                    &close_brush,
                    2.0,
                    None,
                );
                self.d2d_context.DrawLine(
                    D2D_POINT_2F {
                        x: cx + arm,
                        y: cy - arm,
                    },
                    D2D_POINT_2F {
                        x: cx - arm,
                        y: cy + arm,
                    },
                    &close_brush,
                    2.0,
                    None,
                );
            }
        } else if attention_count > 0 && slots.len() <= 1 {
            let cx = wf / 2.0;
            let cy = hf / 2.0;
            let icon_size = (wf.min(hf) - 8.0).clamp(12.0, 24.0);
            let half = icon_size * 0.5;
            let dest = D2D_RECT_F {
                left: cx - half,
                top: cy - half,
                right: cx + half,
                bottom: cy + half,
            };

            let first_slot = slots.first();
            if let Some(icon) = first_slot.and_then(|s| s.icon)
                && let Some(bmp) = self.create_bitmap_from_icon(icon)
            {
                unsafe {
                    self.d2d_context.DrawBitmap(
                        &bmp,
                        Some(&dest),
                        1.0,
                        windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR,
                        None,
                        None,
                    );
                }
            } else if let Some(fmt) = self.create_text_format(12.0, true, true) {
                let initial = first_slot
                    .and_then(|s| s.title.chars().find(|c| c.is_alphanumeric()))
                    .map(|c| c.to_ascii_uppercase().to_string())
                    .unwrap_or_else(|| "!".to_string());
                self.draw_text_utf8(&initial, dest, &fmt, &amber_dot_brush);
            }

            if attention_count > 1 {
                let badge_r = 5.5;
                let badge_cx = (wf - badge_r - 2.0).max(badge_r);
                let badge_cy = badge_r + 2.0;
                let badge_ellipse = D2D1_ELLIPSE {
                    point: D2D_POINT_2F {
                        x: badge_cx,
                        y: badge_cy,
                    },
                    radiusX: badge_r,
                    radiusY: badge_r,
                };
                unsafe {
                    self.d2d_context
                        .FillEllipse(&badge_ellipse, &amber_dot_brush);
                }
                if let Some(badge_fmt) = self.create_text_format(8.5, true, true) {
                    let badge_rect = D2D_RECT_F {
                        left: badge_cx - badge_r,
                        top: badge_cy - badge_r,
                        right: badge_cx + badge_r,
                        bottom: badge_cy + badge_r,
                    };
                    let count_str = attention_count.min(9).to_string();
                    let badge_text_brush = self.create_brush(0.08, 0.10, 0.14, 1.0)?;
                    self.draw_text_utf8(&count_str, badge_rect, &badge_fmt, &badge_text_brush);
                }
            }
        } else if slot_count > 0 && style == EdgePillStyle::AppIconsStack {
            let count = slot_count.min(8);
            let vertical = hf >= wf;
            let pad = 4.0;
            let primary = if vertical { hf } else { wf };
            let span_per = ((primary - 2.0 * pad) / count as f32).max(12.0);
            let monogram_fmt = self.create_text_format(11.0, true, true);

            for i in 0..count {
                let is_active = i == active_index;
                let slot_vis = slots.get(i);
                let needs_att = slot_vis.is_some_and(|s| s.needs_attention);

                let cell = if vertical {
                    let y0 = pad + (i as f32) * span_per + 1.0;
                    let y1 = (pad + ((i + 1) as f32) * span_per - 1.0).max(y0 + 10.0);
                    D2D_RECT_F {
                        left: 3.0,
                        top: y0,
                        right: (wf - 3.0).max(13.0),
                        bottom: y1,
                    }
                } else {
                    let x0 = pad + (i as f32) * span_per + 1.0;
                    let x1 = (pad + ((i + 1) as f32) * span_per - 1.0).max(x0 + 10.0);
                    D2D_RECT_F {
                        left: x0,
                        top: 3.0,
                        right: x1,
                        bottom: (hf - 3.0).max(13.0),
                    }
                };

                let cell_rr = D2D1_ROUNDED_RECT {
                    rect: cell,
                    radiusX: 5.0,
                    radiusY: 5.0,
                };
                unsafe {
                    if is_active {
                        self.d2d_context
                            .FillRoundedRectangle(&cell_rr, &active_bg_brush);
                        self.d2d_context
                            .DrawRoundedRectangle(&cell_rr, &accent_brush, 1.3, None);
                    } else if needs_att {
                        self.d2d_context
                            .FillRoundedRectangle(&cell_rr, &amber_bg_brush);
                        self.d2d_context.DrawRoundedRectangle(
                            &cell_rr,
                            &amber_halo_brush,
                            1.8,
                            None,
                        );
                    } else {
                        self.d2d_context
                            .FillRoundedRectangle(&cell_rr, &inactive_bg_brush);
                    }
                    if needs_att {
                        let dot_cx = cell.right - 4.5;
                        let dot_cy = cell.top + 4.5;
                        let dot = D2D1_ELLIPSE {
                            point: D2D_POINT_2F {
                                x: dot_cx,
                                y: dot_cy,
                            },
                            radiusX: 3.5,
                            radiusY: 3.5,
                        };
                        self.d2d_context.FillEllipse(&dot, &amber_dot_brush);
                    }
                }

                let cx = (cell.left + cell.right) * 0.5;
                let cy = (cell.top + cell.bottom) * 0.5;
                let icon_size =
                    ((cell.right - cell.left).min(cell.bottom - cell.top) - 4.0).clamp(10.0, 18.0);
                let half = icon_size * 0.5;
                let dest = D2D_RECT_F {
                    left: cx - half,
                    top: cy - half,
                    right: cx + half,
                    bottom: cy + half,
                };

                if let Some(icon) = slot_vis.and_then(|s| s.icon)
                    && let Some(bmp) = self.create_bitmap_from_icon(icon)
                {
                    let opacity = if is_active { 1.0 } else { 0.85 };
                    unsafe {
                        self.d2d_context.DrawBitmap(
                            &bmp,
                            Some(&dest),
                            opacity,
                            windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR,
                            None,
                            None,
                        );
                    }
                } else if let Some(fmt) = &monogram_fmt {
                    let initial = slot_vis
                        .and_then(|s| s.title.chars().find(|c| c.is_alphanumeric()))
                        .map(|c| c.to_ascii_uppercase().to_string())
                        .unwrap_or_else(|| (i + 1).to_string());
                    self.draw_text_utf8(&initial, cell, fmt, &text_brush);
                } else {
                    let dot = D2D1_ELLIPSE {
                        point: D2D_POINT_2F { x: cx, y: cy },
                        radiusX: 3.5,
                        radiusY: 3.5,
                    };
                    let b = if is_active {
                        &accent_brush
                    } else if needs_att {
                        &amber_dot_brush
                    } else {
                        &muted_brush
                    };
                    unsafe {
                        self.d2d_context.FillEllipse(&dot, b);
                    }
                }
            }
        } else if slot_count > 0 {
            let count = slot_count.min(6);
            let vertical = hf >= wf;
            let step = if vertical {
                hf / (count as f32 + 1.0)
            } else {
                wf / (count as f32 + 1.0)
            };
            for i in 0..count {
                let slot_vis = slots.get(i);
                let needs_att = slot_vis.is_some_and(|s| s.needs_attention);
                let brush = if i == active_index {
                    &accent_brush
                } else if needs_att {
                    &amber_dot_brush
                } else {
                    &muted_brush
                };
                let (cx, cy) = if vertical {
                    (wf / 2.0, step * (i as f32 + 1.0))
                } else {
                    (step * (i as f32 + 1.0), hf / 2.0)
                };
                let dot_r = if i == active_index || needs_att {
                    3.5
                } else {
                    2.2
                };
                let ellipse = D2D1_ELLIPSE {
                    point: D2D_POINT_2F { x: cx, y: cy },
                    radiusX: dot_r,
                    radiusY: dot_r,
                };
                unsafe {
                    self.d2d_context.FillEllipse(&ellipse, brush);
                }
            }
        }

        self.end_and_commit()
    }

    /// Renders the translucent `28px` docked drawer action strip (`[title ... Close | Unstash | Pin]`).
    pub fn render_drawer_header(
        &mut self,
        width: u32,
        height: u32,
        visual: &DrawerHeaderVisual<'_>,
    ) -> Result<()> {
        let _target = self.begin_target(width, height)?;
        let wf = width.max(1) as f32;
        let hf = height.max(1) as f32;

        let bg_brush = self.create_brush(0.08, 0.10, 0.15, 0.94)?;
        let bottom_border = self.create_brush(1.0, 1.0, 1.0, 0.16)?;
        let title_brush = self.create_brush(0.92, 0.95, 0.99, 0.95)?;
        let count_brush = self.create_brush(0.65, 0.72, 0.84, 0.80)?;
        let glyph_brush = self.create_brush(0.88, 0.92, 0.98, 0.94)?;
        let hover_neutral = self.create_brush(1.0, 1.0, 1.0, 0.14)?;
        let hover_close = self.create_brush(0.94, 0.27, 0.27, 0.42)?;
        let pin_float_bg = self.create_brush(0.22, 0.74, 0.97, 0.28)?;
        let pin_float_stroke = self.create_brush(0.22, 0.74, 0.97, 0.95)?;
        let pin_dock_bg = self.create_brush(0.20, 0.83, 0.60, 0.28)?;
        let pin_dock_stroke = self.create_brush(0.20, 0.83, 0.60, 0.95)?;

        let bar_rect = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: wf,
            bottom: hf,
        };
        unsafe {
            self.d2d_context.FillRectangle(&bar_rect, &bg_brush);
            self.d2d_context.DrawLine(
                D2D_POINT_2F {
                    x: 0.0,
                    y: hf - 0.5,
                },
                D2D_POINT_2F { x: wf, y: hf - 0.5 },
                &bottom_border,
                1.0,
                None,
            );
        }

        let buttons = header_button_rects(width as i32, height as i32);
        let btns_left = buttons[0].1.left as f32;

        if let Some(title_fmt) = self.create_text_format(12.0, false, false) {
            let label = if visual.slot_count > 1 {
                format!(
                    "[ {}/{} ]  {}",
                    visual.slot_index + 1,
                    visual.slot_count,
                    visual.title
                )
            } else {
                visual.title.to_string()
            };
            let title_rect = D2D_RECT_F {
                left: 10.0,
                top: 0.0,
                right: (btns_left - 8.0).max(20.0),
                bottom: hf,
            };
            let brush = if visual.dock_mode != DockMode::FloatingOverlay {
                &title_brush
            } else {
                &count_brush
            };
            let _ = brush;
            self.draw_text_utf8(&label, title_rect, &title_fmt, &title_brush);
        }

        for (kind, r) in buttons {
            let hovered = visual.hovered_button == Some(kind);
            let rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: r.left as f32,
                    top: r.top as f32,
                    right: r.right as f32,
                    bottom: r.bottom as f32,
                },
                radiusX: 5.0,
                radiusY: 5.0,
            };

            match kind {
                HeaderButtonKind::Close => {
                    if hovered {
                        unsafe {
                            self.d2d_context.FillRoundedRectangle(&rr, &hover_close);
                        }
                    }
                    let cx = (rr.rect.left + rr.rect.right) * 0.5;
                    let cy = (rr.rect.top + rr.rect.bottom) * 0.5;
                    let d = 4.5;
                    unsafe {
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx - d,
                                y: cy - d,
                            },
                            D2D_POINT_2F {
                                x: cx + d,
                                y: cy + d,
                            },
                            &glyph_brush,
                            1.6,
                            None,
                        );
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx + d,
                                y: cy - d,
                            },
                            D2D_POINT_2F {
                                x: cx - d,
                                y: cy + d,
                            },
                            &glyph_brush,
                            1.6,
                            None,
                        );
                    }
                }
                HeaderButtonKind::Unstash => {
                    if hovered {
                        unsafe {
                            self.d2d_context.FillRoundedRectangle(&rr, &hover_neutral);
                        }
                    }
                    let cx = (rr.rect.left + rr.rect.right) * 0.5;
                    let cy = (rr.rect.top + rr.rect.bottom) * 0.5;
                    // Box with outward arrow glyph
                    let box_r = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: cx - 5.0,
                            top: cy - 3.5,
                            right: cx + 5.0,
                            bottom: cy + 5.0,
                        },
                        radiusX: 1.5,
                        radiusY: 1.5,
                    };
                    unsafe {
                        self.d2d_context
                            .DrawRoundedRectangle(&box_r, &glyph_brush, 1.3, None);
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx - 1.0,
                                y: cy + 1.0,
                            },
                            D2D_POINT_2F {
                                x: cx + 5.5,
                                y: cy - 5.5,
                            },
                            &glyph_brush,
                            1.5,
                            None,
                        );
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx + 1.5,
                                y: cy - 5.5,
                            },
                            D2D_POINT_2F {
                                x: cx + 5.5,
                                y: cy - 5.5,
                            },
                            &glyph_brush,
                            1.5,
                            None,
                        );
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx + 5.5,
                                y: cy - 5.5,
                            },
                            D2D_POINT_2F {
                                x: cx + 5.5,
                                y: cy - 1.5,
                            },
                            &glyph_brush,
                            1.5,
                            None,
                        );
                    }
                }
                HeaderButtonKind::Pin => {
                    let stroke = match visual.dock_mode {
                        DockMode::FloatingPinned => {
                            unsafe {
                                self.d2d_context.FillRoundedRectangle(&rr, &pin_float_bg);
                                self.d2d_context.DrawRoundedRectangle(
                                    &rr,
                                    &pin_float_stroke,
                                    1.1,
                                    None,
                                );
                            }
                            &pin_float_stroke
                        }
                        DockMode::NativeWorkAreaDock => {
                            unsafe {
                                self.d2d_context.FillRoundedRectangle(&rr, &pin_dock_bg);
                                self.d2d_context.DrawRoundedRectangle(
                                    &rr,
                                    &pin_dock_stroke,
                                    1.1,
                                    None,
                                );
                            }
                            &pin_dock_stroke
                        }
                        DockMode::FloatingOverlay => {
                            if hovered {
                                unsafe {
                                    self.d2d_context.FillRoundedRectangle(&rr, &hover_neutral);
                                }
                            }
                            &glyph_brush
                        }
                    };
                    let cx = (rr.rect.left + rr.rect.right) * 0.5;
                    let cy = (rr.rect.top + rr.rect.bottom) * 0.5;
                    unsafe {
                        // Thumbtack head + collar + needle
                        let head = D2D_RECT_F {
                            left: cx - 3.5,
                            top: cy - 5.5,
                            right: cx + 3.5,
                            bottom: cy - 1.5,
                        };
                        self.d2d_context.DrawRectangle(&head, stroke, 1.4, None);
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx - 5.0,
                                y: cy + 1.0,
                            },
                            D2D_POINT_2F {
                                x: cx + 5.0,
                                y: cy + 1.0,
                            },
                            stroke,
                            1.5,
                            None,
                        );
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx - 3.0,
                                y: cy - 1.5,
                            },
                            D2D_POINT_2F {
                                x: cx - 4.0,
                                y: cy + 1.0,
                            },
                            stroke,
                            1.3,
                            None,
                        );
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F {
                                x: cx + 3.0,
                                y: cy - 1.5,
                            },
                            D2D_POINT_2F {
                                x: cx + 4.0,
                                y: cy + 1.0,
                            },
                            stroke,
                            1.3,
                            None,
                        );
                        self.d2d_context.DrawLine(
                            D2D_POINT_2F { x: cx, y: cy + 1.0 },
                            D2D_POINT_2F { x: cx, y: cy + 6.0 },
                            stroke,
                            1.6,
                            None,
                        );
                        // For Direct Windows Dock, draw vertical dock bar indicator on right side
                        if visual.dock_mode == DockMode::NativeWorkAreaDock {
                            self.d2d_context.DrawLine(
                                D2D_POINT_2F {
                                    x: rr.rect.right - 3.0,
                                    y: rr.rect.top + 3.5,
                                },
                                D2D_POINT_2F {
                                    x: rr.rect.right - 3.0,
                                    y: rr.rect.bottom - 3.5,
                                },
                                stroke,
                                2.0,
                                None,
                            );
                        }
                    }
                }
            }
        }

        self.end_and_commit()
    }

    /// Renders the 3 translucent Direct2D drag drop zones (Top Corner, Center Edge, Bottom Corner) along `edge`.
    pub fn render_drag_preview(
        &mut self,
        width: u32,
        height: u32,
        edge: ScreenEdge,
        edge_zone_px: i32,
        corner_span_px: i32,
        active_target: Option<&crate::trigger::DragDropTarget>,
    ) -> Result<()> {
        let _target = self.begin_target(width, height)?;
        let wf = width.max(1) as f32;
        let hf = height.max(1) as f32;
        let c_span = (corner_span_px as f32).min(hf / 2.0 - 10.0).max(30.0);
        let e_zone = (edge_zone_px as f32).min(wf).max(20.0);

        let active_fill = self.create_brush(0.22, 0.74, 0.97, 0.38)?;
        let active_stroke = self.create_brush(0.22, 0.74, 0.97, 0.95)?;
        let inactive_fill = self.create_brush(0.22, 0.74, 0.97, 0.08)?;
        let inactive_stroke = self.create_brush(0.22, 0.74, 0.97, 0.35)?;
        let text_brush = self.create_brush(1.0, 1.0, 1.0, 0.92)?;
        let font_title = self.create_text_format(12.0, true, true);
        let font_sub = self.create_text_format(10.0, false, true);

        let (
            box_c1,
            is_c1_active,
            label_c1,
            sub_c1,
            box_center,
            is_center_active,
            label_center,
            sub_center,
            box_c2,
            is_c2_active,
            label_c2,
            sub_c2,
        ) = match edge {
            ScreenEdge::Right => {
                let is_c1 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::TopRightDown | ChurnZoneId::TopRightLeft,
                        ..
                    })
                );
                let is_center = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Edge {
                        edge: ScreenEdge::Right,
                        ..
                    })
                );
                let is_c2 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::BottomRightUp | ChurnZoneId::BottomRightLeft,
                        ..
                    })
                );
                (
                    D2D_RECT_F {
                        left: wf - c_span + 2.0,
                        top: 2.0,
                        right: wf - 2.0,
                        bottom: c_span - 2.0,
                    },
                    is_c1,
                    "Top-Right",
                    "Flick Trigger",
                    D2D_RECT_F {
                        left: wf - e_zone + 2.0,
                        top: c_span + 4.0,
                        right: wf - 2.0,
                        bottom: hf - c_span - 4.0,
                    },
                    is_center,
                    "Stash Right",
                    "Default Edge",
                    D2D_RECT_F {
                        left: wf - c_span + 2.0,
                        top: hf - c_span + 2.0,
                        right: wf - 2.0,
                        bottom: hf - 2.0,
                    },
                    is_c2,
                    "Bottom-Right",
                    "Flick Trigger",
                )
            }
            ScreenEdge::Left => {
                let is_c1 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::TopLeftDown | ChurnZoneId::TopLeftRight,
                        ..
                    })
                );
                let is_center = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Edge {
                        edge: ScreenEdge::Left,
                        ..
                    })
                );
                let is_c2 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::BottomLeftUp | ChurnZoneId::BottomLeftRight,
                        ..
                    })
                );
                (
                    D2D_RECT_F {
                        left: 2.0,
                        top: 2.0,
                        right: c_span - 2.0,
                        bottom: c_span - 2.0,
                    },
                    is_c1,
                    "Top-Left",
                    "Flick Trigger",
                    D2D_RECT_F {
                        left: 2.0,
                        top: c_span + 4.0,
                        right: e_zone - 2.0,
                        bottom: hf - c_span - 4.0,
                    },
                    is_center,
                    "Stash Left",
                    "Default Edge",
                    D2D_RECT_F {
                        left: 2.0,
                        top: hf - c_span + 2.0,
                        right: c_span - 2.0,
                        bottom: hf - 2.0,
                    },
                    is_c2,
                    "Bottom-Left",
                    "Flick Trigger",
                )
            }
            ScreenEdge::Top => {
                let is_c1 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::TopLeftDown | ChurnZoneId::TopLeftRight,
                        ..
                    })
                );
                let is_center = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Edge {
                        edge: ScreenEdge::Top,
                        ..
                    })
                );
                let is_c2 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::TopRightDown | ChurnZoneId::TopRightLeft,
                        ..
                    })
                );
                (
                    D2D_RECT_F {
                        left: 2.0,
                        top: 2.0,
                        right: c_span - 2.0,
                        bottom: c_span - 2.0,
                    },
                    is_c1,
                    "Top-Left",
                    "Flick Trigger",
                    D2D_RECT_F {
                        left: c_span + 4.0,
                        top: 2.0,
                        right: wf - c_span - 4.0,
                        bottom: e_zone - 2.0,
                    },
                    is_center,
                    "Stash Top",
                    "Default Edge",
                    D2D_RECT_F {
                        left: wf - c_span + 2.0,
                        top: 2.0,
                        right: wf - 2.0,
                        bottom: c_span - 2.0,
                    },
                    is_c2,
                    "Top-Right",
                    "Flick Trigger",
                )
            }
            ScreenEdge::Bottom => {
                let is_c1 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::BottomLeftUp | ChurnZoneId::BottomLeftRight,
                        ..
                    })
                );
                let is_center = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Edge {
                        edge: ScreenEdge::Bottom,
                        ..
                    })
                );
                let is_c2 = matches!(
                    active_target,
                    Some(crate::trigger::DragDropTarget::Corner {
                        zone: ChurnZoneId::BottomRightUp | ChurnZoneId::BottomRightLeft,
                        ..
                    })
                );
                (
                    D2D_RECT_F {
                        left: 2.0,
                        top: hf - c_span + 2.0,
                        right: c_span - 2.0,
                        bottom: hf - 2.0,
                    },
                    is_c1,
                    "Bottom-Left",
                    "Flick Trigger",
                    D2D_RECT_F {
                        left: c_span + 4.0,
                        top: hf - e_zone + 2.0,
                        right: wf - c_span - 4.0,
                        bottom: hf - 2.0,
                    },
                    is_center,
                    "Stash Bottom",
                    "Default Edge",
                    D2D_RECT_F {
                        left: wf - c_span + 2.0,
                        top: hf - c_span + 2.0,
                        right: wf - 2.0,
                        bottom: hf - 2.0,
                    },
                    is_c2,
                    "Bottom-Right",
                    "Flick Trigger",
                )
            }
        };

        let zones = [
            (box_c1, is_c1_active, label_c1, sub_c1),
            (box_center, is_center_active, label_center, sub_center),
            (box_c2, is_c2_active, label_c2, sub_c2),
        ];

        for (r, is_active, title, sub) in zones {
            let fill = if is_active {
                &active_fill
            } else {
                &inactive_fill
            };
            let stroke = if is_active {
                &active_stroke
            } else {
                &inactive_stroke
            };
            let stroke_w = if is_active { 2.5 } else { 1.0 };
            let rounded = D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: 10.0,
                radiusY: 10.0,
            };
            unsafe {
                self.d2d_context.FillRoundedRectangle(&rounded, fill);
                self.d2d_context
                    .DrawRoundedRectangle(&rounded, stroke, stroke_w, None);
            }
            if let Some(fmt_t) = &font_title {
                let text_r = D2D_RECT_F {
                    left: r.left + 4.0,
                    top: r.top + 8.0,
                    right: r.right - 4.0,
                    bottom: (r.top + 26.0).min(r.bottom - 4.0),
                };
                self.draw_text_utf8(title, text_r, fmt_t, &text_brush);
            }
            if let Some(fmt_s) = &font_sub
                && (r.bottom - r.top) >= 50.0
            {
                let sub_r = D2D_RECT_F {
                    left: r.left + 4.0,
                    top: r.top + 26.0,
                    right: r.right - 4.0,
                    bottom: (r.top + 44.0).min(r.bottom - 4.0),
                };
                self.draw_text_utf8(sub, sub_r, fmt_s, &text_brush);
            }
        }

        self.end_and_commit()
    }

    /// Renders a subtle 4px semitransparent glowing bezel bar indicating active area-aware scroll-to-switch mode.
    pub fn render_area_scroll_shade(
        &mut self,
        width: u32,
        height: u32,
        edge: ScreenEdge,
    ) -> Result<()> {
        let _target = self.begin_target(width, height)?;
        let wf = width.max(1) as f32;
        let hf = height.max(1) as f32;
        let glow_fill = self.create_brush(0.22, 0.74, 0.97, 0.42)?;
        let core_fill = self.create_brush(0.68, 0.91, 1.0, 0.78)?;
        let outer = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: wf,
            bottom: hf,
        };
        let core = match edge {
            ScreenEdge::Left => D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: (wf * 0.5).max(1.5),
                bottom: hf,
            },
            ScreenEdge::Right => D2D_RECT_F {
                left: (wf * 0.5).min(wf - 1.5).max(0.0),
                top: 0.0,
                right: wf,
                bottom: hf,
            },
            ScreenEdge::Top => D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: wf,
                bottom: (hf * 0.5).max(1.5),
            },
            ScreenEdge::Bottom => D2D_RECT_F {
                left: 0.0,
                top: (hf * 0.5).min(hf - 1.5).max(0.0),
                right: wf,
                bottom: hf,
            },
        };
        unsafe {
            self.d2d_context.FillRectangle(&outer, &glow_fill);
            self.d2d_context.FillRectangle(&core, &core_fill);
        }
        self.end_and_commit()
    }

    /// Renders the translucent dark background scrim (`0%..60%`).
    pub fn render_scrim(&mut self, width: u32, height: u32, opacity_percent: u8) -> Result<()> {
        let alpha = (f32::from(opacity_percent.min(60)) / 100.0).clamp(0.0, 0.6);
        self.render_scrim_alpha(width, height, alpha)
    }

    /// Renders the translucent dark background scrim with smooth `0.0..=0.6` fractional alpha.
    pub fn render_scrim_alpha(&mut self, width: u32, height: u32, alpha: f32) -> Result<()> {
        let _target = self.begin_target(width, height)?;
        let clamped = alpha.clamp(0.0, 0.6);
        if clamped > 0.001 {
            let fill = self.create_brush(0.0, 0.0, 0.0, clamped)?;
            let rect = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: width.max(1) as f32,
                bottom: height.max(1) as f32,
            };
            unsafe {
                self.d2d_context.FillRectangle(&rect, &fill);
            }
        }
        self.end_and_commit()
    }

    /// Renders a live visualizer overlay at a physical screen corner showing the corner entry box,
    /// the directional slide arrow, and the angled corridor wedge.
    pub fn render_churn_zone_visual(
        &mut self,
        width: u32,
        height: u32,
        zone: ChurnZoneId,
        corner_box_px: i32,
        slide_distance_px: i32,
        diagonal_angle_deg: i32,
    ) -> Result<()> {
        let _target = self.begin_target(width, height)?;
        let wf = width.max(1) as f32;
        let hf = height.max(1) as f32;
        let c = (corner_box_px.max(16) as f32).min(wf.min(hf) * 0.45);
        let s = (slide_distance_px.max(20) as f32).min(wf.max(hf) - c - 10.0);
        let angle = diagonal_angle_deg.clamp(15, 75);
        let rad = (angle as f32).to_radians();
        let inward_delta = (s * rad.tan()).min(wf.max(hf) - c - 10.0);

        let wedge_fill = self.create_brush(0.0, 0.78, 1.0, 0.18)?;
        let wedge_stroke = self.create_brush(0.0, 0.85, 1.0, 0.75)?;
        let box_fill = self.create_brush(0.0, 0.82, 1.0, 0.35)?;
        let box_stroke = self.create_brush(0.0, 0.95, 1.0, 0.95)?;
        let arrow_brush = self.create_brush(1.0, 1.0, 1.0, 0.95)?;
        let bezel_brush = self.create_brush(0.0, 0.95, 1.0, 0.90)?;
        let threshold_brush = self.create_brush(0.2, 0.9, 0.6, 0.85)?;
        let badge_bg = self.create_brush(0.05, 0.08, 0.14, 0.82)?;
        let badge_stroke = self.create_brush(0.0, 0.82, 1.0, 0.50)?;
        let text_brush = self.create_brush(0.94, 0.97, 1.0, 0.95)?;

        let (box_rect, poly, arrow_from, arrow_to, arrow_h1, arrow_h2, bezel_line, thresh_line) =
            match zone {
                ChurnZoneId::TopRightDown => {
                    let bx = D2D_RECT_F {
                        left: wf - c,
                        top: 0.0,
                        right: wf,
                        bottom: c,
                    };
                    let p = [
                        D2D_POINT_2F { x: wf, y: 0.0 },
                        D2D_POINT_2F { x: wf, y: c + s },
                        D2D_POINT_2F {
                            x: wf - (c + inward_delta),
                            y: c + s,
                        },
                        D2D_POINT_2F { x: wf - c, y: c },
                        D2D_POINT_2F { x: wf - c, y: 0.0 },
                    ];
                    let ax = wf - (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F { x: ax, y: c * 0.5 };
                    let at = D2D_POINT_2F {
                        x: ax,
                        y: c + s - 6.0,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: ax - 4.5,
                        y: c + s - 14.0,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: ax + 4.5,
                        y: c + s - 14.0,
                    };
                    let bz = (
                        D2D_POINT_2F {
                            x: wf - 1.5,
                            y: 0.0,
                        },
                        D2D_POINT_2F {
                            x: wf - 1.5,
                            y: c + s,
                        },
                    );
                    let th = (
                        D2D_POINT_2F { x: wf, y: c + s },
                        D2D_POINT_2F {
                            x: wf - (c + inward_delta),
                            y: c + s,
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::BottomRightUp => {
                    let bx = D2D_RECT_F {
                        left: wf - c,
                        top: hf - c,
                        right: wf,
                        bottom: hf,
                    };
                    let p = [
                        D2D_POINT_2F { x: wf, y: hf },
                        D2D_POINT_2F {
                            x: wf,
                            y: hf - (c + s),
                        },
                        D2D_POINT_2F {
                            x: wf - (c + inward_delta),
                            y: hf - (c + s),
                        },
                        D2D_POINT_2F {
                            x: wf - c,
                            y: hf - c,
                        },
                        D2D_POINT_2F { x: wf - c, y: hf },
                    ];
                    let ax = wf - (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F {
                        x: ax,
                        y: hf - c * 0.5,
                    };
                    let at = D2D_POINT_2F {
                        x: ax,
                        y: hf - (c + s) + 6.0,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: ax - 4.5,
                        y: hf - (c + s) + 14.0,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: ax + 4.5,
                        y: hf - (c + s) + 14.0,
                    };
                    let bz = (
                        D2D_POINT_2F { x: wf - 1.5, y: hf },
                        D2D_POINT_2F {
                            x: wf - 1.5,
                            y: hf - (c + s),
                        },
                    );
                    let th = (
                        D2D_POINT_2F {
                            x: wf,
                            y: hf - (c + s),
                        },
                        D2D_POINT_2F {
                            x: wf - (c + inward_delta),
                            y: hf - (c + s),
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::TopLeftDown => {
                    let bx = D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: c,
                        bottom: c,
                    };
                    let p = [
                        D2D_POINT_2F { x: 0.0, y: 0.0 },
                        D2D_POINT_2F { x: 0.0, y: c + s },
                        D2D_POINT_2F {
                            x: c + inward_delta,
                            y: c + s,
                        },
                        D2D_POINT_2F { x: c, y: c },
                        D2D_POINT_2F { x: c, y: 0.0 },
                    ];
                    let ax = (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F { x: ax, y: c * 0.5 };
                    let at = D2D_POINT_2F {
                        x: ax,
                        y: c + s - 6.0,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: ax - 4.5,
                        y: c + s - 14.0,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: ax + 4.5,
                        y: c + s - 14.0,
                    };
                    let bz = (
                        D2D_POINT_2F { x: 1.5, y: 0.0 },
                        D2D_POINT_2F { x: 1.5, y: c + s },
                    );
                    let th = (
                        D2D_POINT_2F { x: 0.0, y: c + s },
                        D2D_POINT_2F {
                            x: c + inward_delta,
                            y: c + s,
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::BottomLeftUp => {
                    let bx = D2D_RECT_F {
                        left: 0.0,
                        top: hf - c,
                        right: c,
                        bottom: hf,
                    };
                    let p = [
                        D2D_POINT_2F { x: 0.0, y: hf },
                        D2D_POINT_2F {
                            x: 0.0,
                            y: hf - (c + s),
                        },
                        D2D_POINT_2F {
                            x: c + inward_delta,
                            y: hf - (c + s),
                        },
                        D2D_POINT_2F { x: c, y: hf - c },
                        D2D_POINT_2F { x: c, y: hf },
                    ];
                    let ax = (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F {
                        x: ax,
                        y: hf - c * 0.5,
                    };
                    let at = D2D_POINT_2F {
                        x: ax,
                        y: hf - (c + s) + 6.0,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: ax - 4.5,
                        y: hf - (c + s) + 14.0,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: ax + 4.5,
                        y: hf - (c + s) + 14.0,
                    };
                    let bz = (
                        D2D_POINT_2F { x: 1.5, y: hf },
                        D2D_POINT_2F {
                            x: 1.5,
                            y: hf - (c + s),
                        },
                    );
                    let th = (
                        D2D_POINT_2F {
                            x: 0.0,
                            y: hf - (c + s),
                        },
                        D2D_POINT_2F {
                            x: c + inward_delta,
                            y: hf - (c + s),
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::TopLeftRight => {
                    let bx = D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: c,
                        bottom: c,
                    };
                    let p = [
                        D2D_POINT_2F { x: 0.0, y: 0.0 },
                        D2D_POINT_2F { x: c + s, y: 0.0 },
                        D2D_POINT_2F {
                            x: c + s,
                            y: c + inward_delta,
                        },
                        D2D_POINT_2F { x: c, y: c },
                        D2D_POINT_2F { x: 0.0, y: c },
                    ];
                    let ay = (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F { x: c * 0.5, y: ay };
                    let at = D2D_POINT_2F {
                        x: c + s - 6.0,
                        y: ay,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: c + s - 14.0,
                        y: ay - 4.5,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: c + s - 14.0,
                        y: ay + 4.5,
                    };
                    let bz = (
                        D2D_POINT_2F { x: 0.0, y: 1.5 },
                        D2D_POINT_2F { x: c + s, y: 1.5 },
                    );
                    let th = (
                        D2D_POINT_2F { x: c + s, y: 0.0 },
                        D2D_POINT_2F {
                            x: c + s,
                            y: c + inward_delta,
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::TopRightLeft => {
                    let bx = D2D_RECT_F {
                        left: wf - c,
                        top: 0.0,
                        right: wf,
                        bottom: c,
                    };
                    let p = [
                        D2D_POINT_2F { x: wf, y: 0.0 },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: 0.0,
                        },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: c + inward_delta,
                        },
                        D2D_POINT_2F { x: wf - c, y: c },
                        D2D_POINT_2F { x: wf, y: c },
                    ];
                    let ay = (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F {
                        x: wf - c * 0.5,
                        y: ay,
                    };
                    let at = D2D_POINT_2F {
                        x: wf - (c + s) + 6.0,
                        y: ay,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: wf - (c + s) + 14.0,
                        y: ay - 4.5,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: wf - (c + s) + 14.0,
                        y: ay + 4.5,
                    };
                    let bz = (
                        D2D_POINT_2F { x: wf, y: 1.5 },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: 1.5,
                        },
                    );
                    let th = (
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: 0.0,
                        },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: c + inward_delta,
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::BottomLeftRight => {
                    let bx = D2D_RECT_F {
                        left: 0.0,
                        top: hf - c,
                        right: c,
                        bottom: hf,
                    };
                    let p = [
                        D2D_POINT_2F { x: 0.0, y: hf },
                        D2D_POINT_2F { x: c + s, y: hf },
                        D2D_POINT_2F {
                            x: c + s,
                            y: hf - (c + inward_delta),
                        },
                        D2D_POINT_2F { x: c, y: hf - c },
                        D2D_POINT_2F { x: 0.0, y: hf - c },
                    ];
                    let ay = hf - (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F { x: c * 0.5, y: ay };
                    let at = D2D_POINT_2F {
                        x: c + s - 6.0,
                        y: ay,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: c + s - 14.0,
                        y: ay - 4.5,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: c + s - 14.0,
                        y: ay + 4.5,
                    };
                    let bz = (
                        D2D_POINT_2F {
                            x: 0.0,
                            y: hf - 1.5,
                        },
                        D2D_POINT_2F {
                            x: c + s,
                            y: hf - 1.5,
                        },
                    );
                    let th = (
                        D2D_POINT_2F { x: c + s, y: hf },
                        D2D_POINT_2F {
                            x: c + s,
                            y: hf - (c + inward_delta),
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
                ChurnZoneId::BottomRightLeft => {
                    let bx = D2D_RECT_F {
                        left: wf - c,
                        top: hf - c,
                        right: wf,
                        bottom: hf,
                    };
                    let p = [
                        D2D_POINT_2F { x: wf, y: hf },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: hf,
                        },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: hf - (c + inward_delta),
                        },
                        D2D_POINT_2F {
                            x: wf - c,
                            y: hf - c,
                        },
                        D2D_POINT_2F { x: wf, y: hf - c },
                    ];
                    let ay = hf - (c * 0.5).clamp(8.0, 16.0);
                    let af = D2D_POINT_2F {
                        x: wf - c * 0.5,
                        y: ay,
                    };
                    let at = D2D_POINT_2F {
                        x: wf - (c + s) + 6.0,
                        y: ay,
                    };
                    let ah1 = D2D_POINT_2F {
                        x: wf - (c + s) + 14.0,
                        y: ay - 4.5,
                    };
                    let ah2 = D2D_POINT_2F {
                        x: wf - (c + s) + 14.0,
                        y: ay + 4.5,
                    };
                    let bz = (
                        D2D_POINT_2F { x: wf, y: hf - 1.5 },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: hf - 1.5,
                        },
                    );
                    let th = (
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: hf,
                        },
                        D2D_POINT_2F {
                            x: wf - (c + s),
                            y: hf - (c + inward_delta),
                        },
                    );
                    (bx, p, af, at, ah1, ah2, bz, th)
                }
            };

        // 1. Draw wedge geometry
        let factory = unsafe { self.d2d_context.GetFactory()? };
        let path = unsafe { factory.CreatePathGeometry()? };
        let sink = unsafe { path.Open()? };
        unsafe {
            sink.BeginFigure(poly[0], D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&poly[1..]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            self.d2d_context.FillGeometry(&path, &wedge_fill, None);
            self.d2d_context
                .DrawGeometry(&path, &wedge_stroke, 1.5, None);
        }

        // 2. Draw slide completion threshold line
        unsafe {
            self.d2d_context
                .DrawLine(thresh_line.0, thresh_line.1, &threshold_brush, 2.0, None);
        }

        // 3. Draw physical bezel guide line
        unsafe {
            self.d2d_context
                .DrawLine(bezel_line.0, bezel_line.1, &bezel_brush, 3.0, None);
        }

        // 4. Draw corner hit box
        unsafe {
            self.d2d_context.FillRectangle(&box_rect, &box_fill);
            self.d2d_context
                .DrawRectangle(&box_rect, &box_stroke, 1.8, None);
        }

        // 5. Draw slide arrow
        unsafe {
            self.d2d_context
                .DrawLine(arrow_from, arrow_to, &arrow_brush, 2.4, None);
            self.d2d_context
                .DrawLine(arrow_to, arrow_h1, &arrow_brush, 2.4, None);
            self.d2d_context
                .DrawLine(arrow_to, arrow_h2, &arrow_brush, 2.4, None);
        }

        // 6. Draw clean DirectWrite HUD badges
        if let Some(fmt) = self.create_text_format(10.5, true, false) {
            let angle_text = format!("{}° Corridor", angle);
            let slide_text = format!("{}px Slide", slide_distance_px);
            let corner_text = format!("Corner {}px", corner_box_px);

            // Angle badge placed at the inward slope
            let (ang_x, ang_y) = match zone {
                ChurnZoneId::TopRightDown => (wf - c - inward_delta * 0.5 - 35.0, c + s * 0.4),
                ChurnZoneId::BottomRightUp => (
                    wf - c - inward_delta * 0.5 - 35.0,
                    hf - (c + s * 0.4) - 10.0,
                ),
                ChurnZoneId::TopLeftDown => (c + inward_delta * 0.5 - 35.0, c + s * 0.4),
                ChurnZoneId::BottomLeftUp => {
                    (c + inward_delta * 0.5 - 35.0, hf - (c + s * 0.4) - 10.0)
                }
                ChurnZoneId::TopLeftRight => (c + s * 0.4 - 35.0, c + inward_delta * 0.5),
                ChurnZoneId::TopRightLeft => (wf - (c + s * 0.4) - 35.0, c + inward_delta * 0.5),
                ChurnZoneId::BottomLeftRight => {
                    (c + s * 0.4 - 35.0, hf - (c + inward_delta * 0.5) - 10.0)
                }
                ChurnZoneId::BottomRightLeft => (
                    wf - (c + s * 0.4) - 35.0,
                    hf - (c + inward_delta * 0.5) - 10.0,
                ),
            };
            let ang_rect = D2D_RECT_F {
                left: ang_x.max(4.0),
                top: ang_y.max(4.0),
                right: (ang_x + 76.0).min(wf - 4.0),
                bottom: (ang_y + 18.0).min(hf - 4.0),
            };
            let ang_rr = D2D1_ROUNDED_RECT {
                rect: ang_rect,
                radiusX: 3.0,
                radiusY: 3.0,
            };
            unsafe {
                self.d2d_context.FillRoundedRectangle(&ang_rr, &badge_bg);
                self.d2d_context
                    .DrawRoundedRectangle(&ang_rr, &badge_stroke, 1.0, None);
            }
            self.draw_text_utf8(&angle_text, ang_rect, &fmt, &text_brush);

            // Slide distance badge placed near the threshold line
            let (sld_x, sld_y) = match zone {
                ChurnZoneId::TopRightDown => (
                    wf - (c + inward_delta).min(wf * 0.8) - 10.0,
                    (c + s + 3.0).min(hf - 20.0),
                ),
                ChurnZoneId::BottomRightUp => (
                    wf - (c + inward_delta).min(wf * 0.8) - 10.0,
                    (hf - (c + s) - 20.0).max(4.0),
                ),
                ChurnZoneId::TopLeftDown => (4.0, (c + s + 3.0).min(hf - 20.0)),
                ChurnZoneId::BottomLeftUp => (4.0, (hf - (c + s) - 20.0).max(4.0)),
                ChurnZoneId::TopLeftRight => ((c + s + 3.0).min(wf - 75.0), 4.0),
                ChurnZoneId::TopRightLeft => ((wf - (c + s) - 75.0).max(4.0), 4.0),
                ChurnZoneId::BottomLeftRight => ((c + s + 3.0).min(wf - 75.0), hf - 20.0),
                ChurnZoneId::BottomRightLeft => ((wf - (c + s) - 75.0).max(4.0), hf - 20.0),
            };
            let sld_rect = D2D_RECT_F {
                left: sld_x.max(4.0),
                top: sld_y.max(4.0),
                right: (sld_x + 72.0).min(wf - 4.0),
                bottom: (sld_y + 18.0).min(hf - 4.0),
            };
            let sld_rr = D2D1_ROUNDED_RECT {
                rect: sld_rect,
                radiusX: 3.0,
                radiusY: 3.0,
            };
            unsafe {
                self.d2d_context.FillRoundedRectangle(&sld_rr, &badge_bg);
                self.d2d_context
                    .DrawRoundedRectangle(&sld_rr, &badge_stroke, 1.0, None);
            }
            self.draw_text_utf8(&slide_text, sld_rect, &fmt, &text_brush);

            // Corner hit badge
            let (crn_x, crn_y) = match zone {
                ChurnZoneId::TopRightDown | ChurnZoneId::TopRightLeft => {
                    ((box_rect.left - 65.0).max(4.0), 3.0)
                }
                ChurnZoneId::BottomRightUp | ChurnZoneId::BottomRightLeft => {
                    ((box_rect.left - 65.0).max(4.0), hf - 20.0)
                }
                ChurnZoneId::TopLeftDown | ChurnZoneId::TopLeftRight => (box_rect.right + 3.0, 3.0),
                ChurnZoneId::BottomLeftUp | ChurnZoneId::BottomLeftRight => {
                    (box_rect.right + 3.0, hf - 20.0)
                }
            };
            let crn_rect = D2D_RECT_F {
                left: crn_x.max(4.0),
                top: crn_y.max(4.0),
                right: (crn_x + 70.0).min(wf - 4.0),
                bottom: (crn_y + 18.0).min(hf - 4.0),
            };
            let crn_rr = D2D1_ROUNDED_RECT {
                rect: crn_rect,
                radiusX: 3.0,
                radiusY: 3.0,
            };
            unsafe {
                self.d2d_context.FillRoundedRectangle(&crn_rr, &badge_bg);
                self.d2d_context
                    .DrawRoundedRectangle(&crn_rr, &badge_stroke, 1.0, None);
            }
            self.draw_text_utf8(&corner_text, crn_rect, &fmt, &text_brush);
        }

        self.end_and_commit()
    }
}

fn find_target_pid_for_icon(hwnd: HWND) -> u32 {
    let mut cls = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut cls) };
    if len > 0 && String::from_utf16_lossy(&cls[..len as usize]) == "ApplicationFrameWindow" {
        let core_cls: Vec<u16> = "Windows.UI.Core.CoreWindow\0".encode_utf16().collect();
        if let Ok(child) = unsafe { FindWindowExW(hwnd, None, PCWSTR(core_cls.as_ptr()), None) }
            && !child.0.is_null()
        {
            let mut cpid = 0u32;
            unsafe { GetWindowThreadProcessId(child, Some(&mut cpid)) };
            if cpid != 0 {
                return cpid;
            }
        }
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn extract_exe_icon_for_hwnd(hwnd: HWND) -> Option<HICON> {
    let pid = find_target_pid_for_icon(hwnd);
    if pid == 0 {
        return None;
    }
    let proc = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            proc,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok()
    };
    let _ = unsafe { CloseHandle(proc) };
    if !ok || size == 0 {
        return None;
    }
    let mut shfi = SHFILEINFOW::default();
    let res = unsafe {
        SHGetFileInfoW(
            PCWSTR(buf.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            Some(&mut shfi),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if res != 0 && !shfi.hIcon.is_invalid() {
        Some(shfi.hIcon)
    } else {
        None
    }
}

/// Extracts a window's icon (`WM_GETICON`, class icon, or process exe shell icon) into premultiplied BGRA pixels.
pub fn extract_window_icon_bgra(hwnd: HWND) -> Option<IconPixels> {
    unsafe {
        let mut raw_icon = 0usize;
        for icon_type in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
            let mut res = 0usize;
            if SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                windows::Win32::Foundation::WPARAM(icon_type as usize),
                windows::Win32::Foundation::LPARAM(0),
                SMTO_ABORTIFHUNG,
                40,
                Some(&mut res),
            )
            .0 != 0
                && res != 0
            {
                raw_icon = res;
                break;
            }
        }
        if raw_icon == 0 {
            raw_icon = GetClassLongPtrW(hwnd, GCLP_HICONSM);
        }
        if raw_icon == 0 {
            raw_icon = GetClassLongPtrW(hwnd, GCLP_HICON);
        }
        let mut owned_hicon: Option<HICON> = None;
        if raw_icon == 0 {
            owned_hicon = extract_exe_icon_for_hwnd(hwnd);
            if let Some(h) = owned_hicon {
                raw_icon = h.0 as usize;
            }
        }
        if raw_icon == 0 {
            return None;
        }

        let hicon = HICON(raw_icon as *mut _);
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            if let Some(owned) = owned_hicon {
                let _ = DestroyIcon(owned);
            }
            return None;
        }

        let is_mono_mask = info.hbmColor.is_invalid();
        let target_bmp = if !is_mono_mask {
            info.hbmColor
        } else {
            info.hbmMask
        };

        let mut bmp = BITMAP::default();
        if GetObjectW(
            target_bmp,
            size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut _ as *mut _),
        ) == 0
        {
            if !info.hbmColor.is_invalid() {
                let _ = DeleteObject(info.hbmColor);
            }
            if !info.hbmMask.is_invalid() {
                let _ = DeleteObject(info.hbmMask);
            }
            if let Some(owned) = owned_hicon {
                let _ = DestroyIcon(owned);
            }
            return None;
        }

        let width = bmp.bmWidth.max(1) as u32;
        let raw_height = bmp.bmHeight.max(1);
        let height = if is_mono_mask {
            (raw_height / 2).max(1) as u32
        } else {
            raw_height as u32
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];

        let hdc = CreateCompatibleDC(None);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // top-down BGRA
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let scanlines = GetDIBits(
            hdc,
            target_bmp,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = DeleteDC(hdc);
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(info.hbmMask);
        }
        if let Some(owned) = owned_hicon {
            let _ = DestroyIcon(owned);
        }

        if scanlines == 0 {
            return None;
        }

        // Premultiply alpha for Direct2D `D2D1_ALPHA_MODE_PREMULTIPLIED`.
        // Legacy Win32 icons without alpha channel have all 0-alpha bytes; force them to opaque.
        let has_any_alpha = pixels.as_chunks::<4>().0.iter().any(|px| px[3] > 0);
        if !has_any_alpha {
            for px in pixels.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
        } else {
            for px in pixels.as_chunks_mut::<4>().0 {
                let a = px[3] as u16;
                if a < 255 {
                    px[0] = ((px[0] as u16 * a) / 255) as u8;
                    px[1] = ((px[1] as u16 * a) / 255) as u8;
                    px[2] = ((px[2] as u16 * a) / 255) as u8;
                }
            }
        }

        let _ = RECT::default();
        Some(IconPixels {
            width,
            height,
            bgra: pixels,
        })
    }
}
