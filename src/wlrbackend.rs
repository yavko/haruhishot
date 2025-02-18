use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_display::WlDisplay;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{protocol::wl_registry, QueueHandle};
use wayland_client::{Connection, Dispatch, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use std::error::Error;
use std::os::fd::FromRawFd;
use std::{
    ffi::CStr,
    fs::File,
    os::unix::prelude::RawFd,
    time::{SystemTime, UNIX_EPOCH},
};

use nix::{
    fcntl,
    sys::{memfd, mman, stat},
    unistd,
};

use memmap2::MmapMut;

#[derive(Debug)]
enum ScreenCopyState {
    Staging,
    Finished,
    Failed,
}
/// capture_output_frame.
fn create_shm_fd() -> std::io::Result<RawFd> {
    // Only try memfd on linux and freebsd.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    loop {
        // Create a file that closes on succesful execution and seal it's operations.
        match memfd::memfd_create(
            CStr::from_bytes_with_nul(b"wayshot\0").unwrap(),
            memfd::MemFdCreateFlag::MFD_CLOEXEC | memfd::MemFdCreateFlag::MFD_ALLOW_SEALING,
        ) {
            Ok(fd) => {
                // This is only an optimization, so ignore errors.
                // F_SEAL_SRHINK = File cannot be reduced in size.
                // F_SEAL_SEAL = Prevent further calls to fcntl().
                let _ = fcntl::fcntl(
                    fd,
                    fcntl::F_ADD_SEALS(
                        fcntl::SealFlag::F_SEAL_SHRINK | fcntl::SealFlag::F_SEAL_SEAL,
                    ),
                );
                return Ok(fd);
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ENOSYS) => break,
            Err(errno) => return Err(std::io::Error::from(errno)),
        }
    }

    // Fallback to using shm_open.
    let sys_time = SystemTime::now();
    let mut mem_file_handle = format!(
        "/wayshot-{}",
        sys_time.duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
    );
    loop {
        match mman::shm_open(
            // O_CREAT = Create file if does not exist.
            // O_EXCL = Error if create and file exists.
            // O_RDWR = Open for reading and writing.
            // O_CLOEXEC = Close on succesful execution.
            // S_IRUSR = Set user read permission bit .
            // S_IWUSR = Set user write permission bit.
            mem_file_handle.as_str(),
            fcntl::OFlag::O_CREAT
                | fcntl::OFlag::O_EXCL
                | fcntl::OFlag::O_RDWR
                | fcntl::OFlag::O_CLOEXEC,
            stat::Mode::S_IRUSR | stat::Mode::S_IWUSR,
        ) {
            Ok(fd) => match mman::shm_unlink(mem_file_handle.as_str()) {
                Ok(_) => return Ok(fd),
                Err(errno) => match unistd::close(fd) {
                    Ok(_) => return Err(std::io::Error::from(errno)),
                    Err(errno) => return Err(std::io::Error::from(errno)),
                },
            },
            Err(nix::errno::Errno::EEXIST) => {
                // If a file with that handle exists then change the handle
                mem_file_handle = format!(
                    "/wayshot-{}",
                    sys_time.duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
                );
                continue;
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(errno) => return Err(std::io::Error::from(errno)),
        }
    }
}

#[derive(Debug)]
pub struct BufferData {
    //pub buffer: Option<WlBuffer>,
    pub width: u32,
    pub height: u32,
    pub realwidth: i32,
    pub realheight: i32,
    //pub stride: u32,
    shm: WlShm,
    pub frame_mmap: Option<MmapMut>,
    state: ScreenCopyState,
}

impl BufferData {
    fn new(shm: WlShm, (realwidth, realheight): (i32, i32)) -> Self {
        BufferData {
            //buffer: None,
            width: 0,
            height: 0,
            realheight,
            realwidth,
            // stride: 0,
            shm,
            frame_mmap: None,
            state: ScreenCopyState::Staging,
        }
    }
    #[inline]
    fn finished(&self) -> bool {
        matches!(
            self.state,
            ScreenCopyState::Failed | ScreenCopyState::Finished
        )
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for BufferData {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for BufferData {
    fn event(
        _state: &mut Self,
        _proxy: &WlBuffer,
        _event: <WlBuffer as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for BufferData {
    fn event(
        _state: &mut Self,
        _proxy: &WlShmPool,
        _event: <WlShmPool as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for BufferData {
    fn event(
        state: &mut Self,
        proxy: &ZwlrScreencopyFrameV1,
        event: <ZwlrScreencopyFrameV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Ready {
                ..
                //tv_sec_hi,
                //tv_sec_lo,
                //tv_nsec,
            } => {
                state.state = ScreenCopyState::Finished;
                tracing::info!("Receive Ready event");
            }
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let format = match format {
                    WEnum::Value(value) => {
                        value
                    },
                    WEnum::Unknown(e) => {
                        tracing::error!("Unknown format :{}",e);
                        state.state = ScreenCopyState::Failed;
                        return;
                    }
                };
                tracing::info!("Format is {:?}", format);
                state.width = width;
                state.height = height;
                //state.stride = stride;
                let frame_bytes = stride * height;
                let mut state_result = || {
                    let mem_fd = create_shm_fd()?;
                    let mem_file = unsafe {
                        File::from_raw_fd(mem_fd)
                    };
                    mem_file.set_len(frame_bytes as u64)?;

                    let shm_pool = state.shm.create_pool(mem_fd, frame_bytes as i32, qh, ());
                    let buffer =
                        shm_pool.create_buffer(0, width as i32, height as i32, stride as i32, format, qh, ());
                    proxy.copy(&buffer);

                    // TODO:maybe need some adjust
                    state.frame_mmap = Some(unsafe {
                        MmapMut::map_mut(&mem_file)?
                    });
                    Ok::<(), Box<dyn Error>>(())
                };
                if let Err(e) = state_result() {
                    tracing::error!("Something error: {e}");
                    state.state = ScreenCopyState::Failed;
                }
                // buffer done
            }
            zwlr_screencopy_frame_v1::Event::LinuxDmabuf {
                ..
            } => {
                tracing::info!("Receive LinuxDamBuf event");
            }
            zwlr_screencopy_frame_v1::Event::Damage {
                ..
            } => {
                tracing::info!("Receive Damage event");
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {
                tracing::info!("Receive BufferDone event");
            }
            zwlr_screencopy_frame_v1::Event::Flags { .. } => {
                tracing::info!("Receive Flags event");
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                tracing::info!("Receive failed event");
                state.state = ScreenCopyState::Failed;
            }
            _ => unreachable!()
        }
    }
}

pub fn capture_output_frame(
    connection: &Connection,
    output: &WlOutput,
    manager: &ZwlrScreencopyManagerV1,
    display: &WlDisplay,
    shm: wl_shm::WlShm,
    (realwidth, realheight): (i32, i32),
    slurpoption: Option<(i32, i32, i32, i32)>,
) -> Option<BufferData> {
    let mut event_queue = connection.new_event_queue();
    let qh = event_queue.handle();
    display.get_registry(&qh, ());
    let mut framesate = BufferData::new(shm, (realwidth, realheight));
    match slurpoption {
        None => {
            manager.capture_output(0, output, &qh, ());
        }
        Some((x, y, width, height)) => {
            manager.capture_output_region(0, output, x, y, width, height, &qh, ());
        }
    }
    //event_queue.roundtrip(&mut framesate).unwrap();
    loop {
        event_queue.blocking_dispatch(&mut framesate).unwrap();
        if framesate.finished() {
            break;
        }
    }
    match framesate.state {
        ScreenCopyState::Finished => Some(framesate),
        ScreenCopyState::Failed => {
            tracing::error!("Cannot take screen copy");
            None
        }
        _ => unreachable!(),
    }
}
