use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};
use smallvec::SmallVec;
use std::{
    collections::VecDeque,
    ffi::CString,
    os::raw::{c_char, c_double, c_int, c_void},
    ptr,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
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
    fn mpv_render_context_free(context: *mut MpvRenderContext);
}

const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_DOUBLE: c_int = 5;
const FRAME_WIDTH: usize = 960;
const FRAME_HEIGHT: usize = 540;
const RETIRED_FRAME_LIMIT: usize = 6;

enum PlayerCommand {
    Load(String),
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
    statuses: Receiver<MpvStatus>,
    current_frame: Option<std::sync::Arc<RenderImage>>,
    retired_frames: VecDeque<std::sync::Arc<RenderImage>>,
    current_status: MpvStatus,
    worker: Option<JoinHandle<()>>,
}

impl MpvPlayer {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(2);
        let (status_tx, status_rx) = mpsc::sync_channel(2);
        let worker = thread::Builder::new()
            .name("biliguga-libmpv".into())
            .spawn(move || run_mpv(command_rx, frame_tx, status_tx))
            .ok();
        Self {
            commands: command_tx,
            frames: frame_rx,
            statuses: status_rx,
            current_frame: None,
            retired_frames: VecDeque::new(),
            current_status: MpvStatus::default(),
            worker,
        }
    }

    pub fn load(&self, url: String) {
        let _ = self.commands.send(PlayerCommand::Load(url));
    }

    pub fn stop_playback(&self) {
        let _ = self.commands.send(PlayerCommand::StopPlayback);
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
                    latest_packet = Some(packet);
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
                        .replace(std::sync::Arc::new(RenderImage::new(SmallVec::from_elem(
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
    status_tx: SyncSender<MpvStatus>,
) {
    unsafe {
        let handle = mpv_create();
        if handle.is_null() {
            eprintln!("libmpv: mpv_create failed");
            return;
        }

        set_option(handle, "vo", "libmpv");
        set_option(handle, "hwdec", "no");
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
            return;
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
            return;
        }

        let mut buffer = vec![0_u8; FRAME_WIDTH * FRAME_HEIGHT * 4];
        let format = CString::new("rgb0").unwrap();
        let size = [FRAME_WIDTH as c_int, FRAME_HEIGHT as c_int];
        let stride = FRAME_WIDTH * 4;
        let mut running = true;
        let mut last_status = Instant::now();

        while running {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    PlayerCommand::Load(url) => {
                        let command = CString::new(format!("loadfile {} replace", quote(&url)))
                            .unwrap_or_else(|_| CString::new("stop").unwrap());
                        let _ = mpv_command_string(handle, command.as_ptr());
                    }
                    PlayerCommand::StopPlayback => {
                        let command = CString::new("stop").unwrap();
                        let _ = mpv_command_string(handle, command.as_ptr());
                    }
                    PlayerCommand::SetPause(paused) => {
                        let command = CString::new(format!(
                            "set pause {}",
                            if paused { "yes" } else { "no" }
                        ))
                        .unwrap();
                        let _ = mpv_command_string(handle, command.as_ptr());
                    }
                    PlayerCommand::SetVolume(volume) => {
                        let command =
                            CString::new(format!("set volume {:.1}", volume.clamp(0., 100.)))
                                .unwrap();
                        let _ = mpv_command_string(handle, command.as_ptr());
                    }
                    PlayerCommand::SetSpeed(speed) => {
                        let command =
                            CString::new(format!("set speed {:.2}", speed.max(0.1))).unwrap();
                        let _ = mpv_command_string(handle, command.as_ptr());
                    }
                    PlayerCommand::SeekPercent(percent) => {
                        let command = CString::new(format!(
                            "seek {:.3} absolute-percent",
                            percent.clamp(0., 1.) * 100.
                        ))
                        .unwrap();
                        let _ = mpv_command_string(handle, command.as_ptr());
                    }
                    PlayerCommand::Stop => {
                        running = false;
                        break;
                    }
                }
            }
            if !running {
                break;
            }

            let _ = mpv_wait_event(handle, 0.0);
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
                let mut pixels = vec![0_u8; buffer.len()];
                for (source, target) in buffer.chunks_exact(4).zip(pixels.chunks_exact_mut(4)) {
                    target[0] = source[2];
                    target[1] = source[1];
                    target[2] = source[0];
                    target[3] = 255;
                }
                match frame_tx.try_send(FramePacket { pixels }) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => break,
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
                match status_tx.try_send(status) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => break,
                }
                last_status = Instant::now();
            }
            thread::sleep(Duration::from_millis(16));
        }

        mpv_render_context_free(context);
        mpv_terminate_destroy(handle);
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
