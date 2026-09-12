//! Native Windows shared-render-endpoint WASAPI loopback. Never opens eCapture.
//! COM, event handle, client and every GetBuffer/ReleaseBuffer pair stay on MTA.
use super::windows_audio_model::{self as model, Control, Format, Packet, PacketLease, Provider};
use crate::{Error, Result};
use std::{ffi::c_void, ptr, time::Duration};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::Audio::*,
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::Interface,
};

fn win<T>(value: windows::core::Result<T>, step: &str) -> Result<T> {
    value.map_err(|error| {
        Error::action(format!(
            "WASAPI {step} failed (HRESULT 0x{:08x}): {error}",
            error.code().0 as u32
        ))
    })
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
struct Mix(*mut WAVEFORMATEX);
impl Drop for Mix {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast::<c_void>()));
        }
    }
}
impl Mix {
    fn format(&self) -> Result<Format> {
        if self.0.is_null() {
            return Err(Error::action("WASAPI GetMixFormat returned null"));
        }
        let header = unsafe { ptr::read_unaligned(self.0) };
        let size = 18usize + header.cbSize as usize;
        if size > 4096 {
            return Err(Error::action("WASAPI mix format extension exceeds bound"));
        }
        model::parse_wave_format(unsafe { std::slice::from_raw_parts(self.0.cast::<u8>(), size) })
    }
}

struct Loopback {
    // Declaration order is the COM teardown order. In particular capture releases
    // before client, event stays alive through client destruction, apartment last.
    capture: IAudioCaptureClient,
    client: IAudioClient,
    _device: IMMDevice,
    _enumerator: IMMDeviceEnumerator,
    event: Event,
    _apartment: Apartment,
    format: Format,
    capacity: u32,
    running: bool,
}
impl Loopback {
    fn open(control: &Control) -> Result<Self> {
        control.check_start()?;
        win(
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok(),
            "CoInitializeEx(MTA)",
        )?;
        let apartment = Apartment;
        let event = Event(win(
            unsafe { CreateEventW(None, false, false, None) },
            "CreateEvent",
        )?);
        control.check_start()?;
        let enumerator: IMMDeviceEnumerator = win(
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) },
            "MMDeviceEnumerator",
        )?;
        control.check_start()?;
        let device = win(
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) },
            "default render endpoint",
        )?;
        if win(unsafe { device.GetState() }, "render endpoint state")? != DEVICE_STATE_ACTIVE {
            return Err(Error::action("WASAPI render endpoint is not active"));
        }
        control.check_start()?;
        let client: IAudioClient = win(
            unsafe { device.Activate(CLSCTX_ALL, None) },
            "activate render AudioClient",
        )?;
        // Excludes legacy Windows 8 first-use/MTA ambiguity. Windows 10's newer
        // interface is required; bounded polling also handles older event timing.
        let _: IAudioClient3 = win(client.cast(), "Windows 10 AudioClient3 support")?;
        control.check_start()?;
        let mix = Mix(win(unsafe { client.GetMixFormat() }, "GetMixFormat")?);
        let format = mix.format()?;
        control.check_start()?;
        win(
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_NOPERSIST,
                    1_000_000,
                    0,
                    mix.0,
                    None,
                )
            },
            "Initialize shared render loopback",
        )?;
        // Set the event immediately after Initialize so even later startup
        // failures tear down an initialized event-driven client correctly.
        win(unsafe { client.SetEventHandle(event.0) }, "SetEventHandle")?;
        control.check_start()?;
        let capacity = win(unsafe { client.GetBufferSize() }, "GetBufferSize")?;
        if capacity == 0 {
            return Err(Error::action("WASAPI endpoint buffer is empty"));
        }
        format.packet_bytes(capacity)?;
        let capture = win(
            unsafe { client.GetService() },
            "GetService(IAudioCaptureClient)",
        )?;
        drop(mix);
        Ok(Self {
            capture,
            client,
            _device: device,
            _enumerator: enumerator,
            event,
            _apartment: apartment,
            format,
            capacity,
            running: false,
        })
    }
}
impl Provider for Loopback {
    fn format(&self) -> Format {
        self.format
    }
    fn start(&mut self, control: &Control) -> Result<()> {
        control.check_start()?;
        win(unsafe { self.client.Start() }, "Start")?;
        self.running = true;
        // A Start that returned after the deadline is stopped on this same MTA.
        control.check_start()
    }
    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let queued = win(
            unsafe { self.capture.GetNextPacketSize() },
            "GetNextPacketSize",
        )?;
        if queued == 0 {
            return Ok(None);
        }
        if queued > self.capacity {
            return Err(Error::action(
                "WASAPI next packet exceeds endpoint capacity",
            ));
        }
        let mut data = ptr::null_mut();
        let mut frames = 0;
        let mut flags = 0;
        let mut position = 0;
        // The typed wrapper erases success HRESULTs. Keep BUFFER_EMPTY distinct
        // and never release an unacquired/failed buffer.
        let status = unsafe {
            (self.capture.vtable().GetBuffer)(
                self.capture.as_raw(),
                &mut data,
                &mut frames,
                &mut flags,
                &mut position,
                ptr::null_mut(),
            )
        };
        win(status.ok(), "GetBuffer")?;
        if frames == 0 {
            return if status == AUDCLNT_S_BUFFER_EMPTY {
                Ok(None)
            } else {
                Err(Error::action("WASAPI GetBuffer success without a packet"))
            };
        }
        let lease = PacketLease::new(frames, |frames| {
            win(
                unsafe { self.capture.ReleaseBuffer(frames) },
                "ReleaseBuffer",
            )
        });
        let result = (|| {
            if status.0 != 0 || frames > self.capacity || frames != queued {
                return Err(Error::action(
                    "WASAPI packet status/frame count changed unexpectedly",
                ));
            }
            let bytes = self.format.packet_bytes(frames)?;
            if flags & !(model::SILENT | model::DISCONTINUITY | model::TIMESTAMP_ERROR) != 0 {
                return Err(Error::action("Unknown WASAPI packet flags"));
            }
            let mut owned = Vec::new();
            if flags & model::SILENT == 0 {
                if data.is_null() {
                    return Err(Error::action("WASAPI nonsilent packet data is null"));
                }
                owned
                    .try_reserve_exact(bytes)
                    .map_err(|_| Error::action("WASAPI packet allocation failed"))?;
                owned.extend_from_slice(unsafe { std::slice::from_raw_parts(data, bytes) });
            }
            Ok(Some(Packet {
                data: owned,
                frames,
                flags,
                position,
            }))
        })();
        lease.finish(result)
    }
    fn wait(&mut self, timeout: Duration) -> Result<()> {
        let millis = timeout.as_millis().min(10) as u32;
        match unsafe { WaitForSingleObject(self.event.0, millis) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            _ => Err(Error::action(format!(
                "WASAPI event wait failed: {}",
                windows::core::Error::from_thread()
            ))),
        }
    }
    fn stop(&mut self) -> Result<()> {
        if self.running {
            win(unsafe { self.client.Stop() }, "Stop")?;
            self.running = false;
        }
        Ok(())
    }
}
impl Drop for Loopback {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn recorder() -> model::Audio {
    model::Audio::new(|control| Ok(Box::new(Loopback::open(control)?)))
}
