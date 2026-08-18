use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};
use smallvec::SmallVec;
use std::{
    collections::VecDeque,
    ffi::{CStr, CString},
    os::raw::{c_char, c_double, c_int, c_void},
    ptr,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[repr(C)]
struct MpvHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvRenderContext {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvRenderParam {
    type_: c_int,
    data: *mut c_void,
}

#[link(name = "mpv")]
unsafe extern "C" {
    fn mpv_create() -> *mut MpvHandle;
    fn mpv_set_option_string(
        handle: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_initialize(handle: *mut MpvHandle) -> c_int;
    fn mpv_terminate_destroy(handle: *mut MpvHandle);
    fn mpv_command_string(handle: *mut MpvHandle, command: *const c_char) -> c_int;
    fn mpv_get_property(
        handle: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mpv_free(data: *mut c_void);
    fn mpv_wait_event(handle: *mut MpvHandle, timeout: c_double) -> *mut c_void;
    fn mpv_render_context_create(
        context: *mut *mut MpvRenderContext,
        handle: *mut MpvHandle,
        params: *const MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_render(
        context: *mut MpvRenderContext,
        params: *const MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_update(context: *mut MpvRenderContext) -> u64;
    fn mpv_render_context_free(context: *mut MpvRenderContext);
}

const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_DOUBLE: c_int = 5;
const MPV_FORMAT_STRING: c_int = 1;
const MPV_RENDER_UPDATE_FRAME: u64 = 1 << 0;
const FRAME_WIDTH: usize = 960;
const FRAME_HEIGHT: usize = 540;
const RETIRED_FRAME_LIMIT: usize = 3;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

enum PlayerCommand {
    Load {
        url: String,
        volume: f64,
        speed: f64,
    },
    StopPlayback,
    SetPause(bool),
    SetVolume(f64),
    SetSpeed(f64),
    SeekPercent(f64),
    Stop,
}

#[derive(Clone, Copy, Debug)]
pub struct MpvStatus {
    pub time_pos: f64,
    pub duration: f64,
    pub paused: bool,
    pub volume: f64,
    pub speed: f64,
}

impl Default for MpvStatus {
    fn default() -> Self {
        Self {
            time_pos: f64::NAN,
            duration: f64::NAN,
            paused: true,
            volume: 100.,
            speed: 1.,
        }
    }
}

struct FramePacket {
    pixels: Vec<u8>,
}

pub struct MpvPlayer {
    commands: mpsc::Sender<PlayerCommand>,
    frames: Receiver<FramePacket>,
    recycled_frames: mpsc::Sender<Vec<u8>>,
    statuses: Receiver<MpvStatus>,
    current_frame: Option<Arc<RenderImage>>,
    retired_frames: VecDeque<Arc<RenderImage>>,
    current_status: MpvStatus,
    worker: Option<JoinHandle<()>>,
}

impl MpvPlayer {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(2);
        let (recycled_tx, recycled_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::sync_channel(2);
        let worker = thread::Builder::new()
            .name("biliguga-libmpv".into())
            .spawn(move || run_mpv(command_rx, frame_tx, recycled_rx, status_tx))
            .ok();
        Self {
            commands: command_tx,
            frames: frame_rx,
            recycled_frames: recycled_tx,
            statuses: status_rx,
            current_frame: None,
            retired_frames: VecDeque::new(),
            current_status: MpvStatus::default(),
            worker,
        }
    }

    pub fn load(&self, url: String, volume: f64, speed: f64) {
        let _ = self
            .commands
            .send(PlayerCommand::Load { url, volume, speed });
    }

    pub fn stop_playback(&mut self) -> Vec<Arc<RenderImage>> {
        let _ = self.commands.send(PlayerCommand::StopPlayback);
        self.discard_pending_frames();
        self.current_status = MpvStatus::default();
        self.take_all_frames()
    }

    pub fn set_pause(&self, paused: bool) {
        let _ = self.commands.send(PlayerCommand::SetPause(paused));
    }

    pub fn set_volume(&self, volume: f64) {
        let _ = self.commands.send(PlayerCommand::SetVolume(volume));
    }

    pub fn set_speed(&self, speed: f64) {
        let _ = self.commands.send(PlayerCommand::SetSpeed(speed));
    }

    pub fn seek_percent(&self, percent: f64) {
        let _ = self.commands.send(PlayerCommand::SeekPercent(percent));
    }

    pub fn poll_frame(&mut self) {
        let mut latest_packet = None;
        loop {
            match self.frames.try_recv() {
                Ok(packet) => {
                    if let Some(previous) = latest_packet.replace(packet) {
                        let _ = self.recycled_frames.send(previous.pixels);
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if let Some(packet) = latest_packet {
            let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(
                FRAME_WIDTH as u32,
                FRAME_HEIGHT as u32,
                packet.pixels,
            );
            if let Some(buffer) = buffer {
                if let Some(previous) =
                    self.current_frame
                        .replace(Arc::new(RenderImage::new(SmallVec::from_elem(
                            Frame::new(buffer),
                            1,
                        ))))
                {
                    self.retired_frames.push_back(previous);
                }
            }
        }
        while let Ok(status) = self.statuses.try_recv() {
            self.current_status = status;
        }
    }

    pub fn take_expired_frames(&mut self) -> Vec<std::sync::Arc<RenderImage>> {
        let expired = self
            .retired_frames
            .len()
            .saturating_sub(RETIRED_FRAME_LIMIT);
        (0..expired)
            .filter_map(|_| self.retired_frames.pop_front())
            .collect()
    }

    pub fn frame(&self) -> Option<std::sync::Arc<RenderImage>> {
        self.current_frame.clone()
    }

    pub fn discard_pending_frames(&mut self) {
        while let Ok(frame) = self.frames.try_recv() {
            let _ = self.recycled_frames.send(frame.pixels);
        }
    }

    pub fn take_all_frames(&mut self) -> Vec<Arc<RenderImage>> {
        let mut frames = Vec::with_capacity(self.retired_frames.len() + 1);
        if let Some(frame) = self.current_frame.take() {
            frames.push(frame);
        }
        frames.extend(self.retired_frames.drain(..));
        frames
    }

    pub fn debug_frame_counts(&self) -> (bool, usize) {
        (self.current_frame.is_some(), self.retired_frames.len())
    }

    pub fn recycle_frame(&self, frame: Arc<RenderImage>) {
        if let Ok(frame) = Arc::try_unwrap(frame) {
            if let Some(pixels) = frame.into_single_frame_bytes() {
                let _ = self.recycled_frames.send(pixels);
            }
        }
    }

    pub fn status(&self) -> MpvStatus {
        self.current_status
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.commands.send(PlayerCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_mpv(
    command_rx: Receiver<PlayerCommand>,
    frame_tx: SyncSender<FramePacket>,
    recycled_rx: Receiver<Vec<u8>>,
    status_tx: SyncSender<MpvStatus>,
) {
    let mut next_load = None;
    loop {
        let command = match next_load.take() {
            Some(command) => command,
            None => match command_rx.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            PlayerCommand::Load { url, volume, speed } => {
                match run_mpv_session(
                    url,
                    volume,
                    speed,
                    &command_rx,
                    &frame_tx,
                    &recycled_rx,
                    &status_tx,
                ) {
                    SessionExit::Idle => {}
                    SessionExit::Reload(command) => next_load = Some(command),
                    SessionExit::Stop => return,
                }
            }
            PlayerCommand::Stop => return,
            PlayerCommand::StopPlayback
            | PlayerCommand::SetPause(_)
            | PlayerCommand::SetVolume(_)
            | PlayerCommand::SetSpeed(_)
            | PlayerCommand::SeekPercent(_) => {}
        }
    }
}

enum SessionExit {
    Idle,
    Reload(PlayerCommand),
    Stop,
}

fn run_mpv_session(
    url: String,
    volume: f64,
    speed: f64,
    command_rx: &Receiver<PlayerCommand>,
    frame_tx: &SyncSender<FramePacket>,
    recycled_rx: &Receiver<Vec<u8>>,
    status_tx: &SyncSender<MpvStatus>,
) -> SessionExit {
    unsafe {
        let handle = mpv_create();
        if handle.is_null() {
            eprintln!("libmpv: mpv_create failed");
            return SessionExit::Idle;
        }

        set_option(handle, "vo", "libmpv");
        set_option(handle, "hwdec", "auto-safe");
        set_option(handle, "vd-lavc-threads", "4");
        set_option(handle, "cache", "yes");
        set_option(handle, "cache-secs", "3");
        set_option(handle, "demuxer-max-bytes", "16MiB");
        set_option(handle, "demuxer-max-back-bytes", "4MiB");
        set_option(handle, "osd-level", "0");
        set_option(handle, "user-agent", "Mozilla/5.0");
        set_option(
            handle,
            "http-header-fields",
            "Referer: https://www.bilibili.com/",
        );

        if mpv_initialize(handle) < 0 {
            eprintln!("libmpv: mpv_initialize failed");
            mpv_terminate_destroy(handle);
            return SessionExit::Idle;
        }

        let api_type = CString::new("sw").unwrap();
        let create_params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: api_type.as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: 0,
                data: ptr::null_mut(),
            },
        ];
        let mut context = ptr::null_mut();
        if mpv_render_context_create(&mut context, handle, create_params.as_ptr()) < 0
            || context.is_null()
        {
            eprintln!("libmpv: software render context creation failed");
            mpv_terminate_destroy(handle);
            return SessionExit::Idle;
        }

        let load = CString::new(format!("loadfile {} replace", quote(&url)))
            .unwrap_or_else(|_| CString::new("stop").unwrap());
        let _ = mpv_command_string(handle, load.as_ptr());
        run_command(handle, &PlayerCommand::SetVolume(volume));
        run_command(handle, &PlayerCommand::SetSpeed(speed));

        let mut buffer = vec![0_u8; FRAME_WIDTH * FRAME_HEIGHT * 4];
        let format = CString::new("rgb0").unwrap();
        let size = [FRAME_WIDTH as c_int, FRAME_HEIGHT as c_int];
        let stride = FRAME_WIDTH * 4;
        let mut render_enabled = true;
        let mut last_status = Instant::now();
        let mut logged_hwdec = false;
        let exit = 'playback: loop {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    command @ PlayerCommand::Load { .. } => {
                        break 'playback SessionExit::Reload(command);
                    }
                    PlayerCommand::StopPlayback => break 'playback SessionExit::Idle,
                    PlayerCommand::Stop => break 'playback SessionExit::Stop,
                    PlayerCommand::SetPause(paused) => {
                        render_enabled = !paused;
                        run_command(handle, &PlayerCommand::SetPause(paused));
                    }
                    command @ (PlayerCommand::SetVolume(_)
                    | PlayerCommand::SetSpeed(_)
                    | PlayerCommand::SeekPercent(_)) => run_command(handle, &command),
                }
            }

            let _ = mpv_wait_event(handle, 0.0);
            let update = mpv_render_context_update(context);
            if render_enabled && update & MPV_RENDER_UPDATE_FRAME != 0 {
                let mut params = [
                    MpvRenderParam {
                        type_: MPV_RENDER_PARAM_SW_SIZE,
                        data: size.as_ptr() as *mut c_void,
                    },
                    MpvRenderParam {
                        type_: MPV_RENDER_PARAM_SW_FORMAT,
                        data: format.as_ptr() as *mut c_void,
                    },
                    MpvRenderParam {
                        type_: MPV_RENDER_PARAM_SW_STRIDE,
                        data: &stride as *const usize as *mut c_void,
                    },
                    MpvRenderParam {
                        type_: MPV_RENDER_PARAM_SW_POINTER,
                        data: buffer.as_mut_ptr() as *mut c_void,
                    },
                    MpvRenderParam {
                        type_: 0,
                        data: ptr::null_mut(),
                    },
                ];
                if mpv_render_context_render(context, params.as_mut_ptr()) >= 0 {
                    if !logged_hwdec {
                        let hwdec = get_string_property(handle, "hwdec-current")
                            .unwrap_or_else(|| "<unavailable>".into());
                        if std::env::var_os("BILIGUGA_MEM_DEBUG").is_some()
                            || std::env::var_os("BILIGUGA_MPV_DEBUG").is_some()
                        {
                            eprintln!(
                                "[biliguga-mpv] hwdec-current={:?} render-path=software-buffer",
                                hwdec
                            );
                        }
                        logged_hwdec = true;
                    }
                    let mut pixels = recycled_rx
                        .try_recv()
                        .ok()
                        .filter(|pixels| pixels.len() == buffer.len())
                        .unwrap_or_else(|| vec![0_u8; buffer.len()]);
                    for (source, target) in buffer.chunks_exact(4).zip(pixels.chunks_exact_mut(4)) {
                        target[0] = source[2];
                        target[1] = source[1];
                        target[2] = source[0];
                        target[3] = 255;
                    }
                    match frame_tx.try_send(FramePacket { pixels }) {
                        Ok(()) | Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => {
                            break 'playback SessionExit::Stop;
                        }
                    }
                }
            }
            if last_status.elapsed() >= Duration::from_millis(100) {
                let status = MpvStatus {
                    time_pos: get_double_property(handle, "time-pos").unwrap_or(f64::NAN),
                    duration: get_double_property(handle, "duration").unwrap_or(f64::NAN),
                    paused: get_flag_property(handle, "pause").unwrap_or(true),
                    volume: get_double_property(handle, "volume").unwrap_or(100.),
                    speed: get_double_property(handle, "speed").unwrap_or(1.),
                };
                if status.paused {
                    render_enabled = false;
                }
                match status_tx.try_send(status) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => {
                        break 'playback SessionExit::Stop;
                    }
                }
                last_status = Instant::now();
            }
            thread::sleep(FRAME_INTERVAL);
        };

        mpv_render_context_free(context);
        mpv_terminate_destroy(handle);
        crate::allocator::trim();
        exit
    }
}

unsafe fn run_command(handle: *mut MpvHandle, command: &PlayerCommand) {
    let text = match command {
        PlayerCommand::SetPause(paused) => {
            format!("set pause {}", if *paused { "yes" } else { "no" })
        }
        PlayerCommand::SetVolume(volume) => {
            format!("set volume {:.1}", volume.clamp(0., 100.))
        }
        PlayerCommand::SetSpeed(speed) => format!("set speed {:.2}", speed.max(0.1)),
        PlayerCommand::SeekPercent(percent) => {
            format!("seek {:.3} absolute-percent", percent.clamp(0., 1.) * 100.)
        }
        PlayerCommand::Load { .. } | PlayerCommand::StopPlayback | PlayerCommand::Stop => return,
    };
    if let Ok(command) = CString::new(text) {
        unsafe {
            let _ = mpv_command_string(handle, command.as_ptr());
        }
    }
}

unsafe fn get_double_property(handle: *mut MpvHandle, name: &str) -> Option<f64> {
    let name = CString::new(name).ok()?;
    let mut value = 0.;
    let result = unsafe {
        mpv_get_property(
            handle,
            name.as_ptr(),
            MPV_FORMAT_DOUBLE,
            &mut value as *mut f64 as *mut c_void,
        )
    };
    (result >= 0).then_some(value)
}

unsafe fn get_flag_property(handle: *mut MpvHandle, name: &str) -> Option<bool> {
    let name = CString::new(name).ok()?;
    let mut value: c_int = 0;
    let result = unsafe {
        mpv_get_property(
            handle,
            name.as_ptr(),
            MPV_FORMAT_FLAG,
            &mut value as *mut c_int as *mut c_void,
        )
    };
    (result >= 0).then_some(value != 0)
}

unsafe fn get_string_property(handle: *mut MpvHandle, name: &str) -> Option<String> {
    let name = CString::new(name).ok()?;
    let mut value = ptr::null_mut::<c_char>();
    let result = unsafe {
        mpv_get_property(
            handle,
            name.as_ptr(),
            MPV_FORMAT_STRING,
            &mut value as *mut *mut c_char as *mut c_void,
        )
    };
    if result < 0 || value.is_null() {
        return None;
    }
    let result = unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() };
    unsafe {
        mpv_free(value.cast());
    }
    Some(result)
}

unsafe fn set_option(handle: *mut MpvHandle, name: &str, value: &str) {
    let name = CString::new(name).unwrap();
    let value = CString::new(value).unwrap();
    unsafe {
        let _ = mpv_set_option_string(handle, name.as_ptr(), value.as_ptr());
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('\"', "\\\""))
}
