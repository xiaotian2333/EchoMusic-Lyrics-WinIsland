use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::*;

pub struct ScreenCaptureRequest {
    pub src_x: i32,
    pub src_y: i32,
    pub src_w: i32,
    pub src_h: i32,
    pub dst_w: i32,
    pub dst_h: i32,
    pub use_halftone: bool,
    pub force_opaque_alpha: bool,
}

pub struct ScreenCapture {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

pub fn capture_screen_bgra(request: ScreenCaptureRequest) -> Option<ScreenCapture> {
    if request.src_w <= 0 || request.src_h <= 0 || request.dst_w <= 0 || request.dst_h <= 0 {
        return None;
    }

    // SAFETY: 这里集中执行 GDI 屏幕截图。所有 GDI 句柄都在本函数内创建并检查有效性，
    // 失败路径会释放已获取的资源，成功路径按反向顺序恢复和释放资源。
    unsafe {
        let hdc_screen = GetDC(HWND::default());
        if hdc_screen.is_invalid() {
            return None;
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return None;
        }

        let hbm = CreateCompatibleBitmap(hdc_screen, request.dst_w, request.dst_h);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(HWND::default(), hdc_screen);
            return None;
        }

        let old = SelectObject(hdc_mem, hbm);

        if request.use_halftone {
            let _ = SetStretchBltMode(hdc_mem, STRETCH_BLT_MODE(HALFTONE.0));
        }

        if request.src_w == request.dst_w && request.src_h == request.dst_h {
            let _ = BitBlt(
                hdc_mem,
                0,
                0,
                request.dst_w,
                request.dst_h,
                hdc_screen,
                request.src_x,
                request.src_y,
                SRCCOPY,
            );
        } else {
            let _ = StretchBlt(
                hdc_mem,
                0,
                0,
                request.dst_w,
                request.dst_h,
                hdc_screen,
                request.src_x,
                request.src_y,
                request.src_w,
                request.src_h,
                SRCCOPY,
            );
        }

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = request.dst_w;
        bmi.bmiHeader.biHeight = -request.dst_h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let pixel_count = (request.dst_w * request.dst_h * 4) as usize;
        let mut pixels = vec![0u8; pixel_count];
        let _ = GetDIBits(
            hdc_mem,
            hbm,
            0,
            request.dst_h as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let _ = SelectObject(hdc_mem, old);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(HWND::default(), hdc_screen);

        if request.force_opaque_alpha {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }

        Some(ScreenCapture {
            width: request.dst_w,
            height: request.dst_h,
            pixels,
        })
    }
}
