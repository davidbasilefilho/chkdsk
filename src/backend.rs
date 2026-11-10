//! Native Windows disk discovery and SMART/NVMe telemetry.
//!
//! The platform-specific unsafe code is limited to the Win32 boundary below;
//! device buffers are owned byte vectors and decoded with checked offsets.

// The Win32 half below uses most of these; other platforms only get the stubs.
#[cfg_attr(not(windows), allow(unused_imports))]
use crate::{
    model::{DiskIdentity, DiskKind, QueryState, Telemetry},
    parse,
};
#[cfg_attr(not(windows), allow(unused_imports))]
use std::{fmt, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryErrorState {
    Permission,
    Unsupported,
    Transient,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub state: QueryErrorState,
    pub message: String,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.state, self.message)
    }
}
impl std::error::Error for QueryError {}

fn classify(code: u32) -> QueryErrorState {
    match code {
        5 | 1314 => QueryErrorState::Permission,
        1 | 50 | 87 => QueryErrorState::Unsupported,
        13 => QueryErrorState::Partial,
        _ => QueryErrorState::Transient,
    }
}

fn interface_path(identity: &DiskIdentity) -> Option<&str> {
    identity
        .id
        .strip_prefix("interface:")
        .filter(|path| !path.trim().is_empty())
}

#[cfg(not(windows))]
pub fn discover() -> Result<Vec<DiskIdentity>, String> {
    // Native discovery is Windows-only: elsewhere there is nothing to list.
    Ok(Vec::new())
}

#[cfg(not(windows))]
pub fn query(_: &DiskIdentity) -> Result<Telemetry, QueryError> {
    Err(QueryError {
        state: QueryErrorState::Unsupported,
        message: "native disk telemetry is only available on Windows".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_device_data_is_partial_not_transient() {
        assert_eq!(classify(13), QueryErrorState::Partial);
        assert_eq!(classify(5), QueryErrorState::Permission);
        assert_eq!(classify(1), QueryErrorState::Unsupported);
        assert_eq!(classify(1460), QueryErrorState::Transient);
    }

    #[test]
    fn query_provenance_comes_from_interface_identity_not_ordinal() {
        let identity = DiskIdentity {
            id: r"interface:\?\usbstor#disk&ven_foo&prod_bar#abc".to_owned(),
            index: 42,
            model: None,
            serial: Some("abc".to_owned()),
            firmware: None,
            capacity_bytes: None,
            bus: None,
            kind: DiskKind::Unknown,
        };
        assert_eq!(
            interface_path(&identity),
            Some(r"\?\usbstor#disk&ven_foo&prod_bar#abc")
        );
        let ordinal_only = DiskIdentity {
            id: "serial:abc".to_owned(),
            ..identity
        };
        assert_eq!(interface_path(&ordinal_only), None);
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        mem::size_of,
        os::windows::ffi::OsStrExt,
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
        System::IO::DeviceIoControl,
    };

    const DIGCF_PRESENT: u32 = 0x0000_0002;
    const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
    const ERROR_NO_MORE_ITEMS: u32 = 259;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = ctl_code(0x2d, 0x0500, 0, 0);
    const IOCTL_DISK_GET_LENGTH_INFO: u32 = ctl_code(0x07, 0x0017, 0, 1);
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = ctl_code(0x2d, 0x0420, 0, 0);
    const IOCTL_ATA_PASS_THROUGH: u32 = ctl_code(0x04, 0x040b, 0, 3);
    const IOCTL_SMART_RCV_DRIVE_DATA: u32 = ctl_code(0x07, 0x0022, 0, 3);
    const IOCTL_STORAGE_PREDICT_FAILURE: u32 = ctl_code(0x2d, 0x0440, 0, 1);
    const STORAGE_DEVICE_PROPERTY: u32 = 0;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
    const STORAGE_ADAPTER_PROTOCOL_SPECIFIC_PROPERTY: u32 = 49;
    const STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY: u32 = 50;
    const PROPERTY_STANDARD_QUERY: u32 = 0;
    const PROTOCOL_TYPE_NVME: u32 = 3;
    const NVME_DATA_TYPE_LOG_PAGE: u32 = 2;
    const NVME_LOG_PAGE_HEALTH_INFO: u32 = 2;
    const ATA_FLAGS_DRDY_REQUIRED: u16 = 0x01;
    const ATA_FLAGS_DATA_IN: u16 = 0x02;
    const ATA_STATUS_ERR: u8 = 0x01;
    const ATA_STATUS_DF: u8 = 0x20;
    const ATA_SMART_COMMAND: u8 = 0xb0;
    const ATA_SMART_READ_DATA: u8 = 0xd0;
    const ATA_SMART_READ_THRESHOLDS: u8 = 0xd1;
    const ATA_SMART_RETURN_STATUS: u8 = 0xda;

    const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
        (device_type << 16) | (access << 14) | (function << 2) | method
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const GUID_DEVINTERFACE_DISK: Guid = Guid {
        data1: 0x53f56307,
        data2: 0xb6bf,
        data3: 0x11d0,
        data4: [0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b],
    };

    #[repr(C)]
    struct DeviceInterfaceData {
        cb_size: u32,
        interface_class_guid: Guid,
        flags: u32,
        reserved: usize,
    }

    #[link(name = "setupapi")]
    unsafe extern "system" {
        fn SetupDiGetClassDevsW(
            class_guid: *const Guid,
            enumerator: *const u16,
            parent: *mut c_void,
            flags: u32,
        ) -> *mut c_void;
        fn SetupDiEnumDeviceInterfaces(
            device_info_set: *mut c_void,
            device_info_data: *const c_void,
            class_guid: *const Guid,
            member_index: u32,
            interface_data: *mut DeviceInterfaceData,
        ) -> i32;
        fn SetupDiGetDeviceInterfaceDetailW(
            device_info_set: *mut c_void,
            interface_data: *const DeviceInterfaceData,
            detail_data: *mut c_void,
            detail_data_size: u32,
            required_size: *mut u32,
            device_info_data: *mut c_void,
        ) -> i32;
        fn SetupDiDestroyDeviceInfoList(device_info_set: *mut c_void) -> i32;
    }

    // CreateFileW is declared here with an opaque security-attributes pointer,
    // so this module does not depend on windows-sys' Win32_Security types (the
    // elevation module enables that feature for its own token query).
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            filename: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: HANDLE,
        ) -> HANDLE;
    }

    struct InfoSet(*mut c_void);
    impl Drop for InfoSet {
        fn drop(&mut self) {
            unsafe {
                SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }

    struct Device(HANDLE);
    impl Drop for Device {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn open_path_access(path: &[u16], access: u32) -> Result<Device, u32> {
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(unsafe { GetLastError() })
        } else {
            Ok(Device(handle))
        }
    }

    fn open_path(path: &[u16]) -> Result<Device, u32> {
        open_path_access(path, GENERIC_READ)
    }

    fn ioctl(device: &Device, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, u32> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                device.0,
                code,
                if input.is_empty() {
                    null()
                } else {
                    input.as_ptr().cast()
                },
                input.len() as u32,
                if output.is_empty() {
                    null_mut()
                } else {
                    output.as_mut_ptr().cast()
                },
                output.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 {
            Err(unsafe { GetLastError() })
        } else {
            Ok(returned as usize)
        }
    }

    fn win_error(code: u32) -> QueryError {
        QueryError {
            state: classify(code),
            message: io::Error::from_raw_os_error(code as i32).to_string(),
        }
    }

    fn query_storage_property(
        device: &Device,
        property: u32,
        extra: &[u8],
        output_len: usize,
    ) -> Result<Vec<u8>, u32> {
        let input = storage_property_input(property, extra).ok_or(87u32)?;
        let mut output = vec![0u8; output_len];
        let returned = ioctl(device, IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut output)?;
        output.truncate(returned.min(output.len()));
        Ok(output)
    }

    fn storage_property_input(property: u32, extra: &[u8]) -> Option<Vec<u8>> {
        let payload_end = 8usize.checked_add(extra.len())?;
        let minimum_len = payload_end.max(12);
        let input_len = minimum_len.checked_add(3)? & !3;
        u32::try_from(input_len).ok()?;

        let mut input = vec![0; input_len];
        put_u32(&mut input, 0, property);
        put_u32(&mut input, 4, PROPERTY_STANDARD_QUERY);
        input.get_mut(8..payload_end)?.copy_from_slice(extra);
        Some(input)
    }

    fn put_u32(buf: &mut [u8], at: usize, value: u32) {
        buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn get_u32(buf: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
    }
    fn get_i64(buf: &[u8], at: usize) -> Option<i64> {
        Some(i64::from_le_bytes(buf.get(at..at + 8)?.try_into().ok()?))
    }

    type DeviceDescriptor = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        u32,
    );

    fn descriptor(device: &Device) -> Option<DeviceDescriptor> {
        let bytes = query_storage_property(device, STORAGE_DEVICE_PROPERTY, &[0; 1], 4096).ok()?;
        if bytes.len() < 36 {
            return None;
        }
        let bus_type = get_u32(&bytes, 28)?;
        let read_string = |offset_at: usize| -> Option<String> {
            let offset = get_u32(&bytes, offset_at)? as usize;
            if offset == 0 || offset >= bytes.len() {
                return None;
            }
            let end = bytes[offset..]
                .iter()
                .position(|b| *b == 0)
                .map(|n| offset + n)
                .unwrap_or(bytes.len());
            let value = String::from_utf8_lossy(&bytes[offset..end])
                .trim()
                .to_owned();
            (!value.is_empty()).then_some(value)
        };
        let vendor = read_string(12);
        let product = read_string(16);
        let revision = read_string(20);
        let serial = read_string(24);
        let model = match (vendor, product) {
            (Some(v), Some(p)) => Some(format!("{v} {p}")),
            (Some(v), None) => Some(v),
            (None, p) => p,
        };
        Some((
            model,
            serial,
            revision,
            Some(bus_name(bus_type).to_owned()),
            bus_type,
        ))
    }

    fn bus_name(bus: u32) -> &'static str {
        match bus {
            1 => "SCSI",
            3 => "ATA",
            7 => "USB",
            8 => "RAID",
            9 => "iSCSI",
            11 => "SATA",
            15 => "File-backed virtual",
            16 => "Storage Spaces",
            17 => "NVMe",
            18 => "SCM",
            _ => "Unknown",
        }
    }

    fn capacity(device: &Device) -> Option<u64> {
        let mut bytes = [0u8; 8];
        let n = ioctl(device, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut bytes).ok()?;
        (n >= 8)
            .then(|| get_i64(&bytes, 0).unwrap_or_default())
            .and_then(|v| u64::try_from(v).ok())
    }

    fn device_index(device: &Device, fallback: u32) -> u32 {
        let mut bytes = [0u8; 12];
        if ioctl(device, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut bytes).is_ok() {
            get_u32(&bytes, 4).unwrap_or(fallback)
        } else {
            fallback
        }
    }

    fn kind(bus: u32, device: &Device) -> DiskKind {
        if bus == 17 {
            return DiskKind::Nvme;
        }
        let result =
            query_storage_property(device, STORAGE_DEVICE_SEEK_PENALTY_PROPERTY, &[0; 1], 12);
        match result {
            Ok(b) if b.len() >= 9 && b[8] == 0 => DiskKind::Ssd,
            Ok(b) if b.len() >= 9 && b[8] != 0 => DiskKind::Hdd,
            _ => DiskKind::Unknown,
        }
    }

    fn path_from_interface(set: &InfoSet, data: &DeviceInterfaceData) -> Result<Vec<u16>, u32> {
        let mut required = 0u32;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, data, null_mut(), 0, &mut required, null_mut());
        }
        let code = unsafe { GetLastError() };
        if required < 6 || code != ERROR_INSUFFICIENT_BUFFER {
            return Err(code);
        }
        let mut buffer = vec![0u8; required as usize];
        // SP_DEVICE_INTERFACE_DETAIL_DATA_W has a 6-byte cbSize on x86 and 8 on x64/ARM64.
        let cb_size = if cfg!(target_pointer_width = "64") {
            8u32
        } else {
            6u32
        };
        put_u32(&mut buffer, 0, cb_size);
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                data,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() });
        }
        let path_bytes = buffer.get(4..).ok_or(87u32)?;
        let (wide_chunks, _) = path_bytes.as_chunks::<2>();
        let wide: Vec<u16> = wide_chunks
            .iter()
            .map(|chunk| u16::from_le_bytes(*chunk))
            .take_while(|c| *c != 0)
            .collect();
        if wide.is_empty() {
            return Err(87);
        }
        let mut terminated = wide;
        terminated.push(0);
        Ok(terminated)
    }

    pub fn discover() -> Result<Vec<DiskIdentity>, String> {
        let raw = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVINTERFACE_DISK,
                null(),
                null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if raw as isize == -1 {
            return Err(format!(
                "SetupDiGetClassDevsW failed: {}",
                io::Error::last_os_error()
            ));
        }
        let set = InfoSet(raw);
        let mut disks = Vec::new();
        let mut member = 0u32;
        loop {
            let mut data = DeviceInterfaceData {
                cb_size: size_of::<DeviceInterfaceData>() as u32,
                interface_class_guid: GUID_DEVINTERFACE_DISK,
                flags: 0,
                reserved: 0,
            };
            let ok = unsafe {
                SetupDiEnumDeviceInterfaces(
                    set.0,
                    null(),
                    &GUID_DEVINTERFACE_DISK,
                    member,
                    &mut data,
                )
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if disks.is_empty() {
                    return Err(format!(
                        "SetupDiEnumDeviceInterfaces failed: {}",
                        io::Error::from_raw_os_error(code as i32)
                    ));
                }
                break;
            }
            member = member.saturating_add(1);
            let path = match path_from_interface(&set, &data) {
                Ok(path) => path,
                Err(_) => {
                    let index = member.saturating_sub(1);
                    disks.push(DiskIdentity {
                        id: format!("setupapi-member:{index}"),
                        index,
                        model: None,
                        serial: None,
                        firmware: None,
                        capacity_bytes: None,
                        bus: None,
                        kind: DiskKind::Unknown,
                    });
                    continue;
                }
            };
            let display_path = String::from_utf16_lossy(&path[..path.len().saturating_sub(1)]);
            let opened = open_path(&path);
            let fallback_index = member.saturating_sub(1);
            let (index, model, serial, firmware, bus, capacity_bytes, kind) =
                if let Ok(device) = opened {
                    let (model, serial, firmware, bus, bus_type) =
                        descriptor(&device).unwrap_or((None, None, None, None, 0));
                    (
                        device_index(&device, fallback_index),
                        model,
                        serial,
                        firmware,
                        bus,
                        capacity(&device),
                        kind(bus_type, &device),
                    )
                } else {
                    (
                        fallback_index,
                        None,
                        None,
                        None,
                        None,
                        None,
                        DiskKind::Unknown,
                    )
                };
            // Keep the enumerated interface path in the identity even when a
            // serial is available: telemetry must reopen this exact device,
            // never the potentially stale SetupAPI ordinal.
            let id = format!("interface:{}", display_path.to_ascii_lowercase());
            disks.push(DiskIdentity {
                id,
                index,
                model,
                serial,
                firmware,
                capacity_bytes,
                bus,
                kind,
            });
        }
        Ok(disks)
    }

    #[repr(C)]
    struct AtaPassThroughEx {
        length: u16,
        ata_flags: u16,
        path_id: u8,
        target_id: u8,
        lun: u8,
        reserved_byte: u8,
        data_transfer_length: u32,
        timeout_value: u32,
        reserved: u32,
        data_buffer_offset: usize,
        previous_task_file: [u8; 8],
        current_task_file: [u8; 8],
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum AtaCommandError {
        Io(u32),
        ShortResponse,
        Device {
            status: u8,
            error: u8,
        },
        InvalidSmartData(String),
        InvalidSmartThresholds(String),
        FallbackFailed {
            primary: Box<AtaCommandError>,
            legacy: Box<AtaCommandError>,
        },
    }

    impl AtaCommandError {
        fn code(&self) -> u32 {
            match self {
                Self::Io(code) => *code,
                Self::ShortResponse => 13,
                Self::Device { .. } => 31,
                Self::InvalidSmartData(_) => 13,
                Self::InvalidSmartThresholds(_) => 13,
                Self::FallbackFailed { primary, legacy } => {
                    if classify(primary.code()) == QueryErrorState::Permission {
                        primary.code()
                    } else {
                        legacy.code()
                    }
                }
            }
        }

        fn message(&self) -> String {
            match self {
                Self::Io(code) => io::Error::from_raw_os_error(*code as i32).to_string(),
                Self::ShortResponse => "ATA pass-through returned an incomplete response".into(),
                Self::Device { status, error } => {
                    format!("ATA command failed (status 0x{status:02x}, error 0x{error:02x})")
                }
                Self::InvalidSmartData(reason) => {
                    format!("ATA SMART READ DATA was malformed: {reason}")
                }
                Self::InvalidSmartThresholds(reason) => {
                    format!("ATA SMART READ THRESHOLDS was malformed: {reason}")
                }
                Self::FallbackFailed { primary, legacy } => format!(
                    "ATA pass-through failed: {}; SMART_RCV_DRIVE_DATA fallback failed: {}",
                    primary.message(),
                    legacy.message()
                ),
            }
        }
    }

    fn ata_completion_error(task_file: &[u8; 8]) -> Option<AtaCommandError> {
        let status = task_file[6];
        if status & (ATA_STATUS_ERR | ATA_STATUS_DF) != 0 {
            Some(AtaCommandError::Device {
                status,
                error: task_file[0],
            })
        } else {
            None
        }
    }

    fn ata_task_file(feature: u8, command: u8, read_sector: bool) -> [u8; 8] {
        let mut task_file = [0; 8];
        task_file[0] = feature;
        if read_sector {
            task_file[1] = 1;
            task_file[2] = smart_sector_number(feature);
        }
        task_file[3] = 0x4f;
        task_file[4] = 0xc2;
        task_file[5] = 0xa0;
        task_file[6] = command;
        task_file
    }

    fn smart_sector_number(feature: u8) -> u8 {
        // Use sector number 1 for D1 threshold reads, but 0 for D0 data reads.
        // Keep the command-specific inputs aligned across both Windows IOCTLs.
        u8::from(feature == ATA_SMART_READ_THRESHOLDS)
    }

    fn ata_command(
        device: &Device,
        command: u8,
        feature: u8,
        read_sector: bool,
    ) -> Result<(Vec<u8>, AtaPassThroughEx), AtaCommandError> {
        let header = size_of::<AtaPassThroughEx>();
        let transfer = if read_sector { 512usize } else { 0 };
        let mut buffer = vec![0u8; header + transfer];
        let pass = AtaPassThroughEx {
            length: header as u16,
            ata_flags: if read_sector {
                ATA_FLAGS_DATA_IN | ATA_FLAGS_DRDY_REQUIRED
            } else {
                ATA_FLAGS_DRDY_REQUIRED
            },
            path_id: 0,
            target_id: 0,
            lun: 0,
            reserved_byte: 0,
            data_transfer_length: transfer as u32,
            timeout_value: 10,
            reserved: 0,
            data_buffer_offset: if read_sector { header } else { 0 },
            previous_task_file: [0; 8],
            current_task_file: ata_task_file(feature, command, read_sector),
        };
        // The only ABI cast is to the repr(C) header followed by its inline data buffer.
        unsafe {
            std::ptr::write_unaligned(buffer.as_mut_ptr().cast::<AtaPassThroughEx>(), pass);
        }
        let returned_bytes = ioctl_inout(device, IOCTL_ATA_PASS_THROUGH, &mut buffer, header)
            .map_err(AtaCommandError::Io)?;
        if returned_bytes < header {
            return Err(AtaCommandError::ShortResponse);
        }
        let returned =
            unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<AtaPassThroughEx>()) };
        if let Some(error) = ata_completion_error(&returned.current_task_file) {
            return Err(error);
        }
        if returned_bytes < header + transfer || (returned.data_transfer_length as usize) < transfer
        {
            return Err(AtaCommandError::ShortResponse);
        }
        Ok((buffer.get(header..).unwrap_or_default().to_vec(), returned))
    }

    fn ioctl_inout(
        device: &Device,
        code: u32,
        buffer: &mut [u8],
        input_length: usize,
    ) -> Result<usize, u32> {
        let mut returned = 0u32;
        let ok = unsafe {
            let ptr = buffer.as_mut_ptr().cast::<c_void>();
            DeviceIoControl(
                device.0,
                code,
                ptr,
                input_length as u32,
                ptr,
                buffer.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 {
            Err(unsafe { GetLastError() })
        } else {
            Ok(returned as usize)
        }
    }

    fn nvme_health(device: &Device) -> Result<crate::model::NvmeHealth, u32> {
        // STORAGE_PROTOCOL_SPECIFIC_DATA is 40 bytes on all supported ABIs.
        let mut protocol = [0u8; 40];
        put_u32(&mut protocol, 0, PROTOCOL_TYPE_NVME);
        put_u32(&mut protocol, 4, NVME_DATA_TYPE_LOG_PAGE);
        put_u32(&mut protocol, 8, NVME_LOG_PAGE_HEALTH_INFO);
        put_u32(&mut protocol, 16, 40);
        put_u32(&mut protocol, 20, 512);
        let output = query_storage_property(
            device,
            STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY,
            &protocol,
            8 + 40 + 512,
        )
        .or_else(|_| {
            query_storage_property(
                device,
                STORAGE_ADAPTER_PROTOCOL_SPECIFIC_PROPERTY,
                &protocol,
                8 + 40 + 512,
            )
        })?;
        if output.len() < 48
            || get_u32(&output, 0) != Some(48)
            || get_u32(&output, 4).unwrap_or(0) < 48
        {
            return Err(13);
        }
        let data_offset = get_u32(&output, 24).ok_or(13u32)? as usize;
        let data_length = get_u32(&output, 28).ok_or(13u32)? as usize;
        let start = 8usize.checked_add(data_offset).ok_or(13u32)?;
        let end = start.checked_add(data_length.min(512)).ok_or(13u32)?;
        if data_offset < 40 || data_length < 512 || end > output.len() {
            return Err(13);
        }
        parse::parse_nvme_health_log(&output[start..end]).map_err(|_| 13)
    }

    fn legacy_smart_input(feature: u8) -> [u8; 32] {
        // WDK packs SENDCMDINPARAMS to 33 bytes; sizeof - 1 is this 32-byte input.
        // Its bDriveNumber field is opaque and ignored by Windows; the caller's
        // handle, opened from this disk's interface path, selects the device.
        let mut input = [0u8; 32];
        put_u32(&mut input, 0, 512);
        input[4] = feature;
        input[5] = 1; // SMART READ DATA / THRESHOLDS sector count.
        input[6] = smart_sector_number(feature); // Sector Number / LBA Low.
        input[7] = 0x4f;
        input[8] = 0xc2;
        input[9] = 0xa0;
        input[10] = ATA_SMART_COMMAND;
        input
    }

    fn legacy_smart_sector_with<T>(
        device: &T,
        feature: u8,
        send: impl FnOnce(&T, u32, &[u8], &mut [u8]) -> Result<usize, u32>,
    ) -> Result<Vec<u8>, AtaCommandError> {
        // The packed 16-byte SENDCMDOUTPARAMS header is followed by one sector.
        let input = legacy_smart_input(feature);
        let mut output = [0u8; 16 + 512];
        let returned = send(device, IOCTL_SMART_RCV_DRIVE_DATA, &input, &mut output)
            .map_err(AtaCommandError::Io)?;
        legacy_smart_response(&output, returned).map(|sector| sector.to_vec())
    }

    fn legacy_smart_sector(device: &Device, feature: u8) -> Result<Vec<u8>, AtaCommandError> {
        legacy_smart_sector_with(device, feature, |device, code, input, output| {
            ioctl(device, code, input, output)
        })
    }

    fn legacy_smart_response(output: &[u8], returned: usize) -> Result<&[u8], AtaCommandError> {
        if returned < 16 + 512 || output.len() < 16 + 512 || get_u32(output, 0).unwrap_or(0) < 512 {
            return Err(AtaCommandError::ShortResponse);
        }
        // DRIVERSTATUS.bDriverError reports command/driver failure. bIDEError
        // is only the ATA IDE Error register and is not an ATA Status register.
        let driver_error = output[4];
        if driver_error != 0 {
            return Err(AtaCommandError::Device {
                status: 0,
                error: driver_error,
            });
        }
        Ok(&output[16..16 + 512])
    }

    fn ata_data_sector_validated(
        pass_through: impl FnOnce() -> Result<Vec<u8>, AtaCommandError>,
        legacy: impl FnOnce() -> Result<Vec<u8>, AtaCommandError>,
        validate: impl Fn(&[u8]) -> Result<(), AtaCommandError>,
    ) -> Result<Vec<u8>, AtaCommandError> {
        let primary_error = match pass_through() {
            Ok(sector) => match validate(&sector) {
                Ok(()) => return Ok(sector),
                Err(error) => error,
            },
            Err(error) => error,
        };
        let legacy_error = match legacy() {
            Ok(sector) => match validate(&sector) {
                Ok(()) => return Ok(sector),
                Err(error) => error,
            },
            Err(error) => error,
        };
        Err(AtaCommandError::FallbackFailed {
            primary: Box::new(primary_error),
            legacy: Box::new(legacy_error),
        })
    }

    fn validate_ata_smart_data_sector(sector: &[u8]) -> Result<(), AtaCommandError> {
        let health = parse::parse_ata_smart_data(sector)
            .map_err(|error| AtaCommandError::InvalidSmartData(error.to_string()))?;
        if health.attributes.is_empty() {
            return Err(AtaCommandError::InvalidSmartData(
                "valid sector contains no SMART attributes".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_ata_smart_thresholds_sector(sector: &[u8]) -> Result<(), AtaCommandError> {
        // Parse into a throwaway value before selecting the pass-through
        // response. Duplicate IDs or a bad checksum must permit the existing
        // legacy IOCTL to retry, without mutating the valid READ DATA result.
        let mut scratch = crate::model::AtaHealth::default();
        parse::apply_ata_smart_thresholds(&mut scratch, sector)
            .map_err(|error| AtaCommandError::InvalidSmartThresholds(error.to_string()))
    }

    fn read_ata_smart_thresholds_with(
        health: &mut crate::model::AtaHealth,
        pass_through: impl FnOnce() -> Result<Vec<u8>, AtaCommandError>,
        legacy: impl FnOnce() -> Result<Vec<u8>, AtaCommandError>,
    ) -> Result<(), AtaCommandError> {
        let sector =
            ata_data_sector_validated(pass_through, legacy, validate_ata_smart_thresholds_sector)?;
        parse::apply_ata_smart_thresholds(health, &sector)
            .map_err(|error| AtaCommandError::InvalidSmartThresholds(error.to_string()))
    }

    fn ata_health(device: &Device) -> Result<crate::model::AtaHealth, AtaCommandError> {
        let sector = ata_data_sector_validated(
            || {
                ata_command(device, ATA_SMART_COMMAND, ATA_SMART_READ_DATA, true)
                    .map(|(sector, _)| sector)
            },
            || legacy_smart_sector(device, ATA_SMART_READ_DATA),
            validate_ata_smart_data_sector,
        )?;
        let mut health = parse::parse_ata_smart_data(&sector)
            .map_err(|error| AtaCommandError::InvalidSmartData(error.to_string()))?;

        let _ = read_ata_smart_thresholds_with(
            &mut health,
            || {
                ata_command(device, ATA_SMART_COMMAND, ATA_SMART_READ_THRESHOLDS, true)
                    .map(|(sector, _)| sector)
            },
            || legacy_smart_sector(device, ATA_SMART_READ_THRESHOLDS),
        ); // Optional failure must not invalidate a checksummed READ DATA sector.
        Ok(health)
    }

    fn ata_failure_status(device: &Device) -> Result<bool, AtaCommandError> {
        let (_, pass) = ata_command(device, ATA_SMART_COMMAND, ATA_SMART_RETURN_STATUS, false)?;
        match (pass.current_task_file[3], pass.current_task_file[4]) {
            (0x4f, 0xc2) => Ok(false),
            (0xf4, 0x2c) => Ok(true),
            _ => Err(AtaCommandError::ShortResponse),
        }
    }

    fn legacy_failure_status(device: &Device) -> Result<bool, u32> {
        let mut buffer = [0u8; 516];
        let returned = ioctl(device, IOCTL_STORAGE_PREDICT_FAILURE, &[], &mut buffer)?;
        if returned < 4 {
            return Err(13);
        }
        // VendorSpecific is opaque IOCTL_STORAGE_PREDICT_FAILURE data, not
        // guaranteed to be an ATA SMART sector. Preserve only its coarse flag.
        Ok(get_u32(&buffer, 0).ok_or(13u32)? != 0)
    }

    pub fn query(identity: &DiskIdentity) -> Result<Telemetry, QueryError> {
        let name = interface_path(identity).ok_or_else(|| QueryError {
            state: QueryErrorState::Partial,
            message: "disk identity does not contain an interface path".to_owned(),
        })?;
        let path: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let device = open_path_access(&path, GENERIC_READ | GENERIC_WRITE)
            .or_else(|_| open_path(&path))
            .map_err(win_error)?;
        let mut telemetry = Telemetry::default();
        let mut probe_errors = Vec::new();

        match nvme_health(&device) {
            Ok(health) => telemetry.apply_nvme(health),
            Err(code) => {
                probe_errors.push(classify(code));
                telemetry.nvme = if matches!(classify(code), QueryErrorState::Unsupported) {
                    QueryState::Unsupported
                } else {
                    QueryState::Failed {
                        message: io::Error::from_raw_os_error(code as i32).to_string(),
                    }
                }
            }
        }
        match ata_health(&device) {
            Ok(health) => telemetry.apply_ata(health, identity.model.as_deref()),
            Err(code) => {
                let state = classify(code.code());
                probe_errors.push(state);
                telemetry.ata = if matches!(state, QueryErrorState::Unsupported) {
                    QueryState::Unsupported
                } else {
                    QueryState::Failed {
                        message: code.message(),
                    }
                }
            }
        }

        match ata_failure_status(&device) {
            Ok(failed) => telemetry.overall_failure = QueryState::Success(failed),
            Err(ata_error) => match legacy_failure_status(&device) {
                Ok(failed) => {
                    telemetry.overall_failure = QueryState::Success(failed);
                }
                Err(legacy_error) => {
                    let code = if classify(ata_error.code()) == QueryErrorState::Permission {
                        ata_error.code()
                    } else {
                        legacy_error
                    };
                    probe_errors.push(classify(code));
                    telemetry.overall_failure = if classify(code) == QueryErrorState::Unsupported {
                        QueryState::Unsupported
                    } else {
                        QueryState::Failed {
                            message: io::Error::from_raw_os_error(code as i32).to_string(),
                        }
                    };
                }
            },
        }

        let has_any = !matches!(
            &telemetry.nvme,
            QueryState::NotQueried | QueryState::Unsupported | QueryState::Failed { .. }
        ) || !matches!(
            &telemetry.ata,
            QueryState::NotQueried | QueryState::Unsupported | QueryState::Failed { .. }
        ) || !matches!(
            &telemetry.overall_failure,
            QueryState::NotQueried | QueryState::Unsupported | QueryState::Failed { .. }
        );
        if !has_any {
            // Query completed but every supported transport probe failed; retain
            // the per-probe states while making the aggregate partiality visible.
            let detail = [
                state_message(&telemetry.nvme),
                state_message(&telemetry.ata),
                state_message(&telemetry.overall_failure),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
            let state = if probe_errors.contains(&QueryErrorState::Permission) {
                QueryErrorState::Permission
            } else if !probe_errors.is_empty()
                && probe_errors
                    .iter()
                    .all(|s| *s == QueryErrorState::Unsupported)
            {
                QueryErrorState::Unsupported
            } else if probe_errors.contains(&QueryErrorState::Transient) {
                QueryErrorState::Transient
            } else {
                QueryErrorState::Partial
            };
            return Err(QueryError {
                state,
                message: detail,
            });
        }
        Ok(telemetry)
    }

    fn state_message<T>(state: &QueryState<T>) -> Option<String> {
        match state {
            QueryState::Failed { message } => Some(message.clone()),
            QueryState::Unsupported => Some("probe is unsupported".to_owned()),
            QueryState::NotQueried | QueryState::Success(_) => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn smart_data_and_return_status_task_file_fixtures_are_distinct() {
            assert_eq!(
                ata_task_file(ATA_SMART_READ_DATA, ATA_SMART_COMMAND, true),
                [0xd0, 1, 0, 0x4f, 0xc2, 0xa0, 0xb0, 0]
            );
            assert_eq!(
                ata_task_file(ATA_SMART_RETURN_STATUS, ATA_SMART_COMMAND, false),
                [0xda, 0, 0, 0x4f, 0xc2, 0xa0, 0xb0, 0]
            );
            assert_eq!(
                ata_task_file(ATA_SMART_READ_THRESHOLDS, ATA_SMART_COMMAND, true),
                [0xd1, 1, 1, 0x4f, 0xc2, 0xa0, 0xb0, 0]
            );
        }

        fn finish_smart_checksum(sector: &mut [u8; 512]) {
            sector[511] = 0;
            sector[511] = 0u8.wrapping_sub(
                sector[..511]
                    .iter()
                    .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            );
        }

        fn valid_smart_data_sector() -> Vec<u8> {
            let mut sector = [0u8; 512];
            sector[2..7].copy_from_slice(&[197, 1, 0, 100, 100]);
            finish_smart_checksum(&mut sector);
            sector.to_vec()
        }

        fn smart_threshold_sector(entries: &[(u8, u8)]) -> Vec<u8> {
            let mut sector = [0u8; 512];
            for (slot, (id, threshold)) in entries.iter().copied().enumerate() {
                let offset = 2 + slot * 12;
                sector[offset] = id;
                sector[offset + 1] = threshold;
            }
            finish_smart_checksum(&mut sector);
            sector.to_vec()
        }

        #[test]
        fn malformed_pass_through_sector_retries_legacy_and_preserves_both_errors_if_needed() {
            let valid = valid_smart_data_sector();
            let io_fallback = ata_data_sector_validated(
                || Err(AtaCommandError::Io(5)),
                || Ok(valid.clone()),
                validate_ata_smart_data_sector,
            )
            .unwrap();
            assert_eq!(io_fallback, valid);

            let mut bad_checksum = valid.clone();
            bad_checksum[511] ^= 1;
            let read = ata_data_sector_validated(
                || Ok(bad_checksum),
                || Ok(valid.clone()),
                validate_ata_smart_data_sector,
            )
            .unwrap();
            assert_eq!(read, valid);

            let read = ata_data_sector_validated(
                || Ok(vec![0; 512]), // Checksummed-but-empty is not mapped data.
                || Ok(valid.clone()),
                validate_ata_smart_data_sector,
            )
            .unwrap();
            assert_eq!(read, valid);

            let error = ata_data_sector_validated(
                || Ok(vec![0; 511]),
                || Ok(vec![0; 512]),
                validate_ata_smart_data_sector,
            )
            .unwrap_err();
            let message = error.message();
            assert!(message.contains("pass-through failed"));
            assert!(message.contains("SMART_RCV_DRIVE_DATA fallback failed"));
            assert!(message.contains("expected 512-byte sector, got 511"));
            assert!(message.contains("valid sector contains no SMART attributes"));
            assert_eq!(error.code(), 13);
        }

        #[test]
        fn malformed_pass_through_threshold_page_retries_legacy_and_joins_valid_thresholds() {
            let valid_legacy = smart_threshold_sector(&[(197, 10)]);
            let mut bad_pass = valid_legacy.clone();
            bad_pass[511] ^= 1;
            let mut legacy_called = false;
            let mut health = crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 197,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: [0; 6],
                }],
                thresholds_available: false,
            };
            read_ata_smart_thresholds_with(
                &mut health,
                || Ok(bad_pass),
                || {
                    legacy_called = true;
                    Ok(valid_legacy.clone())
                },
            )
            .unwrap();
            assert!(legacy_called);
            assert!(health.thresholds_available);
            assert_eq!(health.attributes[0].threshold, Some(10));
        }

        #[test]
        fn valid_pass_through_threshold_page_skips_legacy_and_both_invalid_leave_read_data_usable()
        {
            let valid = smart_threshold_sector(&[(197, 10)]);
            let mut threshold_health = crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 197,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: [0; 6],
                }],
                thresholds_available: false,
            };
            read_ata_smart_thresholds_with(
                &mut threshold_health,
                || Ok(valid.clone()),
                || panic!("valid pass-through threshold page must not use legacy"),
            )
            .unwrap();
            assert_eq!(threshold_health.attributes[0].threshold, Some(10));

            let mut bad_checksum = smart_threshold_sector(&[(197, 10)]);
            bad_checksum[511] ^= 1;
            let duplicate = smart_threshold_sector(&[(197, 10), (197, 5)]);
            let mut read_data = crate::model::AtaHealth {
                attributes: vec![crate::model::AtaAttribute {
                    id: 197,
                    flags: 0,
                    current: 100,
                    worst: 100,
                    threshold: None,
                    raw: [1, 0, 0, 0, 0, 0],
                }],
                thresholds_available: false,
            };
            let original_read_data = read_data.clone();
            let error = read_ata_smart_thresholds_with(
                &mut read_data,
                || Ok(bad_checksum),
                || Ok(duplicate),
            )
            .unwrap_err();
            let message = error.message();
            assert!(message.contains("pass-through failed"));
            assert!(message.contains("SMART_RCV_DRIVE_DATA fallback failed"));
            assert!(message.contains("ATA SMART READ THRESHOLDS was malformed"));
            assert!(message.contains("ATA SMART data checksum is invalid"));
            assert!(message.contains("duplicate ATA SMART threshold ID"));
            assert_eq!(read_data, original_read_data);
            assert!(!read_data.thresholds_available);
        }

        #[test]
        fn legacy_smart_receive_header_uses_documented_sector_request() {
            assert_eq!(IOCTL_SMART_RCV_DRIVE_DATA, 508040);
            for (feature, sector_number) in
                [(ATA_SMART_READ_DATA, 0), (ATA_SMART_READ_THRESHOLDS, 1)]
            {
                let input = legacy_smart_input(feature);
                assert_eq!(input.len(), 32);
                assert_eq!(get_u32(&input, 0), Some(512));
                assert_eq!(
                    &input[4..12],
                    &[
                        feature,
                        1,
                        sector_number,
                        0x4f,
                        0xc2,
                        0xa0,
                        ATA_SMART_COMMAND,
                        0
                    ]
                );
                assert_eq!(input[12], 0, "bDriveNumber is opaque and unused");
            }
        }

        #[test]
        fn legacy_smart_request_uses_selected_interface_handle_not_disk_ordinal() {
            let identity = DiskIdentity {
                id: r"interface:\\?\usbstor#disk&ven_example&prod_drive#physical42".into(),
                index: 42,
                model: None,
                serial: Some("physical42".into()),
                firmware: None,
                capacity_bytes: None,
                bus: None,
                kind: DiskKind::Unknown,
            };
            let selected_interface = interface_path(&identity).unwrap().to_owned();

            let sector = legacy_smart_sector_with(
                &selected_interface,
                ATA_SMART_READ_DATA,
                |handle, code, input, output| {
                    assert_eq!(handle, &selected_interface);
                    assert_eq!(code, IOCTL_SMART_RCV_DRIVE_DATA);
                    assert_eq!(input.len(), 32);
                    assert_eq!(output.len(), 16 + 512);
                    assert_eq!(input[12], 0, "the OS selects the target from the handle");
                    put_u32(output, 0, 512);
                    output[16..].fill(0x42);
                    Ok(output.len())
                },
            )
            .unwrap();

            assert_eq!(sector, vec![0x42; 512]);
        }

        #[test]
        fn legacy_smart_response_uses_driver_error_not_ide_error_as_status() {
            let mut output = [0u8; 16 + 512];
            put_u32(&mut output, 0, 512);
            // These bits overlap ATA status ERR/DF, but here byte 5 is
            // DRIVERSTATUS.bIDEError, diagnostic data only.
            output[5] = ATA_STATUS_ERR | ATA_STATUS_DF;
            assert_eq!(
                legacy_smart_response(&output, output.len()).unwrap().len(),
                512
            );
            assert_eq!(
                legacy_smart_response(&output, output.len() - 1),
                Err(AtaCommandError::ShortResponse)
            );
            output[4] = 0x04;
            assert_eq!(
                legacy_smart_response(&output, output.len()),
                Err(AtaCommandError::Device {
                    status: 0,
                    error: 0x04,
                })
            );
        }

        fn diagnostic_observation(
            identity: &DiskIdentity,
            result: &Result<Telemetry, QueryError>,
        ) -> serde_json::Value {
            let summary = match result {
                Ok(telemetry) => {
                    let health_score = crate::health::score_health(identity.kind, telemetry);
                    let ata = match &telemetry.ata {
                        QueryState::Success(health) => {
                            let attributes: Vec<_> = health
                                .attributes
                                .iter()
                                .map(|attribute| {
                                    serde_json::json!({
                                        "id": attribute.id,
                                        "flags": attribute.flags,
                                        "current": attribute.current,
                                        "worst": attribute.worst,
                                        "threshold": attribute.threshold,
                                        "raw": attribute.raw,
                                    })
                                })
                                .collect();
                            serde_json::json!({
                                "state": "readable",
                                "attribute_count": attributes.len(),
                                "attributes": attributes,
                                "thresholds_available": health.thresholds_available,
                            })
                        }
                        QueryState::Failed { message } => serde_json::json!({
                            "state": "failed",
                            "message": message,
                        }),
                        QueryState::Unsupported => serde_json::json!({"state": "unsupported"}),
                        QueryState::NotQueried => serde_json::json!({"state": "not_queried"}),
                    };
                    serde_json::json!({
                        "ata": ata,
                        "nvme": telemetry.nvme,
                        "normalized": {
                            "endurance_percent_used": telemetry.endurance_percent,
                            "ata_profile": telemetry.ata_profile,
                            "power_on_hours": telemetry.power_on_hours,
                            "temperature_c": telemetry.temperature_c,
                            "overall_failure": telemetry.overall_failure,
                        },
                        "health_score": {
                            "score": health_score.score,
                            "outcome": health_score.outcome,
                            "confidence": health_score.confidence,
                            "provenance": health_score.provenance,
                            "warnings": health_score.warnings,
                        },
                    })
                }
                Err(error) => serde_json::json!({
                    "query_error": format!("{:?}: {}", error.state, error.message),
                }),
            };
            serde_json::json!({
                "index": identity.index,
                "model": identity.model,
                "bus": identity.bus,
                "kind": format!("{:?}", identity.kind),
                "query": summary,
            })
        }

        #[test]
        fn diagnostic_report_includes_raw_ata_data_normalized_fields_and_score() {
            let identity = DiskIdentity {
                id: "test-disk".into(),
                index: 0,
                model: Some("MACROVIP MV240GB".into()),
                serial: None,
                firmware: None,
                capacity_bytes: None,
                bus: Some("SATA".into()),
                kind: DiskKind::Ssd,
            };
            let telemetry = Telemetry {
                ata: QueryState::Success(crate::model::AtaHealth {
                    attributes: vec![crate::model::AtaAttribute {
                        id: 202,
                        flags: 0x0018,
                        current: 100,
                        worst: 100,
                        threshold: Some(1),
                        raw: [0x34, 0x12, 0, 0, 0, 0],
                    }],
                    thresholds_available: true,
                }),
                overall_failure: QueryState::Success(false),
                ..Default::default()
            };
            let report = diagnostic_observation(&identity, &Ok(telemetry));
            assert_eq!(report["model"], "MACROVIP MV240GB");
            assert!(report.get("serial").is_none());
            assert_eq!(report["query"]["ata"]["attribute_count"], 1);
            assert_eq!(report["query"]["ata"]["attributes"][0]["raw"][0], 0x34);
            assert_eq!(report["query"]["ata"]["attributes"][0]["threshold"], 1);
            assert_eq!(
                report["query"]["normalized"]["endurance_percent_used"],
                serde_json::Value::Null
            );
            assert_eq!(
                report["query"]["normalized"]["overall_failure"]["state"],
                "success"
            );
            assert_eq!(
                report["query"]["normalized"]["overall_failure"]["value"],
                false
            );
            assert_eq!(report["query"]["health_score"]["score"], 100);
            assert_eq!(report["query"]["health_score"]["outcome"], "pass");
            assert!(report["query"]["health_score"]["provenance"].is_array());
            assert_eq!(
                report["query"]["health_score"]["warnings"],
                serde_json::json!([])
            );
        }

        #[test]
        #[ignore = "requires elevated physical-drive access; set CHKDSK_HARDWARE_REPORT to an absolute report path"]
        fn physical_disk_smart_diagnostic() {
            let report_path = std::env::var_os("CHKDSK_HARDWARE_REPORT")
                .expect("set CHKDSK_HARDWARE_REPORT to an absolute report path");
            let report_path = std::path::PathBuf::from(report_path);
            assert!(report_path.is_absolute(), "report path must be absolute");
            let disks = discover().expect("native disk discovery");
            let observations: Vec<_> = disks
                .iter()
                .map(|identity| diagnostic_observation(identity, &query(identity)))
                .collect();
            let report = serde_json::json!({
                "discovered": disks.len(),
                "observations": observations,
            });
            std::fs::write(report_path, serde_json::to_vec_pretty(&report).unwrap())
                .expect("write hardware report");
        }

        #[test]
        fn ata_error_and_status_output_registers_reject_failed_completions() {
            let rejected_read_data = [0x04, 1, 0, 0x4f, 0xc2, 0xa0, 0x51, 0];
            assert_eq!(
                ata_completion_error(&rejected_read_data),
                Some(AtaCommandError::Device {
                    status: 0x51,
                    error: 0x04,
                })
            );

            let rejected_return_status = [0x40, 0, 0, 0xf4, 0x2c, 0xa0, 0x60, 0];
            assert_eq!(
                ata_completion_error(&rejected_return_status),
                Some(AtaCommandError::Device {
                    status: 0x60,
                    error: 0x40,
                })
            );
        }

        #[test]
        fn stale_error_register_does_not_reject_clean_ata_status() {
            let clean_completion = [0x04, 0, 0, 0x4f, 0xc2, 0xa0, 0x50, 0];
            assert_eq!(ata_completion_error(&clean_completion), None);
        }

        fn assert_query_header(input: &[u8], property: u32) {
            assert_eq!(input.len() % 4, 0);
            assert_eq!(get_u32(input, 0), Some(property));
            assert_eq!(get_u32(input, 4), Some(PROPERTY_STANDARD_QUERY));
        }

        #[test]
        fn storage_property_input_preserves_empty_payload_and_query_abi() {
            let input = storage_property_input(STORAGE_DEVICE_PROPERTY, &[]).unwrap();

            assert_eq!(
                input,
                [
                    STORAGE_DEVICE_PROPERTY.to_le_bytes().as_slice(),
                    PROPERTY_STANDARD_QUERY.to_le_bytes().as_slice(),
                    &[0; 4],
                ]
                .concat()
            );
            assert_eq!(input.len(), 12);
            assert_query_header(&input, STORAGE_DEVICE_PROPERTY);
        }

        #[test]
        fn storage_property_input_pads_only_after_one_byte_payload() {
            let input =
                storage_property_input(STORAGE_DEVICE_SEEK_PENALTY_PROPERTY, &[0xA5]).unwrap();

            assert_eq!(input.len(), 12);
            assert_query_header(&input, STORAGE_DEVICE_SEEK_PENALTY_PROPERTY);
            assert_eq!(&input[8..9], &[0xA5]);
            assert_eq!(&input[9..], &[0; 3]);
        }

        #[test]
        fn storage_property_input_preserves_protocol_payload_and_header_abi() {
            let protocol: Vec<u8> = (0..40).collect();
            let input =
                storage_property_input(STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY, &protocol)
                    .unwrap();

            assert_eq!(input.len(), 48);
            assert_query_header(&input, STORAGE_DEVICE_PROTOCOL_SPECIFIC_PROPERTY);
            assert_eq!(&input[8..], protocol);
        }
    }
}

#[cfg(windows)]
pub use windows::{discover, query};
