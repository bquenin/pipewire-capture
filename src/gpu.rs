//! GPU-accelerated DMA buffer import using EGL.
//!
//! This module uses EGL to import DMA buffers from PipeWire and read them
//! using OpenGL, which is significantly faster than mmap on discrete GPUs.

use khronos_egl as egl;
use std::ffi::c_void;
use tracing::{debug, info};

// EGL DMA-BUF extension constants (not in khronos-egl)
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const EGL_NONE: i32 = 0x3038;
const EGL_NO_CONTEXT: *mut c_void = std::ptr::null_mut();

// DRM format constants (fourcc codes)
pub const DRM_FORMAT_XRGB8888: u32 = 0x34325258; // 'XR24'
pub const DRM_FORMAT_ARGB8888: u32 = 0x34325241; // 'AR24'
pub const DRM_FORMAT_XBGR8888: u32 = 0x34324258; // 'XB24'
pub const DRM_FORMAT_ABGR8888: u32 = 0x34324241; // 'AB24'
pub const DRM_FORMAT_RGBX8888: u32 = 0x34325852; // 'RX24'
pub const DRM_FORMAT_RGBA8888: u32 = 0x34324152; // 'RA24'
pub const DRM_FORMAT_BGRX8888: u32 = 0x34325842; // 'BX24'
pub const DRM_FORMAT_BGRA8888: u32 = 0x34324142; // 'BA24'

// GL constants not in the gl crate
const GL_BGRA_EXT: u32 = 0x80E1;

// EGL types for raw function calls
type EGLDisplay = *mut c_void;
type EGLContext = *mut c_void;
type EGLImage = *mut c_void;

/// Type alias for eglCreateImageKHR function pointer.
type EglCreateImageKhrFn = unsafe extern "C" fn(
    dpy: EGLDisplay,
    ctx: EGLContext,
    target: u32,
    buffer: *mut c_void,
    attrib_list: *const i32,
) -> EGLImage;

/// Type alias for eglDestroyImageKHR function pointer.
type EglDestroyImageKhrFn = unsafe extern "C" fn(dpy: EGLDisplay, image: EGLImage) -> u32;

/// Type alias for glEGLImageTargetTexture2DOES function pointer.
type GlEglImageTargetTexture2DOesFn = unsafe extern "C" fn(target: u32, image: EGLImage);

/// GPU-accelerated frame importer using EGL.
pub struct GpuFrameImporter {
    egl: egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
    context: egl::Context,
    #[allow(dead_code)]
    config: egl::Config,
    // Raw display pointer for extension calls
    raw_display: EGLDisplay,
    // Reusable GL resources for EGLImage import
    texture: u32,
    fbo: u32,
    // Cached function pointers for EGL extensions
    egl_create_image_khr: EglCreateImageKhrFn,
    egl_destroy_image_khr: EglDestroyImageKhrFn,
    gl_egl_image_target_texture_2d_oes: GlEglImageTargetTexture2DOesFn,
    // Reusable buffer for pixel data
    pixel_buffer: Vec<u8>,
}

impl GpuFrameImporter {
    /// Create a new GPU frame importer.
    ///
    /// This initializes EGL with a headless context for importing DMA buffers.
    /// Returns an error if EGL is not available or doesn't support required extensions.
    pub fn new() -> Result<Self, String> {
        info!("Initializing GPU frame importer");

        // Load EGL dynamically
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }
            .map_err(|e| format!("Failed to load EGL: {:?}", e))?;

        // Get default display
        let display =
            unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or("No EGL display available")?;

        // Initialize EGL
        let (major, minor) = egl
            .initialize(display)
            .map_err(|e| format!("Failed to initialize EGL: {:?}", e))?;
        info!("EGL initialized: version {}.{}", major, minor);

        // Check for required extensions
        let extensions = egl
            .query_string(Some(display), egl::EXTENSIONS)
            .map_err(|e| format!("Failed to query EGL extensions: {:?}", e))?
            .to_string_lossy()
            .to_string();

        debug!("EGL extensions: {}", extensions);

        if !extensions.contains("EGL_EXT_image_dma_buf_import") {
            return Err("EGL_EXT_image_dma_buf_import extension not supported".to_string());
        }

        if !extensions.contains("EGL_KHR_surfaceless_context") {
            return Err("EGL_KHR_surfaceless_context extension not supported".to_string());
        }

        // Bind OpenGL ES API
        egl.bind_api(egl::OPENGL_ES_API)
            .map_err(|e| format!("Failed to bind OpenGL ES API: {:?}", e))?;

        // Choose config for OpenGL ES 2.0
        let config_attribs = [
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES2_BIT,
            egl::SURFACE_TYPE,
            0, // No surface needed for surfaceless context
            egl::NONE,
        ];

        let config = egl
            .choose_first_config(display, &config_attribs)
            .map_err(|e| format!("Failed to choose EGL config: {:?}", e))?
            .ok_or("No suitable EGL config found")?;

        // Create OpenGL ES 2.0 context
        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];

        let context = egl
            .create_context(display, config, None, &context_attribs)
            .map_err(|e| format!("Failed to create EGL context: {:?}", e))?;

        // Make context current without a surface (surfaceless)
        egl.make_current(display, None, None, Some(context))
            .map_err(|e| format!("Failed to make EGL context current: {:?}", e))?;

        // Load OpenGL functions
        gl::load_with(|s| {
            egl.get_proc_address(s)
                .map(|p| p as *const c_void)
                .unwrap_or(std::ptr::null())
        });

        // Get raw display pointer for extension calls
        let raw_display = display.as_ptr();

        // Get EGL extension function pointers
        let egl_create_image_khr: EglCreateImageKhrFn = unsafe {
            let ptr = egl
                .get_proc_address("eglCreateImageKHR")
                .ok_or("eglCreateImageKHR not available")?;
            std::mem::transmute(ptr)
        };

        let egl_destroy_image_khr: EglDestroyImageKhrFn = unsafe {
            let ptr = egl
                .get_proc_address("eglDestroyImageKHR")
                .ok_or("eglDestroyImageKHR not available")?;
            std::mem::transmute(ptr)
        };

        let gl_egl_image_target_texture_2d_oes: GlEglImageTargetTexture2DOesFn = unsafe {
            let ptr = egl
                .get_proc_address("glEGLImageTargetTexture2DOES")
                .ok_or("glEGLImageTargetTexture2DOES not available")?;
            std::mem::transmute(ptr)
        };

        // Create reusable GL resources
        let mut texture = 0u32;
        let mut fbo = 0u32;
        unsafe {
            gl::GenTextures(1, &mut texture);
            gl::GenFramebuffers(1, &mut fbo);
        }

        info!("GPU frame importer initialized successfully");

        let mut importer = Self {
            egl,
            display,
            context,
            config,
            raw_display,
            texture,
            fbo,
            egl_create_image_khr,
            egl_destroy_image_khr,
            gl_egl_image_target_texture_2d_oes,
            pixel_buffer: Vec::new(),
        };

        // Warmup GPU to avoid first-frame latency
        importer.warmup()?;

        Ok(importer)
    }

    /// Warm up GPU to avoid first-frame latency.
    ///
    /// GPU drivers defer initialization until first use. This method
    /// triggers all deferred work by doing a dummy render+readback cycle.
    fn warmup(&mut self) -> Result<(), String> {
        info!("Warming up GPU...");
        let start = std::time::Instant::now();

        // 1. Create small dummy texture (64x64 RGBA)
        let mut dummy_tex = 0u32;
        unsafe {
            gl::GenTextures(1, &mut dummy_tex);
            gl::BindTexture(gl::TEXTURE_2D, dummy_tex);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                64,
                64,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
        }

        // 2. Attach to FBO
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                dummy_tex,
                0,
            );

            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                gl::DeleteTextures(1, &dummy_tex);
                return Err(format!("Warmup framebuffer incomplete: 0x{:X}", status));
            }
        }

        // 3. Do dummy glReadPixels - triggers all deferred driver initialization
        let mut dummy_pixels = vec![0u8; 64 * 64 * 4];
        unsafe {
            gl::ReadPixels(
                0,
                0,
                64,
                64,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                dummy_pixels.as_mut_ptr() as *mut c_void,
            );
        }

        // 4. glFinish blocks until ALL GPU work is done
        unsafe {
            gl::Finish();
        }

        // 5. Cleanup
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::DeleteTextures(1, &dummy_tex);
        }

        let elapsed = start.elapsed();
        info!("GPU warmup completed in {:?}", elapsed);

        Ok(())
    }

    /// Import a DMA buffer and read its pixels.
    ///
    /// # Arguments
    /// * `dmabuf_fd` - File descriptor of the DMA buffer
    /// * `width` - Width of the image in pixels
    /// * `height` - Height of the image in pixels
    /// * `stride` - Row stride in bytes
    /// * `format` - DRM fourcc format code
    ///
    /// # Returns
    /// BGRA pixel data as a Vec<u8>
    pub fn import_frame(
        &mut self,
        dmabuf_fd: i32,
        width: u32,
        height: u32,
        stride: u32,
        format: u32,
    ) -> Result<Vec<u8>, String> {
        let frame_start = std::time::Instant::now();

        // Build attribute list for EGLImage creation
        // Using i32 for EGL attribs as per EGL spec
        let attribs: [i32; 13] = [
            egl::WIDTH,
            width as i32,
            egl::HEIGHT,
            height as i32,
            EGL_LINUX_DRM_FOURCC_EXT,
            format as i32,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            dmabuf_fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            0,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            stride as i32,
            EGL_NONE,
        ];

        // Create EGLImage using raw function pointer
        let egl_image = unsafe {
            (self.egl_create_image_khr)(
                self.raw_display,
                EGL_NO_CONTEXT,
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(), // No client buffer for DMA-BUF
                attribs.as_ptr(),
            )
        };

        if egl_image.is_null() {
            return Err("Failed to create EGLImage from DMA buffer".to_string());
        }

        // Bind EGLImage to our reusable EGL texture
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.texture);
            (self.gl_egl_image_target_texture_2d_oes)(gl::TEXTURE_2D, egl_image);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
        }

        // Destroy EGLImage (texture keeps reference to the buffer)
        unsafe {
            (self.egl_destroy_image_khr)(self.raw_display, egl_image);
        }

        // Bind EGLImage texture to FBO for direct pixel reading
        // With explicit sync, we don't need the intermediate copy anymore
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                self.texture,
                0,
            );

            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                return Err(format!("Framebuffer incomplete: 0x{:X}", status));
            }
        }

        // Read pixels
        let size = (width * height * 4) as usize;
        if self.pixel_buffer.len() != size {
            self.pixel_buffer.resize(size, 0);
        }

        unsafe {
            gl::ReadPixels(
                0,
                0,
                width as i32,
                height as i32,
                GL_BGRA_EXT,
                gl::UNSIGNED_BYTE,
                self.pixel_buffer.as_mut_ptr() as *mut c_void,
            );
        }

        // Check for GL errors
        let error = unsafe { gl::GetError() };
        if error != gl::NO_ERROR {
            return Err(format!("OpenGL error: 0x{:X}", error));
        }

        // Unbind
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        debug!("Frame import took {:?}", frame_start.elapsed());
        Ok(self.pixel_buffer.clone())
    }
}

impl Drop for GpuFrameImporter {
    fn drop(&mut self) {
        debug!("Cleaning up GPU frame importer");

        unsafe {
            if self.fbo != 0 {
                gl::DeleteFramebuffers(1, &self.fbo);
            }
            if self.texture != 0 {
                gl::DeleteTextures(1, &self.texture);
            }
        }

        // Make sure context is current before destroying
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

/// Convert SPA video format to DRM fourcc format code.
///
/// PipeWire uses SPA video formats which need to be mapped to DRM fourcc codes
/// for EGL DMA-BUF import.
///
/// SPA format enum (from spa/param/video/raw.h):
///   7: RGBx, 8: BGRx, 9: xRGB, 10: xBGR
///  11: RGBA, 12: BGRA, 13: ARGB, 14: ABGR
pub fn spa_format_to_drm(spa_format: u32) -> Option<u32> {
    // SPA and DRM use different conventions for pixel format naming:
    // - SPA names describe memory byte order (first letter = lowest address)
    // - DRM names describe value in a native-endian u32 (first letter = MSB)
    // On little-endian: SPA_VIDEO_FORMAT_BGRA (B,G,R,A in memory) = DRM_FORMAT_ARGB8888
    match spa_format {
        7 => Some(DRM_FORMAT_XBGR8888),  // SPA_VIDEO_FORMAT_RGBx
        8 => Some(DRM_FORMAT_XRGB8888),  // SPA_VIDEO_FORMAT_BGRx
        9 => Some(DRM_FORMAT_BGRX8888),  // SPA_VIDEO_FORMAT_xRGB
        10 => Some(DRM_FORMAT_RGBX8888), // SPA_VIDEO_FORMAT_xBGR
        11 => Some(DRM_FORMAT_ABGR8888), // SPA_VIDEO_FORMAT_RGBA
        12 => Some(DRM_FORMAT_ARGB8888), // SPA_VIDEO_FORMAT_BGRA
        13 => Some(DRM_FORMAT_BGRA8888), // SPA_VIDEO_FORMAT_ARGB
        14 => Some(DRM_FORMAT_RGBA8888), // SPA_VIDEO_FORMAT_ABGR
        _ => None,
    }
}
