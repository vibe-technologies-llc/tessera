use std::{
    iter, ptr,
    sync::Mutex,
    time::{Duration, Instant},
};

use ffmpeg_next::{
    Codec, codec,
    ffi::{
        AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX, AVBufferRef, AVHWDeviceType, av_buffer_ref,
        av_hwdevice_ctx_create, av_hwdevice_iterate_types, av_hwframe_transfer_data,
        avcodec_get_hw_config,
    },
    frame,
};

pub const PREFERRED_HW_ACCELS: &[HwAccel] = &[HwAccel::Vaapi, HwAccel::Vulkan];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HwAccel {
    Vaapi,
    Vulkan,
    Cuda,
    Qsv,
    Drm,
}

impl HwAccel {
    fn from_device_type(device_type: AVHWDeviceType) -> Option<Self> {
        match device_type {
            AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => Some(Self::Vaapi),
            AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN => Some(Self::Vulkan),
            AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => Some(Self::Cuda),
            AVHWDeviceType::AV_HWDEVICE_TYPE_QSV => Some(Self::Qsv),
            AVHWDeviceType::AV_HWDEVICE_TYPE_DRM => Some(Self::Drm),
            _ => None,
        }
    }

    fn device_type(self) -> AVHWDeviceType {
        match self {
            Self::Vaapi => AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            Self::Vulkan => AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
            Self::Cuda => AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            Self::Qsv => AVHWDeviceType::AV_HWDEVICE_TYPE_QSV,
            Self::Drm => AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
        }
    }
}

pub fn available_hw_accels() -> Vec<HwAccel> {
    let none = AVHWDeviceType::AV_HWDEVICE_TYPE_NONE;
    iter::successors(Some(none), |&previous| {
        let next = unsafe { av_hwdevice_iterate_types(previous) };
        (next != none).then_some(next)
    })
    .skip(1)
    .filter_map(HwAccel::from_device_type)
    .collect()
}

const DEVICE_RETRY_AFTER: Duration = Duration::from_secs(30);

struct SharedDevice(*mut AVBufferRef);

unsafe impl Send for SharedDevice {}

enum Device {
    Ready(SharedDevice),
    Failed(Instant),
}

static DEVICES: Mutex<Vec<(HwAccel, Device)>> = Mutex::new(Vec::new());

fn retry_due(failed_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(failed_at) >= DEVICE_RETRY_AFTER
}

fn created_device(accel: HwAccel) -> Device {
    create_device(accel).map_or_else(|| Device::Failed(Instant::now()), Device::Ready)
}

fn device_reference(accel: HwAccel) -> Option<*mut AVBufferRef> {
    let mut devices = DEVICES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let index = match devices.iter().position(|(known, _)| *known == accel) {
        Some(index) => index,
        None => {
            devices.push((accel, created_device(accel)));
            devices.len() - 1
        }
    };
    if let Device::Failed(failed_at) = devices[index].1
        && retry_due(failed_at, Instant::now())
    {
        devices[index].1 = created_device(accel);
    }
    let Device::Ready(SharedDevice(device)) = &devices[index].1 else {
        return None;
    };
    let reference = unsafe { av_buffer_ref(*device) };
    (!reference.is_null()).then_some(reference)
}

fn create_device(accel: HwAccel) -> Option<SharedDevice> {
    let mut device = ptr::null_mut();
    let created = unsafe {
        av_hwdevice_ctx_create(
            &mut device,
            accel.device_type(),
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    (created >= 0 && !device.is_null()).then_some(SharedDevice(device))
}

fn decodes_with(codec: Codec, accel: HwAccel) -> bool {
    let device_type = accel.device_type();
    let device_method = AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32;
    (0..)
        .map_while(|index| unsafe { avcodec_get_hw_config(codec.as_ptr(), index).as_ref() })
        .any(|config| config.device_type == device_type && config.methods & device_method != 0)
}

pub(crate) fn attach_device(
    context: &mut codec::Context,
    codec: Codec,
    accels: &[HwAccel],
    held_frames: i32,
) -> Option<HwAccel> {
    accels
        .iter()
        .copied()
        .filter(|&accel| decodes_with(codec, accel))
        .find_map(|accel| {
            let device = device_reference(accel)?;
            unsafe {
                let context = context.as_mut_ptr();
                (*context).hw_device_ctx = device;
                (*context).extra_hw_frames = held_frames;
            }
            Some(accel)
        })
}

pub(crate) fn is_hardware_frame(frame: &frame::Video) -> bool {
    unsafe { !(*frame.as_ptr()).hw_frames_ctx.is_null() }
}

pub(crate) fn download(frame: &frame::Video) -> Result<frame::Video, ffmpeg_next::Error> {
    let mut software = frame::Video::empty();
    match unsafe { av_hwframe_transfer_data(software.as_mut_ptr(), frame.as_ptr(), 0) } {
        transferred if transferred >= 0 => Ok(software),
        error => Err(ffmpeg_next::Error::from(error)),
    }
}

#[cfg(test)]
mod tests {
    use ffmpeg_next::decoder;

    use super::*;

    #[test]
    fn iteration_terminates_without_duplicates() {
        let accels = available_hw_accels();
        let mut unique = accels.clone();
        unique.dedup();
        assert_eq!(accels, unique);
    }

    #[test]
    fn a_failed_device_is_retried_only_after_a_while() {
        let failed_at = Instant::now();

        assert!(!retry_due(failed_at, failed_at));
        assert!(!retry_due(failed_at, failed_at + DEVICE_RETRY_AFTER / 2));
        assert!(retry_due(failed_at, failed_at + DEVICE_RETRY_AFTER));
        assert!(!retry_due(failed_at + DEVICE_RETRY_AFTER, failed_at));
    }

    #[test]
    fn device_types_round_trip() {
        for accel in [
            HwAccel::Vaapi,
            HwAccel::Vulkan,
            HwAccel::Cuda,
            HwAccel::Qsv,
            HwAccel::Drm,
        ] {
            assert_eq!(HwAccel::from_device_type(accel.device_type()), Some(accel));
        }
    }

    #[test]
    fn codecs_without_hardware_configs_attach_nothing() {
        crate::init().unwrap();
        let codec = decoder::find(codec::Id::PCM_S16LE).unwrap();
        let mut context = codec::Context::new_with_codec(codec);
        assert_eq!(
            attach_device(&mut context, codec, PREFERRED_HW_ACCELS, 0),
            None
        );
        assert!(unsafe { (*context.as_ptr()).hw_device_ctx.is_null() });
    }
}
