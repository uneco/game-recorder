use crate::{
    mixer::{Mixer, RATE, TICKS},
    Options,
};
use anyhow::{bail, Context, Result};
use std::{
    mem::ManuallyDrop,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};
use windows::{
    core::{self as wcore, implement, Interface, GUID, HSTRING, PCWSTR},
    Foundation::{TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::*,
        DirectX::{Direct3D11::*, DirectXPixelFormat},
    },
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIDevice},
        },
        Media::{Audio::*, MediaFoundation::*},
        System::{
            Com::{StructuredStorage::*, *},
            Performance::*,
            Threading::*,
            Variant::VT_BLOB,
            WinRT::{Direct3D11::*, Graphics::Capture::*, *},
        },
        UI::WindowsAndMessaging::*,
    },
};

fn ticks() -> u64 {
    unsafe {
        let (mut c, mut f) = (0, 0);
        QueryPerformanceCounter(&mut c).unwrap();
        QueryPerformanceFrequency(&mut f).unwrap();
        (c as u128 * TICKS as u128 / f as u128) as u64
    }
}
fn span(value: u64) -> TimeSpan {
    TimeSpan {
        Duration: value as i64,
    }
}
struct Shared {
    stop: AtomicBool,
    end: AtomicU64,
    epoch: AtomicU64,
    mixer: Mutex<Mixer>,
    error: Mutex<Option<String>>,
    reason: Mutex<String>,
}
impl Shared {
    fn stop(&self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            let epoch = self.epoch.load(Ordering::SeqCst);
            self.end.store(
                if epoch == 0 {
                    0
                } else {
                    ticks().saturating_sub(epoch)
                },
                Ordering::SeqCst,
            );
        }
    }
    fn fail(&self, e: impl std::fmt::Display) {
        *self.error.lock().unwrap() = Some(e.to_string());
        self.stop();
    }
    fn wait_until(&self, t: u64) {
        while !self.stop.load(Ordering::SeqCst)
            && ticks().saturating_sub(self.epoch.load(Ordering::SeqCst)) < t
        {
            thread::sleep(Duration::from_millis(2));
        }
    }
}
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activation {
    sender: Mutex<Option<mpsc::Sender<wcore::Result<usize>>>>,
}
impl IActivateAudioInterfaceCompletionHandler_Impl for Activation_Impl {
    fn ActivateCompleted(
        &self,
        operation: wcore::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> wcore::Result<()> {
        let result = (|| unsafe {
            let mut hr = wcore::HRESULT(0);
            let mut object = None;
            operation.ok()?.GetActivateResult(&mut hr, &mut object)?;
            hr.ok()?;
            let client: IAudioClient = object.unwrap().cast()?;
            Ok(client.into_raw() as usize)
        })();
        if let Some(sender) = self.sender.lock().unwrap().take() {
            if let Err(mpsc::SendError(Ok(pointer))) = sender.send(result) {
                // Cancellation can close the receiver before activation completes.
                unsafe {
                    drop(IAudioClient::from_raw(pointer as *mut _));
                }
            }
        }
        Ok(())
    }
}
unsafe fn game_audio(pid: u32, shared: &Shared) -> Result<IAudioClient> {
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activation {
        sender: Mutex::new(Some(tx)),
    }
    .into();
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // The VT_BLOB borrows stack-owned activation parameters. PROPVARIANT's normal
    // destructor would call CoTaskMemFree on that stack address.
    let variant = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of_val(&params) as u32,
                        pBlobData: (&mut params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
                    },
                },
                ..Default::default()
            }),
        },
    });
    let _operation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&*variant),
        &handler,
    )
    .context("Process audio capture requires Windows 11 (build 20348 or later)")?;
    let pointer = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("Audio activation callback disconnected")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shared.stop.load(Ordering::SeqCst) {
                    bail!("Recording was stopped during audio initialization");
                }
            }
        }
    };
    Ok(IAudioClient::from_raw(pointer as *mut _))
}
unsafe fn microphone() -> Result<IAudioClient> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let endpoint = enumerator
        .GetDefaultAudioEndpoint(eCapture, eCommunications)
        .context("No default microphone. Select a microphone in Windows sound settings")?;
    Ok(endpoint.Activate(CLSCTX_ALL, None)?)
}
unsafe fn audio_loop(pid: u32, shared: Arc<Shared>, ready: mpsc::Sender<Result<()>>) -> Result<()> {
    CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    let game = game_audio(pid, &shared)?;
    let mic = microphone()?;
    let format = WAVEFORMATEX {
        wFormatTag: 3,
        nChannels: 2,
        nSamplesPerSec: RATE as u32,
        nAvgBytesPerSec: RATE as u32 * 8,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    game.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        2_000_000,
        0,
        &format,
        None,
    )
    .context("Cannot initialize game audio capture")?;
    mic.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        2_000_000,
        0,
        &format,
        None,
    )
    .context(
        "Cannot initialize microphone; allow desktop microphone access in Windows privacy settings",
    )?;
    let clients = [game, mic];
    let capture: [IAudioCaptureClient; 2] = [clients[0].GetService()?, clients[1].GetService()?];
    for client in &clients {
        client.Start()?;
    }
    let _ = ready.send(Ok(()));
    // The helper process runs below normal priority so that it never slows the game, but a
    // starved capture thread would deliver packets too late for the mixer. MMCSS keeps this
    // light polling thread on time; recording continues without it.
    let mut task = 0;
    let _ = AvSetMmThreadCharacteristicsW(wcore::w!("Audio"), &mut task);
    while !shared.stop.load(Ordering::SeqCst) {
        for (source, client) in capture.iter().enumerate() {
            while client.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
                client.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                // AUDCLNT's QPC timestamp is already expressed in 100 ns units.
                let epoch = shared.epoch.load(Ordering::SeqCst);
                if epoch != 0 {
                    let stamp = (qpc as i64 - epoch as i64) * RATE as i64 / TICKS as i64;
                    let silent =
                        flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                    let samples = (!silent).then(|| {
                        std::slice::from_raw_parts(data.cast::<f32>(), frames as usize * 2)
                    });
                    let mut mixer = shared.mixer.lock().unwrap();
                    if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                        mixer.restart(source);
                    }
                    mixer.push(source, stamp, frames as usize, samples);
                }
                client.ReleaseBuffer(frames)?;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    for client in &clients {
        let _ = client.Stop();
    }
    Ok(())
}

// The frame loop copies frames on this device while the encoder's threads use it too, so the
// device is multithread protected.
struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    processor: Option<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor)>,
    copied: ID3D11Query,
}
impl Gpu {
    unsafe fn copy(
        &mut self,
        surface: &IDirect3DSurface,
        width: u32,
        height: u32,
    ) -> wcore::Result<ID3D11Texture2D> {
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        let source: ID3D11Texture2D = access.GetInterface()?;
        let mut src = D3D11_TEXTURE2D_DESC::default();
        source.GetDesc(&mut src);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let mut target = None;
        self.device
            .CreateTexture2D(&desc, None, Some(&mut target))?;
        let target = target.unwrap();
        if src.Width == width && src.Height == height {
            self.context.CopyResource(&target, &source);
        } else {
            let video_device: ID3D11VideoDevice = self.device.cast()?;
            let video_context: ID3D11VideoContext = self.context.cast()?;
            if self.processor.is_none() {
                let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputFrameRate: DXGI_RATIONAL {
                        Numerator: 30,
                        Denominator: 1,
                    },
                    InputWidth: src.Width,
                    InputHeight: src.Height,
                    OutputFrameRate: DXGI_RATIONAL {
                        Numerator: 30,
                        Denominator: 1,
                    },
                    OutputWidth: width,
                    OutputHeight: height,
                    Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                };
                let enumerator = video_device.CreateVideoProcessorEnumerator(&desc)?;
                let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
                let color_context: ID3D11VideoContext1 = video_context.cast()?;
                color_context.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                color_context.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                let source_rect = RECT {
                    left: 0,
                    top: 0,
                    right: src.Width as i32,
                    bottom: src.Height as i32,
                };
                let target_rect = RECT {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                };
                video_context.VideoProcessorSetStreamFrameFormat(
                    &processor,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
                video_context.VideoProcessorSetStreamSourceRect(
                    &processor,
                    0,
                    true,
                    Some(&source_rect),
                );
                video_context.VideoProcessorSetStreamDestRect(
                    &processor,
                    0,
                    true,
                    Some(&target_rect),
                );
                video_context.VideoProcessorSetOutputTargetRect(
                    &processor,
                    true,
                    Some(&target_rect),
                );
                self.processor = Some((enumerator, processor));
            }
            let (enumerator, processor) = self.processor.as_ref().unwrap();
            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            let (mut input, mut output) = (None, None);
            video_device.CreateVideoProcessorInputView(
                &source,
                enumerator,
                &input_desc,
                Some(&mut input),
            )?;
            video_device.CreateVideoProcessorOutputView(
                &target,
                enumerator,
                &output_desc,
                Some(&mut output),
            )?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input),
                ..Default::default()
            };
            let result = video_context.VideoProcessorBlt(
                processor,
                &output.unwrap(),
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
        }
        // The frame is handed to the encoder only once the GPU has written it, and the capture
        // frame the copy reads returns to its pool only after the copy has run.
        self.context.End(&self.copied);
        loop {
            let mut done = wcore::BOOL(0);
            let _ = self.context.GetData(
                &self.copied,
                Some((&mut done as *mut wcore::BOOL).cast()),
                std::mem::size_of::<wcore::BOOL>() as u32,
                0,
            );
            if done.as_bool() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok(target)
    }
}
unsafe fn hardware_encoder() -> Result<()> {
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let (mut activations, mut count) = (std::ptr::null_mut(), 0);
    MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE,
        None,
        Some(&output),
        &mut activations,
        &mut count,
    )?;
    if !activations.is_null() {
        for i in 0..count as usize {
            std::ptr::drop_in_place(activations.add(i));
        }
        CoTaskMemFree(Some(activations.cast()));
    }
    if count == 0 {
        bail!("No hardware H.264 encoder is available. Install the GPU manufacturer's graphics driver.");
    }
    Ok(())
}

unsafe fn media_type(major: &GUID, subtype: &GUID) -> Result<IMFMediaType> {
    let media = MFCreateMediaType()?;
    media.SetGUID(&MF_MT_MAJOR_TYPE, major)?;
    media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
    Ok(media)
}
unsafe fn video_type(subtype: &GUID, width: u32, height: u32) -> Result<IMFMediaType> {
    let media = media_type(&MFMediaType_Video, subtype)?;
    media.SetUINT64(&MF_MT_FRAME_SIZE, (width as u64) << 32 | height as u64)?;
    media.SetUINT64(&MF_MT_FRAME_RATE, 30 << 32 | 1)?;
    media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1 << 32 | 1)?;
    media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    Ok(media)
}
unsafe fn audio_type(subtype: &GUID) -> Result<IMFMediaType> {
    let media = media_type(&MFMediaType_Audio, subtype)?;
    media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE as u32)?;
    media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    Ok(media)
}
/// Media Foundation's sink writer on the capture's own D3D11 device: the color conversion and
/// the hardware H.264 encoder read each frame on the device that wrote it. Encoders running on
/// a device of their own (MediaTranscoder's) turned some complete frames into black ones.
struct Encoder {
    writer: IMFSinkWriter,
    _manager: IMFDXGIDeviceManager,
    video: u32,
    audio: u32,
}
impl Encoder {
    unsafe fn new(
        device: &ID3D11Device,
        path: &std::path::Path,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let (mut token, mut manager) = (0, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.unwrap();
        manager.ResetDevice(device, token)?;
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 4)?;
        let attributes = attributes.unwrap();
        attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
        attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        // Samples arrive in real time; writing one must not wait for the encoder to catch up.
        attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        let file = MFCreateFile(
            MF_ACCESSMODE_WRITE,
            MF_OPENMODE_RESET_IF_EXIST,
            MF_FILEFLAGS_NONE,
            &HSTRING::from(path.as_os_str()),
        )?;
        let writer = MFCreateSinkWriterFromURL(PCWSTR::null(), &file, &attributes)?;
        let output = video_type(&MFVideoFormat_H264, width, height)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, 8_000_000)?;
        let video = writer.AddStream(&output)?;
        let input = video_type(&MFVideoFormat_ARGB32, width, height)?;
        // Without a stride Media Foundation takes RGB frames as bottom-up.
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)?;
        writer
            .SetInputMediaType(video, &input, None)
            .context("The hardware H.264 encoder does not accept the captured frames")?;
        let output = audio_type(&MFAudioFormat_AAC)?;
        output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 160_000 / 8)?;
        let audio = writer.AddStream(&output)?;
        let input = audio_type(&MFAudioFormat_PCM)?;
        input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
        input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE as u32 * 4)?;
        writer.SetInputMediaType(audio, &input, None)?;
        writer.BeginWriting()?;
        Ok(Self {
            writer,
            _manager: manager,
            video,
            audio,
        })
    }
    unsafe fn write(
        &self,
        stream: u32,
        buffer: &IMFMediaBuffer,
        time: u64,
        duration: u64,
    ) -> Result<()> {
        let sample = MFCreateSample()?;
        sample.AddBuffer(buffer)?;
        sample.SetSampleTime(time as i64)?;
        sample.SetSampleDuration(duration as i64)?;
        Ok(self.writer.WriteSample(stream, &sample)?)
    }
    unsafe fn video(&self, frame: &ID3D11Texture2D, time: u64) -> Result<()> {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, frame, 0, false)?;
        buffer.SetCurrentLength(buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?)?;
        self.write(self.video, &buffer, time, TICKS / 30)
    }
    unsafe fn audio(&self, pcm: &[u8], time: u64) -> Result<()> {
        let buffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(pcm.as_ptr(), data, pcm.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(pcm.len() as u32)?;
        self.write(self.audio, &buffer, time, (pcm.len() / 4) as u64 * TICKS / RATE)
    }
}

pub fn record(options: &Options) -> Result<()> {
    unsafe { record_inner(options) }
}
unsafe fn record_inner(options: &Options) -> Result<()> {
    RoInitialize(RO_INIT_MULTITHREADED)?;
    SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS)?;
    if !GraphicsCaptureSession::IsSupported()? {
        bail!("Windows Graphics Capture is unavailable");
    }
    hardware_encoder()?;
    let hwnd = HWND(options.hwnd as *mut _);
    let mut window_pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut window_pid));
    if window_pid != options.pid {
        bail!("Window does not belong to the specified game process");
    }
    let process = OpenProcess(PROCESS_SYNCHRONIZE, false, options.pid)?;
    if options.output.exists() {
        bail!("Output file already exists");
    }
    std::fs::create_dir_all(options.output.parent().context("Output folder missing")?)?;
    let partial = options.output.with_extension("partial.mp4");
    let _file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .context("Cannot create recording output")?;
    drop(_file);
    let mut d3d = None;
    let mut context = None;
    D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut d3d),
        None,
        Some(&mut context),
    )?;
    let d3d = d3d.unwrap();
    let context = context.unwrap();
    let _ = context
        .cast::<ID3D11Multithread>()?
        .SetMultithreadProtected(true);
    let mut copied = None;
    d3d.CreateQuery(
        &D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        },
        Some(&mut copied),
    )?;
    let dxgi: IDXGIDevice = d3d.cast()?;
    let device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
    let mut gpu = Gpu {
        device: d3d,
        context,
        processor: None,
        copied: copied.unwrap(),
    };
    let interop: IGraphicsCaptureItemInterop =
        wcore::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd)?;
    let size = item.Size()?;
    let (width, height) = crate::output_size(size.Width as u32, size.Height as u32);
    if width < 2 || height < 2 {
        bail!("Game window is minimized or has no drawable area");
    }
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )?;
    let session = pool.CreateCaptureSession(&item)?;
    session.SetIsCursorCaptureEnabled(true)?;
    let _ = session.SetMinUpdateInterval(span(TICKS / 30));
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        end: AtomicU64::new(u64::MAX),
        epoch: AtomicU64::new(0),
        mixer: Mutex::new(Mixer::default()),
        error: Mutex::new(None),
        reason: Mutex::new(String::new()),
    });
    let watch_shared = shared.clone();
    let stop_path = options.control.join("stop");
    let process_raw = process.0 as usize;
    let window_raw = hwnd.0 as usize;
    let watcher = thread::spawn(move || {
        while !watch_shared.stop.load(Ordering::SeqCst) {
            if stop_path.exists()
                || WaitForSingleObject(HANDLE(process_raw as *mut _), 0) == WAIT_OBJECT_0
                || !IsWindow(Some(HWND(window_raw as *mut _))).as_bool()
            {
                watch_shared.stop();
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    let latest: Arc<Mutex<Option<Direct3D11CaptureFrame>>> = Arc::new(Mutex::new(None));
    let frame_latest = latest.clone();
    let frame_shared = shared.clone();
    let arrived=pool.FrameArrived(&TypedEventHandler::new(move |sender:wcore::Ref<Direct3D11CaptureFramePool>,_| {
        if frame_shared.stop.load(Ordering::SeqCst) { return Ok(()); }
        match sender.ok()?.TryGetNextFrame() { Ok(frame)=>{ let current=frame.ContentSize()?;
            if current.Width!=size.Width || current.Height!=size.Height {
                *frame_shared.reason.lock().unwrap()="The game window was resized; recording was saved. Start another recording at the new size.".into();
                frame_shared.stop(); let _=frame.Close(); return Ok(());
            }
            if let Some(old)=frame_latest.lock().unwrap().replace(frame) { let _=old.Close(); } },Err(e)=>frame_shared.fail(e) }; Ok(())
    }))?;
    let closed_shared = shared.clone();
    let closed = item.Closed(&TypedEventHandler::new(move |_, _| {
        closed_shared.stop();
        Ok(())
    }))?;
    session.StartCapture()?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let audio_shared = shared.clone();
    let pid = options.pid;
    let audio = thread::spawn(move || {
        if let Err(e) = audio_loop(pid, audio_shared.clone(), ready_tx.clone()) {
            let text = format!("{e:#}");
            let _ = ready_tx.send(Err(anyhow::anyhow!(text.clone())));
            audio_shared.fail(text);
        }
    });
    let audio_ready = ready_rx
        .recv()
        .context("Audio initialization disconnected")?;
    if let Err(e) = audio_ready {
        shared.stop();
        session.Close()?;
        let _ = audio.join();
        return Err(e);
    }
    let encoder = match Encoder::new(&gpu.device, &partial, width, height) {
        Ok(encoder) => encoder,
        Err(e) => {
            shared.stop();
            session.Close()?;
            let _ = audio.join();
            return Err(e);
        }
    };
    shared.epoch.store(ticks(), Ordering::SeqCst);
    options.status("recording", "")?;
    // One loop writes both streams in time order. Audio for a moment is mixed 80 ms after it,
    // when both sources' packets for it have arrived.
    let (mut video_frames, mut audio_frames) = (0u64, 0u64);
    let written = (|| -> Result<()> {
        loop {
            let video_time = video_frames * TICKS / 30;
            let audio_time = audio_frames * TICKS / RATE;
            let video_first = video_time <= audio_time + 800_000;
            shared.wait_until(if video_first {
                video_time
            } else {
                audio_time + 800_000
            });
            if shared.stop.load(Ordering::SeqCst) {
                let end = shared.end.load(Ordering::SeqCst);
                while audio_frames * TICKS / RATE < end {
                    let pcm = shared.mixer.lock().unwrap().take_pcm(audio_frames, 960);
                    encoder.audio(&pcm, audio_frames * TICKS / RATE)?;
                    audio_frames += 960;
                }
                return Ok(());
            }
            if video_first {
                let frame = latest.lock().unwrap().as_ref().map(|f| f.Surface());
                let Some(surface) = frame else {
                    // Nothing captured yet.
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let texture = gpu.copy(&surface?, width, height)?;
                encoder.video(&texture, video_time)?;
                video_frames += 1;
            } else {
                let pcm = shared.mixer.lock().unwrap().take_pcm(audio_frames, 960);
                encoder.audio(&pcm, audio_time)?;
                audio_frames += 960;
            }
        }
    })();
    shared.stop();
    options.status("stopping", "")?;
    let finish = encoder.writer.Finalize();
    session.Close()?;
    pool.RemoveFrameArrived(arrived)?;
    item.RemoveClosed(closed)?;
    pool.Close()?;
    let _ = audio.join();
    let _ = watcher.join();
    // Finalize has closed the file; the writer is released before the file is renamed.
    drop(encoder);
    CloseHandle(process)?;
    if let Some(error) = shared.error.lock().unwrap().take() {
        bail!("{error}");
    }
    written.context("Could not write the recording")?;
    finish.context("Could not finalize the MP4 recording")?;
    std::fs::rename(&partial, &options.output)?;
    options.status("completed", &shared.reason.lock().unwrap())?;
    Ok(())
}
