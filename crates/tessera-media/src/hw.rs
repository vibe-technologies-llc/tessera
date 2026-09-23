use std::iter;

use ffmpeg_next::ffi::{AVHWDeviceType, av_hwdevice_iterate_types};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_terminates_without_duplicates() {
        let accels = available_hw_accels();
        let mut unique = accels.clone();
        unique.dedup();
        assert_eq!(accels, unique);
    }
}
